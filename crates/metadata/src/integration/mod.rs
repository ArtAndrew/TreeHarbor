//! Durable native integration. Source computation happens outside transactions;
//! exact evidence and current authority are rechecked before any ref publication.
mod candidates;
mod evidence;
mod execution;
mod policy;
mod program;
pub(crate) use execution::execution_fence;
pub use program::CheckProgram;
mod queue;
mod wire;
pub use candidates::CandidateRead;
pub use policy::{CheckDefinition, Policy};
pub use wire::WireCommand;

use crate::{
    Authority, Begin, Error, Event, Metadata, Mutation, Request, Result, Scope,
    access::{Authorization, Permission, repository_lock},
    catalog::settings,
};
use serde_json::{Value, json};
use tokio_postgres::Transaction;
use treeharbor_core::{ObjectId, RevisionRef};
use uuid::Uuid;

/// Called only after accepting an owned new version under the repository write
/// lock, in the same transaction. Preserve published history and evidence.
pub(crate) async fn supersede_older_versions(
    tx: &Transaction<'_>,
    scope: &Scope,
    changeset: Uuid,
    version: i64,
    reason: &str,
) -> Result<()> {
    tx.execute(
        r"UPDATE th_native_candidates c SET state='stale' FROM th_native_queue q
          WHERE c.tenant=$1 AND c.repository=$2 AND q.tenant=c.tenant
            AND q.repository=c.repository AND q.entry=c.entry
            AND q.changeset=$3 AND q.version<$4 AND c.state IN ('preparing','checking')",
        &[&scope.tenant(), &scope.repository(), &changeset, &version],
    )
    .await?;
    tx.execute(
        r"UPDATE th_native_queue SET state='stale',reason=$5
          WHERE tenant=$1 AND repository=$2 AND changeset=$3 AND version<$4
            AND state IN ('queued','preparing','checking')",
        &[
            &scope.tenant(),
            &scope.repository(),
            &changeset,
            &version,
            &reason,
        ],
    )
    .await?;
    Ok(())
}

