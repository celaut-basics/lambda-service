# lambda-service

Run a Python function in a child service, like AWS Lambda (without its runtime API).

- The **parent** is a Rust binary built on
  [celaut-project/service-library](https://github.com/celaut-project/service-library). It
  takes a function and an input over HTTP and answers with the result.
- The **child** is only MicroPython: a static binary and a short entry script. It has no
  server, no API and no HTTP client. It reads the function and the input from the directory
  that the parent shares with it (nodo's parent → child shared filesystem), runs the
  function, and writes the result back there. Then the parent stops it.

```
client ──HTTP──▶ parent (Rust) ──StartService──▶ node ──▶ child VM (MicroPython)
                   │  /lambda/<job>/input.json  ◀── virtio-fs share ──▶  /lambda/<job>/...
                   └─ waits for /lambda/<job>/output.json (written by the child)
```

__SIZES__

## The child's id is the hash of the function

Each function runs in **its own child service**, whose id is the hash of the function. The
parent ships the packed child (`child/`, its dependency `LAMBDA_CHILD`), with no function in
it. To run a function, the parent adds one file, `/fn.py`, with the code, to the
filesystem of that packed child, and sends the new service to the node with `StartService`.

- The id of that child is the SHA3-256 of the packed child plus `/fn.py`. The same code
  always gives the same id, and anyone who has the code and the packed child can compute it
  (`POST /id` does it without running anything). So the id names the function, and a
  receipt, a reputation or a payment can point at "this function on this interpreter".
- Only the filesystem changes: the parent splices bytes (`src/wire.rs`) and does not encode
  the Service again, so fields that `celaut-proto` does not know are kept. At the start,
  the parent reads the packed child from its own image and checks that its bytes hash to
  the packed id; it refuses to start otherwise.
- The first run of a function sends the child (about 0.43 MB, the interpreter included) to
  the node; the node keeps it, and the next runs start it by hash.

`"mode": "generic"` runs the packed child itself, which reads the code from the share. Its
id is the same for every function: no new service per function, but the id says nothing
about the code. The tradeoff:

| | built-in (default) | generic |
|---|---|---|
| child id | hash of the function | the same for all functions |
| first run of a function | sends the child (~0.43 MB) to the node, and the node builds its rootfs | starts by hash |
| registry | one entry per function | one entry |
| what a receipt proves | which code ran | only that the interpreter ran |

## API

All bodies are JSON. Port 8080 (`http`).

| Call | Body | Answer |
|---|---|---|
| `POST /invoke` | `{"code": "...", "input": <any JSON>, "mode": "built-in" \| "generic", "timeout_secs": 60, "initial_mu": <optional>}` | `{"ok", "output"` or `"error"` + `"traceback", "child_service_id", "mode", "timings_ms", "child": {"ms", "peak_rss_kb", "heap_used"}, "stop", "initial_mu"}` |
| `POST /id` | `{"code": "..."}` | `{"child_service_id"}`, without running it |
| `GET /health` | | `{"ok": true, "generic_child_id"}` |

The code must define `handler(event)`. `event` is the decoded `input`; the return value must
be JSON-serializable.

```python
def handler(event):
    words = event["text"].split()
    counts = {}
    for w in words:
        counts[w] = counts.get(w, 0) + 1
    return {"words": len(words), "top": sorted(counts.items(), key=lambda kv: -kv[1])[:3]}
```

```sh
curl -s http://<parent>:8080/invoke -H 'content-type: application/json' \
  -d "$(python3 -c 'import json; print(json.dumps({"code": open("examples/word_count.py").read(), "input": {"text": "the cat and the hat and the bat"}}))')"
```

More in `examples/`.

## How a run goes

1. The parent makes `/lambda/<job>/` and writes `input.json` (and `fn.py` in generic mode).
2. It asks the node for a quote of the child and starts it with `initial_mu` = twice the
   start cost plus twice the maintenance for the time limit (the rest comes back at the
   stop), the variable `LAMBDA_JOB=<job>` and the instance name `lam-<job>`.
3. The node boots the child VM. Its init mounts the share at `/lambda` and runs
   `/lambda.py` (`#!/micropython`: the node runs one path with no arguments).
4. The child runs `handler(input)` and writes `output.json` by a rename, so the parent
   never reads half a file. Then it waits for its job directory to go.
5. The parent reads the result, removes the job directory and stops the child.

A child that the parent does not stop exits when its job directory is gone, or after 10
minutes. A function that does not end before `timeout_secs` is stopped, and the answer is
an error.

## Layout

```
Cargo.toml, Cargo.lock, src/       the parent (Rust)
child/lambda.py                    the entry script of the child
amd64/, arm64/                     pack roots of the parent: .service/{Dockerfile,service.json,pack_config.json},
                                   sources by symlink, child -> ../child/<arch>
child/amd64/, child/arm64/         pack roots of the child
examples/                          functions and a test script
```

`nodo pack amd64` packs both (the child is a dependency). Plain `docker build` does not
follow symlinks: build from `cp -RL amd64 /tmp/x`.

## Limits: MicroPython is not CPython

- MicroPython 1.29 implements the Python 3.4 language with some later features
  (f-strings, `:=`, `async`/`await`). The modules of this build (`help("modules")`):
  `array`, `binascii`, `cmath`, `collections`, `deflate`, `errno`, `gc`, `hashlib`,
  `heapq`, `io`, `json`, `math`, `os`, `random`, `re`, `select`, `struct`, `sys`, `time`
  (and `micropython`, `uctypes`, `framebuf`, `machine`, `platform`, `vfs`, `websocket`,
  `_asyncio`). No pip, no C extensions, no `asyncio` package, no `numpy`, `decimal`,
  `datetime`, `itertools`, `functools` or `typing`.
- Some behaviours differ from CPython: for example `str` has no `casefold` or
  `format_map`, user classes get no `__del__`, and error messages are shorter. See
  <https://docs.micropython.org/en/v1.29.0/genrst/index.html>.
- This build has no network (`socket`, `ssl` off), no threads, no FFI, no `btree`, no
  FAT/littlefs. The child has no network anyway: the share is its only channel.
- The heap is 8 MiB (the unix port's default is 2 MiB on 64-bit, and the node gives the
  entry point no arguments, so `-X heapsize` cannot be used). Pages that the heap does
  not use are not touched.
- Input and output are JSON, up to 1 MiB of request body.
- One function per child; the child runs once and is stopped.

## Build pins

- MicroPython v1.29.0, the GitHub tag archive, sha256
  `8072f6ad42c0e3bbfca88ed09bf8f1106ce207a39129af920f70081b8f2fe286`; unix port,
  `VARIANT=standard`, static (musl), stripped; heap patched to 8 MiB (checked by `grep`).
- `alpine:3.22` and `rust:1.99.0-alpine3.22` by the digests of their multi-arch indexes;
  `Cargo.lock` with `--locked`. The apk packages of the build stages are not pinned (alpine
  keeps one version of each), so a rebuild can give another binary, and another id.

## License

GPL-3.0-only, because service-library and bee-rpc are.
