//! Cold backups of the local service. The operator stops ALL writers; locks only
//! exclude cooperating local services and concurrent writes to existing tables.
mod files;

use crate::{config::Config, database::Database};
use clap::Subcommand;
use files::{Budget, Entry, Inventory};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio_postgres::{NoTls, Transaction, config::Host};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "backup input must be private, stopped, regular files/directories without symlinks or overlapping paths"
    )]
    Unsafe,
    #[error("backup input changed; keep every writer stopped and use a fresh output directory")]
    Changed,
    #[error("backup byte, entry, depth or time budget exceeded; incomplete output is retained")]
    Limit,
    #[error(
        "backup database schema, snapshot, storage binding or write lock could not be verified"
    )]
    Database,
    #[error("backup contains missing, corrupt or unsupported data; do not restore it")]
    Integrity,
    #[error(
        "pg_dump failed or emitted diagnostics; inspect the private pg-dump.stderr file; incomplete output is retained"
    )]
    Dump,
    #[error(
        "backup file operation failed; preserve any incomplete output and use a fresh destination"
    )]
    Io(#[from] std::io::Error),
    #[error(
        "backup file operation failed; preserve any incomplete output and use a fresh destination"
    )]
    Unix(#[from] rustix::io::Errno),
}
type Result<T> = std::result::Result<T, Error>;

#[derive(Subcommand)]
pub(crate) enum Action {
    /// Save a stopped local deployment. Never stops/fences workers automatically.
    Create {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// Absolute path to trusted pg_dump matching the server major version.
        #[arg(long)]
        pg_dump: PathBuf,
        /// Explicitly include each stopped private journal/configuration path.
        #[arg(long = "state", required = true, num_args = 1)]
        state: Vec<PathBuf>,
        /// Acknowledge all runtimes, workers, publishers, GC and admin writers are stopped.
        #[arg(long, required = true)]
        writers_stopped: bool,
        #[arg(long, default_value_t = 68_719_476_736, value_parser = clap::value_parser!(u64).range(1..))]
        max_bytes: u64,
        #[arg(long, default_value_t = 100_000, value_parser = clap::value_parser!(u32).range(1..=1_000_000))]
        max_entries: u32,
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..=86400))]
        timeout_seconds: u64,
    },
    /// Verify a completed backup's exact inventory. Does not restore or start anything.
    Verify {
        #[arg(long)]
        backup: PathBuf,
        /// Digest recorded separately when create succeeded; checks the manifest too.
        #[arg(long)]
        manifest_sha256: String,
        #[arg(long, default_value_t = 68_719_476_736, value_parser = clap::value_parser!(u64).range(1..))]
        max_bytes: u64,
        #[arg(long, default_value_t = 100_000, value_parser = clap::value_parser!(u32).range(1..=1_000_000))]
        max_entries: u32,
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..=86400))]
        timeout_seconds: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    format: String,
    treeharbor_version: String,
    created_unix_seconds: u64,
    schema: String,
    schema_version: i32,
    migration_digest: String,
    postgres_version_num: i32,
    backend: String,
    backend_identity: Uuid,
    // Private artifact: paths and copied state may contain customer secrets.
    inputs: Vec<Input>,
    inventory: Inventory,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    source: PathBuf,
    target: String,
}

struct Driver(tokio::task::JoinHandle<()>);
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
const FORMAT: &str = "treeharbor-cold-backup";
const MANIFEST: &str = "manifest.json";
const MAX_MANIFEST: usize = 64 * 1024 * 1024;

pub(crate) async fn run(action: Action) -> crate::Result<bool> {
    let value = match action {
        Action::Create {
            config,
            output,
            pg_dump,
            state,
            writers_stopped,
            max_bytes,
            max_entries,
            timeout_seconds,
        } => {
            if !writers_stopped {
                return Err(Error::Unsafe.into());
            }
            create(
                &config,
                &output,
                &pg_dump,
                state,
                Budget {
                    remaining: max_bytes,
                    entries: max_entries as usize,
                    deadline: Instant::now() + Duration::from_secs(timeout_seconds),
                },
            )
            .await?
        }
        Action::Verify {
            backup,
            manifest_sha256,
            max_bytes,
            max_entries,
            timeout_seconds,
        } => tokio::task::spawn_blocking(move || {
            verify(
                &backup,
                &manifest_sha256,
                Budget {
                    remaining: max_bytes,
                    entries: max_entries as usize,
                    deadline: Instant::now() + Duration::from_secs(timeout_seconds),
                },
            )
        })
        .await
        .map_err(|_| Error::Integrity)??,
    };
    println!("{value}");
    Ok(true)
}