pub struct Command {
    pub key: String,
    pub epoch: i64,
}
fn request(auth: &Authorization, cmd: &Command, kind: &str, data: Value) -> Result<Request> {
    auth.context(cmd.key.clone(), Authority::Standalone, cmd.epoch)
        .request(kind, data)
}
fn bytes(id: ObjectId) -> Vec<u8> {
    id.as_bytes().to_vec()
}
fn id(bytes: Vec<u8>) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        bytes.try_into().map_err(|_| Error::SchemaMismatch)?,
    ))
}
fn ref_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name.split('/').any(|part| {
            part.is_empty()
                || part.len() > 64
                || !part.as_bytes()[0].is_ascii_alphanumeric()
                || part.ends_with('.')
                || part.ends_with(".lock")
                || part.contains("..")
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
    {
        return Err(Error::Invalid("native ref name"));
    }
    Ok(())
}
async fn load_policy(tx: &Transaction<'_>, s: &Scope, digest: &[u8]) -> Result<Policy> {
    let row = tx.query_opt("SELECT definition FROM th_native_policies WHERE tenant=$1 AND repository=$2 AND digest=$3", &[&s.tenant,&s.repository,&digest]).await?.ok_or(Error::NotFound)?;
    Policy::decode(digest, &row.get::<_, Vec<u8>>(0))
}

#[derive(Clone, Debug)]
pub struct RefState {
    pub name: String,
    pub head: ObjectId,
    pub snapshot: ObjectId,
    pub generation: i64,
    pub epoch: i64,
    pub policy: ObjectId,
}
impl RefState {
    fn json(&self) -> Value {
        json!({"name":self.name,"head":RevisionRef::Native(self.head).to_string(),"snapshot":self.snapshot.to_string(),"generation":self.generation,"epoch":self.epoch,"policy":self.policy.to_string()})
    }
}
async fn target(tx: &Transaction<'_>, s: &Scope, name: &str) -> Result<RefState> {
    let row=tx.query_opt("SELECT head,generation,epoch,policy,pin_holder FROM th_native_refs WHERE tenant=$1 AND repository=$2 AND name=$3", &[&s.tenant,&s.repository,&name]).await?.ok_or(Error::NotFound)?;
    let head = id(row.get(0))?;
    let base = tx
        .query_opt(
            "SELECT snapshot FROM th_bases WHERE tenant=$1 AND repository=$2 AND revision=$3",
            &[
                &s.tenant,
                &s.repository,
                &RevisionRef::Native(head).to_string(),
            ],
        )
        .await?
        .ok_or(Error::SchemaMismatch)?;
    Ok(RefState {
        name: name.into(),
        head,
        snapshot: id(base.get(0))?,
        generation: row.get(1),
        epoch: row.get(2),
        policy: id(row.get(3))?,
    })
}
async fn read_authorized(tx: &Transaction<'_>, auth: &Authorization) -> Result<()> {
    repository_lock(tx, auth.scope(), false).await?;
    auth.check_transaction(tx, auth.scope(), auth.principal(), Permission::Read)
        .await
}

impl Metadata {
    pub async fn put_native_policy(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        policy: &Policy,
    ) -> Result<Value> {
        let definition = policy.canonical()?;
        let digest = policy.digest()?;
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.policy.put.v1",
                json!([1, ObjectId::from_bytes(digest).to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        tx.tx.execute("INSERT INTO th_native_policies(tenant,repository,digest,definition,principal) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING", &[&s.tenant,&s.repository,&digest.as_slice(),&definition,&auth.principal()]).await?;
        load_policy(&tx.tx, s, &digest).await?;
        tx.complete(
            json!({"policy":ObjectId::from_bytes(digest).to_string()}),
            vec![],
        )
        .await
    }

    pub async fn create_native_ref(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        name: &str,
        head: ObjectId,
        policy: ObjectId,
    ) -> Result<Value> {
        ref_name(name)?;
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.ref.create.v1",
                json!([1, name, head.to_string(), policy.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        load_policy(&tx.tx, s, policy.as_bytes()).await?;
        let base = tx
            .tx
            .query_opt(
                "SELECT pin_holder FROM th_bases WHERE tenant=$1 AND repository=$2 AND revision=$3",
                &[
                    &s.tenant,
                    &s.repository,
                    &RevisionRef::Native(head).to_string(),
                ],
            )
            .await?
            .ok_or(Error::NotFound)?;
        let exists:bool=tx.tx.query_one("SELECT EXISTS(SELECT 1 FROM th_native_refs WHERE tenant=$1 AND repository=$2 AND name=$3)", &[&s.tenant,&s.repository,&name]).await?.get(0);
        if exists {
            return Err(Error::IntegrationState);
        }
        let pin = format!("native-ref:{}", ObjectId::hash_frame(name.as_bytes()).hex());
        // The registered commit pin retains every parent and tree. This pin is
        // permanent; a later ref update cannot erase published native history.
        tx.copy_root_pin(&base.get::<_, String>(0), &pin).await?;
        tx.tx.execute("INSERT INTO th_native_refs(tenant,repository,name,head,generation,epoch,policy,pin_holder) VALUES($1,$2,$3,$4,1,$5,$6,$7)", &[&s.tenant,&s.repository,&name,&bytes(head),&cmd.epoch,&bytes(policy),&pin]).await?;
        tx.ref_update(name, 1, None, head, policy, None, "create")
            .await?;
        let result = target(&tx.tx, s, name).await?.json();
        tx.complete(result, vec![]).await
    }

    pub async fn change_native_policy(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        name: &str,
        expected_generation: i64,
        policy: ObjectId,
    ) -> Result<Value> {
        ref_name(name)?;
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.ref.policy.v1",
                json!([1, name, expected_generation, policy.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        load_policy(&tx.tx, s, policy.as_bytes()).await?;
        let current = target(&tx.tx, s, name).await?;
        if current.generation != expected_generation || current.epoch != cmd.epoch {
            return Err(Error::StaleCandidate);
        }
        let generation = current
            .generation
            .checked_add(1)
            .ok_or(Error::Invalid("ref generation"))?;
        tx.tx.execute("UPDATE th_native_refs SET generation=$4,policy=$5 WHERE tenant=$1 AND repository=$2 AND name=$3", &[&s.tenant,&s.repository,&name,&generation,&bytes(policy)]).await?;
        tx.tx.execute("UPDATE th_native_candidates c SET state='stale' FROM th_native_queue q WHERE c.tenant=$1 AND c.repository=$2 AND q.tenant=c.tenant AND q.repository=c.repository AND q.entry=c.entry AND q.target=$3 AND c.state IN ('preparing','checking')", &[&s.tenant,&s.repository,&name]).await?;
        tx.tx.execute("UPDATE th_native_queue SET state='stale',reason='policy_changed' WHERE tenant=$1 AND repository=$2 AND target=$3 AND state IN ('preparing','checking')", &[&s.tenant,&s.repository,&name]).await?;
        tx.ref_update(
            name,
            generation,
            Some(current.head),
            current.head,
            policy,
            None,
            "policy",
        )
        .await?;
        let result = target(&tx.tx, s, name).await?.json();
        tx.complete(result, vec![]).await
    }

    pub async fn resolve_native_ref(&mut self, auth: &Authorization, name: &str) -> Result<Value> {
        ref_name(name)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        read_authorized(&tx, auth).await?;
        let result = target(&tx, auth.scope(), name).await?.json();
        read_authorized(&tx, auth).await?;
        tx.commit().await?;
        Ok(result)
    }
}

impl Mutation<'_> {
    #[allow(clippy::too_many_arguments)]
    async fn ref_update(
        &self,
        name: &str,
        generation: i64,
        old: Option<ObjectId>,
        new: ObjectId,
        policy: ObjectId,
        candidate: Option<Uuid>,
        kind: &str,
    ) -> Result<()> {
        let r = &self.request;
        self.tx.execute("INSERT INTO th_native_ref_updates(tenant,repository,target,generation,old_head,new_head,policy,epoch,candidate,principal,operation_key,kind) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)", &[&r.scope.tenant,&r.scope.repository,&name,&generation,&old.map(bytes),&bytes(new),&bytes(policy),&r.epoch,&candidate,&r.principal,&r.key,&kind]).await?;
        Ok(())
    }
}
