"""Verify a real Drakma GET and POST over loopback."""
import http.server
import os
import subprocess
import sys
import threading

requests = []


class Handler(http.server.BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(10)

    def reply(self, status, body):
        self.send_response(status)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        requests.append(("GET", self.path, b""))
        self.reply(200, b"hello from loopback")

    def do_POST(self):
        body = self.rfile.read(int(self.headers["Content-Length"]))
        requests.append(("POST", self.path, body))
        self.reply(201, b"received " + body)


with http.server.HTTPServer(("127.0.0.1", 0), Handler) as server:
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        result = subprocess.run(
            sys.argv[1:],
            env=dict(os.environ, DRAKMA_TEST_PORT=str(server.server_port)),
            timeout=float(os.environ.get("DRAKMA_TEST_TIMEOUT", "600")),
            check=False,
        )
        if result.returncode:
            sys.exit(result.returncode)
        assert requests == [
            ("GET", "/probe", b""),
            ("POST", "/probe", b"octet alias request"),
        ], requests
    finally:
        server.shutdown()
        worker.join(timeout=5)
print("DRAKMA-FIXTURE-OK")
