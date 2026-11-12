//! Prove the complete frozen native DAG against a proposed GitHub ref map.
use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Instant,
};
use treeharbor_core::{GitOid, Object, ObjectStore};
use treeharbor_git_bridge::{self as git, Limits};
use treeharbor_storage::ManagedStore;

const PROJECTION: &str = "native.handoff.projection.v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffProjectionSpec {
    pub version: u8,
    pub freeze_key: String,
    pub snapshot_sha256: String,
    pub refs: BTreeMap<String, String>,
}

impl HandoffProjectionSpec {
    pub(crate) fn validate(&self) -> Result<()> {
        identifier(&self.freeze_key, 128)?;
        if self.version != 1
            || self.refs.is_empty()
            || self.refs.len() > MAX_REF_SNAPSHOT
            || !matches!(self.snapshot_sha256.parse::<ObjectId>(), Ok(id) if id.to_string() == self.snapshot_sha256)
        {
            return Err(Error::Invalid("handoff projection"));
        }
        for (name, revision) in &self.refs {
            ref_name(
                name.strip_prefix("refs/heads/")
                    .ok_or(Error::Invalid("handoff Git ref"))?,
            )?;
            git_id(revision)?;
        }
        Ok(())
    }
}

pub(crate) async fn verified_projection(
    tx: &Transaction<'_>,
    scope: &Scope,
    epoch: i64,
    key: &str,
) -> Result<HandoffProjectionSpec> {
    identifier(key, 128)?;
    let receipt: Value = tx.query_opt(
        "SELECT result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind=$4",
        &[&scope.tenant(), &scope.repository(), &key, &PROJECTION],
    ).await?.ok_or(Error::NativeHandoffMismatch)?.get(0);
    if receipt["version"] != 1
        || receipt["state"] != "verified"
        || receipt["authority"] != "standalone"
        || receipt["epoch"] != epoch
        || receipt["tenant"] != scope.tenant()
        || receipt["repository"] != json!(scope.repository())
        || receipt["key"] != key
    {
        return Err(Error::NativeHandoffMismatch);
    }
    let spec: HandoffProjectionSpec = serde_json::from_value(receipt["specification"].clone())
        .map_err(|_| Error::SchemaMismatch)?;
    spec.validate()?;
    inputs(tx, scope, epoch, &spec).await?;
    Ok(spec)
}

fn git_id(revision: &str) -> Result<GitOid> {
    match revision.parse() {
        Ok(RevisionRef::Git(oid @ GitOid::Sha1(bytes)))
            if bytes != [0; 20] && RevisionRef::Git(oid).to_string() == revision =>
        {
            Ok(oid)
        }
        _ => Err(Error::Invalid("handoff Git revision")),
    }
}

#[derive(PartialEq, Eq)]
struct FrozenInputs {
    freeze: HandoffFreeze,
    backend: String,
    roots: BTreeMap<ObjectId, i64>,
}

