use super::{archive_activation::native, archive_stage::assert_rejected, *};
use crate::native::ok;
use sha2::{Digest, Sha256};
use std::process::Stdio;
use treeharbor_core::ObjectId;
use treeharbor_metadata::{Error, access::Permission};

const FREEZE: &str = "reverse-cutover-freeze";

fn export(f: &Fixture, owner: &Path, freeze_key: &str, output: &Path) -> Output {
    f.cli_with_credential(
        owner,
        &[
            "git-export",
            "--epoch",
            "2",
            "--freeze-key",
            freeze_key,
            "--output",
            output.to_str().unwrap(),
        ],
    )
}

fn exported(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn verify_projection(f: &Fixture, owner: &Path, key: &str, directory: &Path) -> Output {
    f.cli_with_credential(
        owner,
        &[
            "handoff-verify",
            "--key",
            key,
            "--epoch",
            "2",
            "--handoff",
            directory.to_str().unwrap(),
        ],
    )
}

pub(super) async fn exercise(
    f: &Fixture,
    server: Process,
    owner: &Path,
    worker: &Path,
    changeset: &str,
    landed: &Value,
) {
    let entry = Uuid::new_v4();
    ok(native(
        f,
        owner,
        json!({"action":"enqueue","key":"queue-before-freeze","epoch":2,
        "entry":entry,"target":"stable","changeset":changeset,"version":2}),
    ));
    let freeze = json!({"action":"handoff_freeze","key":FREEZE,"epoch":2});
    let snapshot = json!({"action":"handoff_snapshot","freeze_key":FREEZE,"epoch":2});
    let captured = ok(native(f, owner, json!({"action":"ref_snapshot"})));
    assert_rejected(native(f, worker, freeze.clone()), "forbidden");
    let active = f.sql.query_one(r#"
        UPDATE th_outbox SET state='running',worker=$1,lease_until=clock_timestamp()+interval '1 minute'
        WHERE kind='native.queue.requested' AND operation_key='queue-before-freeze' RETURNING id
        "#, &[&Uuid::new_v4()]).await.unwrap().get::<_, i64>(0);
    assert_rejected(native(f, owner, freeze.clone()), "native_handoff_busy");
    f.sql
        .execute(
            "UPDATE th_outbox SET state='pending',worker=NULL,lease_until=NULL WHERE id=$1",
            &[&active],
        )
        .await
        .unwrap();
    f.sql
        .batch_execute(
            r#"
        CREATE FUNCTION reject_handoff_receipt() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.kind='native.handoff.freeze.v1' THEN
                RAISE EXCEPTION 'injected handoff receipt failure';
            END IF;
            RETURN NEW;
        END
        $$;
        CREATE TRIGGER reject_handoff_receipt BEFORE INSERT ON th_operations
        FOR EACH ROW EXECUTE FUNCTION reject_handoff_receipt();
        "#,
        )
        .await
        .unwrap();
    assert_rejected(native(f, owner, freeze.clone()), "internal_error");
    assert_eq!(
        ok(native(f, owner, json!({"action":"ref_snapshot"}))),
        captured
    );
    f.sql.batch_execute("DROP TRIGGER reject_handoff_receipt ON th_operations; DROP FUNCTION reject_handoff_receipt();").await.unwrap();
    let frozen = ok(native(f, owner, freeze.clone()));
    assert_eq!(frozen["state"], "frozen");
    assert_eq!(frozen["snapshot"]["epoch"], 2);
    assert_eq!(frozen["snapshot"]["refs"], captured["refs"]);
    assert_eq!(frozen["snapshot"]["refs"][0]["head"], landed["revision"]);
    assert_eq!(
        ok(native(
            f,
            owner,
            json!({"action":"queue_status","entry":entry})
        ))["state"],
        "queued"
    );
    let start = json!({"action":"candidate_start","key":"candidate-while-frozen","epoch":2,
        "entry":entry,"candidate":Uuid::new_v4()});
    assert_rejected(native(f, worker, start), "repository_fenced");
    assert_rejected(
        native(
            f,
            owner,
            json!({"action":"handoff_freeze","key":"different-freeze","epoch":2}),
        ),
        "repository_fenced",
    );
    assert_rejected(
        native(f, owner, json!({"action":"ref_snapshot"})),
        "repository_fenced",
    );
    assert_rejected(
        f.cli_with_credential(
            owner,
            &[
                "source-export",
                "--authority",
                "standalone",
                "--epoch",
                "2",
                "--revision",
                landed["revision"].as_str().unwrap(),
                "--output",
                f.dir.path().join("generic-frozen-export").to_str().unwrap(),
            ],
        ),
        "repository_fenced",
    );

    // A registered PR source is readable in the active repository, but a
    // frozen branch export grants only the exact frozen roots.
    let mut db = Database::new(f.config.clone())
        .unwrap()
        .connect(true)
        .await
        .unwrap();
    let auth = db
        .metadata
        .authorize(
            &Credential::load(owner).unwrap().token,
            &f.scope,
            Permission::Read,
        )
        .await
        .unwrap();
    let origin: String = f
        .sql
        .query_one(
            "SELECT origin#>>'{head,native_revision}' FROM th_archive_pull_origins",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let RevisionRef::Native(origin) = origin.parse().unwrap() else {
        panic!("expected archived native revision");
    };
    assert!(matches!(
        db.metadata
            .open_frozen_handoff_export(&auth, 2, FREEZE, origin, 120)
            .await,
        Err(Error::NativeHandoffMismatch)
    ));
    drop(db);
    drop(server);
    let server = f.start().await;
    assert_eq!(ok(native(f, owner, freeze)), frozen);
    assert_eq!(ok(native(f, worker, snapshot.clone())), frozen);
    let dir = f.dir.path();
    let bad_output = dir.join("wrong-freeze-export");
    assert_rejected(
        export(f, owner, "wrong-freeze", &bad_output),
        "native_handoff_mismatch",
    );
    assert!(!bad_output.exists());
    let destination = dir.join("frozen-branch-export");
    let receipt = exported(export(f, owner, FREEZE, &destination));
    let proof: Value =
        serde_json::from_slice(&fs::read(destination.join("handoff.json")).unwrap()).unwrap();
    assert_eq!(receipt["handoff"], proof);
    assert_eq!(proof["freeze"], frozen);
    let manifest = fs::read(destination.join("manifest.json")).unwrap();
    let bundle = fs::read(destination.join("history.bundle")).unwrap();
    assert_eq!(
        proof["projection_sha256"],
        ObjectId::from_bytes(Sha256::digest(manifest).into()).to_string()
    );
    assert_eq!(
        proof["bundle_sha256"],
        ObjectId::from_bytes(Sha256::digest(bundle).into()).hex()
    );
    assert_rejected(
        verify_projection(f, worker, "worker-projection", &destination),
        "forbidden",
    );
    let forged = dir.join("forged-handoff-projection");
    fs::create_dir(&forged).unwrap();
    fs::set_permissions(&forged, fs::Permissions::from_mode(0o700)).unwrap();
    let mut altered = receipt["export"].clone();
    altered["refs"]["refs/heads/main"]["git"] = altered["refs"]["refs/heads/stable"]["git"].clone();
    let bytes = serde_json::to_vec(&altered).unwrap();
    let mut forged_proof = proof.clone();
    forged_proof["projection_sha256"] =
        json!(ObjectId::from_bytes(Sha256::digest(&bytes).into()).to_string());
    files::write_private(&forged.join("manifest.json"), &bytes).unwrap();
    files::write_private(
        &forged.join("handoff.json"),
        &serde_json::to_vec(&forged_proof).unwrap(),
    )
    .unwrap();
    assert_rejected(
        verify_projection(f, owner, "verify-exact-projection", &forged),
        "native_handoff_mismatch",
    );
    let parent = f
        .sql
        .query_one(
            r#"
        SELECT e.child,e.child_generation FROM th_native_refs r
        JOIN th_object_edges e ON e.tenant=r.tenant AND e.parent=r.head
        JOIN th_objects o ON (o.tenant,o.digest,o.generation)=(e.tenant,e.child,e.child_generation)
        WHERE r.name='stable' AND o.kind=5 ORDER BY e.child LIMIT 1
        "#,
            &[],
        )
        .await
        .unwrap();
    let hex = ObjectId::from_bytes(parent.get::<_, Vec<u8>>(0).try_into().unwrap()).hex();
    let store = f.config.storage.as_ref().unwrap();
    let frame = store
        .root
        .join("managed-v1")
        .join(parent.get::<_, i64>(1).to_string())
        .join("objects")
        .join(f.scope.tenant())
        .join(&hex[..2])
        .join(&hex[2..]);
    let unavailable = dir.join("withheld-frozen-parent");
    fs::rename(&frame, &unavailable).unwrap();
    assert_rejected(
        verify_projection(f, owner, "verify-exact-projection", &destination),
        "internal_error",
    );
    fs::rename(&unavailable, frame).unwrap();
    let verified = ok(verify_projection(
        f,
        owner,
        "verify-exact-projection",
        &destination,
    ));
    assert_eq!(verified["state"], "verified");
    assert_eq!(
        verified["object_count"],
        receipt["export"]["source_objects"]
    );
    assert_eq!(verified["encoded_bytes"], receipt["export"]["source_bytes"]);
    assert_eq!(
        verified["specification"]["snapshot_sha256"],
        frozen["snapshot_sha256"]
    );
    let restored = dir.join("frozen-export.git");
    git_reference::git(
        dir,
        &[
            "clone",
            "--bare",
            destination.join("history.bundle").to_str().unwrap(),
            restored.to_str().unwrap(),
        ],
        b"",
    );
    git_reference::git(&restored, &["fsck", "--strict"], b"");
    let git_refs = String::from_utf8(git_reference::git(
        &restored,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
        ],
        b"",
    ))
    .unwrap();
    let expected = receipt["export"]["refs"].as_object().unwrap();
    assert_eq!(git_refs.lines().count(), expected.len());
    for line in git_refs.lines() {
        let (name, oid) = line.split_once(' ').unwrap();
        assert_eq!(expected[name]["git"], oid);
    }
    assert_eq!(
        git_reference::git(
            &restored,
            &["cat-file", "blob", "refs/heads/main:z-payload.bin"],
            b""
        ),
        b"proposal from a fork\x00\xff\n"
    );

    // Pause ordinary Git after the CLI downloaded all frozen source. The
    // terminal receipt check must fence publication when the owner resumes.
    let waiting = dir.join("wait-handoff-git");
    files::write_private(
        &waiting,
        br#"#!/bin/sh
dir=${0%/*}
if [ ! -e "$dir/handoff-release" ]; then
    : > "$dir/handoff-in-git"
    while [ ! -e "$dir/handoff-release" ]; do /bin/sleep 0.05; done
fi
exec /usr/bin/git "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(&waiting, fs::Permissions::from_mode(0o700)).unwrap();
    let race_output = dir.join("interrupted-frozen-export");
    let child = Command::new(&f.binary)
        .args(["client", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(owner)
        .args([
            "git-export",
            "--epoch",
            "2",
            "--freeze-key",
            FREEZE,
            "--timeout-seconds",
            "15",
            "--output",
        ])
        .arg(&race_output)
        .arg("--git")
        .arg(&waiting)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(8);
    while !dir.join("handoff-in-git").exists() {
        assert!(Instant::now() < until, "frozen export did not reach Git");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let resume = json!({"action":"handoff_resume","key":"return-to-native-writes","epoch":2,
        "freeze_key":FREEZE,"target_stays_fenced":true});
    let mut unsafe_resume = resume.clone();
    unsafe_resume["target_stays_fenced"] = json!(false);
    assert_rejected(native(f, owner, unsafe_resume), "invalid_request");
    let resumed = ok(native(f, owner, resume.clone()));
    assert_eq!(resumed["epoch"], 3);
    assert_eq!(resumed["snapshot_sha256"], frozen["snapshot_sha256"]);
    files::write_private(&dir.join("handoff-release"), b"resume\n").unwrap();
    assert_rejected(child.wait_with_output().unwrap(), "repository_fenced");
    assert!(!race_output.exists());
    assert_eq!(ok(native(f, owner, resume)), resumed);
    assert_eq!(
        ok(verify_projection(
            f,
            owner,
            "verify-exact-projection",
            &destination
        )),
        verified
    );
    assert_rejected(
        verify_projection(f, owner, "verify-after-resume", &destination),
        "repository_fenced",
    );
    assert_rejected(native(f, owner, snapshot), "repository_fenced");
    assert_eq!(
        ok(native(
            f,
            owner,
            json!({"action":"queue_status","entry":entry})
        ))["state"],
        "stale"
    );
    let live = ok(native(f, owner, json!({"action":"ref_snapshot"})));
    let mut expected = captured["refs"].clone();
    for tip in expected.as_array_mut().unwrap() {
        tip["epoch"] = json!(3);
    }
    assert_eq!(live["refs"], expected);
    assert_eq!(
        f.sql
            .query_one("SELECT count(*) FROM th_change_versions", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    assert!(fs::read_dir(dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".treeharbor-branch-packs-")
    }));
    drop(server);
}