fn input(path: &Path, target: &str) -> Result<Input> {
    if !path.is_absolute() {
        return Err(Error::Unsafe);
    }
    crate::files::private_directory(path.parent().ok_or(Error::Unsafe)?)
        .map_err(|_| Error::Unsafe)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() && !meta.is_dir() {
        return Err(Error::Unsafe);
    }
    // Resolve trusted ancestor paths once; the final component is checked again
    // with O_NOFOLLOW and every descendant is opened relative to its parent FD.
    Ok(Input {
        source: path.canonicalize()?,
        target: target.into(),
    })
}

async fn create(
    config_path: &Path,
    output: &Path,
    pg_dump: &Path,
    state: Vec<PathBuf>,
    mut budget: Budget,
) -> Result<Value> {
    if !pg_dump.is_absolute() || !output.is_absolute() || state.is_empty() || state.len() > 32 {
        return Err(Error::Unsafe);
    }
    let config_bytes =
        crate::files::read_private(config_path, 16 * 1024).map_err(|_| Error::Unsafe)?;
    let config: Config = serde_json::from_slice(&config_bytes).map_err(|_| Error::Unsafe)?;
    let pg = config.database_config().map_err(|_| Error::Database)?;
    let storage = config.storage.as_ref().ok_or(Error::Unsafe)?;
    let mut inputs = vec![
        input(config_path, "service-config.json")?,
        input(&storage.root, "objects")?,
    ];
    for (index, path) in state.iter().enumerate() {
        inputs.push(input(path, &format!("private-state/{index:02}"))?);
    }
    let parent = crate::files::private_directory(output.parent().ok_or(Error::Unsafe)?)
        .map_err(|_| Error::Unsafe)?;
    let output = parent.join(output.file_name().ok_or(Error::Unsafe)?);
    for (i, source) in inputs.iter().enumerate() {
        if output.starts_with(&source.source) || source.source.starts_with(&output) {
            return Err(Error::Unsafe);
        }
        for other in &inputs[..i] {
            if source.source.starts_with(&other.source) || other.source.starts_with(&source.source)
            {
                return Err(Error::Unsafe);
            }
        }
    }
    let _socket_lock = crate::server::lock_socket(&config.socket).map_err(|_| Error::Unsafe)?;
    let (mut client, connection) = pg.connect(NoTls).await.map_err(|_| Error::Database)?;
    let _driver = Driver(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let tx = client.transaction().await.map_err(|_| Error::Database)?;
    tx.batch_execute(
        "SET LOCAL statement_timeout='15s'; SET LOCAL idle_in_transaction_session_timeout=0",
    )
    .await
    .map_err(|_| Error::Database)?;
    tx.query_one(
        "SELECT set_config('search_path',$1,true)",
        &[&config.schema],
    )
    .await
    .map_err(|_| Error::Database)?;
    let tables = tx.query("SELECT format('%I.%I',n.nspname,c.relname) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname::text=$1 AND c.relkind IN ('r','p','f') ORDER BY c.relname LIMIT 1025", &[&config.schema]).await.map_err(|_| Error::Database)?;
    if tables.is_empty() || tables.len() > 1024 {
        return Err(Error::Database);
    }
    for row in tables {
        budget.tick()?;
        tx.batch_execute(&format!(
            "LOCK TABLE {} IN SHARE MODE NOWAIT",
            row.get::<_, String>(0)
        ))
        .await
        .map_err(|_| Error::Database)?;
    }
    // Check the installed schema with the normal durability/schema validator,
    // while table writes/alterations are excluded by the transaction above.
    drop(
        Database::new(config.clone())
            .map_err(|_| Error::Database)?
            .connect(true)
            .await
            .map_err(|_| Error::Database)?,
    );
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
    if bindings.len() != 1 || bindings[0].get::<_, String>(0) != storage.backend {
        return Err(Error::Database);
    }
    let identity: Uuid = bindings[0].get(1);
    if crate::files::read_private(&storage.root.join("backend-id"), 36)
        .map_err(|_| Error::Integrity)?
        != identity.to_string().as_bytes()
    {
        return Err(Error::Integrity);
    }
    let snapshot: String = tx
        .query_one("SELECT pg_export_snapshot()", &[])
        .await
        .map_err(|_| Error::Database)?
        .get(0);
    let postgres_version_num: i32 = tx
        .query_one("SELECT current_setting('server_version_num')::integer", &[])
        .await
        .map_err(|_| Error::Database)?
        .get(0);
    // CREATE_NEW directory is a durable incomplete marker. Never overwrite or
    // automatically delete an interrupted backup or an unrelated destination.
    files::mkdir(&output)?;
    File::open(&parent)?.sync_all()?;
    dump(pg_dump, &pg, &config.schema, &snapshot, &output, &budget).await?;
    let copied_output = output.clone();
    let (inputs, inventory, returned_budget) = tokio::task::spawn_blocking(move || -> Result<_> {
        let mut inventory = Inventory::new();
        inventory.insert(
            "".into(),
            Entry {
                mode: 0o700,
                sha256: None,
                bytes: 0,
            },
        );
        budget.entries = budget.entries.checked_sub(1).ok_or(Error::Limit)?;
        for name in ["metadata.dump", "pg-dump.stderr", "pgpass.empty"] {
            files::walk(
                &files::open(&copied_output.join(name), false)?,
                None,
                name,
                &mut inventory,
                &mut budget,
                0,
            )?;
        }
        files::mkdir(&copied_output.join("private-state"))?;
        inventory.insert(
            "private-state".into(),
            Entry {
                mode: 0o700,
                sha256: None,
                bytes: 0,
            },
        );
        budget.entries = budget.entries.checked_sub(1).ok_or(Error::Limit)?;
        for source in &inputs {
            let meta = fs::symlink_metadata(&source.source)?;
            files::walk(
                &files::open(&source.source, meta.is_dir())?,
                Some(&copied_output.join(&source.target)),
                &source.target,
                &mut inventory,
                &mut budget,
                0,
            )?;
        }
        File::open(copied_output.join("private-state"))?.sync_all()?;
        File::open(&copied_output)?.sync_all()?;
        Ok((inputs, inventory, budget))
    })
    .await
    .map_err(|_| Error::Integrity)??;
    budget = returned_budget;
    for (name, expected) in [
        ("service-config.json", config_bytes),
        ("objects/backend-id", identity.to_string().into_bytes()),
    ] {
        if inventory
            .get(name)
            .and_then(|entry| entry.sha256.as_deref())
            != Some(
                treeharbor_core::ObjectId::hash_frame(&expected)
                    .hex()
                    .as_str(),
            )
        {
            return Err(Error::Changed);
        }
    }
    check_objects(&tx, &storage.backend, &inventory, &budget).await?;
    budget.tick()?;
    // Confirm that the snapshot owner is still alive after filesystem work.
    tx.simple_query("SELECT 1")
        .await
        .map_err(|_| Error::Database)?;
    let manifest = Manifest {
        version: 1,
        format: FORMAT.into(),
        treeharbor_version: env!("CARGO_PKG_VERSION").into(),
        created_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Integrity)?
            .as_secs(),
        schema: config.schema,
        schema_version: version.get(0),
        migration_digest: version.get(1),
        postgres_version_num,
        backend: storage.backend.clone(),
        backend_identity: identity,
        inputs,
        inventory,
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|_| Error::Integrity)?;
    if bytes.len() > MAX_MANIFEST {
        return Err(Error::Limit);
    }
    crate::files::write_private(&output.join(MANIFEST), &bytes).map_err(|_| Error::Integrity)?;
    // No metadata mutation. Releasing the guard never restarts any process.
    tx.rollback().await.map_err(|_| Error::Database)?;
    Ok(summary(&manifest, &bytes))
}

