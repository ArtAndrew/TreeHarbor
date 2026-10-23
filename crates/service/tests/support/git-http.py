#!/usr/bin/python3
"""Local smart HTTP fixture. Uses only a synthetic token and disposable Git data."""
import base64
import http.server
import json
import os
import pathlib
import subprocess
import sys
import urllib.parse

root, port_file, audit_file = sys.argv[1:]
expected = "Basic " + base64.b64encode(
    b"x-access-token:synthetic-only-not-a-credential"
).decode()


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def handle_git(self):
        valid = self.headers.get("Authorization") == expected
        with open(audit_file, "a", encoding="utf-8") as audit:
            audit.write(json.dumps({"method": self.command, "authorized": valid}) + "\n")
        if not valid:
            self.send_error(401)
            return
        if pathlib.Path(root, "redirect").exists():
            self.send_response(302)
            self.send_header("Location", "/redirect-target")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        url = urllib.parse.urlsplit(self.path)
        if not url.path.startswith("/demo.git/"):
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", "0"))
        if not 0 <= length <= 1048576:
            self.send_error(413)
            return
        environment = {
            "PATH": "/usr/bin:/bin",
            "GIT_PROJECT_ROOT": root,
            "GIT_HTTP_EXPORT_ALL": "1",
            "PATH_INFO": url.path,
            "QUERY_STRING": url.query,
            "REQUEST_METHOD": self.command,
            "CONTENT_TYPE": self.headers.get("Content-Type", ""),
            "CONTENT_LENGTH": str(length),
            "REMOTE_USER": "fixture",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": "/dev/null",
        }
        response = subprocess.run(
            ["/usr/bin/git", "http-backend"],
            input=self.rfile.read(length), capture_output=True, env=environment,
            timeout=15, check=True,
        ).stdout
        if url.path.endswith('/git-receive-pack') and pathlib.Path(root, 'drop-push-response').exists():
            # The backend already applied the mutation. Lose only its response.
            self.close_connection = True
            return
        headers, body = response.split(b"\r\n\r\n", 1)
        self.send_response(200)
        for line in headers.decode().split("\r\n"):
            name, value = line.split(":", 1)
            if name.lower() != "status":
                self.send_header(name, value.strip())
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    do_GET = handle_git
    do_POST = handle_git


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
pathlib.Path(port_file).write_text(str(server.server_port), encoding="ascii")
server.serve_forever()
