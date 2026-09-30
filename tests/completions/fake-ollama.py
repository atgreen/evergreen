#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

"""Run an Ollama-shaped /api/chat server, then run the command under test.

Usage: fake-ollama.py COMMAND [ARG...]

The server echoes the user's message back as the assistant's reply, so the
Lisp side can assert that the text it sent survived the round trip in both
directions. EGCL_OLLAMA_PORT is exported to the child. Exits with the child's
status.
"""
import http.server
import json
import os
import subprocess
import sys
import threading


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
        prompt = ""
        for message in request.get("messages", []):
            if message.get("role") == "user":
                prompt = message.get("content", "")
        payload = json.dumps({
            "model": request.get("model"),
            "message": {
                "role": "assistant",
                "content": f"echo:{prompt} (model={request.get('model')})",
            },
            "done": True,
            "prompt_eval_count": 7,
            "eval_count": 11,
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args):
        pass


def main(argv):
    if not argv:
        print("usage: fake-ollama.py COMMAND [ARG...]", file=sys.stderr)
        return 2
    # Bind port 0 so concurrent runs cannot collide.
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = server.socket.getsockname()[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    env = dict(os.environ, EGCL_OLLAMA_PORT=str(port))
    try:
        return subprocess.call(argv, env=env)
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
