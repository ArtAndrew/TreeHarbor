//! Durable object reachability. Internal service API; tenant identifiers and
//! upload tickets are not a substitute for authenticated repository access.
use crate::{Error, Metadata, Mutation, Result, identifier, ttl_valid};
use std::collections::{BTreeMap, BTreeSet};
use tokio_postgres::{Row, Transaction};
use treeharbor_core::ObjectId;
use treeharbor_storage::{DurableObject, ManagedStore, PreparedObject};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectKey {
    pub(crate) tenant: String,
    pub(crate) backend: String,
    pub(crate) id: ObjectId,
    pub(crate) generation: i64,
}
impl ObjectKey {
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    pub fn backend(&self) -> &str {
        &self.backend
    }
    pub fn id(&self) -> ObjectId {
        self.id
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
}
#[derive(Clone, Debug)]
pub struct ObjectLease {
    pub(crate) key: ObjectKey,
    pub(crate) token: Uuid,
    pub(crate) upload: bool,
}
impl ObjectLease {
    pub fn key(&self) -> &ObjectKey {
        &self.key
    }
    pub fn token(&self) -> Uuid {
        self.token
    }
    pub fn requires_upload(&self) -> bool {
        self.upload
    }
}
#[derive(Clone, Debug)]
pub struct Retirement {
    key: ObjectKey,
}
impl Retirement {
    pub fn key(&self) -> &ObjectKey {
        &self.key
    }
}

fn bytes(id: ObjectId) -> Vec<u8> {
    id.as_bytes().to_vec()
}
fn id(raw: Vec<u8>) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        raw.try_into().map_err(|_| Error::SchemaMismatch)?,
    ))
}
pub(crate) async fn settings(tx: &Transaction<'_>) -> Result<()> {
    tx.batch_execute("SET LOCAL synchronous_commit=on; SET LOCAL lock_timeout='5s'; SET LOCAL statement_timeout='15s'").await?;
    Ok(())
}
async fn object_row(tx: &Transaction<'_>, tenant: &str, digest: &[u8]) -> Result<Row> {
    tx.query_opt("SELECT generation,backend,kind,length,state FROM th_objects WHERE tenant=$1 AND digest=$2 FOR UPDATE",&[&tenant,&digest]).await?.ok_or(Error::ObjectNotLive)
}
pub(crate) async fn live_lease(tx: &Transaction<'_>, lease: &ObjectLease) -> Result<()> {
    check_live_lease(tx, lease, false).await
}
pub(crate) async fn live_read_lease(tx: &Transaction<'_>, lease: &ObjectLease) -> Result<()> {
    check_live_lease(tx, lease, true).await
}
async fn check_live_lease(tx: &Transaction<'_>, lease: &ObjectLease, shared: bool) -> Result<()> {
    let key = &lease.key;
    let digest = bytes(key.id);
    let row = if shared {
        // Readers of the same root can open concurrently. FOR SHARE still
        // conflicts with retirement, generation changes and hold release.
        tx.query_opt("SELECT generation,backend,kind,length,state FROM th_objects WHERE tenant=$1 AND digest=$2 FOR SHARE", &[&key.tenant,&digest]).await?.ok_or(Error::ObjectNotLive)?
    } else {
        object_row(tx, &key.tenant, &digest).await?
    };
    if row.get::<_, i64>(0) != key.generation
        || row.get::<_, String>(1) != key.backend
        || row.get::<_, String>(4) != "live"
    {
        return Err(Error::ObjectNotLive);
    }
    let valid:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM th_object_holds WHERE tenant=$1 AND token=$2 AND digest=$3 AND generation=$4 AND expires_at>clock_timestamp())",
        &[&key.tenant,&lease.token,&digest,&key.generation]).await?.get(0);
    if !valid {
        return Err(Error::StaleLease);
    }
    Ok(())
}