async fn inputs(
    tx: &Transaction<'_>,
    scope: &Scope,
    epoch: i64,
    spec: &HandoffProjectionSpec,
) -> Result<FrozenInputs> {
    let freeze = current(tx, scope, epoch, &spec.freeze_key).await?;
    check_refs(tx, scope, &freeze).await?;
    if freeze.snapshot_sha256 != spec.snapshot_sha256
        || freeze.snapshot.refs.len() != spec.refs.len()
        || freeze
            .snapshot
            .refs
            .iter()
            .any(|tip| !spec.refs.contains_key(&format!("refs/heads/{}", tip.name)))
    {
        return Err(Error::NativeHandoffMismatch);
    }
    let rows = tx.query(r#"
        SELECT r.name,r.head,p.generation,o.backend
        FROM th_native_refs r JOIN th_object_pins p ON (p.tenant,p.repository,p.holder)=(r.tenant,r.repository,r.pin_holder)
        JOIN th_objects o ON (o.tenant,o.digest,o.generation)=(p.tenant,p.digest,p.generation)
        WHERE r.tenant=$1 AND r.repository=$2 AND r.epoch=$3 AND p.digest=r.head AND o.kind=5 AND o.state='live'
        ORDER BY r.name COLLATE "C"
        "#, &[&scope.tenant(), &scope.repository(), &epoch]).await?;
    if rows.len() != freeze.snapshot.refs.len() {
        return Err(Error::ObjectNotLive);
    }
    let backend: String = rows
        .first()
        .ok_or(Error::NativeHandoffMismatch)?
        .get("backend");
    let mut roots = BTreeMap::new();
    for (row, tip) in rows.into_iter().zip(&freeze.snapshot.refs) {
        let head = id(row.get("head"))?;
        let generation: i64 = row.get("generation");
        if row.get::<_, &str>("name") != tip.name
            || RevisionRef::Native(head).to_string() != tip.head
            || row.get::<_, &str>("backend") != backend
            || roots
                .insert(head, generation)
                .is_some_and(|old| old != generation)
        {
            return Err(Error::NativeHandoffMismatch);
        }
    }
    Ok(FrozenInputs {
        freeze,
        backend,
        roots,
    })
}

async fn closure(
    tx: &Transaction<'_>,
    scope: &Scope,
    input: &FrozenInputs,
    deadline: Instant,
) -> Result<BTreeMap<ObjectId, i64>> {
    let limits = Limits::default();
    let mut generations = input.roots.clone();
    let mut pending: BTreeSet<_> = generations.keys().copied().collect();
    let mut edges_left = limits.edges;
    while !pending.is_empty() {
        if Instant::now() >= deadline {
            return Err(Error::UploadLimit);
        }
        let ids: Vec<_> = pending.iter().take(128).copied().collect();
        let parents: Vec<_> = ids.iter().map(|id| id.as_bytes().to_vec()).collect();
        let epochs: Vec<_> = ids.iter().map(|id| generations[id]).collect();
        let rows = tx.query(r#"
            SELECT e.child,e.child_generation,o.backend,o.state
            FROM unnest($2::bytea[],$3::bigint[]) AS p(digest,generation)
            JOIN th_object_edges e ON e.tenant=$1 AND (e.parent,e.parent_generation)=(p.digest,p.generation)
            LEFT JOIN th_objects o ON (o.tenant,o.digest,o.generation)=(e.tenant,e.child,e.child_generation)
            LIMIT $4
            "#, &[&scope.tenant(), &parents, &epochs, &(edges_left as i64 + 1)]).await?;
        edges_left = edges_left
            .checked_sub(rows.len())
            .ok_or(Error::UploadLimit)?;
        for id in ids {
            pending.remove(&id);
        }
        for row in rows {
            let child = id(row.get("child"))?;
            let generation: i64 = row.get("child_generation");
            if row.get::<_, Option<&str>>("backend") != Some(input.backend.as_str())
                || row.get::<_, Option<&str>>("state") != Some("live")
            {
                return Err(Error::ObjectNotLive);
            }
            match generations.insert(child, generation) {
                None => {
                    pending.insert(child);
                }
                Some(previous) if previous != generation => {
                    return Err(Error::NativeHandoffMismatch);
                }
                _ => {}
            }
            if generations.len() > limits.objects {
                return Err(Error::UploadLimit);
            }
        }
    }
    Ok(generations)
}

struct FrozenStore<'a> {
    store: &'a ManagedStore,
    generations: &'a BTreeMap<ObjectId, i64>,
}
impl ObjectStore for FrozenStore<'_> {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        let generation = self
            .generations
            .get(&id)
            .ok_or(treeharbor_core::Error::MissingObject(id))?;
        self.store.get(*generation, id)
    }
    fn put(&self, _: &Object) -> treeharbor_core::Result<ObjectId> {
        Err(treeharbor_core::Error::InvalidObject(
            "read-only handoff proof",
        ))
    }
}

pub enum HandoffProjectionWork {
    Replay(Value),
    Verify(Box<FrozenProjection>),
}

pub struct FrozenProjection {
    input: FrozenInputs,
    spec: HandoffProjectionSpec,
    generations: BTreeMap<ObjectId, i64>,
}

pub struct VerifiedFrozenProjection {
    input: FrozenInputs,
    spec: HandoffProjectionSpec,
    objects: usize,
    encoded_bytes: u64,
}

