//! Offline staging only: no service configuration, credentials or activation.
use super::*;
use clap::Args;
use std::path::Component;

#[derive(Args)]
pub(crate) struct Options {
    #[arg(long)]
    backup: PathBuf,
    /// Independently retained digest from backup creation.
    #[arg(long)]
    manifest_sha256: String,
    /// New absolute private directory; existing/partial output is never reused.
    #[arg(long)]
    directory: PathBuf,
    /// Private connection file for an isolated local PostgreSQL server; needs CREATEDB.
    #[arg(long)]
    database_url_file: PathBuf,
    /// Absolute trusted pg_restore matching the backup/server major version.
    #[arg(long)]
    pg_restore: PathBuf,
    /// Acknowledge isolated target infrastructure and a trusted executable SQL backup.
    #[arg(long, required = true)]
    isolated_target: bool,
    #[arg(long, default_value_t = 68_719_476_736, value_parser = clap::value_parser!(u64).range(1..))]
    max_bytes: u64,
    #[arg(long, default_value_t = 100_000, value_parser = clap::value_parser!(u32).range(1..=1_000_000))]
    max_entries: u32,
    #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..=86400))]
    timeout_seconds: u64,
}

pub(super) async fn stage(options: Options) -> Result<Value> {
    // Neither connection errors nor subprocess output may disclose credentials.
    stage_inner(options).await.map_err(|_| Error::Restore)
}

