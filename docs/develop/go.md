# Go

Like Rust, Go builds one binary that starts in milliseconds and needs nothing installed, with a
simpler toolchain: JSON is in the standard library, and one command cross-compiles for every
system.

- **Needs:** nothing for users; you build one binary per system.
- **Example:** [`examples/plugins/echo-go`](https://github.com/kairos-launcher/kairos/tree/main/examples/plugins/echo-go),
  standard library only.

## The example

`bufio.Scanner` reads one message per line (its buffer is raised to 1 MiB, the largest line the
launcher sends); `send` writes one line and flushes it.

::: code-group

<<< @/../examples/plugins/echo-go/plugin.toml [plugin.toml]

<<< @/../examples/plugins/echo-go/main.go [main.go]

<<< @/../examples/plugins/echo-go/go.mod{txt} [go.mod]

:::

## Build and try it

```sh
cd examples/plugins/echo-go
go build                                                 # echo-go (echo-go.exe on Windows)
cd ../../.. && KAIROS_PLUGINS=$PWD/examples/plugins kairos   # then type: echogo hello
```

Without the launcher:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
              '{"jsonrpc":"2.0","id":2,"method":"query","params":{"qid":1,"text":"hi","limit":8}}' | ./echo-go
```

## Streaming output

Reply to `execute`, then send `output` notifications from a goroutine while the main loop keeps
reading stdin. Guard `send` with a mutex, as two goroutines now write to stdout:

```go
var mu sync.Mutex
var cancelled sync.Map // exec number -> true

func send(msg object) {
	mu.Lock()
	defer mu.Unlock()
	msg["jsonrpc"] = "2.0"
	line, _ := json.Marshal(msg)
	out.Write(append(line, '\n'))
	out.Flush()
}

func streamBack(exec int64, text string) {
	for _, word := range strings.Fields(text) {
		if _, stop := cancelled.Load(exec); stop {
			return
		}
		time.Sleep(150 * time.Millisecond)
		send(object{"method": "output", "params": object{"exec": exec, "append": word + " "}})
	}
	send(object{"method": "output", "params": object{"exec": exec, "done": true}})
}

// in "execute" (add Exec int64 and Query string to the params struct):
//   reply(msg.ID, object{"ok": true, "output": object{"title": "Echo", "streaming": true}})
//   go streamBack(msg.Params.Exec, msg.Params.Query)
// in "$/cancel":
//   cancelled.Store(msg.Params.Exec, true)
```

## Tips

- **Cross-compile** from any system, no extra tools:

  ```sh
  GOOS=windows GOARCH=amd64 go build -ldflags="-s -w" -o echo-go.exe
  GOOS=linux   GOARCH=amd64 go build -ldflags="-s -w" -o echo-go
  GOOS=darwin  GOARCH=arm64 go build -ldflags="-s -w" -o echo-go
  ```

  `-s -w` drops debug information and makes the binary about a third smaller.
- **HTTP:** `net/http` honours `HTTPS_PROXY` / `NO_PROXY` by default, which the launcher sets from
  its proxy setting.
- **Settings** arrive in `initialize` as `params.settings`; add a `Settings json.RawMessage` field
  to the params struct and decode what you need.
- **Logs go to stderr:** `fmt.Fprintln(os.Stderr, ...)` or the `log` package (which writes to
  stderr by default).
