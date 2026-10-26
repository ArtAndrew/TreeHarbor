"""Disposable PostgreSQL/object-store restore drill; never operate on user data.

Called only by the managed Linux acceptance harness, after real checked agent
changes have landed. No schema/data edits simulate accepted source or validation.
This verifies durable source recovery, not Connected host reconciliation or node
resumption. Backup artifacts contain test credentials and stay in private tempdir.
"""
import json
from pathlib import Path
import shutil
import subprocess
import uuid


def exercise(h, landed, managed):
    if not h.schema.startswith("th_test_") or "dbname=treeharbor_test" not in h.url:
        raise RuntimeError("restore drill requires the disposable acceptance database")
    target = "th_restore_" + uuid.uuid4().hex
    backup = h.root / "cold-backup"
    backup.mkdir(mode=0o700)
    archive = backup / "metadata.dump"
    original_config = json.loads(h.config.read_text())
    original_store = Path(original_config["storage"]["root"])
    hidden_store = h.root / "original-objects-unavailable"
    restored_store = h.root / "restored-objects"
    created, hidden = False, False

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
        for case in h.cases:
            managed.docker("rm", "--force", "treeharbor-" + case["attempt"], check=False)
        before = snapshot("treeharbor_test")
        with archive.open("xb") as output:
            subprocess.run(["docker", "exec", h.pg, "pg_dump", "-U", "treeharbor_test",
                            "-d", "treeharbor_test", "--format=custom", "--schema", h.schema],
                           stdout=output, check=True, timeout=60)
        archive.chmod(0o600)
        shutil.copytree(original_store, backup / "objects")
        managed.docker("exec", h.pg, "createdb", "-U", "treeharbor_test", target)
        created = True
        with archive.open("rb") as source:
            subprocess.run(["docker", "exec", "-i", h.pg, "pg_restore", "-U", "treeharbor_test",
                            "--dbname", target, "--no-owner", "--single-transaction", "--exit-on-error"],
                           stdin=source, check=True, timeout=60)
        assert snapshot(target) == before, "restored tables or sequence values differ"
        print("PASS cold restore: actual pg_dump/pg_restore retains all tables, receipts, outbox, pins and sequences", flush=True)
        shutil.copytree(backup / "objects", restored_store)
        original_store.rename(hidden_store)
        hidden = True
        assert not original_store.exists()
        config = dict(original_config)
        config["database_url"] = h.url.replace("dbname=treeharbor_test", "dbname=" + target)
        config["storage"] = dict(original_config["storage"], root=str(restored_store))
        managed.private_json(h.config, config)
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
    finally:
        if h.server and h.server.poll() is None:
            h.server.kill()
            h.server.wait(timeout=10)
        if created:
            managed.docker("exec", h.pg, "dropdb", "-U", "treeharbor_test", "--force", target, check=False)
        if hidden:
            hidden_store.rename(original_store)
        managed.private_json(h.config, original_config)