pub(crate) async fn reserve_in(
    tx: &Transaction<'_>,
    store: &ManagedStore,
    object: &PreparedObject,
    token: Uuid,
    ttl: i32,
) -> Result<ObjectLease> {
    ttl_valid(ttl)?;
    let tenant = store.tenant();
    let digest = bytes(object.id());
    let kind = object.kind() as i16;
    let length = object.length() as i64;
    let inserted=tx.execute("INSERT INTO th_objects (tenant,digest,generation,backend,kind,length,state) VALUES ($1,$2,1,$3,$4,$5,'uploading') ON CONFLICT DO NOTHING",
            &[&tenant,&digest,&store.backend(),&kind,&length]).await?==1;
    let row = object_row(tx, tenant, &digest).await?;
    let mut generation: i64 = row.get(0);
    let state: String = row.get(4);
    if row.get::<_, String>(1) != store.backend()
        || row.get::<_, i16>(2) != kind
        || row.get::<_, i64>(3) != length
    {
        return Err(Error::ReceiptMismatch);
    }
    if state == "retiring" {
        return Err(Error::ObjectBusy);
    }
    if state == "deleted" {
        generation = generation
            .checked_add(1)
            .ok_or(Error::Invalid("object generation overflow"))?;
        tx.execute(
            "DELETE FROM th_object_holds WHERE tenant=$1 AND digest=$2",
            &[&tenant, &digest],
        )
        .await?;
        tx.execute("UPDATE th_objects SET generation=$3,state='uploading',orphaned_at=NULL WHERE tenant=$1 AND digest=$2",&[&tenant,&digest,&generation]).await?;
    }
    let old_hold=tx.query_opt("SELECT digest,generation,expires_at>clock_timestamp() FROM th_object_holds WHERE tenant=$1 AND token=$2",&[&tenant,&token]).await?;
    if let Some(ref hold) = old_hold {
        if hold.get::<_, Vec<u8>>(0) != digest || hold.get::<_, i64>(1) != generation {
            return Err(Error::ReceiptMismatch);
        }
        if !hold.get::<_, bool>(2) {
            return Err(Error::StaleLease);
        }
    }
    if state == "uploading" && !inserted && old_hold.is_none() {
        return Err(Error::ObjectBusy);
    }
    let updated=tx.execute("INSERT INTO th_object_holds (tenant,token,digest,generation,expires_at) VALUES ($1,$2,$3,$4,clock_timestamp()+($5::integer*interval '1 second')) ON CONFLICT (tenant,token) DO UPDATE SET expires_at=EXCLUDED.expires_at WHERE th_object_holds.digest=EXCLUDED.digest AND th_object_holds.generation=EXCLUDED.generation",
            &[&tenant,&token,&digest,&generation,&ttl]).await?;
    if updated != 1 {
        return Err(Error::ReceiptMismatch);
    }
    tx.execute(
        "UPDATE th_objects SET orphaned_at=NULL WHERE tenant=$1 AND digest=$2",
        &[&tenant, &digest],
    )
    .await?;
    Ok(ObjectLease {
        key: ObjectKey {
            tenant: tenant.into(),
            backend: store.backend().into(),
            id: object.id(),
            generation,
        },
        token,
        upload: state != "live",
    })
}

