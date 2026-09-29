#!/usr/bin/env python3
"""A fake language server for ned-daemon's tests.

Logs every message it receives as a JSON line to the file named by its first
argument. On `initialized`, it asks for configuration and begins indexing
progress. It ends the progress when a document it's given contains "done",
and exits at once when one contains "crash". Each line containing ERROR, WARN
or HINT gets a diagnostic of that severity, published with the document's
version. Further arguments are flags:

- `fail`: print an error and exit instead of initializing;
- `pull`: offer pull diagnostics instead of publishing them;
- `cancel-once`: with `pull`, cancel the first diagnostic request;
- `ra`: act like rust-analyzer: report no progress, and only say it's
  quiescent once given a document containing "done";
- `document-changes`: answer renames with `documentChanges`;
- `rename-file`: answer renames with a file rename;
- `rename-error`: refuse renames with an error.

It renames the word at the position wherever it occurs as a whole word, in
open documents and in files under the root with the same extension, and
answers null where there's no word.

"""

import json
import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlparse

log = open(sys.argv[1], "a", buffering=1)
flags = set(sys.argv[2:])
stdin = sys.stdin.buffer
stdout = sys.stdout.buffer
requests = 0
documents = {}
root = None
SEVERITIES = {"ERROR": 1, "WARN": 2, "HINT": 4}


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


def diagnostics(text):
    found = []
    for line, content in enumerate(text.split("\n")):
        for word, severity in SEVERITIES.items():
            if word in content:
                character = content.index(word)
                diagnostic = {
                    "range": {
                        "start": {"line": line, "character": character},
                        "end": {"line": line, "character": character + len(word)},
                    },
                    "severity": severity,
                    "message": f"{word.lower()} here",
                    "source": "fake",
                }
                if word == "ERROR":
                    diagnostic["code"] = "F1"
                found.append(diagnostic)
    return found


def rename(params):
    uri = params["textDocument"]["uri"]
    position = params["position"]
    line = documents[uri].split("\n")[position["line"]]
    words = [
        m
        for m in re.finditer(r"\w+", line)
        if m.start() <= position["character"] < m.end()
    ]
    if not words:
        return None
    if "rename-file" in flags:
        return {
            "documentChanges": [{"kind": "rename", "oldUri": uri, "newUri": uri + "x"}]
        }
    suffix = Path(unquote(urlparse(uri).path)).suffix
    texts = {p.as_uri(): p.read_text() for p in sorted(root.rglob("*" + suffix))}
    texts.update(documents)
    changes = {}
    for file, text in texts.items():
        for number, content in enumerate(text.split("\n")):
            for m in re.finditer(rf"\b{re.escape(words[0].group())}\b", content):
                edit = {
                    "range": {
                        "start": {"line": number, "character": m.start()},
                        "end": {"line": number, "character": m.end()},
                    },
                    "newText": params["newName"],
                }
                changes.setdefault(file, []).append(edit)
    if "document-changes" in flags:
        edits = [
            {"textDocument": {"uri": file, "version": None}, "edits": edits}
            for file, edits in changes.items()
        ]
        return {"documentChanges": edits}
    return {"changes": changes}


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
    if method == "initialize" and "fail" in flags:
        print("fake: cannot start: broken on purpose", file=sys.stderr)
        sys.exit(2)
    if method == "initialize":
        root = Path(unquote(urlparse(params["rootUri"]).path))
        capabilities = {"textDocumentSync": 1, "renameProvider": True}
        if "pull" in flags:
            capabilities["diagnosticProvider"] = {
                "interFileDependencies": False,
                "workspaceDiagnostics": False,
            }
        result = {"capabilities": capabilities}
        if "ra" in flags:
            result["serverInfo"] = {"name": "rust-analyzer"}
        send({"id": message["id"], "result": result})
    elif method == "initialized":
        request(
            "workspace/configuration", {"items": [{"section": "a"}, {"section": "b"}]}
        )
        if "ra" not in flags:
            request("window/workDoneProgress/create", {"token": "index"})
            progress("begin")
    elif method in ("textDocument/didOpen", "textDocument/didChange"):
        document = params["textDocument"]
        if method == "textDocument/didOpen":
            text = document["text"]
        else:
            text = params["contentChanges"][-1]["text"]
        documents[document["uri"]] = text
        if "crash" in text:
            sys.exit(1)
        if "done" in text and "ra" in flags:
            status = {"health": "ok", "quiescent": True}
            send({"method": "experimental/serverStatus", "params": status})
        elif "done" in text:
            progress("end")
        if "pull" not in flags:
            publish = {
                "uri": document["uri"],
                "version": document["version"],
                "diagnostics": diagnostics(text),
            }
            send({"method": "textDocument/publishDiagnostics", "params": publish})
    elif method == "textDocument/diagnostic":
        if "cancel-once" in flags:
            flags.discard("cancel-once")
            error = {
                "code": -32802,
                "message": "cancelled",
                "data": {"retriggerRequest": True},
            }
            send({"id": message["id"], "error": error})
        else:
            text = documents[params["textDocument"]["uri"]]
            report = {"kind": "full", "items": diagnostics(text)}
            send({"id": message["id"], "result": report})
    elif method == "textDocument/rename" and "rename-error" in flags:
        error = {"code": -32803, "message": "cannot rename a keyword"}
        send({"id": message["id"], "error": error})
    elif method == "textDocument/rename":
        send({"id": message["id"], "result": rename(params)})
    elif method == "shutdown":
        send({"id": message["id"], "result": None})
    elif method == "exit":
        sys.exit(0)
