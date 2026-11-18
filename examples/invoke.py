#!/usr/bin/env python3
"""POST examples/<name>.py with an input to a running parent.

    python3 examples/invoke.py <ip:port> <example> '<input json>' [built-in|generic]
"""
import json, sys, urllib.request

target, name, event = sys.argv[1], sys.argv[2], json.loads(sys.argv[3])
mode = sys.argv[4] if len(sys.argv) > 4 else "built-in"
with open(f"{__file__.rsplit('/', 1)[0]}/{name}.py") as f:
    body = json.dumps({"code": f.read(), "input": event, "mode": mode}).encode()
req = urllib.request.Request(f"http://{target}/invoke", body, {"content-type": "application/json"})
print(json.dumps(json.load(urllib.request.urlopen(req, timeout=700)), indent=2))
