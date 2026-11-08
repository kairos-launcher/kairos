#!/usr/bin/env python3
"""Scriptable test plugin for src/plugin/external/protocol_tests.rs.

The query text picks a scenario (pid, settings, noise, stream:N, stream-nodone, sleep:MS,
wait, error, crash, huge, icon); the execute action id picks a reply (ok, stay, fail, fail-nomsg,
or an output scenario: out-static, stream, stream-early, stream-forever, stream-crash, stream-err).
If settings has "marker", events ("init <pid>", "shutdown <pid>", "cancel <qid>",
"cancel-exec <exec>") are appended to it. `wait` streams nothing until a `$/cancel` for its qid
arrives; `stream-forever` appends from a thread until a `$/cancel` for its exec arrives.
"""
import json
import os
import sys
import threading
import time

settings = {}
data_dir = None
# Output can stream from a thread while the main loop answers requests.
write_lock = threading.Lock()
cancelled_execs = set()


def send(obj):
    with write_lock:
        print(json.dumps(obj), flush=True)


def reply(id_, result):
    send({"jsonrpc": "2.0", "id": id_, "result": result})


def mark(event):
    if settings.get("marker"):
        with open(settings["marker"], "a") as f:
            f.write(f"{event} {os.getpid()}\n")


def item(id_, title, **extra):
    return {"id": id_, "title": title, **extra}


def notify(qid, results, done):
    send({"jsonrpc": "2.0", "method": "results", "params": {"qid": qid, "results": results, "done": done}})


def query(id_, p):
    text, qid = p["text"], p["qid"]
    if text == "pid":
        reply(id_, {"results": [item("pid", str(os.getpid()))]})
    elif text == "data_dir":
        reply(id_, {"results": [item("data_dir", data_dir or "")]})
    elif text == "settings":
        reply(id_, {"results": [item("settings", json.dumps(settings, sort_keys=True))]})
    elif text == "noise":
        print("this is not json", flush=True)
        send({"jsonrpc": "2.0", "method": "log", "params": {"msg": "hi"}})
        send({"jsonrpc": "2.0", "id": 999999, "result": {"results": [item("wrong-id", "x")]}})
        reply(id_, {"results": [item("noise", "clean")]})
    elif text.startswith("stream:"):
        n = int(text.split(":")[1])
        reply(id_, {"results": [item("r0", "from reply")], "streaming": True})
        notify(qid - 1, [item("stale", "old qid")], True)
        for i in range(1, n + 1):
            notify(qid, [item(f"r{i}", f"batch {i}")], i == n)
    elif text == "stream-nodone":
        reply(id_, {"results": [], "streaming": True})
        notify(qid, [item("partial", "partial")], False)
    elif text == "wait":
        reply(id_, {"results": [], "streaming": True})
        for line in sys.stdin:
            msg = json.loads(line)
            if msg.get("method") == "$/cancel" and msg["params"]["qid"] == qid:
                mark(f"cancel {qid}")
                break
    elif text.startswith("sleep:"):
        time.sleep(int(text.split(":")[1]) / 1000)
        reply(id_, {"results": [item("late", "late")]})
    elif text == "error":
        send({"jsonrpc": "2.0", "id": id_, "error": {"code": -32000, "message": "boom"}})
    elif text == "crash":
        sys.exit(3)
    elif text == "huge":
        reply(id_, {"results": [item("huge", "x" * (2 << 20))]})
    elif text == "icon":
        reply(id_, {"results": [
            item("rel", "rel", icon={"path": "img/a.png"}),
            item("abs", "abs", icon={"path": "/abs/b.png"}),
            item("lazy", "lazy", icon={"id": "known"}),
        ]})
    else:
        reply(id_, {"results": []})


def output(exec_, **params):
    send({"jsonrpc": "2.0", "method": "output", "params": {"exec": exec_, **params}})


def forever(exec_):
    i = 0
    while exec_ not in cancelled_execs:
        output(exec_, append=f"{i} ")
        i += 1
        time.sleep(0.02)


def execute(id_, p):
    action, exec_ = p["action_id"], p.get("exec")
    if action == "out-static":
        reply(id_, {"ok": True, "output": {"title": "T", "text": "all of it", "actions": [
            {"id": "again", "title": "Again", "default": True},
            {"id": "c", "title": "Copy", "kind": "copy", "value": "v"},
        ]}})
    elif action == "stream":
        reply(id_, {"ok": True, "output": {"title": "S", "text": "a", "streaming": True}})
        output(exec_ - 1, append="stale")
        output(exec_, append="b")
        output(exec_, append="c", actions=[{"id": "more", "title": "More"}])
        output(exec_, done=True)
        output(exec_, append="after done")
    elif action == "stream-early":
        output(exec_, append="early")
        reply(id_, {"ok": True, "output": {"streaming": True}})
        output(exec_, text="replaced", done=True)
    elif action == "stream-forever":
        reply(id_, {"ok": True, "output": {"streaming": True}})
        threading.Thread(target=forever, args=(exec_,), daemon=True).start()
    elif action == "stream-crash":
        reply(id_, {"ok": True, "output": {"streaming": True}})
        output(exec_, append="x")
        os._exit(4)
    elif action == "echo-input":
        reply(id_, {"ok": True, "output": {"text": p.get("input", "<none>")}})
    elif action == "stream-err":
        reply(id_, {"ok": True, "output": {"streaming": True}})
        output(exec_, error="quota exceeded")
    elif action == "stay":
        reply(id_, {"ok": True, "hide": False})
    elif action == "fail":
        reply(id_, {"ok": False, "message": "nope"})
    elif action == "fail-nomsg":
        reply(id_, {"ok": False})
    else:
        reply(id_, {"ok": True})


for line in sys.stdin:
    msg = json.loads(line)
    method, id_, params = msg.get("method"), msg.get("id"), msg.get("params", {})
    if method == "initialize":
        settings = params.get("settings") or {}
        data_dir = params.get("data_dir")
        mark("init")
        reply(id_, {"api": "0.1"})
    elif method == "query":
        query(id_, params)
    elif method == "execute":
        execute(id_, params)
    elif method == "icon":
        reply(id_, {"path": "x.png"} if params["icon_id"] == "known" else None)
    elif method == "$/cancel" and "exec" in params:
        cancelled_execs.add(params["exec"])
        mark(f"cancel-exec {params['exec']}")
    elif method == "shutdown":
        mark("shutdown")
        reply(id_, {})
        break
    elif id_ is not None:
        send({"jsonrpc": "2.0", "id": id_, "error": {"code": -32601, "message": "unknown method"}})
