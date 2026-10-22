//! Authenticated complete-source reads from a repository's registered native base.
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
pub struct ExportFence {
    pub authority: Authority,
    pub epoch: i64,
    pub revision: ObjectId,
}
pub struct SourceExport {
    auth: Authorization,
    fence: ExportFence,
    snapshot: ObjectId,
    session: ReadSession,
}
impl SourceExport {
    pub fn root(&self) -> ReadNode {
        self.session.root()
    }
    pub fn snapshot(&self) -> ObjectId {
        self.snapshot
    }
}
async fn context(
    tx: &Transaction<'_>,
    auth: &Authorization,
    fence: ExportFence,
) -> Result<(ObjectId, String)> {
    if fence.epoch <= 0 {
        return Err(Error::Invalid("export epoch"));
    }
    let s = auth.scope();
    auth.check_transaction(tx, s, auth.principal(), Permission::Read)
        .await?;
    let repo = tx
        .query_one(
            "SELECT authority,epoch,state FROM th_repositories WHERE tenant=$1 AND repository=$2",
            &[&s.tenant(), &s.repository()],
        )
        .await?;
    if repo.get::<_, String>(0) != fence.authority.as_str()
        || repo.get::<_, i64>(1) != fence.epoch
        || repo.get::<_, String>(2) != "active"
    {
        return Err(Error::Fenced);
    }
    let revision = RevisionRef::Native(fence.revision).to_string();
    let row = tx.query_opt("SELECT snapshot,pin_holder FROM th_bases WHERE tenant=$1 AND repository=$2 AND revision=$3", &[&s.tenant(), &s.repository(), &revision]).await?.ok_or(Error::NotFound)?;
    let snapshot = ObjectId::from_bytes(
        row.get::<_, Vec<u8>>(0)
            .try_into()
            .map_err(|_| Error::SchemaMismatch)?,
    );
    auth.check_transaction(tx, s, auth.principal(), Permission::Read)
        .await?;
    Ok((snapshot, row.get(1)))
}
impl Metadata {
    pub async fn open_source_export(
        &mut self,
        auth: &Authorization,
        fence: ExportFence,
        ttl: i32,
    ) -> Result<SourceExport> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), true).await?;
        let (snapshot, holder) = context(&tx, auth, fence).await?;
        let lease = lease_pin_in(&tx, auth.scope(), &holder, Uuid::new_v4(), ttl).await?;
        if lease.key().id() != fence.revision {
            return Err(Error::ReadScope);
        }
        auth.check_transaction(&tx, auth.scope(), auth.principal(), Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(SourceExport {
            auth: auth.clone(),
            fence,
            snapshot,
            session: ReadSession::from_lease(lease),
        })
    }
    async fn check_source_export(&mut self, read: &SourceExport) -> Result<()> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, read.auth.scope(), false).await?;
        let (snapshot, _) = context(&tx, &read.auth, read.fence).await?;
        if snapshot != read.snapshot {
            return Err(Error::ReadScope);
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn read_source_export(
        &mut self,
        read: &SourceExport,
        store: &ManagedStore,
        nodes: &[ReadNode],
        byte_limit: u64,
    ) -> Result<ReadBatch> {
        self.check_source_export(read).await?;
        self.open_read_prefix(&read.session, store, nodes, byte_limit)
            .await
    }
    /// Close even when revoked/fenced. A cancellation instead leaves an expiring hold.
    pub async fn finish_source_export(&mut self, read: SourceExport) -> Result<()> {
        let check = self.check_source_export(&read).await;
        let close = self.close_read_session(read.session).await;
        check?;
        close
    }
}
