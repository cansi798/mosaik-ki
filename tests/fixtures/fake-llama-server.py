#!/usr/bin/env python3
"""Ersetzt llama-server in Tests: /v1/models, Echo-Chat, /quit. Schreibt argv nach <install>/args.json."""
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(HERE, "..", "args.json"), "w", encoding="utf-8") as fh:
    json.dump(sys.argv[1:], fh)
port = int(sys.argv[sys.argv.index("--port") + 1])


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def _send(self, code, obj):
        body = json.dumps(obj).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        self.wfile.flush()

    def do_GET(self):
        if self.path == "/v1/models":
            self._send(200, {"data": [{"id": "mosaik"}]})
        elif self.path == "/quit":
            self._send(200, {})
            os._exit(0)
        else:
            self._send(404, {})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        req = json.loads(self.rfile.read(length))
        last = req["messages"][-1]["content"]
        self._send(200, {"choices": [{"message": {"role": "assistant", "content": "ECHO: " + last}}]})


HTTPServer(("127.0.0.1", port), Handler).serve_forever()
