//! Authenticated source sessions rooted in a current, owned workspace. These
//! server-side values cannot be reconstructed from a digest supplied by a client.
use crate::{
    Authority, Error, Metadata, Result,
    access::{Authorization, Permission, repository_lock},
    catalog::{lease_pin_in, settings},
    reads::{ReadBatch, ReadNode, ReadSession},
};
use tokio_postgres::Transaction;
use treeharbor_core::{ObjectId, RevisionRef};
use treeharbor_storage::ManagedStore;
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub struct SourceFence {
    pub workspace: Uuid,
    pub generation: i64,
    pub authority: Authority,
    pub epoch: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceIdentity {
    pub revision: RevisionRef,
    pub snapshot: ObjectId,
    pub profile: ObjectId,
}
pub struct WorkspaceRead {
    auth: Authorization,
    fence: SourceFence,
    identity: SourceIdentity,
    session: ReadSession,
}
impl WorkspaceRead {
    pub fn root(&self) -> ReadNode {
        self.session.root()
    }
    pub fn identity(&self) -> &SourceIdentity {
        &self.identity
    }
}

// Repository locking also serializes workspace mutations. Recheck token expiry
// after all lookups: a read cannot retain an earlier cached authorization forever.
async fn context_in(
    tx: &Transaction<'_>,
    auth: &Authorization,
    fence: SourceFence,
) -> Result<(SourceIdentity, String)> {
    if fence.generation <= 0 || fence.epoch <= 0 {
        return Err(Error::Invalid("source fence"));
    }
    let scope = auth.scope();
    auth.check_transaction(tx, scope, auth.principal(), Permission::Read)
        .await?;
    let repo = tx
        .query_one(
            "SELECT authority,epoch,state FROM th_repositories WHERE tenant=$1 AND repository=$2",
            &[&scope.tenant(), &scope.repository()],
        )
        .await?;
    if repo.get::<_, String>(0) != fence.authority.as_str()
        || repo.get::<_, i64>(1) != fence.epoch
        || repo.get::<_, String>(2) != "active"
    {
        return Err(Error::Fenced);
    }
    let row = tx.query_opt("SELECT w.base_revision,b.snapshot,w.resolved_profile,w.pin_holder,w.generation,w.epoch,w.state FROM th_workspaces w JOIN th_bases b ON b.tenant=w.tenant AND b.repository=w.repository AND b.revision=w.base_revision WHERE w.tenant=$1 AND w.repository=$2 AND w.workspace=$3 AND w.principal=$4", &[&scope.tenant(),&scope.repository(),&fence.workspace,&auth.principal()]).await?.ok_or(Error::NotFound)?;
    if row.get::<_, i64>(4) != fence.generation
        || row.get::<_, i64>(5) != fence.epoch
        || !matches!(
            row.get::<_, String>(6).as_str(),
            "requested" | "ready" | "in_use" | "source_ready"
        )
    {
        return Err(Error::ExecutionState);
    }
    let profile: Vec<u8> = row
        .get::<_, Option<Vec<u8>>>(2)
        .ok_or(Error::ExecutionState)?;
    let identity = SourceIdentity {
        revision: row.get::<_, String>(0).parse()?,
        snapshot: ObjectId::from_bytes(
            row.get::<_, Vec<u8>>(1)
                .try_into()
                .map_err(|_| Error::SchemaMismatch)?,
        ),
        profile: ObjectId::from_bytes(profile.try_into().map_err(|_| Error::SchemaMismatch)?),
    };
    auth.check_transaction(tx, scope, auth.principal(), Permission::Read)
        .await?;
    Ok((identity, row.get(3)))
}

impl Metadata {
    pub async fn open_workspace_source(
        &mut self,
        auth: &Authorization,
        fence: SourceFence,
        ttl: i32,
    ) -> Result<WorkspaceRead> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), true).await?;
        let (identity, holder) = context_in(&tx, auth, fence).await?;
        let RevisionRef::Native(commit) = identity.revision else {
            return Err(Error::Invalid("native source required"));
        };
        let lease = lease_pin_in(&tx, auth.scope(), &holder, Uuid::new_v4(), ttl).await?;
        if lease.key().id() != commit {
            return Err(Error::ReadScope);
        }
        auth.check_transaction(&tx, auth.scope(), auth.principal(), Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(WorkspaceRead {
            auth: auth.clone(),
            fence,
            identity,
            session: ReadSession::from_lease(lease),
        })
    }
    async fn check_workspace_source(&mut self, read: &WorkspaceRead) -> Result<()> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, read.auth.scope(), false).await?;
        let (identity, _) = context_in(&tx, &read.auth, read.fence).await?;
        if identity != read.identity {
            return Err(Error::ExecutionState);
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn open_workspace_source_batch(
        &mut self,
        read: &WorkspaceRead,
        store: &ManagedStore,
        nodes: &[ReadNode],
        byte_limit: u64,
    ) -> Result<ReadBatch> {
        self.check_workspace_source(read).await?;
        self.open_read_batch(&read.session, store, nodes, byte_limit)
            .await
    }
    /// Call even for a missing path or an IO error. No buffered result or
    /// existence information may be sent until this final authorization succeeds.
    /// The hold is released even when revocation/drain rejects the result.
    pub async fn finish_workspace_source(&mut self, read: WorkspaceRead) -> Result<SourceIdentity> {
        let check = self.check_workspace_source(&read).await;
        let close = self.close_read_session(read.session).await;
        check?;
        close?;
        Ok(read.identity)
    }
}
