//! PostgreSQL primitives for trusted service code, including scoped credentials
//! and transactional authorization. Only authenticated contexts may be used at
//! external boundaries. Callers finish object upload before a short mutation;
//! authentication alone never grants merge admission or proves runtime isolation.

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_postgres::{Client, Transaction};
use uuid::Uuid;

pub mod access;
pub mod catalog;
pub mod cleanup;
pub mod commands;
pub mod exports;
pub mod intake;
pub mod integration;
pub mod preparations;
pub mod profiles;
pub mod proposals;
pub mod reads;
pub mod rebases;
pub mod review;
pub mod runs;
pub mod seals;
pub mod uploads;
pub mod workspace_reads;
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/001_transactions.sql"),
    include_str!("../migrations/002_object_catalog.sql"),
    include_str!("../migrations/003_change_intake.sql"),
    include_str!("../migrations/004_access_control.sql"),
    include_str!("../migrations/005_source_profiles.sql"),
    include_str!("../migrations/006_source_uploads.sql"),
    include_str!("../migrations/007_workspace_preparations.sql"),
    include_str!("../migrations/008_managed_runs.sql"),
    include_str!("../migrations/009_change_uploads.sql"),
    include_str!("../migrations/010_native_integration.sql"),
    include_str!("../migrations/011_check_execution.sql"),
    include_str!("../migrations/012_change_rebase.sql"),
    include_str!("../migrations/013_workspace_seals.sql"),
    include_str!("../migrations/014_workspace_cleanup.sql"),
];
const MAX_JSON: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database operation failed: {0}")]
    Database(#[from] tokio_postgres::Error),
    #[error("invalid metadata input: {0}")]
    Invalid(&'static str),
    #[error("idempotency key reused with different request")]
    IdempotencyConflict,
    #[error("repository not found in tenant")]
    NotFound,
    #[error("repository authority, epoch or active state changed")]
    Fenced,
    #[error("worker lease is expired or superseded")]
    StaleLease,
    #[error("metadata schema is unsupported or migration checksum changed")]
    SchemaMismatch,
    #[error("database must have fsync enabled")]
    UnsafeDurability,
    #[error("object storage contract failed: {0}")]
    Objects(#[from] treeharbor_core::Error),
    #[error("object is being uploaded or retired")]
    ObjectBusy,
    #[error("referenced object is not live or has the wrong type")]
    ObjectNotLive,
    #[error("object receipt does not match its reservation")]
    ReceiptMismatch,
    #[error("root holder already refers to another object")]
    PinConflict,
    #[error("workspace or agent run is not in the required current state")]
    ExecutionState,
    #[error("changeset version changed or belongs to another principal")]
    VersionConflict,
    #[error("change rebase is closed, expired or uses an unsupported profile")]
    RebaseState,
    #[error("object read is outside its session or verified ancestry")]
    ReadScope,
    #[error("object read exceeds its batch resource limit")]
    ReadLimit,
    #[error("credential is missing, invalid, expired or revoked")]
    Unauthenticated,
    #[error("principal lacks permission for this repository operation")]
    Forbidden,
    #[error("secure credential generation failed")]
    CredentialGeneration,
    #[error("source upload is closed, expired or outside its scope")]
    UploadState,
    #[error("source upload exceeds its resource budget")]
    UploadLimit,
    #[error("source upload does not contain all referenced objects")]
    UploadMembership,
    #[error("native integration state is not ready for this operation")]
    IntegrationState,
    #[error("native target head, generation or policy changed")]
    StaleCandidate,
    #[error("native candidate lacks current checks or review authorization")]
    AdmissionBlocked,
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authority {
    Connected,
    Standalone,
}
impl Authority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Standalone => "standalone",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Scope {
    tenant: String,
    repository: Uuid,
}
impl Scope {
    /// A scope is an identifier, NOT proof that a principal may access it.
    pub fn new(tenant: impl Into<String>, repository: Uuid) -> Result<Self> {
        let tenant = tenant.into();
        tenant_valid(&tenant)?;
        Ok(Self { tenant, repository })
    }
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    pub fn repository(&self) -> Uuid {
        self.repository
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    scope: Scope,
    key: String,
    principal: String,
    kind: String,
    authority: Authority,
    epoch: i64,
    digest: Vec<u8>,
    authorization: Option<std::sync::Arc<access::Authorization>>,
}
impl Request {
    /// `command` must be the service's complete, canonical command representation.
    /// Scope, identity, method, authority and epoch are additionally bound here.
    pub fn new(
        scope: Scope,
        key: &str,
        principal: &str,
        kind: &str,
        authority: Authority,
        epoch: i64,
        command: &[u8],
    ) -> Result<Self> {
        identifier(key, 128)?;
        identifier(principal, 128)?;
        identifier(kind, 64)?;
        if epoch <= 0 || command.len() > MAX_JSON {
            return Err(Error::Invalid("epoch or command size"));
        }
        let mut digest = Sha256::new();
        digest.update(b"treeharbor-operation\0v1\0");
        for part in [
            scope.tenant.as_bytes(),
            scope.repository.as_bytes(),
            key.as_bytes(),
            principal.as_bytes(),
            kind.as_bytes(),
            authority.as_str().as_bytes(),
            &epoch.to_be_bytes(),
            command,
        ] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part);
        }
        Ok(Self {
            scope,
            key: key.into(),
            principal: principal.into(),
            kind: kind.into(),
            authority,
            epoch,
            digest: digest.finalize().to_vec(),
            authorization: None,
        })
    }
}

pub struct Event {
    pub kind: String,
    pub payload: Value,
}

pub enum Begin<'a> {
    Replay(Value),
    Fresh(Mutation<'a>),
}

/// Dropping a Mutation rolls back. A result is acknowledged only after complete()
/// returns, which commits the caller's SQL effects, response and outbox together.
pub struct Mutation<'a> {
    tx: Transaction<'a>,
    request: Request,
}
impl Mutation<'_> {
    /// Trusted internal modules only. Never expose SQL execution through agent APIs.
    /// The repository row is locked for the duration of this short transaction.
    pub fn transaction(&self) -> &Transaction<'_> {
        &self.tx
    }
    pub async fn complete(self, result: Value, events: Vec<Event>) -> Result<Value> {
        self.write_completion(&result, &events).await?;
        self.tx.commit().await?;
        Ok(result)
    }
    /// Stage the same receipt/outbox as complete(), allowing an internal caller
    /// to check its fixed deadline after every write and before committing.
    pub(crate) async fn write_completion(&self, result: &Value, events: &[Event]) -> Result<()> {
        json_bound(result)?;
        if events.len() > 256 {
            return Err(Error::Invalid("too many outbox events"));
        }
        for event in events {
            identifier(&event.kind, 64)?;
            json_bound(&event.payload)?;
        }
        let r = &self.request;
        self.tx.execute("INSERT INTO th_operations (tenant,repository,key,request_digest,principal,kind,result) VALUES ($1,$2,$3,$4,$5,$6,$7)",
            &[&r.scope.tenant, &r.scope.repository, &r.key, &r.digest, &r.principal, &r.kind, &result]).await?;
        for (ordinal, event) in events.iter().enumerate() {
            self.tx.execute("INSERT INTO th_outbox (tenant,repository,operation_key,ordinal,kind,payload) VALUES ($1,$2,$3,$4,$5,$6)",
                &[&r.scope.tenant, &r.scope.repository, &r.key, &(ordinal as i32), &event.kind, &event.payload]).await?;
        }
        if let Some(auth) = &r.authorization {
            auth.check_transaction(
                &self.tx,
                &r.scope,
                &r.principal,
                access::Permission::for_command(&r.kind)?,
            )
            .await?;
        }
        Ok(())
    }
}

