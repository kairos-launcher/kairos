#!/usr/bin/env python3
"""Minimal remote plugin server (plain HTTP on 127.0.0.1:8080).

The launcher only talks https, so put a TLS terminator in front, e.g.
    caddy reverse-proxy --from localhost:8443 --to 127.0.0.1:8080
and run:  TOKEN=secret python3 server.py
Store the same token for the launcher:  printf secret | kairos --set-token echo-remote
"""
import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

TOKEN = os.environ.get("TOKEN", "secret")


class Rpc(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.headers.get("Authorization") != f"Bearer {TOKEN}":
            self.send_response(401)
            self.end_headers()
            return
        req = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        method, p = req["method"], req.get("params", {})
        if method == "query":
            result = {"results": [{
                "id": "1",
                "title": f"echo: {p['text']}",
                "subtitle": "from the remote server",
                "actions": [
                    # Host-side actions: no round trip back to this server.
                    {"id": "open", "title": "Open example.com", "kind": "open_url", "value": "https://example.com/"},
                    {"id": "copy", "title": "Copy text", "kind": "copy", "value": p["text"]},
                    # Plain action: the launcher POSTs `execute` here.
                    {"id": "ping", "title": "Ask the server"},
                ],
            }]}
        elif method == "execute":
            result = {"copy": f"pong: {p['query']}", "hide": True}
        else:  # initialize, shutdown, icon
            result = {}
        body = json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


HTTPServer(("127.0.0.1", 8080), Rpc).serve_forever()