impl FrozenProjection {
    pub fn verify(
        self: Box<Self>,
        store: &ManagedStore,
        scratch: &Path,
        deadline: Instant,
    ) -> Result<VerifiedFrozenProjection> {
        if store.backend() != self.input.backend
            || store.tenant() != self.input.freeze.snapshot.tenant
        {
            return Err(Error::NativeHandoffMismatch);
        }
        let store = FrozenStore {
            store,
            generations: &self.generations,
        };
        let mut refs = BTreeMap::new();
        for tip in &self.input.freeze.snapshot.refs {
            let RevisionRef::Native(head) = tip.head.parse()? else {
                return Err(Error::NativeHandoffMismatch);
            };
            let name = format!("refs/heads/{}", tip.name);
            let git = git_id(
                self.spec
                    .refs
                    .get(&name)
                    .ok_or(Error::NativeHandoffMismatch)?,
            )?;
            if !matches!(store.get(head)?, Object::NativeCommit(commit) if commit.snapshot.to_string()==tip.snapshot)
            {
                return Err(Error::NativeHandoffMismatch);
            }
            refs.insert(name, (head, git));
        }
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return Err(Error::UploadLimit);
        }
        let verified = git::verify_branch_mappings(
            &store,
            refs,
            scratch,
            Limits {
                timeout,
                ..Limits::default()
            },
        )
        .map_err(|error| match error {
            git::Error::Limit | git::Error::Source(treeharbor_core::Error::TooLarge) => {
                Error::UploadLimit
            }
            git::Error::Source(error) => Error::Objects(error),
            _ => Error::NativeHandoffMismatch,
        })?;
        if verified.source_objects() != self.generations.len() {
            return Err(Error::NativeHandoffMismatch);
        }
        Ok(VerifiedFrozenProjection {
            input: self.input,
            spec: self.spec,
            objects: verified.source_objects(),
            encoded_bytes: verified.source_bytes(),
        })
    }
}

fn projection_request(
    auth: &Authorization,
    command: &Command,
    spec: &HandoffProjectionSpec,
) -> Result<Request> {
    spec.validate()?;
    request(auth, command, PROJECTION, json!([1, spec]))
}

impl Metadata {
    pub async fn prepare_handoff_projection(
        &mut self,
        auth: &Authorization,
        command: &Command,
        spec: &HandoffProjectionSpec,
        deadline: Instant,
    ) -> Result<HandoffProjectionWork> {
        let scope = auth.scope();
        let input = {
            let mutation = match self
                .begin_frozen(projection_request(auth, command, spec)?)
                .await?
            {
                Begin::Replay(value) => return Ok(HandoffProjectionWork::Replay(value)),
                Begin::Fresh(mutation) => mutation,
            };
            let input = inputs(mutation.transaction(), scope, command.epoch, spec).await?;
            mutation.tx.commit().await?;
            input
        };

        // Bulk catalogue reads use the shared repo lock; no mutation lock is
        // retained while verifying physical frames and projecting Git objects.
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, scope, false).await?;
        auth.check_transaction(&tx, scope, auth.principal(), Permission::RepositoryWrite)
            .await?;
        if inputs(&tx, scope, command.epoch, spec).await? != input {
            return Err(Error::NativeHandoffMismatch);
        }
        let generations = closure(&tx, scope, &input, deadline).await?;
        tx.commit().await?;
        Ok(HandoffProjectionWork::Verify(Box::new(FrozenProjection {
            input,
            spec: spec.clone(),
            generations,
        })))
    }

    pub async fn record_handoff_projection(
        &mut self,
        auth: &Authorization,
        command: &Command,
        proof: VerifiedFrozenProjection,
    ) -> Result<Value> {
        let mutation = match self
            .begin_frozen(projection_request(auth, command, &proof.spec)?)
            .await?
        {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(mutation) => mutation,
        };
        let scope = auth.scope();
        if inputs(mutation.transaction(), scope, command.epoch, &proof.spec).await? != proof.input {
            return Err(Error::NativeHandoffMismatch);
        }
        mutation.complete(json!({"version":1,"state":"verified","authority":"standalone","epoch":command.epoch,
            "tenant":scope.tenant(),"repository":scope.repository(),"key":command.key,
            "specification":proof.spec,"object_count":proof.objects,"encoded_bytes":proof.encoded_bytes}), vec![]).await
    }
}