pub(crate) async fn publish_in(
    tx: &Transaction<'_>,
    lease: &ObjectLease,
    receipt: &DurableObject,
) -> Result<()> {
    let key = &lease.key;
    if key.tenant != receipt.tenant()
        || key.backend != receipt.backend()
        || key.generation != receipt.generation()
        || key.id != receipt.id()
    {
        return Err(Error::ReceiptMismatch);
    }
    let mut references = BTreeMap::new();
    for &(child, kind) in receipt.references() {
        if child == key.id {
            return Err(Error::ReceiptMismatch);
        }
        if let Some(previous) = references.insert(child, kind)
            && previous != kind
        {
            return Err(Error::ReceiptMismatch);
        }
    }
    if references.len() > 200_000 {
        return Err(Error::Invalid("reference count"));
    }
    let mut ids: BTreeSet<_> = references.keys().copied().collect();
    ids.insert(key.id);
    let digests: Vec<Vec<u8>> = ids.iter().copied().map(bytes).collect();
    let rows=tx.query("SELECT digest,generation,backend,kind,length,state FROM th_objects WHERE tenant=$1 AND digest=ANY($2::bytea[]) ORDER BY digest FOR UPDATE",&[&key.tenant,&digests]).await?;
    let mut locked = BTreeMap::new();
    for row in rows {
        locked.insert(id(row.get(0))?, row);
    }
    let parent = locked.get(&key.id).ok_or(Error::ObjectNotLive)?;
    if parent.get::<_, i64>(1) != key.generation
        || parent.get::<_, String>(2) != key.backend
        || parent.get::<_, i16>(3) != receipt.kind() as i16
        || parent.get::<_, i64>(4) != receipt.length()
    {
        return Err(Error::ReceiptMismatch);
    }
    let state: String = parent.get(5);
    if state != "uploading" && state != "live" {
        return Err(Error::ObjectNotLive);
    }
    let digest = bytes(key.id);
    let held:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM th_object_holds WHERE tenant=$1 AND token=$2 AND digest=$3 AND generation=$4 AND expires_at>clock_timestamp())",&[&key.tenant,&lease.token,&digest,&key.generation]).await?.get(0);
    if !held {
        return Err(Error::StaleLease);
    }
    let mut children = Vec::new();
    let mut generations = Vec::new();
    for (child, kind) in references {
        let row = locked.get(&child).ok_or(Error::ObjectNotLive)?;
        if row.get::<_, String>(5) != "live" || row.get::<_, i16>(3) != kind as i16 {
            return Err(Error::ObjectNotLive);
        }
        children.push(bytes(child));
        generations.push(row.get::<_, i64>(1));
    }
    tx.execute("INSERT INTO th_object_edges (tenant,parent,parent_generation,child,child_generation) SELECT $1,$2,$3,child,generation FROM unnest($4::bytea[],$5::bigint[]) AS edges(child,generation) ON CONFLICT DO NOTHING",
            &[&key.tenant,&digest,&key.generation,&children,&generations]).await?;
    tx.execute(
        "UPDATE th_objects SET orphaned_at=NULL WHERE tenant=$1 AND digest=ANY($2::bytea[])",
        &[&key.tenant, &digests],
    )
    .await?;
    tx.execute(
        "UPDATE th_objects SET state='live' WHERE tenant=$1 AND digest=$2",
        &[&key.tenant, &digest],
    )
    .await?;
    Ok(())
}

