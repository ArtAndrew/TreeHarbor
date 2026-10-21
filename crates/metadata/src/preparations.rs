//! Durable ownership before node filesystem side effects. These operations do
//! not materialize files, launch a runtime, stop writers or release storage.
use crate::{
    Authority, Error, Job, Metadata, Result, Scope,
    access::{Authorization, Permission, repository_lock},
    catalog::settings,
    identifier, tenant_valid, ttl_valid,
};
use serde_json::{Value, json};
use tokio_postgres::Transaction;
use treeharbor_core::{ObjectId, ProfileId, RevisionRef, SourceProfile};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageSlot {
    pub storage: Uuid,
    pub slot: Uuid,
}

/// Current leased preparation context, constructed from authoritative metadata.
/// It is not a filesystem receipt or permission to bypass a runtime boundary.
#[derive(Clone, Debug)]
pub struct Preparation {
    attempt: Uuid,
    job: Job,
    workspace: Uuid,
    generation: i64,
    authority: Authority,
    epoch: i64,
    node: String,
    slot: StorageSlot,
    revision: RevisionRef,
    snapshot: ObjectId,
    profile: SourceProfile,
    holder: String,
}
impl Preparation {
    pub fn attempt(&self) -> Uuid {
        self.attempt
    }
    pub fn scope(&self) -> &Scope {
        &self.job.scope
    }
    pub fn workspace(&self) -> Uuid {
        self.workspace
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
    pub fn authority(&self) -> Authority {
        self.authority
    }
    pub fn epoch(&self) -> i64 {
        self.epoch
    }
    pub fn node(&self) -> &str {
        &self.node
    }
    pub fn slot(&self) -> StorageSlot {
        self.slot
    }
    pub fn revision(&self) -> &RevisionRef {
        &self.revision
    }
    pub fn snapshot(&self) -> ObjectId {
        self.snapshot
    }
    pub fn profile(&self) -> &SourceProfile {
        &self.profile
    }
    pub fn source_holder(&self) -> &str {
        &self.holder
    }
}

/// Trusted node observation of fully published, durable files. It proves no
/// execution, sandbox isolation, validation success or absence of old writers.
pub struct PreparedFiles {
    pub snapshot: ObjectId,
    pub profile: ProfileId,
    /// SHA-256 of the exact durable manifest bytes, not a canonical source object.
    pub manifest_digest: [u8; 32],
}

/// Authorized current file-ready placement. This is inspection, not a job lease
/// or permission to start an execution. Runtime binding must still be absent.
pub struct PreparedWorkspace {
    pub scope: Scope,
    pub attempt: Uuid,
    pub workspace: Uuid,
    pub generation: i64,
    pub epoch: i64,
    pub authority: Authority,
    pub node: String,
    pub slot: StorageSlot,
    pub revision: RevisionRef,
    pub snapshot: ObjectId,
    pub profile: SourceProfile,
    pub manifest_digest: [u8; 32],
}

async fn authorize(tx: &Transaction<'_>, auth: &Authorization) -> Result<()> {
    auth.check_transaction(
        tx,
        auth.scope(),
        auth.principal(),
        Permission::RuntimeControl,
    )
    .await
}

async fn registered_storage(tx: &Transaction<'_>, auth: &Authorization) -> Result<Uuid> {
    tx.query_opt(
        "SELECT storage_instance FROM th_node_storage WHERE tenant=$1 AND node=$2",
        &[&auth.scope().tenant(), &auth.principal()],
    )
    .await?
    .map(|r| r.get(0))
    .ok_or(Error::NotFound)
}

async fn current_authority(tx: &Transaction<'_>, scope: &Scope, epoch: i64) -> Result<Authority> {
    let row = tx
        .query_one(
            "SELECT authority,epoch,state FROM th_repositories WHERE tenant=$1 AND repository=$2",
            &[&scope.tenant(), &scope.repository()],
        )
        .await?;
    if row.get::<_, i64>(1) != epoch || row.get::<_, String>(2) != "active" {
        return Err(Error::Fenced);
    }
    match row.get::<_, String>(0).as_str() {
        "connected" => Ok(Authority::Connected),
        "standalone" => Ok(Authority::Standalone),
        _ => Err(Error::SchemaMismatch),
    }
}

/// Lock and revalidate the stored event, not just the caller's Job struct.
async fn live_job(
    tx: &Transaction<'_>,
    auth: &Authorization,
    job: &Job,
) -> Result<(Uuid, i64, i64)> {
    if job.scope.tenant() != auth.scope().tenant()
        || job.scope.repository() != auth.scope().repository()
        || job.kind != "workspace.prepare"
    {
        return Err(Error::StaleLease);
    }
    let row=tx.query_opt("SELECT kind,payload FROM th_outbox WHERE id=$1 AND tenant=$2 AND repository=$3 AND state='running' AND worker=$4 AND generation=$5 AND lease_until>clock_timestamp() FOR UPDATE",
        &[&job.id,&job.scope.tenant(),&job.scope.repository(),&job.worker,&job.generation]).await?.ok_or(Error::StaleLease)?;
    if row.get::<_, String>(0) != job.kind || row.get::<_, Value>(1) != job.payload {
        return Err(Error::StaleLease);
    }
    let workspace = job.payload["workspace"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(Error::StaleLease)?;
    let generation = job.payload["generation"]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or(Error::StaleLease)?;
    let epoch = job.payload["epoch"]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or(Error::StaleLease)?;
    Ok((workspace, generation, epoch))
}

fn digest(bytes: Vec<u8>) -> Result<[u8; 32]> {
    bytes.try_into().map_err(|_| Error::SchemaMismatch)
}

async fn bound_preparation(
    tx: &Transaction<'_>,
    auth: &Authorization,
    plan: &Preparation,
) -> Result<tokio_postgres::Row> {
    let scope = auth.scope();
    tx.query_opt(r#"SELECT p.state,p.manifest_digest,p.completed_worker,p.completed_job_generation,w.state
            FROM th_workspace_preparations p JOIN th_workspaces w ON w.tenant=p.tenant AND w.repository=p.repository AND w.workspace=p.workspace
            JOIN th_bases b ON b.tenant=w.tenant AND b.repository=w.repository AND b.revision=w.base_revision
            WHERE p.attempt=$1 AND p.tenant=$2 AND p.repository=$3 AND p.node=$4 AND p.storage_instance=$5 AND p.slot=$6
            AND p.generation=w.generation AND p.epoch=w.epoch AND p.generation=$7 AND p.epoch=$8 AND p.job_id=$9
            AND w.node=p.node AND w.runtime IS NULL AND p.base_revision=w.base_revision AND p.snapshot=b.snapshot AND p.profile=w.resolved_profile
            AND p.snapshot=$10 AND p.profile=$11 AND p.workspace=$12 AND p.base_revision=$13 AND w.state IN ('requested','ready')"#,
            &[&plan.attempt,&scope.tenant(),&scope.repository(),&auth.principal(),&plan.slot.storage,&plan.slot.slot,&plan.generation,&plan.epoch,&plan.job.id,&plan.snapshot.as_bytes().as_slice(),&plan.profile.id().as_bytes().as_slice(),&plan.workspace,&plan.revision.to_string()]).await?.ok_or(Error::ExecutionState)
}

impl Metadata {
    pub async fn prepared_workspace(
        &mut self,
        auth: &Authorization,
        workspace: Uuid,
    ) -> Result<PreparedWorkspace> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let scope = auth.scope();
        repository_lock(&tx, scope, false).await?;
        authorize(&tx, auth).await?;
        let storage = registered_storage(&tx, auth).await?;
        let row = tx.query_opt(r#"SELECT p.attempt,p.generation,p.epoch,p.slot,p.base_revision,p.snapshot,p.profile,f.definition,p.manifest_digest
            FROM th_workspace_preparations p
            JOIN th_workspaces w ON w.tenant=p.tenant AND w.repository=p.repository AND w.workspace=p.workspace
            JOIN th_bases b ON b.tenant=p.tenant AND b.repository=p.repository AND b.revision=p.base_revision
            JOIN th_source_profiles f ON f.tenant=p.tenant AND f.repository=p.repository AND f.digest=p.profile
            WHERE p.tenant=$1 AND p.repository=$2 AND p.workspace=$3 AND p.node=$4 AND p.storage_instance=$5
              AND p.state='prepared' AND w.state='ready' AND w.runtime IS NULL AND w.node=p.node
              AND w.generation=p.generation AND w.epoch=p.epoch AND w.base_revision=p.base_revision
              AND w.resolved_profile=p.profile AND b.snapshot=p.snapshot"#,
            &[&scope.tenant(), &scope.repository(), &workspace, &auth.principal(), &storage])
            .await?.ok_or(Error::ExecutionState)?;
        let epoch = row.get(2);
        let authority = current_authority(&tx, scope, epoch).await?;
        let profile = SourceProfile::decode(&row.get::<_, Vec<u8>>(7))?;
        if profile.id().as_bytes() != &digest(row.get(6))? {
            return Err(Error::SchemaMismatch);
        }
        let result = PreparedWorkspace {
            scope: scope.clone(),
            attempt: row.get(0),
            workspace,
            generation: row.get(1),
            epoch,
            authority,
            node: auth.principal().into(),
            slot: StorageSlot {
                storage,
                slot: row.get(3),
            },
            revision: row.get::<_, String>(4).parse()?,
            snapshot: ObjectId::from_bytes(digest(row.get(5))?),
            profile,
            manifest_digest: digest(row.get(8))?,
        };
        authorize(&tx, auth).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Trusted administration only. Register an existing Node principal and its
    /// persistent storage UUID; neither the UUID nor its owner can be remapped.
    pub async fn register_node_storage(
        &mut self,
        tenant: &str,
        node: &str,
        storage: Uuid,
        operator: &str,
    ) -> Result<()> {
        tenant_valid(tenant)?;
        identifier(node, 128)?;
        identifier(operator, 128)?;
        if storage.is_nil() {
            return Err(Error::Invalid("node storage identity"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let row = tx
            .query_opt(
                "SELECT kind,enabled FROM th_principals WHERE tenant=$1 AND principal=$2 FOR SHARE",
                &[&tenant, &node],
            )
            .await?
            .ok_or(Error::NotFound)?;
        if row.get::<_, String>(0) != "node" || !row.get::<_, bool>(1) {
            return Err(Error::Forbidden);
        }
        let inserted=tx.execute("INSERT INTO th_node_storage(tenant,node,storage_instance) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",&[&tenant,&node,&storage]).await?;
        let row = tx
            .query_opt(
                "SELECT storage_instance FROM th_node_storage WHERE tenant=$1 AND node=$2",
                &[&tenant, &node],
            )
            .await?
            .ok_or(Error::Fenced)?;
        if row.get::<_, Uuid>(0) != storage {
            return Err(Error::Fenced);
        }
        if inserted == 1 {
            tx.execute("INSERT INTO th_security_audit(tenant,operator,action,principal) VALUES($1,$2,'node.storage.register',$3)",&[&tenant,&operator,&node]).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Claim only native preparation jobs permitted to this node. A durable
    /// assignment stays on its node even when a previous worker lease expires.
    pub async fn claim_preparation(
        &mut self,
        auth: &Authorization,
        storage: Uuid,
        worker: Uuid,
        ttl_seconds: i32,
    ) -> Result<Option<Job>> {
        ttl_valid(ttl_seconds)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let scope = auth.scope();
        repository_lock(&tx, scope, false).await?;
        authorize(&tx, auth).await?;
        if registered_storage(&tx, auth).await? != storage {
            return Err(Error::Fenced);
        }
        let row=tx.query_opt(r#"WITH selected AS (
            SELECT o.id FROM th_outbox o
            JOIN th_workspaces w ON w.tenant=o.tenant AND w.repository=o.repository AND w.workspace::text=o.payload->>'workspace'
            JOIN th_repositories r ON r.tenant=w.tenant AND r.repository=w.repository
            WHERE o.tenant=$1 AND o.repository=$2 AND o.kind='workspace.prepare'
              AND ((o.state='pending' AND o.available_at<=clock_timestamp()) OR (o.state='running' AND o.lease_until<=clock_timestamp()))
              AND w.state='requested' AND w.format='native' AND w.resolved_profile IS NOT NULL AND w.runtime IS NULL
              AND (w.node IS NULL OR w.node=$3) AND w.epoch=r.epoch AND r.state='active'
              AND o.payload->>'generation'=w.generation::text AND o.payload->>'epoch'=w.epoch::text
            ORDER BY o.id LIMIT 1 FOR UPDATE OF o SKIP LOCKED
        ) UPDATE th_outbox o SET state='running',worker=$4,generation=generation+1,
            lease_until=clock_timestamp()+($5::integer * interval '1 second')
          FROM selected WHERE o.id=selected.id RETURNING o.id,o.generation,o.kind,o.payload"#,
            &[&scope.tenant(),&scope.repository(),&auth.principal(),&worker,&ttl_seconds]).await?;
        let job = row.map(|row| Job {
            id: row.get(0),
            scope: scope.clone(),
            generation: row.get(1),
            worker,
            kind: row.get(2),
            payload: row.get(3),
        });
        authorize(&tx, auth).await?;
        tx.commit().await?;
        Ok(job)
    }

    /// Reserve before touching a slot. Retry/reclaim on this node returns the
    /// same attempt; it never creates an alternative path for the same generation.
    pub async fn reserve_preparation(
        &mut self,
        auth: &Authorization,
        job: &Job,
        slot: StorageSlot,
    ) -> Result<Preparation> {
        if slot.slot.is_nil() || slot.storage.is_nil() {
            return Err(Error::Invalid("storage slot"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let scope = auth.scope();
        repository_lock(&tx, scope, true).await?;
        authorize(&tx, auth).await?;
        if registered_storage(&tx, auth).await? != slot.storage {
            return Err(Error::Fenced);
        }
        let (workspace, generation, epoch) = live_job(&tx, auth, job).await?;
        let authority = current_authority(&tx, scope, epoch).await?;
        let row=tx.query_opt(r#"SELECT w.base_revision,b.snapshot,w.resolved_profile,p.definition,w.pin_holder
            FROM th_workspaces w JOIN th_bases b ON b.tenant=w.tenant AND b.repository=w.repository AND b.revision=w.base_revision
            JOIN th_source_profiles p ON p.tenant=w.tenant AND p.repository=w.repository AND p.digest=w.resolved_profile
            WHERE w.tenant=$1 AND w.repository=$2 AND w.workspace=$3 AND w.generation=$4 AND w.epoch=$5
            AND w.state='requested' AND w.format='native' AND w.runtime IS NULL AND (w.node IS NULL OR w.node=$6)"#,
            &[&scope.tenant(),&scope.repository(),&workspace,&generation,&epoch,&auth.principal()]).await?.ok_or(Error::ExecutionState)?;
        let revision: String = row.get(0);
        let parsed_revision = revision.parse()?;
        let snapshot = digest(row.get(1))?;
        let profile_bytes = digest(row.get(2))?;
        let profile = SourceProfile::decode(&row.get::<_, Vec<u8>>(3))?;
        if profile.id().as_bytes() != &profile_bytes {
            return Err(Error::SchemaMismatch);
        }
        let holder: String = row.get(4);
        tx.execute(r#"INSERT INTO th_workspace_preparations(attempt,tenant,repository,workspace,generation,epoch,node,storage_instance,slot,job_id,base_revision,snapshot,profile,state)
            VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,'reserved') ON CONFLICT DO NOTHING"#,
            &[&Uuid::new_v4(),&scope.tenant(),&scope.repository(),&workspace,&generation,&epoch,&auth.principal(),&slot.storage,&slot.slot,&job.id,&revision,&snapshot.as_slice(),&profile_bytes.as_slice()]).await?;
        let existing=tx.query_opt(r#"SELECT attempt FROM th_workspace_preparations
            WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND generation=$4 AND epoch=$5 AND node=$6
            AND storage_instance=$7 AND slot=$8 AND job_id=$9 AND base_revision=$10 AND snapshot=$11 AND profile=$12 AND state='reserved'"#,
            &[&scope.tenant(),&scope.repository(),&workspace,&generation,&epoch,&auth.principal(),&slot.storage,&slot.slot,&job.id,&revision,&snapshot.as_slice(),&profile_bytes.as_slice()]).await?.ok_or(Error::ExecutionState)?;
        let attempt = existing.get(0);
        tx.execute(
            "UPDATE th_workspaces SET node=$4 WHERE tenant=$1 AND repository=$2 AND workspace=$3",
            &[
                &scope.tenant(),
                &scope.repository(),
                &workspace,
                &auth.principal(),
            ],
        )
        .await?;
        // A long lock wait must not turn an expired lease/token into authority.
        live_job(&tx, auth, job).await?;
        authorize(&tx, auth).await?;
        tx.commit().await?;
        Ok(Preparation {
            attempt,
            job: job.clone(),
            workspace,
            generation,
            authority,
            epoch,
            node: auth.principal().into(),
            slot,
            revision: parsed_revision,
            snapshot: ObjectId::from_bytes(snapshot),
            profile,
            holder,
        })
    }

    /// Read durable ownership for restart/cleanup planning. This response is
    /// intentionally not a live Job or permission to start/delete any process.
    pub async fn preparation_status(
        &mut self,
        auth: &Authorization,
        workspace: Uuid,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let scope = auth.scope();
        repository_lock(&tx, scope, false).await?;
        authorize(&tx, auth).await?;
        let storage = registered_storage(&tx, auth).await?;
        let row=tx.query_opt("SELECT attempt,generation,epoch,storage_instance,slot,state,manifest_digest FROM th_workspace_preparations WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND node=$4 ORDER BY generation DESC LIMIT 1",
            &[&scope.tenant(),&scope.repository(),&workspace,&auth.principal()]).await?.ok_or(Error::NotFound)?;
        if row.get::<_, Uuid>(3) != storage {
            return Err(Error::Fenced);
        }
        let manifest: Option<Vec<u8>> = row.get(6);
        let manifest = manifest
            .map(digest)
            .transpose()?
            .map(|d| ObjectId::from_bytes(d).to_string());
        let result = json!({"attempt":row.get::<_,Uuid>(0).to_string(),"workspace":workspace.to_string(),"generation":row.get::<_,i64>(1),"epoch":row.get::<_,i64>(2),"storage":storage.to_string(),"slot":row.get::<_,Uuid>(4).to_string(),"state":row.get::<_,String>(5),"manifest_digest":manifest});
        authorize(&tx, auth).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Extend only a still-current preparation. A cached node credential or
    /// workspace which has begun draining cannot keep a job alive indefinitely.
    pub async fn renew_preparation(
        &mut self,
        auth: &Authorization,
        plan: &Preparation,
        ttl_seconds: i32,
    ) -> Result<()> {
        ttl_valid(ttl_seconds)?;
        if auth.scope().tenant() != plan.scope().tenant()
            || auth.scope().repository() != plan.scope().repository()
            || auth.principal() != plan.node
        {
            return Err(Error::Forbidden);
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), true).await?;
        authorize(&tx, auth).await?;
        if registered_storage(&tx, auth).await? != plan.slot.storage
            || current_authority(&tx, auth.scope(), plan.epoch).await? != plan.authority
        {
            return Err(Error::Fenced);
        }
        let row = bound_preparation(&tx, auth, plan).await?;
        if row.get::<_, String>(0) != "reserved" || row.get::<_, String>(4) != "requested" {
            return Err(Error::ExecutionState);
        }
        live_job(&tx, auth, &plan.job).await?;
        let changed=tx.execute("UPDATE th_outbox SET lease_until=clock_timestamp()+($4::integer * interval '1 second') WHERE id=$1 AND state='running' AND worker=$2 AND generation=$3 AND lease_until>clock_timestamp()",
            &[&plan.job.id,&plan.job.worker,&plan.job.generation,&ttl_seconds]).await?;
        if changed != 1 {
            return Err(Error::StaleLease);
        }
        authorize(&tx, auth).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Acknowledge durable files, atomically completing the preparation job.
    /// The runtime remains NULL: a separate run-attempt path must bind execution.
    pub async fn complete_preparation(
        &mut self,
        auth: &Authorization,
        plan: &Preparation,
        observed: &PreparedFiles,
    ) -> Result<Value> {
        if auth.scope().tenant() != plan.scope().tenant()
            || auth.scope().repository() != plan.scope().repository()
            || auth.principal() != plan.node
        {
            return Err(Error::Forbidden);
        }
        if observed.snapshot != plan.snapshot || observed.profile != plan.profile.id() {
            return Err(Error::ReceiptMismatch);
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let scope = auth.scope();
        repository_lock(&tx, scope, true).await?;
        authorize(&tx, auth).await?;
        if registered_storage(&tx, auth).await? != plan.slot.storage {
            return Err(Error::Fenced);
        }
        if current_authority(&tx, scope, plan.epoch).await? != plan.authority {
            return Err(Error::Fenced);
        }
        let row = bound_preparation(&tx, auth, plan).await?;
        match row.get::<_, String>(0).as_str() {
            "reserved" if row.get::<_, String>(4) == "requested" => {
                live_job(&tx, auth, &plan.job).await?;
                tx.execute("UPDATE th_workspace_preparations SET state='prepared',manifest_digest=$2,completed_worker=$3,completed_job_generation=$4,prepared_at=clock_timestamp() WHERE attempt=$1",
                    &[&plan.attempt,&observed.manifest_digest.as_slice(),&plan.job.worker,&plan.job.generation]).await?;
                tx.execute("UPDATE th_workspaces SET state='ready' WHERE tenant=$1 AND repository=$2 AND workspace=$3",&[&scope.tenant(),&scope.repository(),&plan.workspace]).await?;
                let changed=tx.execute("UPDATE th_outbox SET state='done',worker=NULL,lease_until=NULL,completed_at=clock_timestamp() WHERE id=$1 AND state='running' AND worker=$2 AND generation=$3 AND lease_until>clock_timestamp()",
                    &[&plan.job.id,&plan.job.worker,&plan.job.generation]).await?;
                if changed != 1 {
                    return Err(Error::StaleLease);
                }
            }
            "prepared" if row.get::<_, String>(4) == "ready" => {
                if row.get::<_, Option<Vec<u8>>>(1).as_deref()
                    != Some(observed.manifest_digest.as_slice())
                {
                    return Err(Error::ReceiptMismatch);
                }
                if row.get::<_, Option<Uuid>>(2) != Some(plan.job.worker)
                    || row.get::<_, Option<i64>>(3) != Some(plan.job.generation)
                {
                    return Err(Error::StaleLease);
                }
            }
            _ => return Err(Error::ExecutionState),
        }
        authorize(&tx, auth).await?;
        tx.commit().await?;
        Ok(
            json!({"workspace":plan.workspace.to_string(),"generation":plan.generation,"state":"ready","preparation":plan.attempt.to_string(),"manifest_digest":ObjectId::from_bytes(observed.manifest_digest).to_string()}),
        )
    }
}