async fn stage_inner(o: Options) -> Result<Value> {
    if !o.isolated_target
        || !o.pg_restore.is_absolute()
        || !o.backup.is_absolute()
        || !o.directory.is_absolute()
        || !matches!(
            o.directory.components().next_back(),
            Some(Component::Normal(_))
        )
    {
        return Err(Error::Unsafe);
    }
    let parent = crate::files::private_directory(o.directory.parent().ok_or(Error::Unsafe)?)
        .map_err(|_| Error::Unsafe)?;
    let output = parent.join(o.directory.file_name().ok_or(Error::Unsafe)?);
    output.to_str().ok_or(Error::Unsafe)?;
    let source = crate::files::private_directory(&o.backup).map_err(|_| Error::Unsafe)?;
    if output.starts_with(&source) || source.starts_with(&output) {
        return Err(Error::Unsafe);
    }
    let url = String::from_utf8(
        crate::files::read_private(&o.database_url_file, 8192).map_err(|_| Error::Unsafe)?,
    )
    .map_err(|_| Error::Unsafe)?;
    let url = url.trim_end_matches(['\r', '\n']);
    let budget = Budget {
        remaining: o.max_bytes,
        entries: o.max_entries as usize,
        deadline: Instant::now() + Duration::from_secs(o.timeout_seconds),
    };
    let hash = o.manifest_sha256.clone();
    let source_for_verify = source.clone();
    let verify_budget = budget.clone();
    let (manifest, _) = tokio::task::spawn_blocking(move || {
        verified_manifest(&source_for_verify, &hash, verify_budget)
    })
    .await
    .map_err(|_| Error::Integrity)??;
    // Validate schema/backend/connection with the ordinary configuration parser,
    // but never write a runnable configuration. Source paths are not reused.
    let config_value = json!({"database_url":url,"schema":manifest.schema,
        "socket":output.join("unused.sock"),
        "storage":{"root":output.join("backup/objects"),"backend":manifest.backend}});
    let config: Config = serde_json::from_value(config_value).map_err(|_| Error::Unsafe)?;
    let mut pg = config.database_config().map_err(|_| Error::Database)?;
    for input in &manifest.inputs {
        if output.starts_with(&input.source) || input.source.starts_with(&output) {
            return Err(Error::Unsafe);
        }
    }
    let (admin, connection) = pg.connect(NoTls).await.map_err(|_| Error::Database)?;
    let _admin_driver = Driver(tokio::spawn(async move {
        let _ = connection.await;
    }));
    admin
        .batch_execute("SET statement_timeout='30s'")
        .await
        .map_err(|_| Error::Database)?;
    let server = admin
        .query_one(
            "SELECT current_setting('server_version_num')::integer, current_setting('fsync')",
            &[],
        )
        .await
        .map_err(|_| Error::Database)?;
    if manifest.postgres_version_num < 100_000
        || server.get::<_, i32>(0) / 10_000 != manifest.postgres_version_num / 10_000
        || server.get::<_, String>(1) != "on"
    {
        return Err(Error::Database);
    }
    // Exclusive filesystem claim precedes any database creation. Copied bytes
    // (not the original path) are verified again before executing the dump.
    files::mkdir(&output)?;
    File::open(&parent)?.sync_all()?;
    let copied = output.join("backup");
    let copy_budget = budget.clone();
    let copied_path = copied.clone();
    let hash = o.manifest_sha256.clone();
    let manifest = tokio::task::spawn_blocking(move || -> Result<Manifest> {
        let mut pass = copy_budget.clone();
        pass.entries = pass.entries.checked_add(1).ok_or(Error::Limit)?;
        pass.remaining = pass.remaining.saturating_add(MAX_MANIFEST as u64);
        files::walk(
            &files::open(&source, true)?,
            Some(&copied_path),
            "",
            &mut Inventory::new(),
            &mut pass,
            0,
        )?;
        let (manifest, _) = verified_manifest(&copied_path, &hash, copy_budget)?;
        Ok(manifest)
    })
    .await
    .map_err(|_| Error::Integrity)??;
    File::open(&output)?.sync_all()?;
    budget.tick()?;
    let database = format!("th_restore_{}", Uuid::new_v4().simple());
    // Persist the chosen identity before CREATE DATABASE: even an uncertain
    // server result leaves the operator a precise private cleanup record.
    crate::files::write_private(
        &output.join("target.json"),
        &serde_json::to_vec(&json!({
            "version":1,"database":database,"schema":manifest.schema,
            "admin_connection":url,"manifest_sha256":o.manifest_sha256,
            "activation_authorized":false
        }))
        .map_err(|_| Error::Integrity)?,
    )
    .map_err(|_| Error::Integrity)?;
    // Name consists only of a fixed prefix and generated hexadecimal UUID.
    admin
        .batch_execute(&format!("CREATE DATABASE {database} TEMPLATE template0"))
        .await
        .map_err(|_| Error::Database)?;
    admin
        .batch_execute(&format!(
            "REVOKE CONNECT ON DATABASE {database} FROM PUBLIC"
        ))
        .await
        .map_err(|_| Error::Database)?;
    pg.dbname(&database);
    restore_dump(&o.pg_restore, &pg, &copied, &output, &budget).await?;
    let (mut client, connection) = pg.connect(NoTls).await.map_err(|_| Error::Database)?;
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
    client
        .batch_execute("SET statement_timeout='15s'; SET synchronous_commit=on")
        .await
        .map_err(|_| Error::Database)?;
    // The normal metadata validator checks the compiled migration chain too.
    // Use the parsed target connection: URI inputs cannot be extended by
    // appending keyword connection parameters.
    let (schema_client, schema_connection) =
        pg.connect(NoTls).await.map_err(|_| Error::Database)?;
    let _schema_driver = Driver(tokio::spawn(async move {
        let _ = schema_connection.await;
    }));
    schema_client
        .query_one(
            "SELECT set_config('search_path',$1,false)",
            &[&manifest.schema],
        )
        .await
        .map_err(|_| Error::Database)?;
    treeharbor_metadata::Metadata::new(schema_client)
        .await
        .map_err(|_| Error::Database)?
        .check_schema()
        .await
        .map_err(|_| Error::Database)?;
    let tx = client.transaction().await.map_err(|_| Error::Database)?;
    let version = tx
        .query_one(
            "SELECT version,encode(migration_digest,'hex') FROM th_schema_version WHERE singleton",
            &[],
        )
        .await
        .map_err(|_| Error::Database)?;
    let bindings = tx
        .query("SELECT backend,identity FROM th_storage_backends", &[])
        .await
        .map_err(|_| Error::Database)?;
    if version.get::<_, i32>(0) != manifest.schema_version
        || version.get::<_, String>(1) != manifest.migration_digest
        || bindings.len() != 1
        || bindings[0].get::<_, String>(0) != manifest.backend
        || bindings[0].get::<_, Uuid>(1) != manifest.backend_identity
        || crate::files::read_private(&copied.join("objects/backend-id"), 36)
            .map_err(|_| Error::Integrity)?
            != manifest.backend_identity.to_string().as_bytes()
    {
        return Err(Error::Integrity);
    }
    check_objects(&tx, &manifest.backend, &manifest.inventory, &budget).await?;
    // Keep proposals/history/outbox unchanged. Old bearer credentials must never
    // become live again at the recovered endpoint. Append evidence for each one.
    let revoked = tx.execute("WITH revoked AS (UPDATE th_access_tokens SET revoked_at=clock_timestamp() WHERE revoked_at IS NULL RETURNING tenant,repository,principal,token,permissions) INSERT INTO th_security_audit(tenant,repository,operator,action,principal,token,permissions) SELECT tenant,repository,'restore-stage','restore.token.revoke',principal,token,permissions FROM revoked", &[])
        .await.map_err(|_| Error::Database)?;
    budget.tick()?;
    tx.commit().await.map_err(|_| Error::Database)?;
    let receipt = json!({"version":1,"status":"staged","database":database,
        "schema":manifest.schema,"manifest_sha256":o.manifest_sha256,
        "directory":output,"copied_backup":copied,"revoked_tokens":revoked,
        "service_started":false,"activation_authorized":false,
        "old_infrastructure_fenced":false});
    crate::files::write_private(
        &output.join("staged.json"),
        &serde_json::to_vec(&receipt).map_err(|_| Error::Integrity)?,
    )
    .map_err(|_| Error::Integrity)?;
    Ok(receipt)
}

