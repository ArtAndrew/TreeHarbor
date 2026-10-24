#!/usr/bin/env python3
"""Real TLS ingress, Rust receiver and PostgreSQL; no installed GitHub App."""
import hashlib
import hmac
import http.client
import json
import os
from pathlib import Path
import select
import socket
import shutil
import ssl
import subprocess
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parent.parent
PRODUCT = ROOT
BINARY = Path(os.environ.get("TH_TREEHARBOR_BIN", PRODUCT / "target/debug/treeharbor"))
CADDY = Path(os.environ["TH_CADDY_BIN"])
PSQL = Path(os.environ.get("TH_PSQL", shutil.which("psql") or "psql"))
DATABASE = os.environ["TH_SERVICE_TEST_DATABASE_URL"]
CADDYFILE = PRODUCT / "deploy/webhook/Caddyfile"


def main():
    os.umask(0o077)
    schema = "th_tls_" + uuid.uuid4().hex
    children = []
    with tempfile.TemporaryDirectory(prefix="th-tls-", dir="/tmp") as name:
        directory = Path(name)
        environment = os.environ.copy()
        environment.update(XDG_DATA_HOME=str(directory / "data"), XDG_CONFIG_HOME=str(directory / "config"))
        def run(*arguments):
            result = subprocess.run([str(a) for a in arguments], capture_output=True, text=True, timeout=45, env=environment)
            if result.returncode:
                raise AssertionError(f"command failed: {arguments[0]}\n{result.stdout}\n{result.stderr}")
            return result.stdout
        def sql(statement):
            return run(PSQL, DATABASE, "-v", "ON_ERROR_STOP=1", "-c", statement)
        def value(statement):
            return run(PSQL, DATABASE, "-At", "-v", "ON_ERROR_STOP=1", "-c", statement).strip()
        def count():
            return int(value(f'SELECT count(*) FROM "{schema}".th_github_events'))
        def wait_until(predicate, seconds=10):
            deadline = time.monotonic() + seconds
            while not predicate():
                assert time.monotonic() < deadline, "observable condition did not arrive"
                time.sleep(0.05)
        def write(path, value):
            path.write_text(value)
            path.chmod(0o600)
        def start(*arguments):
            log = (directory / f"process-{len(children)}.log").open("w")
            child = subprocess.Popen([str(a) for a in arguments], stdout=log, stderr=log, env=environment)
            children.append((child, log))
            return child
        def lock_repository():
            log = (directory / f"process-{len(children)}.log").open("w")
            process = subprocess.Popen([str(PSQL), DATABASE, "-qAt", "-v", "ON_ERROR_STOP=1"],
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log,
                                       text=True, env=environment)
            children.append((process, log))
            process.stdin.write(f'BEGIN; LOCK TABLE "{schema}".th_repositories IN ACCESS EXCLUSIVE MODE; SELECT pg_backend_pid();\n')
            process.stdin.flush()
            assert select.select([process.stdout], [], [], 5)[0], "lock was not acquired"
            return process, int(process.stdout.readline().strip())
        def unlock(process):
            process.communicate("COMMIT;\n", timeout=5)
            assert process.returncode == 0
        assert run(CADDY, "version").startswith("v2.11.7 "), "install the pinned Caddy"
        sql(f'CREATE SCHEMA "{schema}"')
        try:
            config = {"database_url": DATABASE, "schema": schema,
                      "socket": str(directory / "webhook.sock"), "pool_size": 1,
                      "max_connections": 4, "request_timeout_ms": 10000}
            admin = directory / "admin.json"
            write(admin, json.dumps(config))
            run(BINARY, "admin", "--config", admin, "migrate")
            scope = ["--tenant", "tls-test", "--repository", str(uuid.uuid4())]
            def administer(*arguments):
                return run(BINARY, "admin", "--config", admin, *arguments)
            administer("repository-create", *scope, "--authority", "connected")
            for principal, kind, permissions in [("owner", "human", ["repository.write", "read"]),
                                                 ("bridge", "bridge", ["host.read"])]:
                administer("principal-create", "--tenant", "tls-test", "--principal", principal, "--kind", kind)
                grants = [item for permission in permissions for item in ["--permission", permission]]
                administer("access-set", *scope, "--principal", principal, *grants)
                administer("token-issue", *scope, "--principal", principal, *grants,
                           "--output", directory / f"{principal}.json")
            control = directory / "control.sock"
            control_config = directory / "control.json"
            write(control_config, json.dumps(dict(config, socket=str(control))))
            server = start(BINARY, "serve", "--config", control_config)
            for _ in range(100):
                if control.exists():
                    break
                assert server.poll() is None
                time.sleep(0.05)
            assert control.exists()
            def command(principal, value):
                path = directory / "request.json"
                write(path, json.dumps(value))
                return json.loads(run(BINARY, "client", "--socket", control, "--credential-file",
                                      directory / f"{principal}.json", "github", "--request", path))
            binding = json.loads((PRODUCT / "crates/metadata/tests/fixtures/github-bind-v1.json").read_text())
            command("owner", binding)
            probe = str(uuid.uuid4())
            command("bridge", {"action": "probe_begin", "key": "begin", "epoch": 1,
                                "binding": binding["binding"], "probe": probe})
            observation = json.loads((PRODUCT / "crates/integrations-github/tests/fixtures/probe-v1.json").read_text())
            command("bridge", {"action": "probe_finish", "key": "finish", "epoch": 1,
                                "binding": binding["binding"], "probe": probe, "observation": observation})
            channel = str(uuid.uuid4())
            command("owner", {"action": "event_channel_create", "key": "channel", "epoch": 1,
                               "channel": channel, "specification": {"binding": binding["binding"],
                               "max_events": 8, "max_bytes": 8192, "max_aliases": 2}})
            secret = b"public-tls-prototype-secret-not-a-real-app-key"
            secret_path = directory / "secret"
            write(secret_path, secret.decode())
            receiver_config = directory / "webhook.json"
            write(receiver_config, json.dumps({"database": config, "app_id": 7,
                "keys": [{"id": str(uuid.uuid4()), "secret_file": str(secret_path)}]}))
            receiver = start(BINARY, "webhook-serve", "--config", receiver_config)
            for _ in range(100):
                if (directory / "webhook.sock").exists():
                    break
                assert receiver.poll() is None
                time.sleep(0.05)
            assert (directory / "webhook.sock").exists()
            cert, key = directory / "server.crt", directory / "server.key"
            openssl_config = directory / "openssl.cnf"
            write(openssl_config, "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=extensions\n"
                  "[dn]\nCN=localhost\n[extensions]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n"
                  "basicConstraints=critical,CA:TRUE\nkeyUsage=critical,digitalSignature,keyEncipherment,keyCertSign\n"
                  "extendedKeyUsage=serverAuth\n")
            run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                "-keyout", key, "-out", cert, "-config", openssl_config)
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                port = listener.getsockname()[1]
            environment.update(TH_WEBHOOK_ADDRESS=f"127.0.0.1:{port}", TH_WEBHOOK_BIND="127.0.0.1",
                               TH_WEBHOOK_TLS_CERT=str(cert), TH_WEBHOOK_TLS_KEY=str(key),
                               TH_WEBHOOK_SOCKET=config["socket"])
            missing = subprocess.run([str(CADDY), "validate", "--config", str(CADDYFILE), "--adapter", "caddyfile"],
                                     env={k: v for k, v in environment.items() if not k.startswith("TH_WEBHOOK_")},
                                     capture_output=True, timeout=10)
            assert missing.returncode != 0, "missing TLS configuration was accepted"
            run(CADDY, "validate", "--config", CADDYFILE, "--adapter", "caddyfile")
            proxy = start(CADDY, "run", "--config", CADDYFILE, "--adapter", "caddyfile")
            context = ssl.create_default_context(cafile=str(cert))
            def request(body=b"", signature=None, path="/github/events", method="POST", extra=None, duplicate=None, unread=False):
                connection = http.client.HTTPSConnection("127.0.0.1", port, context=context, timeout=20)
                headers = {"Content-Type": "application/json", "X-GitHub-Event": "ping",
                           "X-GitHub-Delivery": str(uuid.uuid4()),
                           "X-Hub-Signature-256": signature or "sha256=" + hmac.new(secret, body, hashlib.sha256).hexdigest()}
                headers.update(extra or {})
                try:
                    connection.putrequest(method, path)
                    for header, value in headers.items():
                        connection.putheader(header, value)
                    if duplicate:
                        connection.putheader(*duplicate)
                    connection.putheader("Content-Length", str(len(body)))
                    connection.endheaders(body)
                    if unread:
                        return connection
                    response = connection.getresponse()
                    return response.status, response.read()
                finally:
                    if not unread:
                        connection.close()
            for _ in range(100):
                try:
                    if request(method="GET", path="/health")[0] == 404:
                        break
                except ConnectionRefusedError:
                    assert proxy.poll() is None
                    time.sleep(0.05)
            body = b'{ "zen": "raw bytes stay raw", "hook_id": 17, "hook": {"id": 17,"app_id": 7} }\n'
            assert request(body) == (200, b'{"status":"ping_verified"}')
            signature = "sha256=" + hmac.new(secret, body, hashlib.sha256).hexdigest()
            assert request(body + b" ", signature=signature)[0] == 401
            assert request(body, extra={"Content-Encoding": "gzip"})[0] == 415
            assert request(body, path="/github/events?unexpected=1")[0] == 404
            assert request(body, path="/commands")[0] == 404
            assert request(body, method="GET")[0] == 404
            assert request(body, duplicate=("X-GitHub-Event", "ping"))[0] == 400
            assert request(body, extra={"X-Oversized": "x" * 16384})[0] == 431
            assert request(body + b" " * (1024 * 1024 - len(body)))[0] == 200
            assert request(b" " * (1024 * 1024 + 1))[0] == 413
            h2_body = directory / "ping-body.json"
            h2_body.write_bytes(body)
            h2_result = run("curl", "--silent", "--show-error", "--fail", "--http2", "--noproxy", "*",
                            "--max-time", "10", "--cacert", cert, "--data-binary", f"@{h2_body}",
                            "-H", "Content-Type: application/json", "-H", "X-GitHub-Event: ping",
                            "-H", f"X-GitHub-Delivery: {uuid.uuid4()}", "-H", f"X-Hub-Signature-256: {signature}",
                            "--write-out", "\n%{http_code} %{http_version}", f"https://127.0.0.1:{port}/github/events")
            assert h2_result == '{"status":"ping_verified"}\n200 2', h2_result
            vectors = json.loads((PRODUCT / "crates/github-webhook/tests/fixtures/vectors-v1.json").read_text())
            event = next(v for v in vectors["vectors"] if v["name"] == "checks")
            group = event["body"].encode()
            delivery = str(uuid.uuid4())
            headers = {"X-GitHub-Event": "merge_group", "X-GitHub-Delivery": delivery}
            assert request(group, extra=headers) == (200, b'{"status":"durably_received"}')
            assert request(group, extra=headers) == (200, b'{"status":"durably_received"}')
            retained = run(PSQL, DATABASE, "-At", "-v", "ON_ERROR_STOP=1", "-c",
                           f'SELECT encode(body,\'hex\') FROM "{schema}".th_github_events').strip()
            assert retained == group.hex(), "signed group body changed or was stored twice"
            assert request(group + b" ", extra=headers)[0] == 409
            receiver.terminate()
            receiver.wait(timeout=5)
            private_marker = "TH_PRIVATE_HEADER_MARKER"
            assert request(group, extra=dict(headers, **{"X-Diagnostic": private_marker}))[0] == 502
            receiver = start(BINARY, "webhook-serve", "--config", receiver_config)
            for _ in range(100):
                if (directory / "webhook.sock").exists():
                    break
                assert receiver.poll() is None
                time.sleep(0.05)
            assert request(group, extra=headers) == (200, b'{"status":"durably_received"}')
            assert count() == 1

            def new_group(digit):
                changed = json.loads(event["body"])
                changed["merge_group"]["head_sha"] = digit * 40
                changed["merge_group"]["head_commit"]["id"] = digit * 40
                return json.dumps(changed, ensure_ascii=False).encode()
            def delivery_headers():
                return {"X-GitHub-Event": "merge_group", "X-GitHub-Delivery": str(uuid.uuid4())}

            # Observe the commit independently, then discard the unread ACK and
            # kill the proxy. Retrying the original bytes/UUID must deduplicate.
            lost_body, lost_headers = new_group("d"), delivery_headers()
            unread = request(lost_body, extra=lost_headers, unread=True)
            try:
                wait_until(lambda: count() == 2)
                proxy.kill()
                proxy.wait(timeout=5)
            finally:
                unread.close()
            proxy = start(CADDY, "run", "--config", CADDYFILE, "--adapter", "caddyfile")
            def proxy_ready():
                assert proxy.poll() is None
                try:
                    return request(method="GET", path="/health")[0] == 404
                except ConnectionRefusedError:
                    return False
            wait_until(proxy_ready)
            assert request(lost_body, extra=lost_headers)[0] == 200
            assert count() == 2

            # Kill Caddy while the real receiver is blocked on the repository.
            # The outcome is unknown (0 or 1 commits); recovery must settle at 1.
            blocked_body, blocked_headers = new_group("e"), delivery_headers()
            blocker, backend_pid = lock_repository()
            pending = request(blocked_body, extra=blocked_headers, unread=True)
            try:
                wait_until(lambda: int(value(f'SELECT count(*) FROM pg_stat_activity WHERE {backend_pid}=ANY(pg_blocking_pids(pid))')) > 0)
                proxy.kill()
                proxy.wait(timeout=5)
            finally:
                pending.close()
                unlock(blocker)
            assert count() in (2, 3)
            proxy = start(CADDY, "run", "--config", CADDYFILE, "--adapter", "caddyfile")
            wait_until(proxy_ready)
            assert request(blocked_body, extra=blocked_headers)[0] == 200
            assert count() == 3

            # A real DB lock outlives the database's 5-second lock timeout. No success can
            # be fabricated; after release, the exact delivery can still commit.
            timeout_body, timeout_headers = new_group("f"), delivery_headers()
            blocker, backend_pid = lock_repository()
            pending = request(timeout_body, extra=timeout_headers, unread=True)
            try:
                wait_until(lambda: int(value(f'SELECT count(*) FROM pg_stat_activity WHERE {backend_pid}=ANY(pg_blocking_pids(pid))')) > 0)
                reply = pending.getresponse()
                reply_body = reply.read()
                assert (reply.status, reply_body) == (503, b'{"status":"retry_original_delivery"}'), (reply.status, reply_body)
                assert count() == 3
            finally:
                pending.close()
                unlock(blocker)
            assert request(timeout_body, extra=timeout_headers)[0] == 200
            assert count() == 4
            assert int(value(f'SELECT count(*) FROM "{schema}".th_outbox WHERE kind=\'github.events.received\'')) == 4

            def raw_tls(data, deadline):
                with socket.create_connection(("127.0.0.1", port), timeout=5) as tcp:
                    with context.wrap_socket(tcp, server_hostname="127.0.0.1") as stream:
                        stream.settimeout(deadline)
                        stream.sendall(data)
                        # Timeout is a failure: the configured server must reply
                        # or close without the client finishing the request.
                        return stream.recv(4096)
            slow_header = raw_tls(b"POST /github/events HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Slow: ", 8)
            assert not slow_header or slow_header.startswith((b"HTTP/1.1 4", b"HTTP/1.1 5"))
            slow_body = raw_tls((f"POST /github/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
                f"Content-Type: application/json\r\nContent-Length: {len(group)}\r\n"
                f"X-GitHub-Event: merge_group\r\nX-GitHub-Delivery: {uuid.uuid4()}\r\n"
                f"X-Hub-Signature-256: sha256={hmac.new(secret, group, hashlib.sha256).hexdigest()}\r\n\r\n").encode() + group[:4], 22)
            assert not slow_body or slow_body.startswith((b"HTTP/1.1 4", b"HTTP/1.1 5"))
            assert count() == 4
            # Replace the explicit TLS key/certificate and restart; the old test
            # trust anchor must now fail, while a client trusting the new one works.
            previous_context = context
            run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                "-keyout", key, "-out", cert, "-config", openssl_config)
            run(CADDY, "validate", "--config", CADDYFILE, "--adapter", "caddyfile")
            proxy.terminate()
            proxy.wait(timeout=5)
            context = ssl.create_default_context(cafile=str(cert))
            proxy = start(CADDY, "run", "--config", CADDYFILE, "--adapter", "caddyfile")
            wait_until(proxy_ready)
            old_trust = http.client.HTTPSConnection("127.0.0.1", port, context=previous_context, timeout=5)
            try:
                old_trust.request("GET", "/health")
            except ssl.SSLCertVerificationError:
                pass
            else:
                raise AssertionError("replaced certificate was not loaded")
            finally:
                old_trust.close()
            assert request(group, extra=headers)[0] == 200
            assert count() == 4
            command("owner", {"action": "event_channel_revoke", "key": "revoke", "epoch": 1,
                               "channel": channel})
            assert request(group, extra=headers)[0] == 409
            try:
                untrusted = http.client.HTTPSConnection("127.0.0.1", port, timeout=5)
                untrusted.request("GET", "/health")
            except ssl.SSLCertVerificationError:
                pass
            else:
                raise AssertionError("untrusted certificate accepted")
            finally:
                untrusted.close()
            proxy.terminate()
            proxy.wait(timeout=5)
            logs = "\n".join(path.read_text() for path in directory.glob("process-*.log"))
            assert private_marker not in logs and secret.decode() not in logs and signature not in logs
            errors = [json.loads(line) for line in logs.splitlines() if line.startswith("{")]
            errors = [entry for entry in errors if entry.get("logger") == "http.log.error"]
            assert errors, "log filtering was not exercised"
            assert all("headers" not in e.get("request", {}) and "uri" not in e.get("request", {}) for e in errors)
            print("PASS: real TLS certificate verification and untrusted-certificate rejection")
            print("PASS: original signed bytes accepted; modified bytes rejected")
            print("PASS: compression, query suffix, wrong method and control route rejected")
            print("PASS: oversized body rejected through actual Caddy and receiver")
            print("PASS: exact 1 MiB body accepted, oversized headers rejected and HTTP/2 signature preserved")
            print("PASS: duplicate headers rejected; group bytes retained once across replay and restart")
            print("PASS: same-delivery/different-body, receiver outage and channel revocation fail closed")
            print("PASS: Caddy SIGKILL before and after database commit; exact retry retains one fact/outbox")
            print("PASS: locked database deadline and slow headers/body never receive success")
            print("PASS: actual proxy error logs omit request headers, URI and synthetic secret markers")
            print("PASS: missing TLS config rejected and explicit certificate replacement verified after restart")
            print("Local ingress acceptance only; public callback and real App remain unverified.")
        finally:
            for child, log in reversed(children):
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
                log.close()
            sql(f'DROP SCHEMA "{schema}" CASCADE')


if __name__ == "__main__":
    main()