async fn check_objects(
    tx: &Transaction<'_>,
    backend: &str,
    inventory: &Inventory,
    budget: &Budget,
) -> Result<()> {
    let mut tenant = String::new();
    let mut digest = Vec::<u8>::new();
    loop {
        budget.tick()?;
        let rows=tx.query("SELECT tenant,digest,generation,backend,length FROM th_objects WHERE state='live' AND (tenant,digest)>($1,$2) ORDER BY tenant,digest LIMIT 256", &[&tenant,&digest]).await.map_err(|_| Error::Database)?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            tenant = row.get(0);
            digest = row.get(1);
            let id = treeharbor_core::ObjectId::from_bytes(
                digest.clone().try_into().map_err(|_| Error::Integrity)?,
            )
            .hex();
            let generation: i64 = row.get(2);
            let label: String = row.get(3);
            let bytes: i64 = row.get(4);
            let name = format!(
                "objects/managed-v1/{generation}/objects/{tenant}/{}/{}",
                &id[..2],
                &id[2..]
            );
            let entry = inventory.get(&name).ok_or(Error::Integrity)?;
            if label != backend
                || entry.sha256.as_deref() != Some(id.as_str())
                || bytes < 0
                || entry.bytes != bytes as u64
            {
                return Err(Error::Integrity);
            }
        }
    }
    Ok(())
}