impl Metadata {
    /// Reserve before writing bytes. Published objects can be reused with a fresh
    /// hold; incomplete uploads must finish or retire before the digest is reused.
    pub async fn reserve_object(
        &mut self,
        store: &ManagedStore,
        object: &PreparedObject,
        token: Uuid,
        ttl: i32,
    ) -> Result<ObjectLease> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let lease = reserve_in(&tx, store, object, token, ttl).await?;
        tx.commit().await?;
        Ok(lease)
    }

    /// Make bytes reachable only after a storage-minted receipt. All referenced
    /// children must already be LIVE; their row locks serialize publication with GC.
    pub async fn publish_object(
        &mut self,
        lease: &ObjectLease,
        receipt: &DurableObject,
    ) -> Result<()> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        publish_in(&tx, lease, receipt).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn release_object_hold(&mut self, lease: &ObjectLease) -> Result<()> {
        let key = &lease.key;
        let digest = bytes(key.id);
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        object_row(&tx, &key.tenant, &digest).await?;
        tx.execute("DELETE FROM th_object_holds WHERE tenant=$1 AND token=$2 AND digest=$3 AND generation=$4",&[&key.tenant,&lease.token,&digest,&key.generation]).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Acquire protection before returning any locator. The caller must already
    /// have authenticated access to this repository/root holder.
    pub async fn lease_pinned_root(
        &mut self,
        scope: &crate::Scope,
        holder: &str,
        token: Uuid,
        ttl: i32,
    ) -> Result<ObjectLease> {
        identifier(holder, 128)?;
        ttl_valid(ttl)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        // Same repository->object lock order as pin/unpin and authoritative writes.
        tx.query_opt(
            "SELECT epoch FROM th_repositories WHERE tenant=$1 AND repository=$2 FOR UPDATE",
            &[&scope.tenant, &scope.repository],
        )
        .await?
        .ok_or(Error::NotFound)?;
        let lease = lease_pin_in(&tx, scope, holder, token, ttl).await?;
        tx.commit().await?;
        Ok(lease)
    }

    /// Operator/maintenance primitive, not an autonomous GC scheduler. Refcounts
    /// are derived from authoritative edges/pins/holds; age alone never suffices.
    pub async fn try_retire_object(
        &mut self,
        key: &ObjectKey,
        grace_seconds: i32,
    ) -> Result<Option<Retirement>> {
        if !(0..=31_536_000).contains(&grace_seconds) {
            return Err(Error::Invalid("GC grace period"));
        }
        let digest = bytes(key.id);
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let row = object_row(&tx, &key.tenant, &digest).await?;
        if row.get::<_, i64>(0) != key.generation || row.get::<_, String>(1) != key.backend {
            return Err(Error::StaleLease);
        }
        let state: String = row.get(4);
        if state == "deleted" {
            tx.commit().await?;
            return Ok(None);
        }
        if state == "retiring" {
            tx.commit().await?;
            return Ok(Some(Retirement { key: key.clone() }));
        }
        let protected:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM th_object_edges WHERE tenant=$1 AND child=$2 AND child_generation=$3) OR EXISTS(SELECT 1 FROM th_object_pins WHERE tenant=$1 AND digest=$2 AND generation=$3) OR EXISTS(SELECT 1 FROM th_object_holds WHERE tenant=$1 AND digest=$2 AND generation=$3 AND expires_at>clock_timestamp())",
            &[&key.tenant,&digest,&key.generation]).await?.get(0);
        if protected {
            tx.execute(
                "UPDATE th_objects SET orphaned_at=NULL WHERE tenant=$1 AND digest=$2",
                &[&key.tenant, &digest],
            )
            .await?;
            tx.commit().await?;
            return Ok(None);
        }
        tx.execute("UPDATE th_objects SET orphaned_at=COALESCE(orphaned_at,clock_timestamp()) WHERE tenant=$1 AND digest=$2",&[&key.tenant,&digest]).await?;
        let eligible:bool=tx.query_one("SELECT orphaned_at<=clock_timestamp()-($3::integer*interval '1 second') FROM th_objects WHERE tenant=$1 AND digest=$2",&[&key.tenant,&digest,&grace_seconds]).await?.get(0);
        if !eligible {
            tx.commit().await?;
            return Ok(None);
        }
        tx.execute(
            "UPDATE th_objects SET state='retiring' WHERE tenant=$1 AND digest=$2",
            &[&key.tenant, &digest],
        )
        .await?;
        tx.execute("INSERT INTO th_object_retirements (tenant,digest,generation,backend) VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING",&[&key.tenant,&digest,&key.generation,&key.backend]).await?;
        tx.commit().await?;
        Ok(Some(Retirement { key: key.clone() }))
    }

    /// Recovery can safely retry this after either side of the filesystem/DB
    /// boundary. Generation-qualified deletion cannot touch a later generation.
    pub async fn finish_retirement(
        &mut self,
        store: &ManagedStore,
        retirement: &Retirement,
    ) -> Result<()> {
        let key = &retirement.key;
        let digest = bytes(key.id);
        if key.tenant != store.tenant() || key.backend != store.backend() {
            return Err(Error::ReceiptMismatch);
        }
        // Revalidate before destructive IO: an in-memory permit alone is not
        // authority, especially after restore or a changed catalog connection.
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let row = object_row(&tx, &key.tenant, &digest).await?;
        let tombstone=tx.query_opt("SELECT completed_at IS NOT NULL FROM th_object_retirements WHERE tenant=$1 AND digest=$2 AND generation=$3 AND backend=$4",&[&key.tenant,&digest,&key.generation,&key.backend]).await?.ok_or(Error::StaleLease)?;
        let completed: bool = tombstone.get(0);
        let current_generation: i64 = row.get(0);
        let current_state: String = row.get(4);
        if row.get::<_, String>(1) != key.backend
            || (!completed && (current_generation != key.generation || current_state != "retiring"))
            || (completed
                && (current_generation < key.generation
                    || (current_generation == key.generation && current_state != "deleted")))
        {
            return Err(Error::StaleLease);
        }
        tx.commit().await?;
        // IO occurs outside the metadata transaction. Restore must fence old
        // processes before replacing the DB/objects; this is not a restore lock.
        store.remove_retired_generation(key.generation, key.id)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let row = object_row(&tx, &key.tenant, &digest).await?;
        let tombstone=tx.query_opt("SELECT completed_at IS NOT NULL FROM th_object_retirements WHERE tenant=$1 AND digest=$2 AND generation=$3 AND backend=$4",&[&key.tenant,&digest,&key.generation,&key.backend]).await?.ok_or(Error::StaleLease)?;
        if tombstone.get::<_, bool>(0) {
            tx.commit().await?;
            return Ok(());
        }
        if row.get::<_, i64>(0) != key.generation || row.get::<_, String>(4) != "retiring" {
            return Err(Error::StaleLease);
        }
        tx.execute(
            "DELETE FROM th_object_edges WHERE tenant=$1 AND parent=$2 AND parent_generation=$3",
            &[&key.tenant, &digest, &key.generation],
        )
        .await?;
        tx.execute(
            "DELETE FROM th_object_holds WHERE tenant=$1 AND digest=$2 AND generation=$3",
            &[&key.tenant, &digest, &key.generation],
        )
        .await?;
        tx.execute(
            "UPDATE th_objects SET state='deleted' WHERE tenant=$1 AND digest=$2",
            &[&key.tenant, &digest],
        )
        .await?;
        tx.execute("UPDATE th_object_retirements SET completed_at=clock_timestamp() WHERE tenant=$1 AND digest=$2 AND generation=$3",&[&key.tenant,&digest,&key.generation]).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn pending_retirements(&self, tenant: &str, limit: i64) -> Result<Vec<Retirement>> {
        crate::tenant_valid(tenant)?;
        if !(1..=1000).contains(&limit) {
            return Err(Error::Invalid("retirement page limit"));
        }
        self.client.query("SELECT digest,generation,backend FROM th_object_retirements WHERE tenant=$1 AND completed_at IS NULL ORDER BY digest,generation LIMIT $2",&[&tenant,&limit]).await?.into_iter().map(|row|Ok(Retirement {key:ObjectKey {tenant:tenant.into(),id:id(row.get(0))?,generation:row.get(1),backend:row.get(2)}})).collect()
    }
}

