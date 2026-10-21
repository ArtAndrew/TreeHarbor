//! PostgreSQL primitives used only by trusted service code. These do not authenticate
//! agents, prove uploaded bytes durable, or implement merge admission. Callers must
//! finish object upload before opening a short metadata mutation transaction.

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_postgres::{Client, Transaction};
use uuid::Uuid;

const MIGRATION: &str = include_str!("../migrations/001_transactions.sql");
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
        json_bound(&result)?;
        if events.len() > 256 {
            return Err(Error::Invalid("too many outbox events"));
        }
        for event in &events {
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
        self.tx.commit().await?;
        Ok(result)
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
        let digest = Sha256::digest(MIGRATION.as_bytes()).to_vec();
        if exists {
            let row = tx
                .query_opt(
                    "SELECT version,migration_digest FROM th_schema_version WHERE singleton",
                    &[],
                )
                .await?
                .ok_or(Error::SchemaMismatch)?;
            if row.get::<_, i32>(0) != 1 || row.get::<_, Vec<u8>>(1) != digest {
                return Err(Error::SchemaMismatch);
            }
        } else {
            tx.batch_execute(MIGRATION).await?;
            tx.execute(
                "INSERT INTO th_schema_version (version,migration_digest) VALUES (1,$1)",
                &[&digest],
            )
            .await?;
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
        if let Some(row) = tx.query_opt("SELECT request_digest,result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3",
                                       &[&scope.tenant,&scope.repository,&request.key]).await? {
            if row.get::<_,Vec<u8>>(0) != request.digest { return Err(Error::IdempotencyConflict); }
            let result: Value = row.get(1);
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
