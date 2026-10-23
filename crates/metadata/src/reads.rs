//! Incremental local graph reads. These are trusted service primitives, not an
//! authentication boundary or a remotely serializable object-capability protocol.
use crate::catalog::{ObjectKey, ObjectLease, live_lease, live_read_lease, settings};
use crate::{Error, Metadata, Result, Scope, ttl_valid};
use std::collections::BTreeSet;
use treeharbor_core::{Object, ObjectId};
use treeharbor_storage::{ManagedStore, OpenedObject};
use uuid::Uuid;

pub const MAX_READ_OBJECTS: usize = 256;
pub const MAX_READ_BYTES: u64 = 128 * 1024 * 1024;

/// Retains the root between batches. Explicitly close it; process loss leaves only
/// an expiring hold. Expired sessions cannot renew or open further objects.
#[derive(Debug)]
pub struct ReadSession {
    lease: ObjectLease,
}
impl ReadSession {
    pub(crate) fn from_lease(lease: ObjectLease) -> Self {
        Self { lease }
    }
    pub fn root(&self) -> ReadNode {
        ReadNode {
            anchor: self.lease.key().clone(),
            token: self.lease.token(),
            parent: None,
            id: self.lease.key().id(),
            kind: None,
        }
    }
}

/// Minted from a pinned root or the verified bytes of a previous read. A bare
/// digest supplied by an agent cannot become a node; nodes cannot cross sessions.
#[derive(Clone, Debug)]
pub struct ReadNode {
    anchor: ObjectKey,
    token: Uuid,
    parent: Option<ObjectKey>,
    id: ObjectId,
    kind: Option<u8>,
}
impl ReadNode {
    pub fn id(&self) -> ObjectId {
        self.id
    }
}

#[derive(Debug)]
pub struct ReadObject {
    object: Object,
    encoded_length: u64,
    key: ObjectKey,
    anchor: ObjectKey,
    token: Uuid,
}
impl ReadObject {
    /// In-process proof minted only by a verified read of this exact session.
    /// This cannot be reconstructed from a caller-supplied digest or JSON.
    pub(crate) fn scoped_key(&self, session: &ReadSession) -> Result<&ObjectKey> {
        if self.anchor != *session.lease.key() || self.token != session.lease.token() {
            return Err(Error::ReadScope);
        }
        Ok(&self.key)
    }
    pub fn encoded_length(&self) -> u64 {
        self.encoded_length
    }
    pub fn object(&self) -> &Object {
        &self.object
    }
    pub fn id(&self) -> ObjectId {
        self.key.id()
    }
    pub fn into_object(self) -> Object {
        self.object
    }
    /// One frontier only. Reading a commit does not read all its parents or trees.
    pub fn children(&self) -> impl Iterator<Item = ReadNode> + '_ {
        self.object
            .references()
            .into_iter()
            .map(|(id, kind)| ReadNode {
                anchor: self.anchor.clone(),
                token: self.token,
                parent: Some(self.key.clone()),
                id,
                kind: Some(kind),
            })
    }
}

impl ReadSession {
    pub(crate) async fn check(&self, tx: &tokio_postgres::Transaction<'_>) -> Result<()> {
        live_read_lease(tx, &self.lease).await
    }
}

#[derive(Debug)]
struct PendingRead {
    file: OpenedObject,
    key: ObjectKey,
    kind: u8,
}

/// Owns at most MAX_READ_OBJECTS descriptors and MAX_READ_BYTES encoded bytes.
/// Dropping the batch closes them; no persistent pins or background task leak.
#[derive(Debug)]
pub struct ReadBatch {
    objects: Vec<PendingRead>,
    anchor: ObjectKey,
    token: Uuid,
}
impl ReadBatch {
    pub fn len(&self) -> usize {
        self.objects.len()
    }
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
    /// Blocking verification, intended for a bounded coordinator IO pool. No bytes
    /// or derived nodes escape until every requested object passes verification.
    pub fn read(self) -> Result<Vec<ReadObject>> {
        self.objects
            .into_iter()
            .map(|pending| {
                let encoded_length = pending.file.length();
                let object = pending.file.read()?;
                if object.kind() != pending.kind {
                    return Err(Error::ReceiptMismatch);
                }
                Ok(ReadObject {
                    object,
                    encoded_length,
                    key: pending.key,
                    anchor: self.anchor.clone(),
                    token: self.token,
                })
            })
            .collect()
    }
}

impl Metadata {
    /// Caller authenticates repository/root access before opening the session.
    /// Long operations must also enforce revocation at the external service layer.
    pub async fn open_read_session(
        &mut self,
        scope: &Scope,
        holder: &str,
        ttl: i32,
    ) -> Result<ReadSession> {
        let lease = self
            .lease_pinned_root(scope, holder, Uuid::new_v4(), ttl)
            .await?;
        Ok(ReadSession { lease })
    }

