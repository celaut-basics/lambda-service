#!/micropython
# The entry point of the lambda child: PID 1 of its VM, run by the kernel through the
# line above (the node runs one path with no arguments, so the shebang names the
# interpreter). There is no server, no API and no network client here: the only channel
# is /lambda, the directory that the parent exports to its children (a nodo shared
# filesystem, role "guest" in service.json).
#
# One run = one job. The parent sets LAMBDA_JOB to the name of a directory in /lambda:
#   /lambda/<job>/input.json   the event, written by the parent
#   /lambda/<job>/fn.py        the function, written by the parent (generic mode only)
#   /lambda/<job>/output.json  written here, last, by a rename: the parent waits for it
# /fn.py in the image holds the function when the parent built a child for it: the parent
# adds that one file to the packed child, so the id of that child is the hash of the
# function. The packed child has no /fn.py: it is the generic child, which runs the fn.py
# of the job.
import sys, os, json, gc, time

ROOT = "/lambda"
t0 = time.ticks_ms()


def peak_kb():
    # VmHWM: the peak RSS of this process, in kB, from /proc (the guest init mounts it).
    try:
        with open("/proc/self/status") as f:
            for line in f:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1])
    except OSError:
        pass
    return None


def finish(job_dir, result):
    result["ms"] = time.ticks_diff(time.ticks_ms(), t0)
    result["peak_rss_kb"] = peak_kb()
    result["heap_used"] = gc.mem_alloc()
    tmp = job_dir + "/output.json.tmp"
    with open(tmp, "w") as f:
        f.write(json.dumps(result))
    os.rename(tmp, job_dir + "/output.json")


def read(path):
    with open(path) as f:
        return f.read()


def main():
    job = os.getenv("LAMBDA_JOB") or ""
    if not job or len(job) > 64 or not all(c.isalpha() or c.isdigit() or c in "-_" for c in job):
        print("lambda: LAMBDA_JOB is missing or not a plain name:", repr(job))
        return
    job_dir = ROOT + "/" + job
    try:
        try:
            source = read("/fn.py")
            mode = "built-in"
        except OSError:
            source = read(job_dir + "/fn.py")
            mode = "generic"
        event = json.loads(read(job_dir + "/input.json"))
        scope = {"__name__": "fn"}
        exec(source, scope)
        handler = scope.get("handler")
        if handler is None:
            raise NameError("the function file defines no handler(event)")
        output = handler(event)
        finish(job_dir, {"ok": True, "output": output, "mode": mode})
    except Exception as e:
        import io
        buf = io.StringIO()
        sys.print_exception(e, buf)
        try:
            finish(job_dir, {"ok": False, "error": type(e).__name__ + ": " + str(e),
                             "traceback": buf.getvalue()})
        except Exception as e2:
            print("lambda: cannot write the result:", e2)


main()
# The parent reads the result and stops this instance. If it does not (it died), PID 1
# exits once the job directory is gone or after WAIT_S: the VM ends and the node frees
# the instance. Not exiting at once keeps a child that ends before its StartService has
# answered from looking like a failed launch.
WAIT_S = 600
job = os.getenv("LAMBDA_JOB") or "-"
for _ in range(WAIT_S * 5):
    try:
        os.stat(ROOT + "/" + job)
    except OSError:
        break
    time.sleep_ms(200)
