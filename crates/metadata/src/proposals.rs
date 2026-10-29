//! Purpose-bound immutable change uploads. No filesystem capture, source-import
//! authority or validation result is inferred from accepting an agent proposal.
use crate::{
    Authority, Begin, Error, Event, Metadata, Request, Result, Scope,
    access::{Authorization, Permission, repository_lock},
    catalog::{ObjectKey, ObjectLease, lease_pin_in, live_lease, publish_in, reserve_in, settings},
    intake::{ChangeSubmission, CommandContext},
    reads::{ReadBatch, ReadNode, ReadSession},
    uploads::UploadLimits,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_postgres::{Row, Transaction};
use treeharbor_core::{AppliedChange, Changes, Object, ObjectId, RevisionRef};
use treeharbor_storage::{DurableObject, ManagedStore, PreparedObject};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunSource {
    pub revision: RevisionRef,
    pub snapshot: ObjectId,
    pub pin: String,
    pub external_api: bool,
}
pub(crate) async fn run_source_in(
    tx: &Transaction<'_>,
    scope: &Scope,
    principal: &str,
    run: Uuid,
    generation: i64,
    epoch: i64,
) -> Result<RunSource> {
    if generation <= 0 {
        return Err(Error::Invalid("workspace generation"));
    }
    let row=tx.query_opt(r#"SELECT w.base_revision,b.snapshot,w.pin_holder,r.origin FROM th_agent_runs r
        JOIN th_workspaces w USING (tenant,repository,workspace)
        JOIN th_bases b ON b.tenant=w.tenant AND b.repository=w.repository AND b.revision=w.base_revision
        WHERE r.tenant=$1 AND r.repository=$2 AND r.run=$3 AND r.principal=$4 AND w.principal=$4
        AND r.epoch=$5 AND w.epoch=$5
        AND r.workspace_generation=$6 AND w.generation=$6 AND w.resolved_profile IS NOT NULL
        AND ((r.origin='control_plane' AND w.delivery='files' AND w.state='in_use' AND
             ((r.state='running' AND r.runtime=w.runtime) OR
              (r.state IN ('succeeded','failed','cancelled','lost') AND EXISTS
               (SELECT 1 FROM th_managed_runs m WHERE m.tenant=r.tenant AND m.repository=r.repository
                AND m.run=r.run AND m.removed))))
          OR (r.origin='external_api' AND w.delivery='api' AND w.state='source_ready'
              AND r.state='recorded' AND r.runtime IS NULL AND r.started_at IS NULL
              AND w.node IS NULL AND w.runtime IS NULL))"#,
        &[&scope.tenant,&scope.repository,&run,&principal,&epoch,&generation]).await?.ok_or(Error::ExecutionState)?;
    Ok(RunSource {
        revision: row.get::<_, String>(0).parse()?,
        snapshot: ObjectId::from_bytes(
            row.get::<_, Vec<u8>>(1)
                .try_into()
                .map_err(|_| Error::SchemaMismatch)?,
        ),
        pin: row.get(2),
        external_api: row.get::<_, String>(3) == "external_api",
    })
}

pub struct ChangeUploadSpec {
    pub upload: Uuid,
    pub run: Uuid,
    pub generation: i64,
    pub limits: UploadLimits,
}
/// This receipt is minted only after authenticated reservation of complete blob
/// bytes. It cannot be reconstructed from an external digest or storage path.
pub struct ChangeBlobTicket {
    upload: Uuid,
    lease: ObjectLease,
}
impl ChangeBlobTicket {
    pub fn generation(&self) -> i64 {
        self.lease.key().generation()
    }
}

pub struct ProposalCommand {
    pub key: String,
    pub authority: Authority,
    pub epoch: i64,
    pub upload: Uuid,
    pub changeset: Uuid,
    pub previous_version: i64,
    pub operations: Changes,
}
impl ProposalCommand {
    fn request(&self, auth: &Authorization) -> Result<Request> {
        self.previous_version
            .checked_add(1)
            .filter(|_| self.previous_version >= 0)
            .ok_or(Error::Invalid("previous version"))?;
        auth.context(self.key.clone(), self.authority, self.epoch)
            .request(
                "change.submit.v1",
                json!([
                    1,
                    self.upload.to_string(),
                    self.changeset.to_string(),
                    self.previous_version,
                    Object::ChangeOperations(self.operations.clone())
                        .id()?
                        .to_string()
                ]),
            )
    }
}
pub enum ProposalStart {
    Replay(Value),
    Ready(Box<ProposalRead>),
}
pub struct ProposalRead {
    auth: Authorization,
    upload: Uuid,
    source: RunSource,
    session: ReadSession,
}
impl ProposalRead {
    pub fn root(&self) -> ReadNode {
        self.session.root()
    }
    pub fn snapshot(&self) -> ObjectId {
        self.source.snapshot
    }
}
async fn authorized(tx: &Transaction<'_>, auth: &Authorization) -> Result<()> {
    repository_lock(tx, auth.scope(), true).await?;
    auth.check_transaction(tx, auth.scope(), auth.principal(), Permission::ChangeWrite)
        .await
}
async fn active(
    tx: &Transaction<'_>,
    s: &Scope,
    principal: &str,
    upload: Uuid,
) -> Result<(Row, RunSource)> {
    let row=tx.query_opt("SELECT backend,authority,epoch,run,workspace_generation,max_objects,max_bytes,reserved_objects,reserved_bytes,state='active' AND expires_at>clock_timestamp() FROM th_change_uploads WHERE tenant=$1 AND repository=$2 AND upload=$3 AND principal=$4 FOR UPDATE",
        &[&s.tenant,&s.repository,&upload,&principal]).await?.ok_or(Error::NotFound)?;
    if !row.get::<_, bool>(9) {
        return Err(Error::UploadState);
    }
    let repo = tx
        .query_one(
            "SELECT authority,epoch,state FROM th_repositories WHERE tenant=$1 AND repository=$2",
            &[&s.tenant, &s.repository],
        )
        .await?;
    if row.get::<_, String>(1) != repo.get::<_, String>(0)
        || row.get::<_, i64>(2) != repo.get::<_, i64>(1)
        || repo.get::<_, String>(2) != "active"
    {
        return Err(Error::Fenced);
    }
    let source = run_source_in(tx, s, principal, row.get(3), row.get(4), row.get(2)).await?;
    Ok((row, source))
}
fn item_lease(s: &Scope, backend: String, id: ObjectId, row: &Row) -> ObjectLease {
    ObjectLease {
        key: ObjectKey {
            tenant: s.tenant.clone(),
            backend,
            id,
            generation: row.get(0),
        },
        token: row.get(1),
        upload: false,
    }
}
async fn blob_lease(
    tx: &Transaction<'_>,
    s: &Scope,
    upload: Uuid,
    backend: String,
    id: ObjectId,
) -> Result<ObjectLease> {
    let row=tx.query_opt("SELECT generation,hold_token FROM th_change_upload_items WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=$4 AND complete", &[&s.tenant,&s.repository,&upload,&id.as_bytes().as_slice()]).await?.ok_or(Error::UploadMembership)?;
    let lease = item_lease(s, backend, id, &row);
    live_lease(tx, &lease).await?;
    Ok(lease)
}
async fn replacement_membership(
    tx: &Transaction<'_>,
    s: &Scope,
    upload: Uuid,
    backend: &str,
    changes: &Changes,
) -> Result<()> {
    let ids: std::collections::BTreeSet<_> = changes
        .operations()
        .iter()
        .filter_map(|op| op.replacement.map(|r| r.target))
        .collect();
    for id in ids {
        blob_lease(tx, s, upload, backend.into(), id).await?;
    }
    Ok(())
}
// Internal deterministic reservation identity, not an external capability.
// Stable across loss of a process/response, scoped to one immutable upload.
fn derived_token(scope: &Scope, upload: Uuid, object: ObjectId) -> Uuid {
    let mut h = Sha256::new();
    h.update(b"treeharbor-proposal-object\0v1\0");
    h.update((scope.tenant.len() as u64).to_be_bytes());
    h.update(scope.tenant.as_bytes());
    h.update(scope.repository.as_bytes());
    h.update(upload.as_bytes());
    h.update(object.as_bytes());
    let digest = h.finalize();
    let mut raw = [0; 16];
    raw.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(raw)
}
impl Metadata {
    /// Trusted server computation only, never an arbitrary client frame endpoint.
    /// References must come from protected base ancestry or uploaded new blobs.
    pub async fn reserve_proposal_object(
        &mut self,
        auth: &Authorization,
        upload: Uuid,
        store: &ManagedStore,
        object: &PreparedObject,
    ) -> Result<ObjectLease> {
        if !(2..=4).contains(&object.kind()) {
            return Err(Error::Invalid("derived proposal object"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        authorized(&tx, auth).await?;
        let s = auth.scope();
        let (row, _) = active(&tx, s, auth.principal(), upload).await?;
        if store.tenant() != s.tenant || store.backend() != row.get::<_, String>(0) {
            return Err(Error::ReceiptMismatch);
        }
        let token = derived_token(s, upload, object.id());
        let old=tx.query_opt("SELECT generation,hold_token FROM th_change_derived_items WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=$4", &[&s.tenant,&s.repository,&upload,&object.id().as_bytes().as_slice()]).await?;
        if old.is_none()
            && (row.get::<_, i64>(7) >= row.get::<_, i64>(5)
                || object.length() as i64 > row.get::<_, i64>(6) - row.get::<_, i64>(8))
        {
            return Err(Error::UploadLimit);
        }
        let lease = reserve_in(&tx, store, object, token, 3600).await?;
        if let Some(old) = old {
            if old.get::<_, i64>(0) != lease.key().generation() || old.get::<_, Uuid>(1) != token {
                return Err(Error::UploadState);
            }
        } else {
            tx.execute("INSERT INTO th_change_derived_items(tenant,repository,upload,digest,generation,hold_token,kind,length) VALUES($1,$2,$3,$4,$5,$6,$7,$8)", &[&s.tenant,&s.repository,&upload,&object.id().as_bytes().as_slice(),&lease.key().generation(),&token,&(object.kind() as i16),&(object.length() as i64)]).await?;
            tx.execute("UPDATE th_change_uploads SET reserved_objects=reserved_objects+1,reserved_bytes=reserved_bytes+$4 WHERE tenant=$1 AND repository=$2 AND upload=$3", &[&s.tenant,&s.repository,&upload,&(object.length() as i64)]).await?;
        }
        tx.execute("UPDATE th_object_holds h SET expires_at=u.expires_at FROM th_change_uploads u WHERE u.tenant=$1 AND u.repository=$2 AND u.upload=$3 AND h.tenant=u.tenant AND h.token=$4", &[&s.tenant,&s.repository,&upload,&token]).await?;
        active(&tx, s, auth.principal(), upload).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::ChangeWrite)
            .await?;
        tx.commit().await?;
        Ok(lease)
    }
    pub async fn begin_change_upload(
        &mut self,
        ctx: &CommandContext,
        backend: &str,
        spec: &ChangeUploadSpec,
    ) -> Result<Value> {
        treeharbor_storage::TenantId::new(backend)?;
        spec.limits.validate()?;
        let request = ctx.request(
            "change.begin.v1",
            json!([
                1,
                spec.upload.to_string(),
                backend,
                spec.run.to_string(),
                spec.generation,
                spec.limits.objects,
                spec.limits.bytes,
                spec.limits.ttl_seconds
            ]),
        )?;
        let tx = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        run_source_in(
            &tx.tx,
            s,
            &ctx.principal,
            spec.run,
            spec.generation,
            ctx.epoch,
        )
        .await?;
        let count:i64=tx.tx.query_one("SELECT count(*) FROM th_change_uploads WHERE tenant=$1 AND repository=$2 AND principal=$3 AND state='active' AND expires_at>clock_timestamp()", &[&s.tenant,&s.repository,&ctx.principal]).await?.get(0);
        if count >= 4 {
            return Err(Error::UploadLimit);
        }
        let row=tx.tx.query_opt("INSERT INTO th_change_uploads(tenant,repository,upload,principal,authority,epoch,run,workspace_generation,backend,expires_at,max_objects,max_bytes) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,clock_timestamp()+($10::integer*interval '1 second'),$11,$12) ON CONFLICT DO NOTHING RETURNING (extract(epoch from expires_at)*1000)::bigint",
            &[&s.tenant,&s.repository,&spec.upload,&ctx.principal,&ctx.authority.as_str(),&ctx.epoch,&spec.run,&spec.generation,&backend,&spec.limits.ttl_seconds,&spec.limits.objects,&spec.limits.bytes]).await?.ok_or(Error::UploadState)?;
        tx.complete(json!({"upload":spec.upload.to_string(),"state":"active","run":spec.run.to_string(),"generation":spec.generation,
            "max_objects":spec.limits.objects,"max_bytes":spec.limits.bytes,"expires_at_unix_ms":row.get::<_,i64>(0)}),vec![]).await
    }
    pub async fn change_upload_status(
        &mut self,
        auth: &Authorization,
        upload: Uuid,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        authorized(&tx, auth).await?;
        let s = auth.scope();
        let row=tx.query_opt("SELECT state,expires_at<=clock_timestamp(),reserved_objects,reserved_bytes,(extract(epoch from expires_at)*1000)::bigint,run,workspace_generation FROM th_change_uploads WHERE tenant=$1 AND repository=$2 AND upload=$3 AND principal=$4", &[&s.tenant,&s.repository,&upload,&auth.principal()]).await?.ok_or(Error::NotFound)?;
        let value = json!({"upload":upload.to_string(),"state":row.get::<_,String>(0),"expired":row.get::<_,bool>(1),"reserved_objects":row.get::<_,i64>(2),"reserved_bytes":row.get::<_,i64>(3),"expires_at_unix_ms":row.get::<_,i64>(4),"run":row.get::<_,Uuid>(5).to_string(),"generation":row.get::<_,i64>(6)});
        auth.check_transaction(&tx, s, auth.principal(), Permission::ChangeWrite)
            .await?;
        tx.commit().await?;
        Ok(value)
    }
    pub async fn reserve_change_blob(
        &mut self,
        auth: &Authorization,
        upload: Uuid,
        store: &ManagedStore,
        object: &PreparedObject,
    ) -> Result<ChangeBlobTicket> {
        if object.kind() != 1 {
            return Err(Error::Invalid("change upload requires blob"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        authorized(&tx, auth).await?;
        let s = auth.scope();
        let (row, _) = active(&tx, s, auth.principal(), upload).await?;
        if store.tenant() != s.tenant || store.backend() != row.get::<_, String>(0) {
            return Err(Error::ReceiptMismatch);
        }
        let id = object.id();
        let old=tx.query_opt("SELECT generation,hold_token FROM th_change_upload_items WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=$4", &[&s.tenant,&s.repository,&upload,&id.as_bytes().as_slice()]).await?;
        if old.is_none()
            && (row.get::<_, i64>(7) >= row.get::<_, i64>(5)
                || object.length() as i64 > row.get::<_, i64>(6) - row.get::<_, i64>(8))
        {
            return Err(Error::UploadLimit);
        }
        let token = old.as_ref().map(|r| r.get(1)).unwrap_or_else(Uuid::new_v4);
        let lease = reserve_in(&tx, store, object, token, 3600).await?;
        if let Some(old) = old {
            if lease.key().generation() != old.get::<_, i64>(0) {
                return Err(Error::UploadState);
            }
        } else {
            tx.execute("INSERT INTO th_change_upload_items(tenant,repository,upload,digest,generation,hold_token,length) VALUES($1,$2,$3,$4,$5,$6,$7)", &[&s.tenant,&s.repository,&upload,&id.as_bytes().as_slice(),&lease.key().generation(),&token,&(object.length() as i64)]).await?;
            tx.execute("UPDATE th_change_uploads SET reserved_objects=reserved_objects+1,reserved_bytes=reserved_bytes+$4 WHERE tenant=$1 AND repository=$2 AND upload=$3", &[&s.tenant,&s.repository,&upload,&(object.length() as i64)]).await?;
        }
        tx.execute("UPDATE th_object_holds h SET expires_at=u.expires_at FROM th_change_uploads u WHERE u.tenant=$1 AND u.repository=$2 AND u.upload=$3 AND h.tenant=u.tenant AND h.token=$4", &[&s.tenant,&s.repository,&upload,&token]).await?;
        active(&tx, s, auth.principal(), upload).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::ChangeWrite)
            .await?;
        tx.commit().await?;
        Ok(ChangeBlobTicket { upload, lease })
    }
    pub async fn publish_change_blob(
        &mut self,
        auth: &Authorization,
        ticket: &ChangeBlobTicket,
        receipt: &DurableObject,
    ) -> Result<()> {
        if receipt.kind() != 1 {
            return Err(Error::ReceiptMismatch);
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        authorized(&tx, auth).await?;
        let s = auth.scope();
        active(&tx, s, auth.principal(), ticket.upload).await?;
        if s.tenant != ticket.lease.key().tenant() {
            return Err(Error::ReceiptMismatch);
        }
        let row=tx.query_opt("SELECT generation,hold_token FROM th_change_upload_items WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=$4", &[&s.tenant,&s.repository,&ticket.upload,&receipt.id().as_bytes().as_slice()]).await?.ok_or(Error::UploadMembership)?;
        if row.get::<_, i64>(0) != ticket.generation()
            || row.get::<_, Uuid>(1) != ticket.lease.token()
        {
            return Err(Error::ReceiptMismatch);
        }
        publish_in(&tx, &ticket.lease, receipt).await?;
        tx.execute("UPDATE th_change_upload_items SET complete=true WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=$4", &[&s.tenant,&s.repository,&ticket.upload,&receipt.id().as_bytes().as_slice()]).await?;
        active(&tx, s, auth.principal(), ticket.upload).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::ChangeWrite)
            .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn abort_change_upload(
        &mut self,
        ctx: &CommandContext,
        upload: Uuid,
    ) -> Result<Value> {
        let tx = match self
            .begin(ctx.request("change.abort.v1", json!([1, upload.to_string()]))?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        // Owner may abort after a workspace drain or session expiry, but cannot
        // revive a closed upload or change its immutable identity/deadline.
        let changed=tx.tx.execute("UPDATE th_change_uploads SET state='aborted' WHERE tenant=$1 AND repository=$2 AND upload=$3 AND principal=$4 AND state='active' AND authority=$5 AND epoch=$6", &[&s.tenant,&s.repository,&upload,&ctx.principal,&ctx.authority.as_str(),&ctx.epoch]).await?;
        if changed != 1 {
            return Err(Error::UploadState);
        }
        tx.complete(
            json!({"upload":upload.to_string(),"state":"aborted"}),
            vec![],
        )
        .await
    }
    /// Check replay before reading source or requiring an active upload. Retried
    /// accepted operations still succeed after their workspace has been drained.
    pub async fn open_change_proposal(
        &mut self,
        auth: &Authorization,
        command: &ProposalCommand,
    ) -> Result<ProposalStart> {
        let tx = match self.begin(command.request(auth)?).await? {
            Begin::Replay(v) => return Ok(ProposalStart::Replay(v)),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let (row, source) = active(&tx.tx, s, auth.principal(), command.upload).await?;
        replacement_membership(
            &tx.tx,
            s,
            command.upload,
            &row.get::<_, String>(0),
            &command.operations,
        )
        .await?;
        let RevisionRef::Native(commit) = source.revision else {
            return Err(Error::Invalid("native proposal required"));
        };
        let lease = lease_pin_in(&tx.tx, s, &source.pin, Uuid::new_v4(), 300).await?;
        if lease.key().id() != commit {
            return Err(Error::ReadScope);
        }
        auth.check_transaction(&tx.tx, s, auth.principal(), Permission::ChangeWrite)
            .await?;
        tx.tx.commit().await?;
        Ok(ProposalStart::Ready(Box::new(ProposalRead {
            auth: auth.clone(),
            upload: command.upload,
            source,
            session: ReadSession::from_lease(lease),
        })))
    }
    async fn check_proposal_read(&mut self, read: &ProposalRead) -> Result<()> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        authorized(&tx, &read.auth).await?;
        let (_, source) =
            active(&tx, read.auth.scope(), read.auth.principal(), read.upload).await?;
        if source != read.source {
            return Err(Error::ExecutionState);
        }
        read.auth
            .check_transaction(
                &tx,
                read.auth.scope(),
                read.auth.principal(),
                Permission::ChangeWrite,
            )
            .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn open_proposal_source_batch(
        &mut self,
        read: &ProposalRead,
        store: &ManagedStore,
        nodes: &[ReadNode],
        byte_limit: u64,
    ) -> Result<ReadBatch> {
        self.check_proposal_read(read).await?;
        self.open_read_batch(&read.session, store, nodes, byte_limit)
            .await
    }
    pub async fn open_proposal_blob(
        &mut self,
        read: &ProposalRead,
        store: &ManagedStore,
        id: ObjectId,
    ) -> Result<ReadBatch> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        authorized(&tx, &read.auth).await?;
        let s = read.auth.scope();
        let (row, source) = active(&tx, s, read.auth.principal(), read.upload).await?;
        if source != read.source {
            return Err(Error::ExecutionState);
        }
        let lease = blob_lease(&tx, s, read.upload, row.get(0), id).await?;
        read.auth
            .check_transaction(&tx, s, read.auth.principal(), Permission::ChangeWrite)
            .await?;
        tx.commit().await?;
        let session = ReadSession::from_lease(lease);
        self.open_read_batch(
            &session,
            store,
            &[session.root()],
            crate::reads::MAX_READ_BYTES,
        )
        .await
    }
    pub async fn close_change_proposal(&mut self, read: ProposalRead) -> Result<()> {
        self.close_read_session(read.session).await
    }
    /// Accept only a service-computed result, after durable object publication.
    /// Session completion, version/pins, replay and outbox share one transaction.
    pub async fn finish_change_proposal(
        &mut self,
        auth: &Authorization,
        command: &ProposalCommand,
        applied: &AppliedChange,
        operations: &ObjectLease,
        result: &ObjectLease,
    ) -> Result<Value> {
        if applied.operations() != Object::ChangeOperations(command.operations.clone()).id()? {
            return Err(Error::ReceiptMismatch);
        }
        let tx = match self.begin(command.request(auth)?).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let (row, source) = active(&tx.tx, s, auth.principal(), command.upload).await?;
        if source.snapshot != applied.base_snapshot() {
            return Err(Error::ReceiptMismatch);
        }
        replacement_membership(
            &tx.tx,
            s,
            command.upload,
            &row.get::<_, String>(0),
            &command.operations,
        )
        .await?;
        let value = tx
            .accept_change_version(&ChangeSubmission {
                id: command.changeset,
                previous_version: command.previous_version,
                run: row.get(3),
                workspace_generation: row.get(4),
                applied,
                operations,
                result,
            })
            .await?;
        let updated=tx.tx.execute("UPDATE th_change_uploads SET state='completed' WHERE tenant=$1 AND repository=$2 AND upload=$3 AND state='active' AND expires_at>clock_timestamp()", &[&s.tenant,&s.repository,&command.upload]).await?;
        if updated != 1 {
            return Err(Error::UploadState);
        }
        tx.complete(
            value.clone(),
            vec![Event {
                kind: "change.accepted".into(),
                payload: value,
            }],
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn derived_reservation_matches_independent_vector() {
        let scope = Scope::new("test-tenant", Uuid::from_u128(1)).unwrap();
        let token = derived_token(&scope, Uuid::from_u128(2), ObjectId::from_bytes([17; 32]));
        assert_eq!(
            token.to_string(),
            include_str!("../tests/fixtures/proposal-reservation-v1.txt").trim()
        );
        assert_ne!(
            token,
            derived_token(&scope, Uuid::from_u128(3), ObjectId::from_bytes([17; 32]))
        );
    }
}
