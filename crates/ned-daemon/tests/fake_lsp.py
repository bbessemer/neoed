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
- `rename-error`: refuse renames with an error;
- `links`: answer definitions with `LocationLink`s;
- `format`: offer formatting, which strips trailing spaces;
- `flycheck`: ask for saves; on one, like rust-analyzer's `cargo check`,
  begin progress, then 200 ms later publish, unversioned, the document's
  diagnostics plus an error for each line containing CARGO, and end it;
- `flycheck-slow`: with `flycheck`, never end that progress.


It renames the word at the position wherever it occurs as a whole word, in
open documents and in files under the root with the same extension, and
answers null where there's no word. The first occurrence right after `fn `,
or else in the requested document, is the definition; the others are the
references.

"""

import json
import re
import sys
import time
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


def progress(kind, token="index"):
    send({"method": "$/progress", "params": {"token": token, "value": {"kind": kind}}})


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


def cargo_errors(text):
    return [
        {
            "range": {
                "start": {"line": line, "character": 0},
                "end": {"line": line, "character": len(content)},
            },
            "severity": 1,
            "message": "cargo here",
            "source": "cargo",
        }
        for line, content in enumerate(text.split("\n"))
        if "CARGO" in content
    ]


def occurrences(params):
    """The (uri, range) of each whole-word occurrence of the word at the
    position, the declaration first: the first right after `fn `, else the
    requested document's first. None if there's no word there."""
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
    suffix = Path(unquote(urlparse(uri).path)).suffix
    texts = {p.as_uri(): p.read_text() for p in sorted(root.rglob("*" + suffix))}
    texts.update(documents)
    found = []
    for file, text in sorted(texts.items(), key=lambda item: item[0] != uri):
        for number, content in enumerate(text.split("\n")):
            for m in re.finditer(rf"\b{re.escape(words[0].group())}\b", content):
                start = {"line": number, "character": m.start()}
                end = {"line": number, "character": m.end()}
                declares = content[: m.start()].endswith("fn ")
                found.append((not declares, file, {"start": start, "end": end}))
    found.sort(key=lambda occurrence: occurrence[0])
    return [(file, range_) for _, file, range_ in found]


def rename(params):
    found = occurrences(params)
    if found is None:
        return None
    if "rename-file" in flags:
        uri = params["textDocument"]["uri"]
        return {
            "documentChanges": [{"kind": "rename", "oldUri": uri, "newUri": uri + "x"}]
        }
    changes = {}
    for file, range_ in found:
        changes.setdefault(file, []).append(
            {"range": range_, "newText": params["newName"]}
        )
    if "document-changes" in flags:
        edits = [
            {"textDocument": {"uri": file, "version": None}, "edits": edits}
            for file, edits in changes.items()
        ]
        return {"documentChanges": edits}
    return {"changes": changes}


def references(params):
    found = occurrences(params) or [None]
    return [{"uri": file, "range": range_} for file, range_ in found[1:]]


def definition(params):
    found = occurrences(params)
    if not found:
        return None
    file, range_ = found[0]
    if "links" in flags:
        whole = {
            "start": {"line": range_["start"]["line"], "character": 0},
            "end": range_["end"],
        }
        link = {"targetUri": file, "targetRange": whole, "targetSelectionRange": range_}
        return [link]
    return {"uri": file, "range": range_}


def strip_trailing_spaces(text):
    edits = []
    for line, content in enumerate(text.split("\n")):
        kept = len(content.rstrip(" "))
        if kept < len(content):
            span = {
                "start": {"line": line, "character": kept},
                "end": {"line": line, "character": len(content)},
            }
            edits.append({"range": span, "newText": ""})
    return edits


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
        if "flycheck" in flags:
            sync = {"openClose": True, "change": 1, "save": {"includeText": False}}
            capabilities["textDocumentSync"] = sync
        if "format" in flags:
            capabilities["documentFormattingProvider"] = True
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
    elif method == "textDocument/didSave" and "flycheck" in flags:
        uri = params["textDocument"]["uri"]
        request("window/workDoneProgress/create", {"token": "check"})
        progress("begin", "check")
        time.sleep(0.2)
        found = diagnostics(documents[uri]) + cargo_errors(documents[uri])
        publish = {"uri": uri, "diagnostics": found}
        send({"method": "textDocument/publishDiagnostics", "params": publish})
        if "flycheck-slow" not in flags:
            progress("end", "check")
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
    elif method == "textDocument/references":
        send({"id": message["id"], "result": references(params)})
    elif method == "textDocument/definition":
        send({"id": message["id"], "result": definition(params)})
    elif method == "textDocument/formatting":
        text = documents[params["textDocument"]["uri"]]
        send({"id": message["id"], "result": strip_trailing_spaces(text)})
    elif method == "shutdown":
        send({"id": message["id"], "result": None})
    elif method == "exit":
        sys.exit(0)
