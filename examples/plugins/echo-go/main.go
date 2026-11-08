// Reference launcher plugin in Go: JSON-RPC 2.0, one message per line on stdio. Logs go to stderr.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
)

type message struct {
	Method string          `json:"method"`
	ID     json.RawMessage `json:"id,omitempty"` // absent for notifications
	Params struct {
		Text string `json:"text"`
	} `json:"params"`
}

type object = map[string]any

var out = bufio.NewWriter(os.Stdout)

// send writes one message as a single line and flushes it, so the launcher sees it at once.
func send(msg object) {
	msg["jsonrpc"] = "2.0"
	line, _ := json.Marshal(msg)
	out.Write(append(line, '\n'))
	out.Flush()
}

func reply(id json.RawMessage, result any) { send(object{"id": id, "result": result}) }

func main() {
	in := bufio.NewScanner(os.Stdin)
	in.Buffer(make([]byte, 64*1024), 1<<20) // the launcher sends lines up to 1 MiB

	// stdin closing means: exit
	for in.Scan() {
		var msg message
		if json.Unmarshal(in.Bytes(), &msg) != nil {
			continue
		}
		switch msg.Method {
		case "initialize":
			reply(msg.ID, object{"api": "0.1", "capabilities": object{"streaming": false, "actions": true}})
		case "query":
			results := []object{}
			if text := msg.Params.Text; text != "" {
				results = append(results, object{
					"id":       "echo:" + text,
					"title":    text,
					"subtitle": "Echo (Go)",
					"icon":     object{"glyph": "G"},
					"score":    0.9,
					"actions":  []object{{"id": "noop", "title": "Do nothing", "default": true}},
				})
			}
			reply(msg.ID, object{"results": results})
		case "execute":
			fmt.Fprintln(os.Stderr, "executed", in.Text())
			reply(msg.ID, object{"ok": true})
		case "shutdown":
			reply(msg.ID, object{})
			return
		default:
			if msg.ID != nil {
				send(object{"id": msg.ID, "error": object{"code": -32601, "message": "unknown method"}})
			}
		}
	}
}
