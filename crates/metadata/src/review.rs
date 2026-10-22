//! Repository-read scoped immutable review inputs. Root locators are derived
//! from accepted versions, prepared candidates or registered revisions, never
//! supplied by the caller.
use crate::{
    Authority, Error, Metadata, Result,
    access::{Authorization, Permission, repository_lock},
    catalog::{lease_pin_in, settings},
    reads::{ReadBatch, ReadNode, ReadSession},
};
use serde_json::{Value, json};
use tokio_postgres::Transaction;
use treeharbor_core::{ObjectId, RevisionRef};
use treeharbor_storage::ManagedStore;
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub enum ReviewTarget {
    Change { changeset: Uuid, version: i64 },
    Revisions { before: ObjectId, after: ObjectId },
    Candidate { candidate: Uuid },
}
#[derive(Clone, Copy, Debug)]
pub struct ReviewFence {
    pub authority: Authority,
    pub epoch: i64,
    pub target: ReviewTarget,
}
#[derive(PartialEq, Eq)]
struct Input {
    identity: Value,
    snapshots: [ObjectId; 2],
    roots: [ObjectId; 2],
    holders: [String; 2],
}
pub struct ReviewRead {
    auth: Authorization,
    fence: ReviewFence,
    input: Input,
    sessions: Vec<ReadSession>,
}
impl ReviewRead {
    pub fn identity(&self) -> &Value {
        &self.input.identity
    }
    pub fn snapshots(&self) -> [ObjectId; 2] {
        self.input.snapshots
    }
    pub fn roots(&self) -> Vec<ReadNode> {
        self.sessions.iter().map(ReadSession::root).collect()
    }
}
fn id(raw: Vec<u8>) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        raw.try_into().map_err(|_| Error::SchemaMismatch)?,
    ))
}
async fn context(tx: &Transaction<'_>, auth: &Authorization, fence: ReviewFence) -> Result<Input> {
    let s = auth.scope();
    auth.check_transaction(tx, s, auth.principal(), Permission::Read)
        .await?;
    let repo = tx
        .query_one(
            "SELECT authority,epoch,state FROM th_repositories WHERE tenant=$1 AND repository=$2",
            &[&s.tenant(), &s.repository()],
        )
        .await?;
    if fence.epoch <= 0
        || repo.get::<_, String>(0) != fence.authority.as_str()
        || repo.get::<_, i64>(1) != fence.epoch
        || repo.get::<_, String>(2) != "active"
    {
        return Err(Error::Fenced);
    }
    match fence.target {
        ReviewTarget::Candidate { candidate } => candidate_input(tx, auth.scope(), candidate).await,
        ReviewTarget::Change { changeset, version } => {
            if version <= 0 {
                return Err(Error::Invalid("review version"));
            }
            let row=tx.query_opt("SELECT run,base_revision,base_snapshot,result_snapshot,base_pin,result_pin FROM th_change_versions WHERE tenant=$1 AND repository=$2 AND changeset=$3 AND version=$4",&[&s.tenant(),&s.repository(),&changeset,&version]).await?.ok_or(Error::NotFound)?;
            let revision: RevisionRef = row.get::<_, String>(1).parse()?;
            let RevisionRef::Native(base) = revision else {
                return Err(Error::Invalid("native review base"));
            };
            let before = id(row.get(2))?;
            let after = id(row.get(3))?;
            Ok(Input {
                identity: json!({"kind":"change","changeset":changeset.to_string(),"version":version,"run":row.get::<_,Uuid>(0).to_string(),"base_revision":revision.to_string()}),
                snapshots: [before, after],
                roots: [base, after],
                holders: [row.get(4), row.get(5)],
            })
        }
        ReviewTarget::Revisions { before, after } => {
            let mut snapshots = Vec::new();
            let mut holders = Vec::new();
            for revision in [before, after] {
                let row=tx.query_opt("SELECT snapshot,pin_holder FROM th_bases WHERE tenant=$1 AND repository=$2 AND revision=$3",&[&s.tenant(),&s.repository(),&RevisionRef::Native(revision).to_string()]).await?.ok_or(Error::NotFound)?;
                snapshots.push(id(row.get(0))?);
                holders.push(row.get(1));
            }
            Ok(Input {
                identity: json!({"kind":"revisions","before":RevisionRef::Native(before).to_string(),"after":RevisionRef::Native(after).to_string()}),
                snapshots: snapshots.try_into().map_err(|_| Error::SchemaMismatch)?,
                roots: [before, after],
                holders: holders.try_into().map_err(|_| Error::SchemaMismatch)?,
            })
        }
    }
}