async fn restore_dump(
    binary: &Path,
    pg: &tokio_postgres::Config,
    backup: &Path,
    output: &Path,
    budget: &Budget,
) -> Result<()> {
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output.join("pg-restore.stderr"))?;
    let stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output.join("pg-restore.stdout"))?;
    let pass = output.join("pgpass.empty");
    crate::files::write_private(&pass, b"").map_err(|_| Error::Unsafe)?;
    let mut command = Command::new(binary);
    command
        .env_clear()
        .env("LC_ALL", "C")
        .env("PGCONNECT_TIMEOUT", "5")
        .env("PGPASSFILE", pass);
    match &pg.get_hosts()[0] {
        Host::Unix(path) => {
            command.env("PGHOST", path);
        }
        _ => return Err(Error::Database),
    }
    if let Some(port) = pg.get_ports().first() {
        command.env("PGPORT", port.to_string());
    }
    if let Some(user) = pg.get_user() {
        command.env("PGUSER", user);
    }
    if let Some(password) = pg.get_password() {
        use std::os::unix::ffi::OsStrExt;
        command.env("PGPASSWORD", std::ffi::OsStr::from_bytes(password));
    }
    let database = pg.get_dbname().ok_or(Error::Database)?;
    command
        .env("PGDATABASE", database)
        .args([
            "--dbname",
            database,
            "--no-password",
            "--no-owner",
            "--no-privileges",
            "--single-transaction",
            "--exit-on-error",
        ])
        .stdin(files::open(&backup.join("metadata.dump"), false)?)
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let mut child = Process(command.spawn().map_err(|_| Error::Restore)?);
    loop {
        budget.tick()?;
        if stderr.metadata()?.len() > 1_048_576 || stdout.metadata()?.len() > 1_048_576 {
            return Err(Error::Limit);
        }
        if let Some(status) = child.0.try_wait()? {
            stdout.sync_all()?;
            stderr.sync_all()?;
            if !status.success() || stderr.metadata()?.len() != 0 || stdout.metadata()?.len() != 0 {
                return Err(Error::Restore);
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