impl Mutation<'_> {
    pub(crate) async fn pin_kind(
        &self,
        holder: &str,
        lease: &ObjectLease,
        kind: i16,
    ) -> Result<()> {
        live_lease(&self.tx, lease).await?;
        let actual: i16 = object_row(&self.tx, lease.key.tenant(), &bytes(lease.key.id))
            .await?
            .get(2);
        if actual != kind {
            return Err(Error::ObjectNotLive);
        }
        self.pin_object(holder, lease).await
    }

    /// Copy an existing repository root while the repository and object are
    /// locked. The source pin, not an unprotected locator, grants retention.
    pub(crate) async fn copy_root_pin(&self, source: &str, target: &str) -> Result<()> {
        identifier(target, 128)?;
        let scope = &self.request.scope;
        let row = self.tx.query_opt("SELECT digest,generation FROM th_object_pins WHERE tenant=$1 AND repository=$2 AND holder=$3", &[&scope.tenant,&scope.repository,&source]).await?.ok_or(Error::NotFound)?;
        let digest: Vec<u8> = row.get(0);
        let generation: i64 = row.get(1);
        let object = object_row(&self.tx, &scope.tenant, &digest).await?;
        if object.get::<_, i64>(0) != generation || object.get::<_, String>(4) != "live" {
            return Err(Error::ObjectNotLive);
        }
        self.tx.execute("INSERT INTO th_object_pins (tenant,repository,holder,digest,generation) VALUES ($1,$2,$3,$4,$5)", &[&scope.tenant,&scope.repository,&target,&digest,&generation]).await?;
        Ok(())
    }

    /// Atomic with the acknowledged service effect and operation receipt.
    /// Persistent workspace/branch/backup pins have no automatic TTL.
    pub async fn pin_object(&self, holder: &str, lease: &ObjectLease) -> Result<()> {
        identifier(holder, 128)?;
        let scope = &self.request.scope;
        let key = &lease.key;
        if scope.tenant != key.tenant {
            return Err(Error::NotFound);
        }
        live_lease(&self.tx, lease).await?;
        let digest = bytes(key.id);
        self.tx.execute("INSERT INTO th_object_pins (tenant,repository,holder,digest,generation) VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",&[&scope.tenant,&scope.repository,&holder,&digest,&key.generation]).await?;
        let row=self.tx.query_one("SELECT digest,generation FROM th_object_pins WHERE tenant=$1 AND repository=$2 AND holder=$3",&[&scope.tenant,&scope.repository,&holder]).await?;
        if row.get::<_, Vec<u8>>(0) != digest || row.get::<_, i64>(1) != key.generation {
            return Err(Error::PinConflict);
        }
        self.tx
            .execute(
                "UPDATE th_objects SET orphaned_at=NULL WHERE tenant=$1 AND digest=$2",
                &[&scope.tenant, &digest],
            )
            .await?;
        Ok(())
    }
    /// The owning lifecycle controller must first satisfy its retention/drain
    /// policy. Lease expiry alone never authorizes removal of an active workspace.
    pub async fn unpin_object(&self, holder: &str) -> Result<()> {
        identifier(holder, 128)?;
        let scope = &self.request.scope;
        let row=self.tx.query_opt("SELECT digest,generation FROM th_object_pins WHERE tenant=$1 AND repository=$2 AND holder=$3",&[&scope.tenant,&scope.repository,&holder]).await?;
        if let Some(row) = row {
            let digest: Vec<u8> = row.get(0);
            object_row(&self.tx, &scope.tenant, &digest).await?;
            self.tx
                .execute(
                    "DELETE FROM th_object_pins WHERE tenant=$1 AND repository=$2 AND holder=$3",
                    &[&scope.tenant, &scope.repository, &holder],
                )
                .await?;
        }
        Ok(())
    }
}