async fn dump(
    binary: &Path,
    pg: &tokio_postgres::Config,
    schema: &str,
    snapshot: &str,
    output: &Path,
    budget: &Budget,
) -> Result<()> {
    let stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output.join("metadata.dump"))?;
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output.join("pg-dump.stderr"))?;
    let empty_password_file = output.join("pgpass.empty");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&empty_password_file)?
        .sync_all()?;
    let mut command = Command::new(binary);
    command
        .env_clear()
        .env("LC_ALL", "C")
        .env("PGCONNECT_TIMEOUT", "5")
        .env("PGPASSFILE", &empty_password_file);
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
    if let Some(database) = pg.get_dbname() {
        command.env("PGDATABASE", database);
    }
    if let Some(password) = pg.get_password() {
        use std::os::unix::ffi::OsStrExt;
        command.env("PGPASSWORD", std::ffi::OsStr::from_bytes(password));
    }
    command
        .args([
            "--format=custom",
            "--no-password",
            "--strict-names",
            "--lock-wait-timeout=5s",
            "--schema",
            schema,
            "--snapshot",
            snapshot,
        ])
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let mut child = Process(command.spawn().map_err(|_| Error::Dump)?);
    loop {
        budget.tick()?;
        if stdout.metadata()?.len() > budget.remaining || stderr.metadata()?.len() > 1_048_576 {
            return Err(Error::Limit);
        }
        if let Some(status) = child.0.try_wait()? {
            if !status.success() || stderr.metadata()?.len() != 0 || stdout.metadata()?.len() == 0 {
                return Err(Error::Dump);
            }
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stdout.sync_all()?;
    stderr.sync_all()?;
    Ok(())
}

fn summary(manifest: &Manifest, bytes: &[u8]) -> Value {
    json!({"version":1,"format":FORMAT,"complete":true,"manifest_sha256":treeharbor_core::ObjectId::hash_frame(bytes).hex(),
        "entries":manifest.inventory.len(),"bytes":manifest.inventory.values().map(|entry|entry.bytes).sum::<u64>(),
        "scope":"cold local metadata, objects and enumerated private state","restore_authorized":false})
}
fn verify(path: &Path, expected_hash: &str, mut budget: Budget) -> Result<Value> {
    let bytes = crate::files::read_private(&path.join(MANIFEST), MAX_MANIFEST)
        .map_err(|_| Error::Integrity)?;
    if treeharbor_core::ObjectId::hash_frame(&bytes).hex() != expected_hash {
        return Err(Error::Integrity);
    }
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|_| Error::Integrity)?;
    if manifest.version != 1
        || manifest.format != FORMAT
        || manifest.inventory.contains_key(MANIFEST)
    {
        return Err(Error::Integrity);
    }
    // The completion marker has its own bound; payload limits match create.
    budget.entries = budget.entries.checked_add(1).ok_or(Error::Limit)?;
    budget.remaining = budget.remaining.saturating_add(bytes.len() as u64);
    let mut inventory = files::scan(path, &mut budget)?;
    let marker = inventory.remove(MANIFEST).ok_or(Error::Integrity)?;
    if marker.sha256.as_deref() != Some(expected_hash) || marker.bytes != bytes.len() as u64 {
        return Err(Error::Changed);
    }
    if inventory != manifest.inventory {
        return Err(Error::Integrity);
    }
    Ok(summary(&manifest, &bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cold_backup_manifest_v1_matches_independent_encoding_vector() {
        let bytes = include_bytes!("../tests/fixtures/cold-backup-v1.json");
        let digest = include_str!("../tests/fixtures/cold-backup-v1.sha256").trim();
        assert_eq!(treeharbor_core::ObjectId::hash_frame(bytes).hex(), digest);
        let manifest: Manifest = serde_json::from_slice(bytes).unwrap();
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.format, FORMAT);
        assert_eq!(manifest.inventory["metadata.dump"].bytes, 14);
        assert_eq!(
            manifest.inventory["private-state/00"].sha256.as_deref(),
            Some("44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a")
        );
        assert_eq!(serde_json::to_vec(&manifest).unwrap(), bytes);
    }
}