async fn candidate_input(tx: &Transaction<'_>, s: &crate::Scope, candidate: Uuid) -> Result<Input> {
    let row = tx
        .query_opt(
            r"SELECT c.entry,c.expected_head,c.expected_snapshot,c.expected_generation,
                 c.epoch,c.policy,c.merge_profile,c.revision,c.snapshot,c.pin_holder,
                 q.target,q.changeset,q.version,v.run,v.base_revision,
                 b.pin_holder AS base_pin,b.snapshot AS registered_snapshot
          FROM th_native_candidates c
          JOIN th_native_queue q USING(tenant,repository,entry)
          JOIN th_change_versions v ON v.tenant=q.tenant AND v.repository=q.repository
               AND v.changeset=q.changeset AND v.version=q.version
          JOIN th_bases b ON b.tenant=c.tenant AND b.repository=c.repository
               AND b.revision='native:sha256:'||encode(c.expected_head,'hex')
          WHERE c.tenant=$1 AND c.repository=$2 AND c.candidate=$3",
            &[&s.tenant(), &s.repository(), &candidate],
        )
        .await?
        .ok_or(Error::NotFound)?;
    // Once populated, these three fields are immutable. A prepared stale,
    // cancelled or landed candidate is still a valid historical review input.
    // No mutable target/check/review state is a claim of admission here.
    let revision = id(row
        .get::<_, Option<Vec<u8>>>("revision")
        .ok_or(Error::IntegrationState)?)?;
    let after = id(row
        .get::<_, Option<Vec<u8>>>("snapshot")
        .ok_or(Error::IntegrationState)?)?;
    let pin: String = row
        .get::<_, Option<String>>("pin_holder")
        .ok_or(Error::IntegrationState)?;
    let before = id(row.get("expected_snapshot"))?;
    if before != id(row.get("registered_snapshot"))? {
        return Err(Error::ReadScope);
    }
    let head = id(row.get("expected_head"))?;
    Ok(Input {
        identity: json!({
            "kind": "candidate",
            "candidate": candidate.to_string(),
            "entry": row.get::<_, Uuid>("entry").to_string(),
            "target": row.get::<_, String>("target"),
            "changeset": row.get::<_, Uuid>("changeset").to_string(),
            "version": row.get::<_, i64>("version"),
            "run": row.get::<_, Uuid>("run").to_string(),
            "proposal_base_revision": row.get::<_, String>("base_revision"),
            "expected_head": RevisionRef::Native(head).to_string(),
            "expected_generation": row.get::<_, i64>("expected_generation"),
            "candidate_epoch": row.get::<_, i64>("epoch"),
            "revision": RevisionRef::Native(revision).to_string(),
            "policy": id(row.get("policy"))?.to_string(),
            "merge_profile": row.get::<_, String>("merge_profile"),
        }),
        snapshots: [before, after],
        roots: [head, revision],
        holders: [row.get("base_pin"), pin],
    })
}
impl Metadata {
    pub async fn open_review(
        &mut self,
        auth: &Authorization,
        fence: ReviewFence,
    ) -> Result<ReviewRead> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        let input = context(&tx, auth, fence).await?;
        tx.query("SELECT o.digest FROM th_objects o JOIN th_object_pins p ON p.tenant=o.tenant AND p.digest=o.digest WHERE p.tenant=$1 AND p.repository=$2 AND p.holder=ANY($3::text[]) ORDER BY o.digest FOR UPDATE OF o",&[&s.tenant(),&s.repository(),&input.holders.as_slice()]).await?;
        let mut sessions = Vec::new();
        for (i, holder) in input.holders.iter().enumerate() {
            let lease = lease_pin_in(&tx, s, holder, Uuid::new_v4(), 120).await?;
            if lease.key().id() != input.roots[i] {
                return Err(Error::ReadScope);
            }
            sessions.push(ReadSession::from_lease(lease));
        }
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(ReviewRead {
            auth: auth.clone(),
            fence,
            input,
            sessions,
        })
    }
    async fn check_review(&mut self, read: &ReviewRead) -> Result<()> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, read.auth.scope(), false).await?;
        if context(&tx, &read.auth, read.fence).await? != read.input {
            return Err(Error::ReadScope);
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn read_review(
        &mut self,
        read: &ReviewRead,
        source: usize,
        store: &ManagedStore,
        nodes: &[ReadNode],
        budget: u64,
    ) -> Result<ReadBatch> {
        self.check_review(read).await?;
        self.open_read_batch(
            read.sessions.get(source).ok_or(Error::ReadScope)?,
            store,
            nodes,
            budget,
        )
        .await
    }
    pub async fn finish_review(&mut self, read: ReviewRead) -> Result<()> {
        let mut error = self.check_review(&read).await.err();
        for session in read.sessions {
            if let Err(e) = self.close_read_session(session).await {
                error.get_or_insert(e);
            }
        }
        error.map_or(Ok(()), Err)
    }
}