    pub async fn renew_read_session(&mut self, session: &ReadSession, ttl: i32) -> Result<()> {
        ttl_valid(ttl)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        live_lease(&tx, &session.lease).await?;
        tx.execute("UPDATE th_object_holds SET expires_at=clock_timestamp()+($3::integer*interval '1 second') WHERE tenant=$1 AND token=$2", &[&session.lease.key().tenant(),&session.lease.token(),&ttl]).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn close_read_session(&mut self, session: ReadSession) -> Result<()> {
        self.release_object_hold(&session.lease).await
    }

    /// Resolve one bounded frontier in one catalog query, then open descriptors
    /// while the root row is locked against retirement. No whole-DAG scan and no
    /// SQL transaction across reading/hashing file contents. Currently local only;
    /// an S3 implementation must supply its own equivalent protected-read contract.
    pub async fn open_read_batch(
        &mut self,
        session: &ReadSession,
        store: &ManagedStore,
        nodes: &[ReadNode],
        byte_limit: u64,
    ) -> Result<ReadBatch> {
        self.open_read_batch_inner(session, store, nodes, byte_limit, false)
            .await
    }

    /// Opens a nonempty prefix fitting the byte budget. `batch.len()` advances
    /// the caller's frontier without repeatedly querying oversized combinations.
    pub async fn open_read_prefix(
        &mut self,
        session: &ReadSession,
        store: &ManagedStore,
        nodes: &[ReadNode],
        byte_limit: u64,
    ) -> Result<ReadBatch> {
        self.open_read_batch_inner(session, store, nodes, byte_limit, true)
            .await
    }

    async fn open_read_batch_inner(
        &mut self,
        session: &ReadSession,
        store: &ManagedStore,
        nodes: &[ReadNode],
        byte_limit: u64,
        prefix: bool,
    ) -> Result<ReadBatch> {
        if nodes.is_empty()
            || nodes.len() > MAX_READ_OBJECTS
            || byte_limit == 0
            || byte_limit > MAX_READ_BYTES
        {
            return Err(Error::ReadLimit);
        }
        let anchor = session.lease.key();
        if anchor.tenant() != store.tenant() {
            return Err(Error::ReadScope);
        }
        let mut seen = BTreeSet::new();
        for node in nodes {
            if node.token != session.lease.token() || node.anchor != *anchor {
                return Err(Error::ReadScope);
            }
            if !seen.insert(node.id) {
                return Err(Error::Invalid("duplicate read node"));
            }
        }
        let ids: Vec<Vec<u8>> = nodes.iter().map(|n| n.id.as_bytes().to_vec()).collect();
        let parents: Vec<Option<Vec<u8>>> = nodes
            .iter()
            .map(|n| n.parent.as_ref().map(|p| p.id().as_bytes().to_vec()))
            .collect();
        let generations: Vec<Option<i64>> = nodes
            .iter()
            .map(|n| n.parent.as_ref().map(ObjectKey::generation))
            .collect();
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        // Filesystem open/stat may stall. A terminated transaction never returns
        // a batch: the commit below must succeed before descriptors escape.
        tx.batch_execute("SET LOCAL idle_in_transaction_session_timeout='5s'")
            .await?;
        live_read_lease(&tx, &session.lease).await?;
        let rows = tx.query("SELECT o.generation,o.backend,o.kind,o.length,o.state, \
            (r.parent IS NULL OR EXISTS(SELECT 1 FROM th_object_edges e WHERE e.tenant=o.tenant AND e.parent=r.parent AND e.parent_generation=r.parent_generation AND e.child=o.digest AND e.child_generation=o.generation)) \
            FROM unnest($2::bytea[],$3::bytea[],$4::bigint[]) WITH ORDINALITY AS r(digest,parent,parent_generation,ordinal) \
            JOIN th_objects o ON o.tenant=$1 AND o.digest=r.digest ORDER BY r.ordinal",
            &[&anchor.tenant(),&ids,&parents,&generations]).await?;
        if rows.len() != nodes.len() {
            return Err(Error::ObjectNotLive);
        }
        let mut descriptors = Vec::with_capacity(rows.len());
        let mut total = 0u64;
        for (row, node) in rows.into_iter().zip(nodes) {
            let generation: i64 = row.get(0);
            let backend: String = row.get(1);
            let kind: i16 = row.get(2);
            let length: i64 = row.get(3);
            if row.get::<_, String>(4) != "live"
                || node
                    .kind
                    .is_some_and(|expected| i16::from(expected) != kind)
            {
                return Err(Error::ObjectNotLive);
            }
            if backend != store.backend() {
                return Err(Error::ReceiptMismatch);
            }
            if !row.get::<_, bool>(5)
                || (node.parent.is_none()
                    && (node.id != anchor.id() || generation != anchor.generation()))
            {
                return Err(Error::ReadScope);
            }
            let length = u64::try_from(length).map_err(|_| Error::SchemaMismatch)?;
            total = total.checked_add(length).ok_or(Error::ReadLimit)?;
            if total > byte_limit {
                if prefix && !descriptors.is_empty() {
                    break;
                }
                return Err(Error::ReadLimit);
            }
            descriptors.push((
                ObjectKey {
                    tenant: anchor.tenant().into(),
                    backend,
                    id: node.id,
                    generation,
                },
                kind as u8,
                length,
            ));
        }
        let mut objects = Vec::with_capacity(descriptors.len());
        for (key, kind, length) in descriptors {
            let file = store.open_object(key.generation(), key.id())?;
            if file.length() != length {
                return Err(Error::ReceiptMismatch);
            }
            objects.push(PendingRead { file, key, kind });
        }
        tx.commit().await?;
        Ok(ReadBatch {
            objects,
            anchor: anchor.clone(),
            token: session.lease.token(),
        })
    }
}
