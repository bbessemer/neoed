#!/usr/bin/env python3
"""A fake language server for ned-daemon's tests.

Logs every message it receives as a JSON line to the file named by its first
argument. On `initialized`, it asks for configuration and begins indexing
progress. It ends the progress when a document it's given contains "done",
and exits at once when one contains "crash". With a second argument,
`fail`, it prints an error and exits instead of initializing.
"""

import json
import sys

log = open(sys.argv[1], "a", buffering=1)
stdin = sys.stdin.buffer
stdout = sys.stdout.buffer
requests = 0


def send(message):
    body = json.dumps({"jsonrpc": "2.0", **message}).encode()
    stdout.write(b"Content-Length: %d\r\n\r\n%s" % (len(body), body))
    stdout.flush()


def request(method, params):
    global requests
    requests += 1
    send({"id": f"s{requests}", "method": method, "params": params})


def progress(kind):
    send(
        {"method": "$/progress", "params": {"token": "index", "value": {"kind": kind}}}
    )


def read():
    length = None
    while line := stdin.readline():
        name, _, value = line.decode().partition(":")
        if not line.strip():
            return json.loads(stdin.read(length))
        if name.lower() == "content-length":
            length = int(value)
    return None


while (message := read()) is not None:
    log.write(json.dumps(message) + "\n")
    method = message.get("method")
    params = message.get("params") or {}
    if method == "initialize" and sys.argv[2:] == ["fail"]:
        print("fake: cannot start: broken on purpose", file=sys.stderr)
        sys.exit(2)
    if method == "initialize":
        send({"id": message["id"], "result": {"capabilities": {"textDocumentSync": 1}}})
    elif method == "initialized":
        request(
            "workspace/configuration", {"items": [{"section": "a"}, {"section": "b"}]}
        )
        request("window/workDoneProgress/create", {"token": "index"})
        progress("begin")
    elif method in ("textDocument/didOpen", "textDocument/didChange"):
        if method == "textDocument/didOpen":
            text = params["textDocument"]["text"]
        else:
            text = params["contentChanges"][-1]["text"]
        if "crash" in text:
            sys.exit(1)
        if "done" in text:
            progress("end")
    elif method == "shutdown":
        send({"id": message["id"], "result": None})
    elif method == "exit":
        sys.exit(0)
