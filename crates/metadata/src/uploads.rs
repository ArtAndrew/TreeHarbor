//! Authenticated source ingestion. Complete verified bytes prove possession;
//! tenant-level CAS deduplication alone never grants repository membership.
mod reuse;
pub use reuse::ReusedSourceBase;

use crate::access::{Authorization, Permission};
use crate::catalog::{ObjectKey, ObjectLease, publish_blobs_in, publish_in, reserve_in, settings};
use crate::intake::CommandContext;
use crate::{Begin, Error, Metadata, Result, Scope, identifier, ttl_valid};
use serde_json::{Value, json};
use tokio_postgres::{Row, Transaction};
use treeharbor_core::{ObjectId, RevisionRef};
use treeharbor_storage::{DurableObject, ManagedStore, PreparedObject};
use uuid::Uuid;

pub const SOURCE_BLOB_BATCH_OBJECTS: usize = 32;

pub struct UploadLimits {
    pub objects: i64,
    pub bytes: i64,
    pub ttl_seconds: i32,
}
impl UploadLimits {
    pub(crate) fn validate(&self) -> Result<()> {
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
    purpose: Purpose,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Human,
    Github,
}
impl Purpose {
    fn permission(self) -> Permission {
        match self {
            Self::Human => Permission::RepositoryWrite,
            Self::Github => Permission::HostRead,
        }
    }
}
impl UploadTicket {
    pub fn generation(&self) -> i64 {
        self.lease.key().generation()
    }
}
pub(crate) fn oid(raw: Vec<u8>) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        raw.try_into().map_err(|_| Error::SchemaMismatch)?,
    ))
}
pub(crate) async fn active(
    tx: &Transaction<'_>,
    scope: &Scope,
    principal: &str,
    upload: Uuid,
) -> Result<Row> {
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
async fn lock_authorized(
    tx: &Transaction<'_>,
    auth: &Authorization,
    purpose: Purpose,
) -> Result<()> {
    crate::access::repository_lock(tx, auth.scope(), true).await?;
    auth.check_transaction(tx, auth.scope(), auth.principal(), purpose.permission())
        .await
}
async fn active_for(
    tx: &Transaction<'_>,
    auth: &Authorization,
    upload: Uuid,
    purpose: Purpose,
) -> Result<Row> {
    let row = active(tx, auth.scope(), auth.principal(), upload).await?;
    // Human sessions are owned by Human principals; HostRead is Bridge-only.
    // Both paths always match the authenticated principal in the source row.
    if purpose == Purpose::Github {
        crate::github::imports::check_upload(tx, auth, upload).await?;
    }
    Ok(row)
}
async fn membership(
    tx: &Transaction<'_>,
    scope: &Scope,
    upload: Uuid,
    refs: &[(ObjectId, u8)],
) -> Result<()> {
    if refs.is_empty() {
        return Ok(());
    }
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

/// The first upload of a blob batch can reserve all its rows together.
/// Existing upload items or catalogue entries use the general path, which
/// handles retries, shared live objects and retired generations.
async fn reserve_fresh_blobs(
    tx: &mut Transaction<'_>,
    scope: &Scope,
    upload: Uuid,
    store: &ManagedStore,
    objects: &[PreparedObject],
    purpose: Purpose,
) -> Result<Option<Vec<UploadTicket>>> {
    let digests: Vec<_> = objects
        .iter()
        .map(|object| object.id().as_bytes().to_vec())
        .collect();
    let has_items: bool = tx
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM th_source_upload_items WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=ANY($4::bytea[]))",
            &[&scope.tenant, &scope.repository, &upload, &digests],
        )
        .await?
        .get(0);
    if has_items {
        return Ok(None);
    }
    let lengths: Vec<_> = objects
        .iter()
        .map(|object| object.length() as i64)
        .collect();
    let expected = objects.len() as u64;
    let batch = tx.transaction().await?;
    let inserted = batch
        .execute(
            "INSERT INTO th_objects (tenant,digest,generation,backend,kind,length,state)
         SELECT $1,b.digest,1,$2,1,b.length,'uploading'
         FROM unnest($3::bytea[],$4::bigint[]) AS b(digest,length) ORDER BY b.digest
         ON CONFLICT DO NOTHING",
            &[&scope.tenant, &store.backend(), &digests, &lengths],
        )
        .await?;
    if inserted != expected {
        // Release the attempted inserts before the fallback takes any locks.
        // Keeping them could violate the digest order on a mixed batch.
        batch.rollback().await?;
        return Ok(None);
    }
    let tokens: Vec<_> = objects.iter().map(|_| Uuid::new_v4()).collect();
    let held = batch
        .execute(
            "INSERT INTO th_object_holds (tenant,token,digest,generation,expires_at)
         SELECT $1,b.token,b.digest,1,u.expires_at
         FROM unnest($4::bytea[],$5::uuid[]) AS b(digest,token)
         JOIN th_source_uploads u ON u.tenant=$1 AND u.repository=$2 AND u.upload=$3
         ON CONFLICT DO NOTHING",
            &[&scope.tenant, &scope.repository, &upload, &digests, &tokens],
        )
        .await?;
    if held != expected {
        return Err(Error::ReceiptMismatch);
    }
    let items = batch.execute(
        "INSERT INTO th_source_upload_items (tenant,repository,upload,digest,generation,hold_token,kind,length,snapshot)
         SELECT $1,$2,$3,b.digest,1,b.token,1,b.length,NULL
         FROM unnest($4::bytea[],$5::uuid[],$6::bigint[]) AS b(digest,token,length)",
        &[&scope.tenant, &scope.repository, &upload, &digests, &tokens, &lengths],
    ).await?;
    if items != expected {
        return Err(Error::UploadMembership);
    }
    batch.commit().await?;
    Ok(Some(
        objects
            .iter()
            .zip(tokens)
            .map(|(object, token)| UploadTicket {
                upload,
                lease: ObjectLease {
                    key: ObjectKey {
                        tenant: scope.tenant.clone(),
                        backend: store.backend().into(),
                        id: object.id(),
                        generation: 1,
                    },
                    token,
                    upload: true,
                },
                purpose,
            })
            .collect(),
    ))
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
        self.source_upload_status_for(auth, upload, Purpose::Human)
            .await
    }
    pub async fn github_source_upload_status(
        &mut self,
        auth: &Authorization,
        upload: Uuid,
    ) -> Result<Value> {
        self.source_upload_status_for(auth, upload, Purpose::Github)
            .await
    }
    async fn source_upload_status_for(
        &mut self,
        auth: &Authorization,
        upload: Uuid,
        purpose: Purpose,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        lock_authorized(&tx, auth, purpose).await?;
        let s = auth.scope();
        let row=tx.query_opt("SELECT state,expires_at<=clock_timestamp(),reserved_objects,reserved_bytes,(extract(epoch from expires_at)*1000)::bigint FROM th_source_uploads WHERE tenant=$1 AND repository=$2 AND upload=$3 AND principal=$4", &[&s.tenant,&s.repository,&upload,&auth.principal()]).await?.ok_or(Error::NotFound)?;
        if purpose == Purpose::Github {
            active_for(&tx, auth, upload, purpose).await?;
        }
        let value = json!({"upload":upload.to_string(),"state":row.get::<_,String>(0),"expired":row.get::<_,bool>(1),"reserved_objects":row.get::<_,i64>(2),"reserved_bytes":row.get::<_,i64>(3),"expires_at_unix_ms":row.get::<_,i64>(4)});
        auth.check_transaction(&tx, s, auth.principal(), purpose.permission())
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
        self.reserve_source_object_for(auth, upload, store, object, Purpose::Human)
            .await
    }
    pub async fn reserve_github_source_object(
        &mut self,
        auth: &Authorization,
        upload: Uuid,
        store: &ManagedStore,
        object: &PreparedObject,
    ) -> Result<UploadTicket> {
        self.reserve_source_object_for(auth, upload, store, object, Purpose::Github)
            .await
    }
    async fn reserve_source_object_for(
        &mut self,
        auth: &Authorization,
        upload: Uuid,
        store: &ManagedStore,
        object: &PreparedObject,
        purpose: Purpose,
    ) -> Result<UploadTicket> {
        let mut tickets = self
            .reserve_source_objects(auth, upload, store, std::slice::from_ref(object), purpose)
            .await?;
        tickets.pop().ok_or(Error::SchemaMismatch)
    }
    /// Blob-only batches acquire catalogue locks in digest order. No part of
    /// the batch can reserve capacity if a frame or current fence is invalid.
    pub async fn reserve_source_blob_batch(
        &mut self,
        auth: &Authorization,
        upload: Uuid,
        store: &ManagedStore,
        objects: &[PreparedObject],
        purpose: Purpose,
    ) -> Result<Vec<UploadTicket>> {
        if !(1..=SOURCE_BLOB_BATCH_OBJECTS).contains(&objects.len())
            || objects.iter().any(|object| object.kind() != 1)
            || objects.windows(2).any(|pair| pair[0].id() >= pair[1].id())
        {
            return Err(Error::Invalid("source blob batch"));
        }
        self.reserve_source_objects(auth, upload, store, objects, purpose)
            .await
    }
    async fn reserve_source_objects(
        &mut self,
        auth: &Authorization,
        upload: Uuid,
        store: &ManagedStore,
        objects: &[PreparedObject],
        purpose: Purpose,
    ) -> Result<Vec<UploadTicket>> {
        let mut tx = self.client.transaction().await?;
        settings(&tx).await?;
        lock_authorized(&tx, auth, purpose).await?;
        let s = auth.scope();
        let row = active_for(&tx, auth, upload, purpose).await?;
        if store.tenant() != s.tenant || store.backend() != row.get::<_, String>(0) {
            return Err(Error::ReceiptMismatch);
        }
        let remaining_objects = row.get::<_, i64>(3) - row.get::<_, i64>(5);
        let remaining_bytes = row.get::<_, i64>(4) - row.get::<_, i64>(6);
        let count = objects.len() as i64;
        let bytes = objects
            .iter()
            .map(|object| object.length() as i64)
            .sum::<i64>();
        let fresh = if count > 1 && count <= remaining_objects && bytes <= remaining_bytes {
            // Only reserve_source_blob_batch calls this with multiple frames,
            // after checking that they are distinct, sorted blobs.
            reserve_fresh_blobs(&mut tx, s, upload, store, objects, purpose).await?
        } else {
            None
        };
        let refresh_holds = fresh.is_none();
        let (tickets, added_objects, added_bytes) = if let Some(tickets) = fresh {
            (tickets, count, bytes)
        } else {
            let mut tickets = Vec::with_capacity(objects.len());
            let mut added_objects = 0_i64;
            let mut added_bytes = 0_i64;
            for object in objects {
                membership(&tx, s, upload, object.references()).await?;
                let digest = object.id().as_bytes().to_vec();
                let old=tx.query_opt("SELECT hold_token,generation FROM th_source_upload_items WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=$4", &[&s.tenant,&s.repository,&upload,&digest]).await?;
                let token = old.as_ref().map(|r| r.get(0)).unwrap_or_else(Uuid::new_v4);
                let length = object.length() as i64;
                if old.is_none()
                    && (added_objects >= remaining_objects
                        || length > remaining_bytes - added_bytes)
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
                    tx.execute("INSERT INTO th_source_upload_items(tenant,repository,upload,digest,generation,hold_token,kind,length,snapshot) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)", &[&s.tenant,&s.repository,&upload,&digest,&lease.key().generation(),&token,&(object.kind() as i16),&length,&snapshot]).await?;
                    if let Some(origin) = object.commit_git_origin() {
                        // Migration 016 introduced this column.
                        let stores_origin = purpose == Purpose::Github
                        || tx
                            .query_one(
                                "SELECT EXISTS(SELECT 1 FROM th_schema_version WHERE singleton AND version>=16)",
                                &[],
                            )
                            .await?
                            .get::<_, bool>(0);
                        if stores_origin {
                            let git_origin = RevisionRef::Git(origin).to_string();
                            tx.execute("UPDATE th_source_upload_items SET git_origin=$5 WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=$4",
                            &[&s.tenant,&s.repository,&upload,&digest,&git_origin]).await?;
                        }
                    }
                    added_objects += 1;
                    added_bytes += length;
                }
                tickets.push(UploadTicket {
                    upload,
                    lease,
                    purpose,
                });
            }
            (tickets, added_objects, added_bytes)
        };
        if added_objects > 0 {
            tx.execute("UPDATE th_source_uploads SET reserved_objects=reserved_objects+$4,reserved_bytes=reserved_bytes+$5 WHERE tenant=$1 AND repository=$2 AND upload=$3", &[&s.tenant,&s.repository,&upload,&added_objects,&added_bytes]).await?;
        }
        if refresh_holds {
            // Retrying a frame may never extend the session's fixed deadline.
            let tokens: Vec<_> = tickets.iter().map(|ticket| ticket.lease.token()).collect();
            tx.execute("UPDATE th_object_holds h SET expires_at=u.expires_at FROM th_source_uploads u WHERE u.tenant=$1 AND u.repository=$2 AND u.upload=$3 AND h.tenant=u.tenant AND h.token=ANY($4::uuid[])", &[&s.tenant,&s.repository,&upload,&tokens]).await?;
        }
        active_for(&tx, auth, upload, purpose).await?;
        auth.check_transaction(&tx, s, auth.principal(), purpose.permission())
            .await?;
        tx.commit().await?;
        Ok(tickets)
    }
    pub async fn publish_source_object(
        &mut self,
        auth: &Authorization,
        ticket: &UploadTicket,
        receipt: &DurableObject,
    ) -> Result<()> {
        self.publish_source_object_for(auth, ticket, receipt, Purpose::Human)
            .await
    }
    pub async fn publish_github_source_object(
        &mut self,
        auth: &Authorization,
        ticket: &UploadTicket,
        receipt: &DurableObject,
    ) -> Result<()> {
        self.publish_source_object_for(auth, ticket, receipt, Purpose::Github)
            .await
    }
    async fn publish_source_object_for(
        &mut self,
        auth: &Authorization,
        ticket: &UploadTicket,
        receipt: &DurableObject,
        purpose: Purpose,
    ) -> Result<()> {
        self.publish_source_objects(
            auth,
            std::slice::from_ref(ticket),
            std::slice::from_ref(receipt),
            purpose,
            false,
        )
        .await
    }
    /// Publish membership only after every blob in the bounded batch is durable.
    pub async fn publish_source_blob_batch(
        &mut self,
        auth: &Authorization,
        tickets: &[UploadTicket],
        receipts: &[DurableObject],
        purpose: Purpose,
    ) -> Result<()> {
        if !(1..=SOURCE_BLOB_BATCH_OBJECTS).contains(&tickets.len())
            || receipts.iter().any(|receipt| receipt.kind() != 1)
            || tickets
                .windows(2)
                .any(|pair| pair[0].lease.key().id() >= pair[1].lease.key().id())
        {
            return Err(Error::Invalid("source blob batch"));
        }
        self.publish_source_objects(auth, tickets, receipts, purpose, true)
            .await
    }
    async fn publish_source_objects(
        &mut self,
        auth: &Authorization,
        tickets: &[UploadTicket],
        receipts: &[DurableObject],
        purpose: Purpose,
        blob_batch: bool,
    ) -> Result<()> {
        let upload = tickets.first().ok_or(Error::ReceiptMismatch)?.upload;
        if tickets.len() != receipts.len() {
            return Err(Error::ReceiptMismatch);
        }
        if tickets
            .iter()
            .any(|ticket| ticket.purpose != purpose || ticket.upload != upload)
        {
            return Err(Error::Forbidden);
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        lock_authorized(&tx, auth, purpose).await?;
        let s = auth.scope();
        active_for(&tx, auth, upload, purpose).await?;
        let digests: Vec<_> = tickets
            .iter()
            .map(|ticket| ticket.lease.key().id().as_bytes().to_vec())
            .collect();
        if tickets
            .iter()
            .any(|ticket| s.tenant != ticket.lease.key().tenant())
        {
            return Err(Error::ReceiptMismatch);
        }
        if blob_batch {
            let items = tx.query(
                "SELECT digest,generation,hold_token FROM th_source_upload_items WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=ANY($4::bytea[]) ORDER BY digest",
                &[&s.tenant, &s.repository, &upload, &digests],
            ).await?;
            if items.len() != tickets.len() {
                return Err(Error::UploadMembership);
            }
            for (ticket, item) in tickets.iter().zip(items) {
                if item.get::<_, Vec<u8>>(0) != ticket.lease.key().id().as_bytes()
                    || item.get::<_, i64>(1) != ticket.generation()
                    || item.get::<_, Uuid>(2) != ticket.lease.token()
                {
                    return Err(Error::ReceiptMismatch);
                }
            }
            let leases: Vec<_> = tickets.iter().map(|ticket| &ticket.lease).collect();
            publish_blobs_in(&tx, &leases, receipts).await?;
        } else {
            for (ticket, receipt) in tickets.iter().zip(receipts) {
                let digest = ticket.lease.key().id().as_bytes().to_vec();
                let item=tx.query_opt("SELECT generation,hold_token FROM th_source_upload_items WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=$4", &[&s.tenant,&s.repository,&upload,&digest]).await?.ok_or(Error::UploadMembership)?;
                if item.get::<_, i64>(0) != ticket.generation()
                    || item.get::<_, Uuid>(1) != ticket.lease.token()
                {
                    return Err(Error::ReceiptMismatch);
                }
                membership(&tx, s, upload, receipt.references()).await?;
                publish_in(&tx, &ticket.lease, receipt).await?;
            }
        }
        tx.execute("UPDATE th_source_upload_items SET complete=true WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=ANY($4::bytea[])", &[&s.tenant,&s.repository,&upload,&digests]).await?;
        active_for(&tx, auth, upload, purpose).await?;
        auth.check_transaction(&tx, s, auth.principal(), purpose.permission())
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