pub struct Metadata {
    client: Client,
}
impl Metadata {
    /// The caller supplies a connection with its deployment's verified TLS or a
    /// private Unix socket. This library never silently falls back to plaintext.
    pub async fn new(client: Client) -> Result<Self> {
        let fsync: String = client.query_one("SHOW fsync", &[]).await?.get(0);
        if fsync != "on" {
            return Err(Error::UnsafeDurability);
        }
        Ok(Self { client })
    }
    /// Check a pooled connection after any dropped request transaction has queued
    /// its rollback; command ordering on the connection completes that first.
    pub async fn ping(&self) -> Result<()> {
        self.client.simple_query("SELECT 1").await?;
        Ok(())
    }
    /// Serving requires the exact current schema. Upgrade is an explicit admin
    /// action, never a side effect of an incoming agent request.
    pub async fn check_schema(&self) -> Result<()> {
        let row = self
            .client
            .query_opt(
                "SELECT version,migration_digest FROM th_schema_version WHERE singleton",
                &[],
            )
            .await?
            .ok_or(Error::SchemaMismatch)?;
        let mut digest = Vec::new();
        for migration in MIGRATIONS {
            let mut hash = Sha256::new();
            hash.update(digest);
            hash.update(migration.as_bytes());
            digest = hash.finalize().to_vec();
        }
        if row.get::<_, i32>(0) != MIGRATIONS.len() as i32 || row.get::<_, Vec<u8>>(1) != digest {
            return Err(Error::SchemaMismatch);
        }
        Ok(())
    }
    pub async fn migrate(&mut self) -> Result<()> {
        let tx = self.client.transaction().await?;
        tx.batch_execute(
            "SET LOCAL synchronous_commit = on; SELECT pg_advisory_xact_lock(7468243701081)",
        )
        .await?;
        let exists: bool = tx
            .query_one("SELECT to_regclass('th_schema_version') IS NOT NULL", &[])
            .await?
            .get(0);
        let mut digests = Vec::new();
        let mut previous = Vec::new();
        for migration in MIGRATIONS {
            let mut hash = Sha256::new();
            hash.update(&previous);
            hash.update(migration.as_bytes());
            previous = hash.finalize().to_vec();
            digests.push(previous.clone());
        }
        let current = if exists {
            let row = tx
                .query_opt(
                    "SELECT version,migration_digest FROM th_schema_version WHERE singleton",
                    &[],
                )
                .await?
                .ok_or(Error::SchemaMismatch)?;
            let version: i32 = row.get(0);
            if version < 1
                || version as usize > MIGRATIONS.len()
                || row.get::<_, Vec<u8>>(1) != digests[version as usize - 1]
            {
                return Err(Error::SchemaMismatch);
            }
            version as usize
        } else {
            0
        };
        for index in current..MIGRATIONS.len() {
            tx.batch_execute(MIGRATIONS[index]).await?;
            tx.execute("INSERT INTO th_schema_version (version,migration_digest) VALUES ($1,$2) ON CONFLICT (singleton) DO UPDATE SET version=EXCLUDED.version,migration_digest=EXCLUDED.migration_digest",
                &[&((index+1) as i32), &digests[index]]).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    /// Repository provisioning; safe to retry with the same authority. This is
    /// not a mode migration. Retired identities cannot be recreated.
    pub async fn create_repository(&mut self, scope: &Scope, authority: Authority) -> Result<()> {
        let tx = self.client.transaction().await?;
        tx.batch_execute("SET LOCAL synchronous_commit = on")
            .await?;
        tx.execute("INSERT INTO th_repositories (tenant,repository,authority) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING",
                   &[&scope.tenant,&scope.repository,&authority.as_str()]).await?;
        let row = tx.query_one("SELECT authority,state,epoch FROM th_repositories WHERE tenant=$1 AND repository=$2 FOR UPDATE",
                              &[&scope.tenant,&scope.repository]).await?;
        if row.get::<_, String>(0) != authority.as_str()
            || row.get::<_, String>(1) != "active"
            || row.get::<_, i64>(2) != 1
        {
            return Err(Error::Fenced);
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn begin(&mut self, request: Request) -> Result<Begin<'_>> {
        let tx = self.client.transaction().await?;
        tx.batch_execute("SET LOCAL synchronous_commit = on; SET LOCAL lock_timeout = '5s'; SET LOCAL statement_timeout = '15s'").await?;
        let scope = &request.scope;
        // Serializes short mutations per repository; bulk object IO happens
        // before this point. This is a deliberate correctness baseline.
        let repo = tx.query_opt("SELECT authority,state,epoch FROM th_repositories WHERE tenant=$1 AND repository=$2 FOR UPDATE",
                               &[&scope.tenant,&scope.repository]).await?.ok_or(Error::NotFound)?;
        if let Some(auth) = &request.authorization {
            auth.check_transaction(
                &tx,
                scope,
                &request.principal,
                access::Permission::for_command(&request.kind)?,
            )
            .await?;
        }
        if let Some(row) = tx.query_opt("SELECT request_digest,result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3",
                                       &[&scope.tenant,&scope.repository,&request.key]).await? {
            if row.get::<_,Vec<u8>>(0) != request.digest { return Err(Error::IdempotencyConflict); }
            let result: Value = row.get(1);
            if let Some(auth) = &request.authorization {
                auth.check_transaction(&tx, scope, &request.principal, access::Permission::for_command(&request.kind)?).await?;
            }
            tx.commit().await?;
            return Ok(Begin::Replay(result));
        }
        if repo.get::<_, String>(0) != request.authority.as_str()
            || repo.get::<_, String>(1) != "active"
            || repo.get::<_, i64>(2) != request.epoch
        {
            return Err(Error::Fenced);
        }
        Ok(Begin::Fresh(Mutation { tx, request }))
    }
    /// At-least-once delivery, scoped by tenant. A reclaimed job gets a newer
    /// generation, so the previous worker cannot acknowledge it.
    pub async fn claim(
        &mut self,
        tenant: &str,
        worker: Uuid,
        ttl_seconds: i32,
    ) -> Result<Option<Job>> {
        tenant_valid(tenant)?;
        ttl_valid(ttl_seconds)?;
        let tx = self.client.transaction().await?;
        tx.batch_execute("SET LOCAL synchronous_commit = on")
            .await?;
        let row = tx.query_opt("WITH selected AS (SELECT id FROM th_outbox WHERE tenant=$1 AND ((state='pending' AND available_at <= clock_timestamp()) OR (state='running' AND lease_until <= clock_timestamp())) ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED) UPDATE th_outbox o SET state='running',worker=$2,generation=generation+1,lease_until=clock_timestamp()+($3::integer * interval '1 second') FROM selected WHERE o.id=selected.id RETURNING o.id,o.repository,o.generation,o.kind,o.payload",
                               &[&tenant,&worker,&ttl_seconds]).await?;
        let job = row.map(|row| Job {
            id: row.get(0),
            scope: Scope {
                tenant: tenant.into(),
                repository: row.get(1),
            },
            generation: row.get(2),
            worker,
            kind: row.get(3),
            payload: row.get(4),
        });
        tx.commit().await?;
        Ok(job)
    }
    pub async fn acknowledge(&mut self, job: &Job) -> Result<()> {
        let tx = self.client.transaction().await?;
        tx.batch_execute("SET LOCAL synchronous_commit = on")
            .await?;
        let count = tx.execute("UPDATE th_outbox SET state='done',worker=NULL,lease_until=NULL,completed_at=clock_timestamp() WHERE id=$1 AND tenant=$2 AND repository=$3 AND state='running' AND worker=$4 AND generation=$5 AND lease_until>clock_timestamp()",
                              &[&job.id,&job.scope.tenant,&job.scope.repository,&job.worker,&job.generation]).await?;
        if count != 1 {
            return Err(Error::StaleLease);
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn renew(&mut self, job: &Job, ttl_seconds: i32) -> Result<()> {
        ttl_valid(ttl_seconds)?;
        let tx = self.client.transaction().await?;
        tx.batch_execute("SET LOCAL synchronous_commit = on")
            .await?;
        let count = tx.execute("UPDATE th_outbox SET lease_until=clock_timestamp()+($6::integer * interval '1 second') WHERE id=$1 AND tenant=$2 AND repository=$3 AND state='running' AND worker=$4 AND generation=$5 AND lease_until>clock_timestamp()",
            &[&job.id,&job.scope.tenant,&job.scope.repository,&job.worker,&job.generation,&ttl_seconds]).await?;
        if count != 1 {
            return Err(Error::StaleLease);
        }
        tx.commit().await?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: i64,
    pub scope: Scope,
    pub generation: i64,
    worker: Uuid,
    pub kind: String,
    pub payload: Value,
}
fn tenant_valid(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(Error::Invalid("tenant"));
    }
    Ok(())
}
fn identifier(value: &str, max: usize) -> Result<()> {
    if value.is_empty() || value.len() > max || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Error::Invalid("identifier"));
    }
    Ok(())
}
fn json_bound(value: &Value) -> Result<()> {
    if value.to_string().len() > MAX_JSON {
        return Err(Error::Invalid("JSON size"));
    }
    Ok(())
}
fn ttl_valid(ttl: i32) -> Result<()> {
    if !(1..=3600).contains(&ttl) {
        return Err(Error::Invalid("lease TTL"));
    }
    Ok(())
}
