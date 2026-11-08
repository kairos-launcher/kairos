# Node.js

JavaScript or TypeScript, with npm's packages for web APIs and file formats.

- **Needs:** Node.js on the user's PATH (or Bun).
- **Example:** [`examples/plugins/echo-node`](https://github.com/kairos-launcher/kairos/tree/main/examples/plugins/echo-node),
  no dependencies.

## The example

`readline` hands over one line per message; `process.stdout.write` sends one line per reply.

::: code-group

<<< @/../examples/plugins/echo-node/plugin.toml [plugin.toml]

<<< @/../examples/plugins/echo-node/echo.js [echo.js]

:::

## Try it

```sh
KAIROS_PLUGINS=$PWD/examples/plugins kairos      # then type: echojs hello
```

Without the launcher:

```sh
cd examples/plugins/echo-node
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
              '{"jsonrpc":"2.0","id":2,"method":"query","params":{"qid":1,"text":"hi","limit":8}}' | node echo.js
```

## Streaming output

Node.js keeps reading stdin while timers and promises run, so streaming needs no threads. Reply to
`execute` first, then send `output` notifications, and stop when a `$/cancel` for that `exec`
arrives:

```js
const cancelled = new Set();

async function streamBack(exec, text) {
  for (const word of text.split(/\s+/)) {
    if (cancelled.has(exec)) return;
    await new Promise((r) => setTimeout(r, 150));
    send({ method: "output", params: { exec, append: word + " " } });
  }
  send({ method: "output", params: { exec, done: true } });
}

// in the message handler:
//   case "execute":
//     reply(id, { ok: true, output: { title: "Echo", streaming: true } });
//     return streamBack(params.exec, params.query);
//   case "$/cancel":
//     if (params.exec !== undefined) cancelled.add(params.exec);
//     return;
```

## Tips

- **Logs go to stderr:** `console.error(...)`. `console.log` writes to stdout and breaks nothing
  only if it prints valid protocol messages.
- **Settings** arrive in `initialize` as `params.settings`, merged with your defaults.
- **Dependencies:** ship `node_modules` inside the plugin folder (users do not run `npm install`),
  or bundle everything into one file with esbuild: `npx esbuild src/main.ts --bundle
  --platform=node --outfile=plugin.js`.
- **TypeScript:** Node.js 22.18 or newer runs `.ts` files directly (`exec = ["node",
  "plugin.ts"]`, type annotations only), or use Bun (`exec = ["bun", "plugin.ts"]`), or compile
  with `tsc` / esbuild and ship the `.js`.
- **Proxy:** the launcher passes its proxy setting in `HTTPS_PROXY`; make sure your HTTP client
  uses it.
- **Start time** is a few tens of milliseconds, paid once: the process keeps running while you
  type.
