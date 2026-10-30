"""Disposable PostgreSQL/object-store restore drill; never operate on user data.

Called only by the managed Linux acceptance harness, after real checked agent
changes have landed. No schema/data edits simulate accepted source or validation.
This verifies source recovery and explicitly fenced replacement-node execution,
not Connected host reconciliation or resumption of an old in-flight task. Backup
artifacts contain test credentials and stay in private tempdir.
"""
import json
from hashlib import sha256
from pathlib import Path
import shlex
import shutil
import subprocess
import uuid


def exercise(h, landed, managed, pending):
    if not h.schema.startswith("th_test_") or "dbname=treeharbor_test" not in h.url:
        raise RuntimeError("restore drill requires the disposable acceptance database")
    target = "th_restore_" + uuid.uuid4().hex
    backup = h.root / "cold-backup"
    archive = backup / "metadata.dump"
    original_config = json.loads(h.config.read_text())
    original_store = Path(original_config["storage"]["root"])
    hidden_store = h.root / "original-objects-unavailable"
    restored_store = h.root / "restored-objects"
    created, hidden = False, False
    original_database = h.database
    credential_names = ("owner", "policy_admin", "reviewer", "integrator", "ci")
    original_credentials = {name: getattr(h, name) for name in credential_names}
    original_agents = list(h.agents)
    original_node_credentials = [(node, node["credential"]) for node in h.nodes]

    def sql(database, query):
        out = managed.docker("exec", h.pg, "psql", "-U", "treeharbor_test", "-d", database,
                             "-At", "-v", "ON_ERROR_STOP=1", "-c", query).stdout.strip()
        return out.splitlines()[-1] if out else ""

    def snapshot(database):
        names = json.loads(sql(database, f"SELECT json_agg(tablename ORDER BY tablename) FROM pg_tables WHERE schemaname='{h.schema}'"))
        rows = {}
        for name in names:
            if not name.startswith("th_") or not name.replace("_", "").isalnum():
                raise RuntimeError("unexpected table in acceptance schema")
            rows[name] = json.loads(sql(database, f"SELECT coalesce(jsonb_agg(to_jsonb(r) ORDER BY to_jsonb(r)::text),'[]') FROM {h.schema}.{name} r"))
        sequences = sql(database, f"SELECT coalesce(jsonb_agg(jsonb_build_array(sequencename,last_value) ORDER BY sequencename),'[]') FROM pg_sequences WHERE schemaname='{h.schema}'")
        return rows, json.loads(sequences)

    def start_service(label):
        with (h.root / ("restored-server-" + label + ".log")).open("w") as log:
            h.server = subprocess.Popen([str(h.binary), "serve", "--config", str(h.config)],
                                        env=h.env, stdout=log, stderr=subprocess.STDOUT)
        h.children.append(h.server)
        managed.wait_until(lambda: h.client("repository-info", check=False).returncode == 0 or h.server.poll() is not None)
        assert h.server.poll() is None, "restored service did not start"

    try:
        # Stop every writer before the metadata/object copy. No GC/admin writer
        # runs in this isolated harness. Existing runtime outcomes were collected
        # by the real tests; external teardown below does not create evidence.
        for child in h.children:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=10)
        if h.proxy:
            pid = h.transport / "proxy.pid"
            if pid.exists():
                managed.command("sudo", "kill", "-TERM", pid.read_text().strip(), check=False)
                h.proxy.wait(timeout=10)
                pid.unlink()
        for case in h.cases:
            managed.docker("rm", "--force", "treeharbor-" + case["attempt"], check=False)
        before = snapshot("treeharbor_test")
        # Use the installed Rust command and real matching container pg_dump;
        # do not synthesize dump bytes, snapshots or success.
        dump = h.root / "pg-dump"
        dump.write_text("#!/bin/sh\nexec /usr/bin/docker exec -i -e PGDATABASE -e PGUSER -e PGPASSWORD "
                        + shlex.quote(h.pg) + ' pg_dump "$@"\n')
        dump.chmod(0o700)
        states = [h.root / "import.json", h.root / "pack", h.root / "first-workspace.json",
                  h.root / "native-advance", h.owner, *h.agents, h.policy_admin,
                  h.reviewer, h.integrator, h.ci]
        for node in h.nodes:
            states.extend([node["credential"], node["root"]])
        assert len(states) <= 32 and len(set(states)) == len(states)
        assert all(path.exists() for path in states)
        arguments = ["backup", "create", "--config", h.config, "--output", backup,
                     "--pg-dump", dump, "--writers-stopped", "--timeout-seconds", "45"]
        for path in states:
            arguments.extend(["--state", path])
        receipt = h.value(*arguments)
        assert receipt["complete"] and receipt["restore_authorized"] is False
        manifest_bytes = (backup / "manifest.json").read_bytes()
        assert sha256(manifest_bytes).hexdigest() == receipt["manifest_sha256"]
        manifest = json.loads(manifest_bytes)
        assert manifest["version"] == 1 and manifest["schema"] == h.schema
        assert manifest["inputs"][2:] == [
            {"source": str(path.resolve()), "target": f"private-state/{index:02}"}
            for index, path in enumerate(states)]
        assert (backup / "service-config.json").read_bytes() == h.config.read_bytes()
        verified = h.value("backup", "verify", "--backup", backup,
                           "--manifest-sha256", receipt["manifest_sha256"],
                           "--max-bytes", str(receipt["bytes"]),
                           "--max-entries", str(receipt["entries"]))
        assert verified == receipt
        assert h.cli(*arguments, check=False).returncode != 0
        assert (backup / "manifest.json").read_bytes() == manifest_bytes
        assert snapshot("treeharbor_test") == before, "backup changed source metadata"
        print("PASS installed cold backup: actual operator create/verify preserves enumerated private state and exact inventory without overwriting output", flush=True)
        managed.docker("exec", h.pg, "createdb", "-U", "treeharbor_test", target)
        created = True
        with archive.open("rb") as source:
            subprocess.run(["docker", "exec", "-i", h.pg, "pg_restore", "-U", "treeharbor_test",
                            "--dbname", target, "--no-owner", "--single-transaction", "--exit-on-error"],
                           stdin=source, check=True, timeout=60)
        assert snapshot(target) == before, "restored tables or sequence values differ"
        print("PASS cold restore: actual pg_dump/pg_restore retains all tables, receipts, outbox, pins and sequences", flush=True)
        # Read recovered credentials from separate files populated from the
        # backup, not the original files. Copying node journals grants no right
        # to resume them; the old-node denial/fresh-node test below still applies.
        credentials = h.root / "restored-credentials"
        credentials.mkdir(mode=0o700)

        def recovered_credential(source):
            target_file = credentials / source.name
            assert not target_file.exists()
            shutil.copy2(backup / "private-state" / f"{states.index(source):02}", target_file)
            assert target_file.read_bytes() == source.read_bytes()
            return target_file

        for name, source in original_credentials.items():
            setattr(h, name, recovered_credential(source))
        h.agents = [recovered_credential(source) for source in original_agents]
        for node, source in original_node_credentials:
            node["credential"] = recovered_credential(source)
        shutil.copytree(backup / "objects", restored_store)
        original_store.rename(hidden_store)
        hidden = True
        assert not original_store.exists()
        config = json.loads((backup / "service-config.json").read_text())
        config["database_url"] = h.url.replace("dbname=treeharbor_test", "dbname=" + target)
        config["storage"] = dict(original_config["storage"], root=str(restored_store))
        managed.private_json(h.config, config)
        h.database = target
        start_service("original")
        assert snapshot(target) == before, "startup altered restored metadata"
        h.export_and_restore(landed, "cold-restored")
        changed = h.client("diff", "--authority", "standalone", "--epoch", "1", "change",
                           "--changeset", h.change["changeset"], "--version", "1", credential=h.agents[0])
        assert changed["after_snapshot"] == h.change["result_snapshot"]
        print("PASS cold restore: service uses restored objects, exports published history and reads accepted agent changes with original storage absent", flush=True)

        digest = landed["revision"].removeprefix("native:sha256:")
        descriptor = json.loads(sql(target, f"SELECT jsonb_build_object('tenant',tenant,'generation',generation) FROM {h.schema}.th_objects WHERE encode(digest,'hex')='{digest}'"))
        frame = restored_store / "managed-v1" / str(descriptor["generation"]) / "objects" / descriptor["tenant"] / digest[:2] / digest[2:]
        saved = frame.read_bytes()
        for label, corrupt in (("missing", None), ("corrupt", b"damaged restore fixture")):
            if corrupt is None:
                frame.unlink()
            else:
                frame.write_bytes(corrupt)
            # A fresh process cannot mask broken restored storage with cached bytes.
            h.server.kill()
            h.server.wait(timeout=10)
            start_service(label)
            output = h.root / ("rejected-restore-" + label)
            result = h.client("source-export", "--authority", "standalone", "--epoch", "1",
                              "--revision", landed["revision"], "--output", output, check=False)
            assert result.returncode != 0, label
            assert not output.exists(), "failed export exposed a successful output"
            frame.write_bytes(saved)
        h.server.kill()
        h.server.wait(timeout=10)
        start_service("repaired")
        h.export_and_restore(landed, "cold-repaired")
        print("PASS cold restore: missing and corrupt restored objects refuse export; restoring exact bytes recovers the complete Git history", flush=True)

        # The old node runtime was never started for this requested check. The
        # backup retains a request, not evidence of execution. Do not infer a
        # successful check or restart an old node merely because metadata exists.
        pending_before = h.native("check_status", h.ci, run=pending["check"])
        assert pending_before["state"] == "requested"
        assert h.native("land", h.integrator, check=False,
                        candidate=pending["candidate"]).returncode != 0
        for node in h.nodes:
            token_id = json.loads(node["credential"].read_text())["token_id"]
            h.admin("token-revoke", *h.scope, "--token-id", token_id)
            denied = h.cli(*h.node_args(node, "run-once"),
                           "--engine-socket", h.transport / "engine.sock", check=False)
            assert denied.returncode != 0
            assert denied.stderr.strip() == "credential file is invalid"
        # Restart the restored service so denial cannot depend on a local cache.
        h.server.kill()
        h.server.wait(timeout=10)
        start_service("fenced")
        for node in h.nodes:
            denied = h.cli(*h.node_args(node, "run-once"),
                           "--engine-socket", h.transport / "engine.sock", check=False)
            assert denied.returncode != 0
            assert denied.stderr.strip() == "credential file is invalid"
        # Keep Engine transport available: denial of the old worker must not be
        # explained by the deliberately stopped proxy or an inaccessible Engine.
        h.transport = h.root / "restored-proxy"
        h.transport.mkdir(mode=0o700)
        h.start_proxy()
        old = h.cli(*h.node_args(pending["node"], "run-once"),
                    "--engine-socket", h.transport / "engine.sock", check=False)
        assert old.returncode != 0
        assert old.stderr.strip() == "credential file is invalid"
        assert not (pending["files"] / "check-executions").exists()
        assert managed.docker("inspect", "treeharbor-" + pending["attempt"],
                              check=False).returncode != 0
        assert h.native("land", h.integrator, check=False,
                        candidate=pending["candidate"]).returncode != 0
        assert h.sql(f"SELECT count(*) FROM th_check_execution_evidence WHERE check_run='{pending['check']}'") == "0"
        print("PASS cold restore: requested check is not validation; revoked old nodes remain denied after restart and cannot execute or publish", flush=True)

        # Only an explicitly registered fresh node, fresh bounded volume and a
        # new approved program/candidate are allowed to resume useful work. No
        # old private node journal is copied into the replacement node.
        name = "restore-replacement-node"
        credential = h.principal(name, "node", ["runtime.control"])
        root = h.root / name
        identity = h.value("node", "initialize", "--root", root,
                           "--tenant", "managed-ci", "--principal", name)
        h.admin("node-register", "--tenant", "managed-ci", "--principal", name,
                "--storage", identity["storage"])
        index = len(h.nodes)
        h.nodes.append({"root": root, "credential": credential, "storage": identity["storage"]})
        fresh = h.check_case(node=index, timeout=17000)
        assert fresh["node"]["root"] != pending["node"]["root"]
        assert fresh["workspace"] != pending["workspace"]
        assert fresh["candidate"] != pending["candidate"]
        assert h.native("land", h.integrator, check=False,
                        candidate=fresh["candidate"]).returncode != 0
        h.completed_check(fresh, h.start(fresh))
        assert (fresh["files"] / "check-executions").read_bytes() == b"one check\n"
        result = h.native("land", h.integrator, key="restored-fresh-land",
                          candidate=fresh["candidate"])
        assert result["state"] == "landed"
        assert result == h.native("land", h.integrator, key="restored-fresh-land",
                                  candidate=fresh["candidate"])
        assert h.native("ref_resolve", h.policy_admin, name=fresh["target"])["head"] == result["revision"]
        h.export_and_restore(result, "replacement-node")
        h.native("check_release", h.ci, run=fresh["check"])
        h.cleanup_workspace(fresh, already_drained=True)
        assert h.native("check_status", h.ci, run=fresh["check"])["state"] == "collected"
        print("PASS cold restore: fresh node executes actual gVisor check, exact candidate lands once, exports to ordinary Git and cleans up", flush=True)

    finally:
        if h.server and h.server.poll() is None:
            h.server.kill()
            h.server.wait(timeout=10)
        if created:
            managed.docker("exec", h.pg, "dropdb", "-U", "treeharbor_test", "--force", target, check=False)
        if hidden:
            hidden_store.rename(original_store)
        h.database = original_database
        for name, source in original_credentials.items():
            setattr(h, name, source)
        h.agents = original_agents
        for node, source in original_node_credentials:
            node["credential"] = source
        managed.private_json(h.config, original_config)
