#!/usr/bin/env node
// Reference launcher plugin in Node.js: JSON-RPC 2.0, one message per line on stdio. Logs go to stderr.
const readline = require("node:readline");

const send = (obj) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...obj }) + "\n");
const reply = (id, result) => send({ id, result });

// stdin closing means: exit
readline.createInterface({ input: process.stdin }).on("line", (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return;
  }
  const { method, id, params = {} } = msg;
  switch (method) {
    case "initialize":
      return reply(id, { api: "0.1", capabilities: { streaming: false, actions: true } });
    case "query": {
      const text = params.text;
      const results = text
        ? [{
            id: "echo:" + text,
            title: text,
            subtitle: "Echo (Node.js)",
            icon: { glyph: "N" },
            score: 0.9,
            actions: [{ id: "noop", title: "Do nothing", default: true }],
          }]
        : [];
      return reply(id, { results });
    }
    case "execute":
      console.error("executed", JSON.stringify(params));
      return reply(id, { ok: true });
    case "shutdown":
      reply(id, {});
      return process.exit(0);
    default:
      if (id !== undefined) send({ id, error: { code: -32601, message: "unknown method" } });
  }
});
