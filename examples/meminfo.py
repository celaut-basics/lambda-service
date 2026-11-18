# The memory of the child VM and of the MicroPython process, from /proc. With
# {"sleep": N} it waits N seconds first, to measure the VM from the host.
def handler(event):
    import time
    info = {}
    with open("/proc/meminfo") as f:
        for line in f:
            k, v = line.split(":")
            if k in ("MemTotal", "MemFree", "MemAvailable", "Shmem", "Cached"):
                info[k] = int(v.split()[0])
    with open("/proc/self/status") as f:
        for line in f:
            if line.startswith(("VmRSS", "VmHWM", "VmSize")):
                info[line.split(":")[0]] = int(line.split()[1])
    time.sleep(event.get("sleep", 0))
    return info
