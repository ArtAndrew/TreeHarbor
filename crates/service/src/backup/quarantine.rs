//! Offline retirement of restored execution intent; never invent runtime outcomes.
use super::*;
use clap::Args;
use sha2::Digest;

#[derive(Args)]
pub(crate) struct Options {
    #[arg(long)]
    stage: PathBuf,
    /// Independently retained identity of the original backup.
    #[arg(long)]
    manifest_sha256: String,
    /// All old and restored services, workers and publishers remain stopped/isolated.
    #[arg(long, required = true)]
    writers_stopped: bool,
    #[arg(long, default_value_t = 68_719_476_736, value_parser = clap::value_parser!(u64).range(1..))]
    max_bytes: u64,
    #[arg(long, default_value_t = 100_000, value_parser = clap::value_parser!(u32).range(1..=1_000_000))]
    max_entries: u32,
    #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..=86400))]
    timeout_seconds: u64,
}

pub(super) async fn run(options: Options) -> Result<Value> {
    inner(options).await.map_err(|_| Error::Quarantine)
}

fn private_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(
        &crate::files::read_private(path, MAX_MANIFEST).map_err(|_| Error::Unsafe)?,
    )
    .map_err(|_| Error::Integrity)
}

async fn inner(o: Options) -> Result<Value> {
    if !o.writers_stopped || !o.stage.is_absolute() {
        return Err(Error::Unsafe);
    }
    let stage = crate::files::private_directory(&o.stage).map_err(|_| Error::Unsafe)?;
    let staged = private_json(&stage.join("staged.json"))?;
    let target = private_json(&stage.join("target.json"))?;
    let database = target["database"].as_str().ok_or(Error::Integrity)?;
    if database.len() != 43
        || !database.starts_with("th_restore_")
        || !database.as_bytes()[11..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
        || target["version"] != 1
        || staged["version"] != 1
        || target["activation_authorized"] != false
        || staged["activation_authorized"] != false
        || staged["status"] != "staged"
        || staged["service_started"] != false
        || staged["database"] != target["database"]
        || staged["schema"] != target["schema"]
        || staged["manifest_sha256"] != o.manifest_sha256
        || target["manifest_sha256"] != o.manifest_sha256
        || staged["directory"] != json!(stage)
        || staged["copied_backup"] != json!(stage.join("backup"))
    {
        return Err(Error::Integrity);
    }
    let budget = Budget {
        remaining: o.max_bytes,
        entries: o.max_entries as usize,
        deadline: Instant::now() + Duration::from_secs(o.timeout_seconds),
    };
    let copied = stage.join("backup");
    let hash = o.manifest_sha256.clone();
    let pass = budget.clone();
    let (manifest, _) =
        tokio::task::spawn_blocking(move || verified_manifest(&copied, &hash, pass))
            .await
            .map_err(|_| Error::Integrity)??;
    if target["schema"] != manifest.schema {
        return Err(Error::Integrity);
    }
    let config: Config = serde_json::from_value(json!({
        "database_url":target["admin_connection"], "schema":manifest.schema,
        "socket":stage.join("unused.sock"),
        "storage":{"root":stage.join("backup/objects"),"backend":manifest.backend}
    }))
    .map_err(|_| Error::Unsafe)?;
    let mut pg = config.database_config().map_err(|_| Error::Database)?;
    pg.dbname(database);
    let (client, connection) = pg.connect(NoTls).await.map_err(|_| Error::Database)?;
    let _driver = Driver(tokio::spawn(async move {
        let _ = connection.await;
    }));
    client
        .query_one(
            "SELECT set_config('search_path',$1,false)",
            &[&manifest.schema],
        )
        .await
        .map_err(|_| Error::Database)?;
    treeharbor_metadata::Metadata::new(client)
        .await
        .map_err(|_| Error::Database)?
        .check_schema()
        .await
        .map_err(|_| Error::Database)?;
    let (mut client, connection) = pg.connect(NoTls).await.map_err(|_| Error::Database)?;
    let _driver2 = Driver(tokio::spawn(async move {
        let _ = connection.await;
    }));
    client
        .query_one(
            "SELECT set_config('search_path',$1,false)",
            &[&manifest.schema],
        )
        .await
        .map_err(|_| Error::Database)?;
    let tx = client.transaction().await.map_err(|_| Error::Database)?;
    tx.batch_execute("SET LOCAL synchronous_commit=on; SET LOCAL statement_timeout='15s'; SET LOCAL lock_timeout='5s';
        LOCK TABLE th_schema_version,th_storage_backends,th_objects,
        th_repositories,th_operations,th_access_tokens,th_principals,th_workspaces,
        th_workspace_preparations,th_managed_runs,th_native_queue,th_native_candidates,
        th_github_bindings,th_github_event_channels IN SHARE ROW EXCLUSIVE MODE NOWAIT")
        .await.map_err(|_| Error::Database)?;
    if tx
        .query_one(
            "SELECT count(*) FROM th_access_tokens WHERE revoked_at IS NULL",
            &[],
        )
        .await
        .map_err(|_| Error::Database)?
        .get::<_, i64>(0)
        != 0
    {
        return Err(Error::Unsafe);
    }
    let binding = tx
        .query("SELECT backend,identity FROM th_storage_backends", &[])
        .await
        .map_err(|_| Error::Database)?;
    if binding.len() != 1
        || binding[0].get::<_, String>(0) != manifest.backend
        || binding[0].get::<_, Uuid>(1) != manifest.backend_identity
    {
        return Err(Error::Integrity);
    }
    let version = tx
        .query_one(
            "SELECT version,encode(migration_digest,'hex') FROM th_schema_version WHERE singleton",
            &[],
        )
        .await
        .map_err(|_| Error::Database)?;
    if version.get::<_, i32>(0) != manifest.schema_version
        || version.get::<_, String>(1) != manifest.migration_digest
    {
        return Err(Error::Integrity);
    }
    check_objects(&tx, &manifest.backend, &manifest.inventory, &budget).await?;
    let repos = tx
        .query(
            "SELECT tenant,repository FROM th_repositories ORDER BY tenant,repository LIMIT $1",
            &[&(i64::from(o.max_entries) + 1)],
        )
        .await
        .map_err(|_| Error::Database)?;
    if repos.is_empty() {
        return Err(Error::Unsafe);
    }
    if repos.len() > o.max_entries as usize {
        return Err(Error::Limit);
    }
    // Every repository stores the same deployment result in one transaction.
    // A changed repository set or conflicting operation cannot authorize a retry.
    let key = format!("restore-quarantine:{}", o.manifest_sha256);
    let digest = sha2::Sha256::digest(format!(
        "treeharbor-restore-quarantine\0v1\0{}\0{database}",
        o.manifest_sha256
    ))
    .to_vec();
    let previous = tx.query("SELECT tenant,repository,kind,principal,request_digest,result FROM th_operations WHERE key=$1 ORDER BY tenant,repository", &[&key])
        .await.map_err(|_| Error::Database)?;
    let result = if !previous.is_empty() {
        if previous.len() != repos.len() {
            return Err(Error::Integrity);
        }
        let result: Value = previous[0].get(5);
        for (old, repo) in previous.iter().zip(&repos) {
            if old.get::<_, String>(0) != repo.get::<_, String>(0)
                || old.get::<_, Uuid>(1) != repo.get::<_, Uuid>(1)
                || old.get::<_, String>(2) != "restore.quarantine.v1"
                || old.get::<_, String>(3) != "restore-quarantine"
                || old.get::<_, Vec<u8>>(4) != digest
                || old.get::<_, Value>(5) != result
            {
                return Err(Error::Integrity);
            }
        }
        result
    } else {
        let counts = quarantine(&tx).await?;
        let result = json!({"version":1,"status":"quarantined","database":database,
            "schema":manifest.schema,"manifest_sha256":o.manifest_sha256,
            "repositories":repos.len(),"changes":counts,"activation_authorized":false,
            "service_started":false,"external_fencing_verified":false});
        for repo in &repos {
            budget.tick()?;
            let tenant: String = repo.get(0);
            let repository: Uuid = repo.get(1);
            tx.execute("INSERT INTO th_operations(tenant,repository,key,request_digest,principal,kind,result) VALUES($1,$2,$3,$4,'restore-quarantine','restore.quarantine.v1',$5)",
                &[&tenant,&repository,&key,&digest,&result]).await.map_err(|_| Error::Database)?;
            tx.execute("INSERT INTO th_security_audit(tenant,repository,operator,action,principal) VALUES($1,$2,'restore-quarantine','restore.quarantine','restore-quarantine')",
                &[&tenant,&repository]).await.map_err(|_| Error::Database)?;
        }
        result
    };
    budget.tick()?;
    tx.commit().await.map_err(|_| Error::Database)?;
    let path = stage.join("quarantined.json");
    // A lost response after commit reuses the database receipt, never drains twice.
    if path.try_exists()? {
        if private_json(&path)? != result {
            return Err(Error::Integrity);
        }
    } else {
        crate::files::write_private(
            &path,
            &serde_json::to_vec(&result).map_err(|_| Error::Integrity)?,
        )
        .map_err(|_| Error::Integrity)?;
    }
    Ok(result)
}

async fn quarantine(tx: &Transaction<'_>) -> Result<Value> {
    let workspaces = tx.execute("UPDATE th_workspaces SET state='draining',generation=generation+1 WHERE delivery='files' AND state IN ('requested','ready','in_use')", &[]).await.map_err(|_| Error::Database)?;
    let preparations = tx.execute("UPDATE th_workspace_preparations SET state='draining' WHERE state IN ('reserved','prepared')", &[]).await.map_err(|_| Error::Database)?;
    let stops = tx.execute("UPDATE th_managed_runs SET stop_requested=true WHERE NOT removed AND NOT stop_requested", &[]).await.map_err(|_| Error::Database)?;
    let candidates = tx.execute("UPDATE th_native_candidates SET state='cancelled' WHERE state IN ('preparing','checking')", &[]).await.map_err(|_| Error::Database)?;
    let queue = tx.execute("UPDATE th_native_queue SET state='cancelled',reason='restore_quarantine' WHERE state IN ('queued','preparing','checking')", &[]).await.map_err(|_| Error::Database)?;
    let nodes = tx.execute("WITH disabled AS (UPDATE th_principals SET enabled=false WHERE kind='node' AND enabled RETURNING tenant,principal) INSERT INTO th_security_audit(tenant,operator,action,principal) SELECT tenant,'restore-quarantine','restore.node.disable',principal FROM disabled", &[]).await.map_err(|_| Error::Database)?;
    let bindings = tx.execute("UPDATE th_github_bindings SET state='revoked',revoked_at=clock_timestamp() WHERE state='active'", &[]).await.map_err(|_| Error::Database)?;
    let channels = tx.execute("UPDATE th_github_event_channels SET state='revoked',revoked_at=clock_timestamp() WHERE state='active'", &[]).await.map_err(|_| Error::Database)?;
    Ok(
        json!({"workspaces_draining":workspaces,"preparations_draining":preparations,
        "stop_requests":stops,"candidates_cancelled":candidates,"queue_cancelled":queue,
        "nodes_disabled":nodes,"bindings_revoked":bindings,"channels_revoked":channels}),
    )
}