/// Caller holds the repository row lock before acquiring the object row.
pub(crate) async fn lease_pin_in(
    tx: &Transaction<'_>,
    scope: &crate::Scope,
    holder: &str,
    token: Uuid,
    ttl: i32,
) -> Result<ObjectLease> {
    identifier(holder, 128)?;
    ttl_valid(ttl)?;
    let pin=tx.query_opt("SELECT digest,generation FROM th_object_pins WHERE tenant=$1 AND repository=$2 AND holder=$3",&[&scope.tenant,&scope.repository,&holder]).await?.ok_or(Error::NotFound)?;
    let digest: Vec<u8> = pin.get(0);
    let generation: i64 = pin.get(1);
    let row = object_row(tx, &scope.tenant, &digest).await?;
    if row.get::<_, String>(4) != "live" || row.get::<_, i64>(0) != generation {
        return Err(Error::ObjectNotLive);
    }
    // A token collision fails, it never reassigns a hold from another object.
    tx.execute("INSERT INTO th_object_holds (tenant,token,digest,generation,expires_at) VALUES ($1,$2,$3,$4,clock_timestamp()+($5::integer*interval '1 second'))",&[&scope.tenant,&token,&digest,&generation,&ttl]).await?;
    tx.execute(
        "UPDATE th_objects SET orphaned_at=NULL WHERE tenant=$1 AND digest=$2",
        &[&scope.tenant, &digest],
    )
    .await?;
    let key = ObjectKey {
        tenant: scope.tenant.clone(),
        backend: row.get(1),
        id: id(digest)?,
        generation,
    };
    Ok(ObjectLease {
        key,
        token,
        upload: false,
    })
}
