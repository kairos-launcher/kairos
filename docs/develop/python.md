# Python

Python is the quickest way to write a plugin: one file, no build step, and the standard library
has JSON, HTTP, SQLite and threads built in.

- **Needs:** Python 3 on the user's machine (Linux has it; on Windows users install it).
- **Example:** [`examples/plugins/echo-py`](https://github.com/kairos-launcher/kairos/tree/main/examples/plugins/echo-py),
  the complete reference plugin, including streamed output.

## The smallest plugin

This one rolls dice when you type `roll 20`. A folder with two files:

`dice/plugin.toml`

```toml
id = "dice"
name = "Dice"
version = "0.1.0"

[run]
exec = ["python3", "dice.py"]   # relative to the plugin folder

[query]
triggers = ["roll "]
global = false                  # only answer when the trigger is typed
timeout_ms = 500
```

`dice/dice.py`

```python
import json, random, sys

def reply(id_, result):
    print(json.dumps({"jsonrpc": "2.0", "id": id_, "result": result}), flush=True)

for line in sys.stdin:                      # stdin closing means: exit
    msg = json.loads(line)
    method, id_, params = msg.get("method"), msg.get("id"), msg.get("params", {})
    if method == "initialize":
        reply(id_, {"api": "0.1"})
    elif method == "query":
        text = params["text"].strip()       # trigger already stripped: "20"
        results = []
        if text.isdigit() and int(text) > 0:
            n = random.randint(1, int(text))
            results.append({
                "id": f"roll:{text}:{n}",
                "title": f"🎲 {n}",
                "subtitle": f"d{text}",
                "score": 0.9,
                "actions": [{"id": "noop", "title": "OK", "default": True}],
            })
        reply(id_, {"results": results})
    elif method == "execute":
        reply(id_, {"ok": True})
    elif method == "shutdown":
        reply(id_, {})
        break
```

Install it with `plugin install ~/dice` in the launcher, then type `roll 20`.

## The reference plugin

`echo-py` echoes what you type. Its **Stream back** action shows how to stream text into the
answer panel: it replies at once, then sends words from a thread while the main loop keeps reading
stdin, so a `$/cancel` (the user pressed Ctrl+C) still arrives.

::: code-group

<<< @/../examples/plugins/echo-py/plugin.toml [plugin.toml]

<<< @/../examples/plugins/echo-py/echo.py [echo.py]

:::

## Try it

```sh
KAIROS_PLUGINS=$PWD/examples/plugins kairos      # then type: echopy hello
```

Without the launcher:

```sh
cd examples/plugins/echo-py
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
              '{"jsonrpc":"2.0","id":2,"method":"query","params":{"qid":1,"text":"hi","limit":8}}' | python3 echo.py
```

## Tips

- **Flush every line.** Use `print(..., flush=True)`; buffered output reaches the launcher late
  and the query times out.
- **Logs go to stderr:** `print("debug", file=sys.stderr)`. Anything else on stdout that is not
  JSON is ignored, but it is wasted work.
- **Settings** arrive in `initialize` as `params["settings"]`, already merged with the defaults
  from your `[[settings]]`. Read them once: a change restarts the plugin.
- **Windows** has `python` (or the `py` launcher), not `python3`. Give each system its command:

  ```toml
  [run.exec]
  windows = ["python", "echo.py"]
  linux   = ["python3", "echo.py"]
  macos   = ["python3", "echo.py"]
  ```

- **Dependencies:** prefer the standard library, so users need nothing installed. If you need a
  package, vendor it into the plugin folder (`pip install --target lib requests`) and add
  `sys.path.insert(0, os.path.join(os.path.dirname(__file__), "lib"))` at the top.
- **Start time** is a few tens of milliseconds for a plain script; keep heavy imports out of the
  top level if only some queries need them.
