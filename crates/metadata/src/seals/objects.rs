//! Scoped publication of a node-verified immutable capture. Trusted node methods
//! do not grant general source import, owner acceptance, or validation authority.
use super::*;
use crate::{
    catalog::{ObjectKey, ObjectLease, lease_pin_in, live_lease, publish_in, reserve_in},
    reads::{ReadBatch, ReadNode, ReadObject, ReadSession},
};
use std::collections::{BTreeMap, BTreeSet};
use treeharbor_core::{Changes, Object, ObjectId, RevisionRef};
use treeharbor_storage::{DurableObject, ManagedStore, PreparedObject};

/// Declaration copied from a locally verified capture, never from agent claims
/// that a runtime stopped. The assigned node is the execution trust boundary.
pub struct SealPublication {
    manifest: [u8; 32],
    operations: ObjectId,
    changes: Changes,
    objects: Vec<ObjectId>,
    dependencies: BTreeMap<ObjectId, u8>,
    bytes: i64,
    definition: Vec<u8>,
}
impl SealPublication {
    pub fn new(
        manifest: [u8; 32],
        changes: Changes,
        objects: Vec<ObjectId>,
        dependencies: BTreeMap<ObjectId, u8>,
        bytes: u64,
    ) -> Result<Self> {
        // Bound allocation before expanding object IDs into JSON strings.
        if objects
            .len()
            .saturating_mul(74)
            .saturating_add(dependencies.len().saturating_mul(78))
            .saturating_add(256)
            > 16 * 1024 * 1024
        {
            return Err(Error::UploadLimit);
        }
        let operations = Object::ChangeOperations(changes.clone()).id()?;
        let unique: BTreeSet<_> = objects.iter().copied().collect();
        if objects.is_empty()
            || objects.len() > 1_000_000
            || objects.len() != unique.len()
            || objects.last() != Some(&operations)
            || dependencies.len() > 1_000_000
            || dependencies
                .iter()
                .any(|(id, kind)| unique.contains(id) || !matches!(kind, 1 | 2))
            || !(22..=68_719_476_736).contains(&bytes)
        {
            return Err(Error::Invalid("seal artifact declaration"));
        }
        let definition = serde_json::to_vec(&json!([
            1,
            hex(&manifest),
            operations.to_string(),
            objects.iter().map(ToString::to_string).collect::<Vec<_>>(),
            dependencies
                .iter()
                .map(|(id, k)| (id.to_string(), k))
                .collect::<Vec<_>>(),
            bytes
        ]))
        .map_err(|_| Error::Invalid("seal artifact declaration"))?;
        if definition.len() > 16 * 1024 * 1024 {
            return Err(Error::UploadLimit);
        }
        Ok(Self {
            manifest,
            operations,
            changes,
            objects,
            dependencies,
            bytes: bytes as i64,
            definition,
        })
    }
}
/// Private session anchored at this run's exact snapshot, not its entire commit
/// ancestry. A digest from a different revision cannot become a baseline proof.
pub struct SealSource {
    seal: Uuid,
    binding: [u8; 32],
    session: ReadSession,
}
impl SealSource {
    pub fn root(&self) -> ReadNode {
        self.session.root()
    }
}
pub struct SealObjectTicket {
    seal: Uuid,
    binding: [u8; 32],
    manifest: [u8; 32],
    lease: ObjectLease,
}
impl SealObjectTicket {
    pub fn generation(&self) -> i64 {
        self.lease.key().generation()
    }
}
fn bytes(id: ObjectId) -> Vec<u8> {
    id.as_bytes().to_vec()
}
fn token(s: &Scope, seal: Uuid, id: ObjectId) -> Uuid {
    let mut h = Sha256::new();
    h.update(b"treeharbor-seal-object\0v1\0");
    h.update((s.tenant().len() as u64).to_be_bytes());
    h.update(s.tenant().as_bytes());
    h.update(s.repository().as_bytes());
    h.update(seal.as_bytes());
    h.update(id.as_bytes());
    Uuid::from_bytes(h.finalize()[..16].try_into().expect("digest length"))
}
async fn artifact(tx: &Transaction<'_>, s: &Scope, seal: Uuid) -> Result<Row> {
    tx.query_opt(
        "SELECT * FROM th_seal_artifacts WHERE tenant=$1 AND repository=$2 AND seal=$3",
        &[&s.tenant(), &s.repository(), &seal],
    )
    .await?
    .ok_or(Error::NotFound)
}
async fn item(tx: &Transaction<'_>, s: &Scope, seal: Uuid, id: ObjectId) -> Result<Row> {
    tx.query_opt(
        "SELECT * FROM th_seal_items WHERE tenant=$1 AND repository=$2 AND seal=$3 AND digest=$4",
        &[&s.tenant(), &s.repository(), &seal, &bytes(id)],
    )
    .await?
    .ok_or(Error::UploadMembership)
}
fn item_lease(s: &Scope, backend: &str, id: ObjectId, row: &Row) -> Result<ObjectLease> {
    if row.get::<_, bool>("baseline") {
        return Err(Error::UploadMembership);
    }
    Ok(ObjectLease {
        key: ObjectKey {
            tenant: s.tenant().into(),
            backend: backend.into(),
            id,
            generation: row
                .get::<_, Option<i64>>("generation")
                .ok_or(Error::UploadMembership)?,
        },
        token: row
            .get::<_, Option<Uuid>>("hold_token")
            .ok_or(Error::UploadMembership)?,
        upload: false,
    })
}
async fn references(
    tx: &Transaction<'_>,
    s: &Scope,
    seal: Uuid,
    ordinal: i64,
    refs: &[(ObjectId, u8)],
) -> Result<()> {
    if refs.is_empty() {
        return Ok(());
    }
    let mut unique = BTreeMap::new();
    for &(id, kind) in refs {
        if unique.insert(id, kind).is_some_and(|old| old != kind) {
            return Err(Error::ReceiptMismatch);
        }
    }
    let ids: Vec<Vec<u8>> = unique.keys().copied().map(bytes).collect();
    let rows=tx.query("SELECT i.digest,i.kind,i.baseline,i.ordinal,i.complete,i.generation,o.generation,o.state FROM th_seal_items i JOIN th_objects o ON o.tenant=i.tenant AND o.digest=i.digest WHERE i.tenant=$1 AND i.repository=$2 AND i.seal=$3 AND i.digest=ANY($4::bytea[])",
        &[&s.tenant(),&s.repository(),&seal,&ids]).await?;
    if rows.len() != unique.len() {
        return Err(Error::UploadMembership);
    }
    for row in rows {
        let id = ObjectId::from_bytes(
            row.get::<_, Vec<u8>>(0)
                .try_into()
                .map_err(|_| Error::SchemaMismatch)?,
        );
        if !row.get::<_, bool>(4)
            || row.get::<_, Option<i16>>(1) != unique.get(&id).map(|k| i16::from(*k))
            || (!row.get::<_, bool>(2) && row.get::<_, Option<i64>>(3).is_none_or(|n| n >= ordinal))
            || row.get::<_, Option<i64>>(5) != Some(row.get(6))
            || row.get::<_, String>(7) != "live"
        {
            return Err(Error::UploadMembership);
        }
    }
    Ok(())
}
impl Metadata {
    /// Recover the exact committing worker's acknowledgement without touching
    /// mutable files, requiring an unexpired claim, or publishing anything again.
    pub async fn completed_workspace_seal(
        &mut self,
        auth: &Authorization,
        plan: &SealPlan,
    ) -> Result<Option<Value>> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        let p = plan.context.prepared();
        repository_lock(&tx, s, false).await?;
        if p.scope.tenant() != s.tenant()
            || p.scope.repository() != s.repository()
            || p.node != auth.principal()
        {
            return Err(Error::Forbidden);
        }
        node_auth(&tx, auth, p.slot.storage).await?;
        let row = tx.query_opt("SELECT c.state,a.receipt,a.finished_worker,a.finished_generation FROM th_workspace_seals c JOIN th_seal_artifacts a USING(tenant,repository,seal) WHERE c.tenant=$1 AND c.repository=$2 AND c.seal=$3",
            &[&s.tenant(),&s.repository(),&plan.id]).await?;
        let value = if let Some(row) = row {
            let value = row.get::<_, Option<Value>>(1);
            if value.is_some() {
                if row.get::<_, String>(0) != "ready" {
                    return Err(Error::SchemaMismatch);
                }
                if row.get::<_, Option<Uuid>>(2) != Some(plan.worker)
                    || row.get::<_, Option<i64>>(3) != Some(plan.generation)
                {
                    return Err(Error::StaleLease);
                }
            }
            value
        } else {
            None
        };
        node_auth(&tx, auth, p.slot.storage).await?;
        tx.commit().await?;
        Ok(value)
    }
    /// Bind the first artifact and backend before any upload. Retries compare the
    /// entire declaration; counters and original expiry are never reset.
    pub async fn begin_seal_publication(
        &mut self,
        auth: &Authorization,
        plan: &SealPlan,
        store: &ManagedStore,
        input: &SealPublication,
    ) -> Result<()> {
        if store.tenant() != auth.scope().tenant()
            || input.objects.len() as i64 > plan.max_objects
            || input.bytes > plan.max_bytes
        {
            return Err(Error::UploadLimit);
        }
        let paths: BTreeSet<_> = plan
            .selection
            .replace()
            .iter()
            .chain(plan.selection.delete())
            .collect();
        if input.changes.operations().len() != paths.len()
            || input.changes.operations().iter().any(|op| {
                !paths.contains(&op.path)
                    || op.replacement.is_some() != plan.selection.replace().contains(&op.path)
            })
        {
            return Err(Error::ReceiptMismatch);
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        checked(&tx, auth, plan).await?;
        let old=tx.query_opt("SELECT definition,backend FROM th_seal_artifacts WHERE tenant=$1 AND repository=$2 AND seal=$3",&[&s.tenant(),&s.repository(),&plan.id]).await?;
        if let Some(old) = old {
            if old.get::<_, Vec<u8>>(0) != input.definition
                || old.get::<_, String>(1) != store.backend()
            {
                return Err(Error::ReceiptMismatch);
            }
        } else {
            tx.execute("INSERT INTO th_seal_artifacts(tenant,repository,seal,manifest,operations,backend,definition,object_count,encoded_bytes) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",
                &[&s.tenant(),&s.repository(),&plan.id,&input.manifest.as_slice(),&bytes(input.operations),&store.backend(),&input.definition,&(input.objects.len() as i64),&input.bytes]).await?;
            let ids: Vec<Vec<u8>> = input.objects.iter().copied().map(bytes).collect();
            tx.execute("INSERT INTO th_seal_items(tenant,repository,seal,digest,baseline,ordinal) SELECT $1,$2,$3,digest,false,ordinal-1 FROM unnest($4::bytea[]) WITH ORDINALITY AS n(digest,ordinal)",
                &[&s.tenant(),&s.repository(),&plan.id,&ids]).await?;
            let ids: Vec<Vec<u8>> = input.dependencies.keys().copied().map(bytes).collect();
            let kinds: Vec<i16> = input.dependencies.values().map(|k| i16::from(*k)).collect();
            tx.execute("INSERT INTO th_seal_items(tenant,repository,seal,digest,baseline,kind) SELECT $1,$2,$3,digest,true,kind FROM unnest($4::bytea[],$5::smallint[]) AS n(digest,kind)",
                &[&s.tenant(),&s.repository(),&plan.id,&ids,&kinds]).await?;
        }
        checked(&tx, auth, plan).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn open_seal_source(
        &mut self,
        auth: &Authorization,
        plan: &SealPlan,
    ) -> Result<SealSource> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        let r = checked(&tx, auth, plan).await?;
        let base =
            lease_pin_in(&tx, s, &r.get::<_, String>("base_pin"), Uuid::new_v4(), 300).await?;
        if plan.context.prepared().revision != RevisionRef::Native(base.key().id()) {
            return Err(Error::ReadScope);
        }
        let snapshot = plan.context.prepared().snapshot;
        let row=tx.query_opt("SELECT o.generation,o.backend FROM th_objects o JOIN th_object_edges e ON e.tenant=o.tenant AND e.child=o.digest AND e.child_generation=o.generation WHERE e.tenant=$1 AND e.parent=$2 AND e.parent_generation=$3 AND o.digest=$4 AND o.kind=3 AND o.state='live' FOR UPDATE OF o",
            &[&s.tenant(),&bytes(base.key().id()),&base.key().generation(),&bytes(snapshot)]).await?.ok_or(Error::ReadScope)?;
        let key = ObjectKey {
            tenant: s.tenant().into(),
            backend: row.get(1),
            id: snapshot,
            generation: row.get(0),
        };
        let lease = ObjectLease {
            key,
            token: Uuid::new_v4(),
            upload: false,
        };
        tx.execute("INSERT INTO th_object_holds(tenant,token,digest,generation,expires_at) SELECT $1,$2,$3,$4,expires_at FROM th_workspace_seals WHERE tenant=$1 AND repository=$5 AND seal=$6",
            &[&s.tenant(),&lease.token(),&bytes(snapshot),&lease.key().generation(),&s.repository(),&plan.id]).await?;
        tx.execute(
            "DELETE FROM th_object_holds WHERE tenant=$1 AND token=$2",
            &[&s.tenant(), &base.token()],
        )
        .await?;
        checked(&tx, auth, plan).await?;
        tx.commit().await?;
        Ok(SealSource {
            seal: plan.id,
            binding: plan.binding,
            session: ReadSession::from_lease(lease),
        })
    }
    pub async fn read_seal_source(
        &mut self,
        auth: &Authorization,
        plan: &SealPlan,
        source: &SealSource,
        store: &ManagedStore,
        nodes: &[ReadNode],
        budget: u64,
    ) -> Result<ReadBatch> {
        if source.seal != plan.id || source.binding != plan.binding {
            return Err(Error::ReadScope);
        }
        self.renew_workspace_seal(auth, plan, 60).await?;
        self.open_read_batch(&source.session, store, nodes, budget)
            .await
    }
    /// Only verified objects from the original snapshot session can satisfy
    /// declared baseline dependencies. A ReadObject from another session fails.
    pub async fn prove_seal_dependency(
        &mut self,
        auth: &Authorization,
        plan: &SealPlan,
        source: &SealSource,
        proof: &ReadObject,
    ) -> Result<()> {
        if source.seal != plan.id || source.binding != plan.binding {
            return Err(Error::ReadScope);
        }
        let key = proof.scoped_key(&source.session)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        checked(&tx, auth, plan).await?;
        source.session.check(&tx).await?;
        let header = artifact(&tx, s, plan.id).await?;
        if key.tenant() != s.tenant() || key.backend() != header.get::<_, String>("backend") {
            return Err(Error::ReadScope);
        }
        let i = item(&tx, s, plan.id, proof.id()).await?;
        if !i.get::<_, bool>("baseline")
            || i.get::<_, Option<i16>>("kind") != Some(i16::from(proof.object().kind()))
        {
            return Err(Error::UploadMembership);
        }
        let old = i.get::<_, Option<i64>>("generation");
        if old.is_some_and(|g| g != key.generation()) {
            return Err(Error::ReceiptMismatch);
        }
        if old.is_none() {
            tx.execute("UPDATE th_seal_items SET generation=$5,length=$6,complete=true WHERE tenant=$1 AND repository=$2 AND seal=$3 AND digest=$4",
                &[&s.tenant(),&s.repository(),&plan.id,&bytes(proof.id()),&key.generation(),&(proof.encoded_length() as i64)]).await?;
        }
        checked(&tx, auth, plan).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn close_seal_source(&mut self, source: SealSource) -> Result<()> {
        self.close_read_session(source.session).await
    }
    pub async fn reserve_seal_object(
        &mut self,
        auth: &Authorization,
        plan: &SealPlan,
        store: &ManagedStore,
        object: &PreparedObject,
    ) -> Result<SealObjectTicket> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        checked(&tx, auth, plan).await?;
        let h = artifact(&tx, s, plan.id).await?;
        if store.tenant() != s.tenant() || store.backend() != h.get::<_, String>("backend") {
            return Err(Error::ReceiptMismatch);
        }
        let i = item(&tx, s, plan.id, object.id()).await?;
        if i.get::<_, bool>("baseline")
            || !matches!(object.kind(), 1 | 2 | 4)
            || (object.kind() == 4) != (bytes(object.id()) == h.get::<_, Vec<u8>>("operations"))
        {
            return Err(Error::UploadMembership);
        }
        let ordinal = i
            .get::<_, Option<i64>>("ordinal")
            .ok_or(Error::SchemaMismatch)?;
        references(&tx, s, plan.id, ordinal, object.references()).await?;
        let old = i.get::<_, Option<i64>>("generation");
        if old.is_none()
            && (h.get::<_, i64>("reserved_objects") >= h.get::<_, i64>("object_count")
                || object.length() as i64
                    > h.get::<_, i64>("encoded_bytes") - h.get::<_, i64>("reserved_bytes"))
        {
            return Err(Error::UploadLimit);
        }
        let token = token(s, plan.id, object.id());
        let lease = reserve_in(&tx, store, object, token, 60).await?;
        if let Some(g) = old {
            if g != lease.key().generation()
                || i.get::<_, Option<Uuid>>("hold_token") != Some(token)
                || i.get::<_, Option<i16>>("kind") != Some(i16::from(object.kind()))
                || i.get::<_, Option<i64>>("length") != Some(object.length() as i64)
            {
                return Err(Error::ReceiptMismatch);
            }
        } else {
            tx.execute("UPDATE th_seal_items SET generation=$5,hold_token=$6,kind=$7,length=$8 WHERE tenant=$1 AND repository=$2 AND seal=$3 AND digest=$4",
                &[&s.tenant(),&s.repository(),&plan.id,&bytes(object.id()),&lease.key().generation(),&token,&i16::from(object.kind()),&(object.length() as i64)]).await?;
            tx.execute("UPDATE th_seal_artifacts SET reserved_objects=reserved_objects+1,reserved_bytes=reserved_bytes+$4 WHERE tenant=$1 AND repository=$2 AND seal=$3",
                &[&s.tenant(),&s.repository(),&plan.id,&(object.length() as i64)]).await?;
        }
        tx.execute("UPDATE th_object_holds h SET expires_at=c.expires_at FROM th_workspace_seals c WHERE c.tenant=$1 AND c.repository=$2 AND c.seal=$3 AND h.tenant=c.tenant AND h.token=$4",
            &[&s.tenant(),&s.repository(),&plan.id,&token]).await?;
        checked(&tx, auth, plan).await?;
        tx.commit().await?;
        Ok(SealObjectTicket {
            seal: plan.id,
            binding: plan.binding,
            manifest: h
                .get::<_, Vec<u8>>("manifest")
                .try_into()
                .map_err(|_| Error::SchemaMismatch)?,
            lease,
        })
    }
    pub async fn publish_seal_object(
        &mut self,
        auth: &Authorization,
        plan: &SealPlan,
        ticket: &SealObjectTicket,
        receipt: &DurableObject,
    ) -> Result<()> {
        if ticket.seal != plan.id || ticket.binding != plan.binding {
            return Err(Error::ReceiptMismatch);
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        checked(&tx, auth, plan).await?;
        let h = artifact(&tx, s, plan.id).await?;
        let i = item(&tx, s, plan.id, receipt.id()).await?;
        let stored = item_lease(s, &h.get::<_, String>("backend"), receipt.id(), &i)?;
        if h.get::<_, Vec<u8>>("manifest") != ticket.manifest
            || stored.key() != ticket.lease.key()
            || stored.token() != ticket.lease.token()
        {
            return Err(Error::ReceiptMismatch);
        }
        references(
            &tx,
            s,
            plan.id,
            i.get::<_, Option<i64>>("ordinal")
                .ok_or(Error::SchemaMismatch)?,
            receipt.references(),
        )
        .await?;
        publish_in(&tx, &ticket.lease, receipt).await?;
        tx.execute("UPDATE th_seal_items SET complete=true WHERE tenant=$1 AND repository=$2 AND seal=$3 AND digest=$4",
            &[&s.tenant(),&s.repository(),&plan.id,&bytes(receipt.id())]).await?;
        checked(&tx, auth, plan).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Permanent operations pin, immutable ready receipt and notification commit
    /// together. This never accepts a ChangeSet or emits a Validation.
    pub async fn finish_seal_publication(
        &mut self,
        auth: &Authorization,
        plan: &SealPlan,
        manifest: [u8; 32],
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        let p = plan.context.prepared();
        if p.scope.tenant() != s.tenant()
            || p.scope.repository() != s.repository()
            || p.node != auth.principal()
        {
            return Err(Error::Forbidden);
        }
        node_auth(&tx, auth, p.slot.storage).await?;
        let h = artifact(&tx, s, plan.id).await?;
        if h.get::<_, Vec<u8>>("manifest") != manifest {
            return Err(Error::ReceiptMismatch);
        }
        if let Some(value) = h.get::<_, Option<Value>>("receipt") {
            if h.get::<_, Option<Uuid>>("finished_worker") != Some(plan.worker)
                || h.get::<_, Option<i64>>("finished_generation") != Some(plan.generation)
            {
                return Err(Error::StaleLease);
            }
            tx.commit().await?;
            return Ok(value);
        }
        checked(&tx, auth, plan).await?;
        if h.get::<_,i64>("reserved_objects")!=h.get::<_,i64>("object_count") || h.get::<_,i64>("reserved_bytes")!=h.get::<_,i64>("encoded_bytes")
            || tx.query_one("SELECT EXISTS(SELECT 1 FROM th_seal_items WHERE tenant=$1 AND repository=$2 AND seal=$3 AND NOT complete)",&[&s.tenant(),&s.repository(),&plan.id]).await?.get::<_,bool>(0) { return Err(Error::UploadMembership); }
        let operations = ObjectId::from_bytes(
            h.get::<_, Vec<u8>>("operations")
                .try_into()
                .map_err(|_| Error::SchemaMismatch)?,
        );
        let i = item(&tx, s, plan.id, operations).await?;
        let lease = item_lease(s, &h.get::<_, String>("backend"), operations, &i)?;
        live_lease(&tx, &lease).await?;
        if i.get::<_, Option<i16>>("kind") != Some(4) {
            return Err(Error::ReceiptMismatch);
        }
        // Reject extraneous declarations. Only declared edges of the operations
        // graph count; baseline subtrees remain pinned by the original source.
        let reachable:i64=tx.query_one(r#"WITH RECURSIVE reached(digest,generation) AS (
            SELECT digest,generation FROM th_seal_items WHERE tenant=$1 AND repository=$2 AND seal=$3 AND digest=$4
            UNION SELECT i.digest,i.generation FROM reached r JOIN th_object_edges e
            ON e.tenant=$1 AND e.parent=r.digest AND e.parent_generation=r.generation
            JOIN th_seal_items i ON i.tenant=$1 AND i.repository=$2 AND i.seal=$3 AND i.digest=e.child AND i.generation=e.child_generation)
            SELECT count(*)::bigint FROM reached"#,&[&s.tenant(),&s.repository(),&plan.id,&bytes(operations)]).await?.get(0);
        let total:i64=tx.query_one("SELECT count(*)::bigint FROM th_seal_items WHERE tenant=$1 AND repository=$2 AND seal=$3",&[&s.tenant(),&s.repository(),&plan.id]).await?.get(0);
        if reachable != total {
            return Err(Error::UploadMembership);
        }
        let pin = format!("seal/{}/operations", plan.id);
        tx.execute("INSERT INTO th_object_pins(tenant,repository,holder,digest,generation) VALUES($1,$2,$3,$4,$5)",&[&s.tenant(),&s.repository(),&pin,&bytes(operations),&lease.key().generation()]).await?;
        let value = json!({"seal":plan.id.to_string(),"run":plan.context.run().to_string(),"state":"ready","manifest_digest":hex(&manifest),"operations":operations.to_string(),"generation":p.generation,"epoch":p.epoch});
        tx.execute("UPDATE th_seal_artifacts a SET finished_worker=c.worker,finished_generation=c.claim_generation,finished_lease_until=c.lease_until,receipt=$4 FROM th_workspace_seals c WHERE a.tenant=$1 AND a.repository=$2 AND a.seal=$3 AND c.tenant=a.tenant AND c.repository=a.repository AND c.seal=a.seal",
            &[&s.tenant(),&s.repository(),&plan.id,&value]).await?;
        tx.execute("UPDATE th_workspace_seals SET state='ready',worker=NULL,lease_until=NULL,manifest_digest=$4,operations=$5,operations_pin=$6,completed_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND seal=$3",
            &[&s.tenant(),&s.repository(),&plan.id,&manifest.as_slice(),&bytes(operations),&pin]).await?;
        let request = auth
            .context(format!("seal/{}/ready", plan.id), p.authority, p.epoch)
            .request(
                "workspace.seal.ready.v1",
                json!([
                    1,
                    plan.id.to_string(),
                    hex(&plan.binding),
                    hex(&manifest),
                    plan.worker.to_string(),
                    plan.generation
                ]),
            )?;
        tx.execute("INSERT INTO th_operations(tenant,repository,key,request_digest,principal,kind,result) VALUES($1,$2,$3,$4,$5,$6,$7)",
            &[&s.tenant(),&s.repository(),&request.key,&request.digest,&request.principal,&request.kind,&value]).await?;
        tx.execute("INSERT INTO th_outbox(tenant,repository,operation_key,ordinal,kind,payload) VALUES($1,$2,$3,0,'workspace.seal.ready',$4)",
            &[&s.tenant(),&s.repository(),&request.key,&value]).await?;
        // Recheck after all writes, including slow triggers. An expired lease
        // rolls back the pins, receipt, outbox and ready transition together.
        let row = record(&tx, s, plan.id).await?;
        current(&tx, s, &row, true).await?;
        node_auth(&tx, auth, p.slot.storage).await?;
        let live:bool=tx.query_one("SELECT c.expires_at>clock_timestamp() AND a.finished_lease_until>clock_timestamp() FROM th_workspace_seals c JOIN th_seal_artifacts a USING(tenant,repository,seal) WHERE c.tenant=$1 AND c.repository=$2 AND c.seal=$3",&[&s.tenant(),&s.repository(),&plan.id]).await?.get(0);
        if !live {
            return Err(Error::StaleLease);
        }
        tx.commit().await?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_object_token_matches_independent_vector_and_scope() {
        let scope = Scope::new("test-tenant", Uuid::from_u128(1)).unwrap();
        let seal = Uuid::from_u128(2);
        let id = ObjectId::from_bytes([17; 32]);
        let expected = token(&scope, seal, id);
        assert_eq!(
            expected.to_string(),
            include_str!("../../tests/fixtures/seal-reservation-v1.txt").trim()
        );
        assert_ne!(
            expected,
            token(
                &Scope::new("another", scope.repository()).unwrap(),
                seal,
                id
            )
        );
        assert_ne!(
            expected,
            token(
                &Scope::new(scope.tenant(), Uuid::from_u128(3)).unwrap(),
                seal,
                id
            )
        );
        assert_ne!(expected, token(&scope, Uuid::from_u128(3), id));
        assert_ne!(
            expected,
            token(&scope, seal, ObjectId::from_bytes([18; 32]))
        );
    }
    #[test]
    fn declaration_rejects_ambiguous_membership_order_and_budget() {
        let changes = Changes::new(vec![]).unwrap();
        let operations = Object::ChangeOperations(changes.clone()).id().unwrap();
        assert!(
            SealPublication::new(
                [1; 32],
                changes.clone(),
                vec![operations],
                BTreeMap::new(),
                22
            )
            .is_ok()
        );
        for objects in [
            vec![],
            vec![operations, operations],
            vec![operations, ObjectId::from_bytes([1; 32])],
        ] {
            assert!(
                SealPublication::new([1; 32], changes.clone(), objects, BTreeMap::new(), 22)
                    .is_err()
            );
        }
        assert!(
            SealPublication::new(
                [1; 32],
                changes.clone(),
                vec![operations],
                BTreeMap::from([(operations, 2)]),
                22
            )
            .is_err()
        );
        assert!(
            SealPublication::new(
                [1; 32],
                changes.clone(),
                vec![operations],
                BTreeMap::from([(ObjectId::from_bytes([1; 32]), 5)]),
                22
            )
            .is_err()
        );
        assert!(
            SealPublication::new([1; 32], changes, vec![operations], BTreeMap::new(), 21).is_err()
        );
    }
}
