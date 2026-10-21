//! Authenticated source ingestion. Complete verified bytes prove possession;
//! tenant-level CAS deduplication alone never grants repository membership.
use crate::access::{Authorization, Permission};
use crate::catalog::{ObjectKey, ObjectLease, publish_in, reserve_in, settings};
use crate::intake::CommandContext;
use crate::{Begin, Error, Metadata, Result, Scope, identifier, ttl_valid};
use serde_json::{Value, json};
use tokio_postgres::{Row, Transaction};
use treeharbor_core::{ObjectId, RevisionRef};
use treeharbor_storage::{DurableObject, ManagedStore, PreparedObject};
use uuid::Uuid;

pub struct UploadLimits {
    pub objects: i64,
    pub bytes: i64,
    pub ttl_seconds: i32,
}
impl UploadLimits {
    fn validate(&self) -> Result<()> {
        ttl_valid(self.ttl_seconds)?;
        if !(1..=1_000_000).contains(&self.objects) || !(22..=68_719_476_736).contains(&self.bytes)
        {
            return Err(Error::Invalid("upload budget"));
        }
        Ok(())
    }
}
/// Private reservation capability, not an authentication token. Every use still
/// checks the current principal, repository epoch, session and hold deadline.
pub struct UploadTicket {
    upload: Uuid,
    lease: ObjectLease,
}
impl UploadTicket {
    pub fn generation(&self) -> i64 {
        self.lease.key().generation()
    }
}
fn oid(raw: Vec<u8>) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        raw.try_into().map_err(|_| Error::SchemaMismatch)?,
    ))
}
async fn active(tx: &Transaction<'_>, scope: &Scope, principal: &str, upload: Uuid) -> Result<Row> {
    let row = tx.query_opt("SELECT backend,authority,epoch,max_objects,max_bytes,reserved_objects,reserved_bytes,expires_at>clock_timestamp() AND state='active' FROM th_source_uploads WHERE tenant=$1 AND repository=$2 AND upload=$3 AND principal=$4 FOR UPDATE", &[&scope.tenant,&scope.repository,&upload,&principal]).await?.ok_or(Error::NotFound)?;
    if !row.get::<_, bool>(7) {
        return Err(Error::UploadState);
    }
    let repo = tx
        .query_one(
            "SELECT authority,epoch,state FROM th_repositories WHERE tenant=$1 AND repository=$2",
            &[&scope.tenant, &scope.repository],
        )
        .await?;
    if row.get::<_, String>(1) != repo.get::<_, String>(0)
        || row.get::<_, i64>(2) != repo.get::<_, i64>(1)
        || repo.get::<_, String>(2) != "active"
    {
        return Err(Error::Fenced);
    }
    Ok(row)
}
async fn lock_authorized(tx: &Transaction<'_>, auth: &Authorization) -> Result<()> {
    crate::access::repository_lock(tx, auth.scope(), true).await?;
    auth.check_transaction(
        tx,
        auth.scope(),
        auth.principal(),
        Permission::RepositoryWrite,
    )
    .await
}
async fn membership(
    tx: &Transaction<'_>,
    scope: &Scope,
    upload: Uuid,
    refs: &[(ObjectId, u8)],
) -> Result<()> {
    let mut expected = std::collections::BTreeMap::new();
    for &(id, kind) in refs {
        if expected.insert(id, kind).is_some_and(|old| old != kind) {
            return Err(Error::UploadMembership);
        }
    }
    let ids: Vec<Vec<u8>> = expected.keys().map(|id| id.as_bytes().to_vec()).collect();
    let rows=tx.query("SELECT i.digest,i.kind FROM th_source_upload_items i JOIN th_objects o ON o.tenant=i.tenant AND o.digest=i.digest AND o.generation=i.generation WHERE i.tenant=$1 AND i.repository=$2 AND i.upload=$3 AND i.digest=ANY($4::bytea[]) AND i.complete AND o.state='live'", &[&scope.tenant,&scope.repository,&upload,&ids]).await?;
    if rows.len() != expected.len() {
        return Err(Error::UploadMembership);
    }
    for row in rows {
        if expected.get(&oid(row.get(0))?) != Some(&(row.get::<_, i16>(1) as u8)) {
            return Err(Error::UploadMembership);
        }
    }
    Ok(())
}
impl Metadata {
    /// Trusted deployment startup. An existing label may not silently point at
    /// an empty or different store. Legacy unbound catalog data needs migration.
    pub async fn bind_storage_backend(&mut self, backend: &str, identity: Uuid) -> Result<()> {
        treeharbor_storage::TenantId::new(backend)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        tx.execute("INSERT INTO th_storage_backends(backend,identity) SELECT $1,$2 WHERE NOT EXISTS(SELECT 1 FROM th_objects WHERE backend=$1) ON CONFLICT DO NOTHING", &[&backend,&identity]).await?;
        let row = tx
            .query_opt(
                "SELECT identity FROM th_storage_backends WHERE backend=$1 FOR SHARE",
                &[&backend],
            )
            .await?
            .ok_or(Error::ReceiptMismatch)?;
        if row.get::<_, Uuid>(0) != identity {
            return Err(Error::ReceiptMismatch);
        }
        tx.commit().await?;
        Ok(())
    }
    /// Backend is trusted deployment configuration, never an agent-supplied path.
    pub async fn begin_source_upload(
        &mut self,
        ctx: &CommandContext,
        backend: &str,
        upload: Uuid,
        limits: &UploadLimits,
    ) -> Result<Value> {
        identifier(backend, 64)?;
        limits.validate()?;
        let request = ctx.request(
            "source.begin.v1",
            json!([
                1,
                upload.to_string(),
                backend,
                limits.objects,
                limits.bytes,
                limits.ttl_seconds
            ]),
        )?;
        let tx = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        let exists:bool=tx.tx.query_one("SELECT EXISTS(SELECT 1 FROM th_source_uploads WHERE tenant=$1 AND repository=$2 AND upload=$3)", &[&s.tenant,&s.repository,&upload]).await?.get(0);
        if exists {
            return Err(Error::UploadState);
        }
        // A conservative per-repository bound, independent of licensing.
        let count:i64=tx.tx.query_one("SELECT count(*) FROM th_source_uploads WHERE tenant=$1 AND repository=$2 AND state='active' AND expires_at>clock_timestamp()", &[&s.tenant,&s.repository]).await?.get(0);
        if count >= 4 {
            return Err(Error::UploadLimit);
        }
        let deadline:i64=tx.tx.query_one("INSERT INTO th_source_uploads(tenant,repository,upload,principal,authority,epoch,backend,expires_at,max_objects,max_bytes) VALUES($1,$2,$3,$4,$5,$6,$7,clock_timestamp()+($8::integer*interval '1 second'),$9,$10) RETURNING (extract(epoch from expires_at)*1000)::bigint", &[&s.tenant,&s.repository,&upload,&ctx.principal,&ctx.authority.as_str(),&ctx.epoch,&backend,&limits.ttl_seconds,&limits.objects,&limits.bytes]).await?.get(0);
        tx.complete(json!({"upload":upload.to_string(),"state":"active","ttl_seconds":limits.ttl_seconds,"max_objects":limits.objects,"max_bytes":limits.bytes,"expires_at_unix_ms":deadline}),vec![]).await
    }
    pub async fn source_upload_status(
        &mut self,
        auth: &Authorization,
        upload: Uuid,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        lock_authorized(&tx, auth).await?;
        let s = auth.scope();
        let row=tx.query_opt("SELECT state,expires_at<=clock_timestamp(),reserved_objects,reserved_bytes,(extract(epoch from expires_at)*1000)::bigint FROM th_source_uploads WHERE tenant=$1 AND repository=$2 AND upload=$3 AND principal=$4", &[&s.tenant,&s.repository,&upload,&auth.principal()]).await?.ok_or(Error::NotFound)?;
        let value = json!({"upload":upload.to_string(),"state":row.get::<_,String>(0),"expired":row.get::<_,bool>(1),"reserved_objects":row.get::<_,i64>(2),"reserved_bytes":row.get::<_,i64>(3),"expires_at_unix_ms":row.get::<_,i64>(4)});
        auth.check_transaction(&tx, s, auth.principal(), Permission::RepositoryWrite)
            .await?;
        tx.commit().await?;
        Ok(value)
    }
    pub async fn reserve_source_object(
        &mut self,
        auth: &Authorization,
        upload: Uuid,
        store: &ManagedStore,
        object: &PreparedObject,
    ) -> Result<UploadTicket> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        lock_authorized(&tx, auth).await?;
        let s = auth.scope();
        let row = active(&tx, s, auth.principal(), upload).await?;
        if store.tenant() != s.tenant || store.backend() != row.get::<_, String>(0) {
            return Err(Error::ReceiptMismatch);
        }
        membership(&tx, s, upload, object.references()).await?;
        let digest = object.id().as_bytes().to_vec();
        let old=tx.query_opt("SELECT hold_token,generation FROM th_source_upload_items WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=$4", &[&s.tenant,&s.repository,&upload,&digest]).await?;
        let token = old.as_ref().map(|r| r.get(0)).unwrap_or_else(Uuid::new_v4);
        if old.is_none()
            && (row.get::<_, i64>(5) >= row.get::<_, i64>(3)
                || object.length() as i64 > row.get::<_, i64>(4) - row.get::<_, i64>(6))
        {
            return Err(Error::UploadLimit);
        }
        let lease = reserve_in(&tx, store, object, token, 3600).await?;
        if let Some(old) = old {
            if lease.key().generation() != old.get::<_, i64>(1) {
                return Err(Error::UploadState);
            }
        } else {
            let snapshot = object.commit_snapshot().map(|id| id.as_bytes().to_vec());
            tx.execute("INSERT INTO th_source_upload_items(tenant,repository,upload,digest,generation,hold_token,kind,length,snapshot) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)", &[&s.tenant,&s.repository,&upload,&digest,&lease.key().generation(),&token,&(object.kind() as i16),&(object.length() as i64),&snapshot]).await?;
            tx.execute("UPDATE th_source_uploads SET reserved_objects=reserved_objects+1,reserved_bytes=reserved_bytes+$4 WHERE tenant=$1 AND repository=$2 AND upload=$3", &[&s.tenant,&s.repository,&upload,&(object.length() as i64)]).await?;
        }
        // Retrying an object may never extend its session's fixed deadline.
        tx.execute("UPDATE th_object_holds h SET expires_at=u.expires_at FROM th_source_uploads u WHERE u.tenant=$1 AND u.repository=$2 AND u.upload=$3 AND h.tenant=u.tenant AND h.token=$4", &[&s.tenant,&s.repository,&upload,&token]).await?;
        active(&tx, s, auth.principal(), upload).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::RepositoryWrite)
            .await?;
        tx.commit().await?;
        Ok(UploadTicket { upload, lease })
    }
    pub async fn publish_source_object(
        &mut self,
        auth: &Authorization,
        ticket: &UploadTicket,
        receipt: &DurableObject,
    ) -> Result<()> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        lock_authorized(&tx, auth).await?;
        let s = auth.scope();
        active(&tx, s, auth.principal(), ticket.upload).await?;
        if s.tenant != ticket.lease.key().tenant() {
            return Err(Error::ReceiptMismatch);
        }
        let digest = ticket.lease.key().id().as_bytes().to_vec();
        let item=tx.query_opt("SELECT generation,hold_token FROM th_source_upload_items WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=$4", &[&s.tenant,&s.repository,&ticket.upload,&digest]).await?.ok_or(Error::UploadMembership)?;
        if item.get::<_, i64>(0) != ticket.generation()
            || item.get::<_, Uuid>(1) != ticket.lease.token()
        {
            return Err(Error::ReceiptMismatch);
        }
        membership(&tx, s, ticket.upload, receipt.references()).await?;
        publish_in(&tx, &ticket.lease, receipt).await?;
        tx.execute("UPDATE th_source_upload_items SET complete=true WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=$4", &[&s.tenant,&s.repository,&ticket.upload,&digest]).await?;
        active(&tx, s, auth.principal(), ticket.upload).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::RepositoryWrite)
            .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn finish_source_upload(
        &mut self,
        ctx: &CommandContext,
        upload: Uuid,
        commit: ObjectId,
    ) -> Result<Value> {
        let request = ctx.request(
            "source.finish.v1",
            json!([1, upload.to_string(), commit.to_string()]),
        )?;
        let tx = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        let row = active(&tx.tx, s, &ctx.principal, upload).await?;
        let digest = commit.as_bytes().to_vec();
        let item=tx.tx.query_opt("SELECT generation,hold_token,snapshot FROM th_source_upload_items WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=$4 AND kind=5 AND complete", &[&s.tenant,&s.repository,&upload,&digest]).await?.ok_or(Error::UploadMembership)?;
        let snapshot = oid(item.get(2))?;
        let lease = ObjectLease {
            key: ObjectKey {
                tenant: s.tenant.clone(),
                backend: row.get(0),
                id: commit,
                generation: item.get(0),
            },
            token: item.get(1),
            upload: false,
        };
        tx.register_base(commit, snapshot, &lease).await?;
        let changed=tx.tx.execute("UPDATE th_source_uploads SET state='completed' WHERE tenant=$1 AND repository=$2 AND upload=$3 AND state='active' AND expires_at>clock_timestamp()", &[&s.tenant,&s.repository,&upload]).await?;
        if changed != 1 {
            return Err(Error::UploadState);
        }
        // Keep temporary holds until their bounded deadline; no unsafe early
        // release of child protection, even for unused objects in this upload.
        tx.complete(json!({"upload":upload.to_string(),"state":"completed","revision":RevisionRef::Native(commit).to_string(),"snapshot":snapshot.to_string()}),vec![]).await
    }
    pub async fn abort_source_upload(
        &mut self,
        ctx: &CommandContext,
        upload: Uuid,
    ) -> Result<Value> {
        let request = ctx.request("source.abort.v1", json!([1, upload.to_string()]))?;
        let tx = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        let changed=tx.tx.execute("UPDATE th_source_uploads SET state='aborted' WHERE tenant=$1 AND repository=$2 AND upload=$3 AND principal=$4 AND state='active' AND authority=$5 AND epoch=$6", &[&s.tenant,&s.repository,&upload,&ctx.principal,&ctx.authority.as_str(),&ctx.epoch]).await?;
        if changed != 1 {
            return Err(Error::UploadState);
        }
        tx.complete(
            json!({"upload":upload.to_string(),"state":"aborted"}),
            vec![],
        )
        .await
    }
}
