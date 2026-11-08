#!/usr/bin/env python3
"""Reference launcher plugin: JSON-RPC 2.0, one message per line on stdio. Logs go to stderr.

"Stream back" shows how an action streams output into the launcher's output panel: it
replies at once, then sends the words from a thread while the main loop keeps reading stdin
(so a `$/cancel` from Esc arrives).
"""
import json
import sys
import threading
import time

lock = threading.Lock()  # stdout is written from two threads
cancelled = set()


def send(obj):
    with lock:
        print(json.dumps(obj), flush=True)


def reply(id_, result):
    send({"jsonrpc": "2.0", "id": id_, "result": result})


def stream_back(exec_, text):
    for word in text.split():
        if exec_ in cancelled:
            print("stream", exec_, "cancelled", file=sys.stderr, flush=True)
            return
        time.sleep(0.15)
        send({"jsonrpc": "2.0", "method": "output", "params": {"exec": exec_, "append": word + " "}})
    send({"jsonrpc": "2.0", "method": "output", "params": {"exec": exec_, "done": True}})


for line in sys.stdin:  # stdin closing means: exit
    msg = json.loads(line)
    method, id_, params = msg.get("method"), msg.get("id"), msg.get("params", {})
    if method == "initialize":
        reply(id_, {"api": "0.1", "capabilities": {"streaming": False, "actions": True}})
    elif method == "query":
        text = params["text"]
        results = []
        if text:
            results.append({
                "id": "echo:" + text,
                "title": text,
                "subtitle": "Echo",
                "icon": {"glyph": "E"},
                "score": 0.9,
                "actions": [
                    {"id": "stream", "title": "Stream back", "default": True, "output": True},
                    {"id": "noop", "title": "Do nothing"},
                ],
            })
        reply(id_, {"results": results})
    elif method == "execute":
        print("executed", params, file=sys.stderr, flush=True)
        if params["action_id"] == "stream":
            reply(id_, {"ok": True, "output": {"title": "Echo", "streaming": True}})
            threading.Thread(target=stream_back, args=(params["exec"], params["query"]), daemon=True).start()
        else:
            reply(id_, {"ok": True})
    elif method == "$/cancel" and "exec" in params:
        cancelled.add(params["exec"])
    elif method == "shutdown":
        reply(id_, {})
        break
    elif id_ is not None:
        send({"jsonrpc": "2.0", "id": id_, "error": {"code": -32601, "message": "unknown method"}})
