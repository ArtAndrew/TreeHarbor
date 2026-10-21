use super::*;
use crate::{
    catalog::{ObjectLease, lease_pin_in, reserve_in},
    reads::{ReadBatch, ReadNode, ReadSession},
};
use sha2::{Digest, Sha256};
use treeharbor_merge::{CandidateInput, PreparedCandidate};
use treeharbor_storage::{ManagedStore, PreparedObject};

pub(super) struct CandidateRecord {
    pub id: Uuid,
    pub entry: Uuid,
    pub target: String,
    pub owner: String,
    pub epoch: i64,
    pub generation: i64,
    pub policy: ObjectId,
    pub profile: String,
    pub state: String,
    pub input: CandidateInput,
    pub revision: Option<ObjectId>,
    pub snapshot: Option<ObjectId>,
    pub pin: Option<String>,
    pub unexpired: bool,
    pub failure: Option<Value>,
    pub source_pins: Vec<String>,
}
impl CandidateRecord {
    pub(super) fn validation(&self, s: &Scope) -> Result<treeharbor_validation::Candidate> {
        Ok(treeharbor_validation::Candidate {
            repository_scope: format!("{}/{}", s.tenant, s.repository),
            authority_epoch: self.epoch as u64,
            revision: RevisionRef::Native(self.revision.ok_or(Error::IntegrationState)?),
            snapshot: self.snapshot.ok_or(Error::IntegrationState)?,
            target_ref: self.target.clone(),
            expected_head: RevisionRef::Native(self.input.current_revision),
            expected_generation: self.generation as u64,
            policy_digest: *self.policy.as_bytes(),
        })
    }
    pub(super) fn json(&self) -> Value {
        json!({"candidate":self.id.to_string(),"entry":self.entry.to_string(),"target":self.target,"state":self.state,"expected_head":RevisionRef::Native(self.input.current_revision).to_string(),"expected_generation":self.generation,"epoch":self.epoch,"policy":self.policy.to_string(),"merge_profile":self.profile,"revision":self.revision.map(|r|RevisionRef::Native(r).to_string()),"snapshot":self.snapshot.map(|s|s.to_string()),"failure":self.failure})
    }
}
pub(super) async fn candidate(
    tx: &Transaction<'_>,
    s: &Scope,
    candidate: Uuid,
) -> Result<CandidateRecord> {
    let row=tx.query_opt(r#"SELECT c.*,q.target,q.principal AS owner,q.changeset,q.version,v.base_revision,v.base_snapshot,v.operations,v.result_snapshot,v.base_pin,v.operations_pin,v.result_pin,(c.expires_at>clock_timestamp()) AS unexpired
        FROM th_native_candidates c JOIN th_native_queue q USING(tenant,repository,entry)
        JOIN th_change_versions v ON v.tenant=q.tenant AND v.repository=q.repository AND v.changeset=q.changeset AND v.version=q.version
        WHERE c.tenant=$1 AND c.repository=$2 AND c.candidate=$3"#,&[&s.tenant,&s.repository,&candidate]).await?.ok_or(Error::NotFound)?;
    let RevisionRef::Native(base) = row.get::<_, String>("base_revision").parse()? else {
        return Err(Error::Invalid("native base required"));
    };
    let current = id(row.get("expected_head"))?;
    let pin = tx
        .query_one(
            "SELECT pin_holder FROM th_bases WHERE tenant=$1 AND repository=$2 AND revision=$3",
            &[
                &s.tenant,
                &s.repository,
                &RevisionRef::Native(current).to_string(),
            ],
        )
        .await?;
    Ok(CandidateRecord {
        id: candidate,
        entry: row.get("entry"),
        target: row.get("target"),
        owner: row.get("owner"),
        epoch: row.get("epoch"),
        generation: row.get("expected_generation"),
        policy: id(row.get("policy"))?,
        profile: row.get("merge_profile"),
        state: row.get("state"),
        revision: row
            .get::<_, Option<Vec<u8>>>("revision")
            .map(id)
            .transpose()?,
        snapshot: row
            .get::<_, Option<Vec<u8>>>("snapshot")
            .map(id)
            .transpose()?,
        pin: row.get("pin_holder"),
        unexpired: row.get("unexpired"),
        failure: row.get("failure"),
        input: CandidateInput {
            base_revision: base,
            base_snapshot: id(row.get("base_snapshot"))?,
            original_operations: id(row.get("operations"))?,
            proposed_snapshot: id(row.get("result_snapshot"))?,
            current_revision: current,
            current_snapshot: id(row.get("expected_snapshot"))?,
            timestamp_seconds: row.get("timestamp_seconds"),
            label: format!(
                "{}/{}/{}",
                row.get::<_, Uuid>("entry"),
                row.get::<_, Uuid>("changeset"),
                row.get::<_, i64>("version")
            ),
        },
        source_pins: vec![
            row.get("base_pin"),
            pin.get(0),
            row.get("operations_pin"),
            row.get("result_pin"),
        ],
    })
}
pub(super) async fn fresh(
    tx: &Transaction<'_>,
    s: &Scope,
    c: &CandidateRecord,
) -> Result<RefState> {
    let current = target(tx, s, &c.target).await?;
    let repo = tx
        .query_one(
            "SELECT authority,state,epoch FROM th_repositories WHERE tenant=$1 AND repository=$2",
            &[&s.tenant, &s.repository],
        )
        .await?;
    if repo.get::<_, String>(0) != "standalone"
        || repo.get::<_, String>(1) != "active"
        || repo.get::<_, i64>(2) != c.epoch
    {
        return Err(Error::Fenced);
    }
    if current.head != c.input.current_revision
        || current.snapshot != c.input.current_snapshot
        || current.generation != c.generation
        || current.epoch != c.epoch
        || current.policy != c.policy
    {
        return Err(Error::StaleCandidate);
    }
    Ok(current)
}
async fn preparing(
    tx: &Transaction<'_>,
    auth: &Authorization,
    id: Uuid,
) -> Result<CandidateRecord> {
    let s = auth.scope();
    repository_lock(tx, s, true).await?;
    auth.check_transaction(tx, s, auth.principal(), Permission::IntegrationControl)
        .await?;
    let c = candidate(tx, s, id).await?;
    if c.state != "preparing" || !c.unexpired || c.profile != treeharbor_merge::PROFILE {
        return Err(Error::IntegrationState);
    }
    fresh(tx, s, &c).await?;
    Ok(c)
}
pub struct CandidateRead {
    auth: Authorization,
    candidate: Uuid,
    input: CandidateInput,
    sessions: Vec<ReadSession>,
}
impl CandidateRead {
    pub fn input(&self) -> &CandidateInput {
        &self.input
    }
    pub fn roots(&self) -> Vec<ReadNode> {
        self.sessions.iter().map(ReadSession::root).collect()
    }
}
pub fn object_token(s: &Scope, candidate: Uuid, id: ObjectId) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"treeharbor-candidate-object\0v1\0");
    hash.update((s.tenant.len() as u64).to_be_bytes());
    hash.update(s.tenant.as_bytes());
    hash.update(s.repository.as_bytes());
    hash.update(candidate.as_bytes());
    hash.update(id.as_bytes());
    Uuid::from_bytes(hash.finalize()[..16].try_into().expect("digest length"))
}

impl Metadata {
    pub async fn native_candidate_status(
        &mut self,
        auth: &Authorization,
        id: Uuid,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        read_authorized(&tx, auth).await?;
        let result = candidate(&tx, auth.scope(), id).await?.json();
        read_authorized(&tx, auth).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn open_native_candidate(
        &mut self,
        auth: &Authorization,
        id: Uuid,
    ) -> Result<CandidateRead> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let c = preparing(&tx, auth, id).await?;
        let s = auth.scope();
        // Lock shared object roots in digest order before creating multiple holds.
        tx.query("SELECT o.digest FROM th_objects o JOIN th_object_pins p ON p.tenant=o.tenant AND p.digest=o.digest WHERE p.tenant=$1 AND p.repository=$2 AND p.holder=ANY($3::text[]) ORDER BY o.digest FOR UPDATE OF o", &[&s.tenant,&s.repository,&c.source_pins]).await?;
        let mut sessions = Vec::new();
        for pin in &c.source_pins {
            sessions.push(ReadSession::from_lease(
                lease_pin_in(&tx, s, pin, Uuid::new_v4(), 300).await?,
            ));
        }
        preparing(&tx, auth, id).await?;
        tx.commit().await?;
        Ok(CandidateRead {
            auth: auth.clone(),
            candidate: id,
            input: c.input,
            sessions,
        })
    }
    pub async fn read_native_candidate(
        &mut self,
        read: &CandidateRead,
        source: usize,
        store: &ManagedStore,
        nodes: &[ReadNode],
        budget: u64,
    ) -> Result<ReadBatch> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let c = preparing(&tx, &read.auth, read.candidate).await?;
        if c.input != read.input {
            return Err(Error::ReceiptMismatch);
        }
        tx.commit().await?;
        self.open_read_batch(
            read.sessions.get(source).ok_or(Error::ReadScope)?,
            store,
            nodes,
            budget,
        )
        .await
    }
    pub async fn close_native_candidate_read(&mut self, read: CandidateRead) -> Result<()> {
        for session in read.sessions {
            self.close_read_session(session).await?;
        }
        Ok(())
    }
    pub async fn reserve_native_candidate_object(
        &mut self,
        auth: &Authorization,
        candidate_id: Uuid,
        store: &ManagedStore,
        object: &PreparedObject,
    ) -> Result<ObjectLease> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        preparing(&tx, auth, candidate_id).await?;
        let s = auth.scope();
        let digest = object.id();
        let token = object_token(s, candidate_id, digest);
        if store.tenant() != s.tenant {
            return Err(Error::Forbidden);
        }
        let old=tx.query_opt("SELECT generation,hold_token,length,backend FROM th_candidate_objects WHERE tenant=$1 AND repository=$2 AND candidate=$3 AND digest=$4", &[&s.tenant,&s.repository,&candidate_id,&bytes(digest)]).await?;
        if old.is_none() {
            let size=tx.query_one("SELECT count(*)::bigint,coalesce(sum(length),0)::bigint FROM th_candidate_objects WHERE tenant=$1 AND repository=$2 AND candidate=$3", &[&s.tenant,&s.repository,&candidate_id]).await?;
            if size.get::<_, i64>(0) >= 4096
                || size.get::<_, i64>(1).saturating_add(object.length() as i64) > 128 * 1024 * 1024
            {
                return Err(Error::UploadLimit);
            }
        }
        let lease = reserve_in(&tx, store, object, token, 300).await?;
        if let Some(old) = old {
            if old.get::<_, i64>(0) != lease.key().generation()
                || old.get::<_, Uuid>(1) != token
                || old.get::<_, i64>(2) != object.length() as i64
                || old.get::<_, String>(3) != store.backend()
            {
                return Err(Error::ReceiptMismatch);
            }
        } else {
            tx.execute("INSERT INTO th_candidate_objects(tenant,repository,candidate,digest,generation,hold_token,length,backend) VALUES($1,$2,$3,$4,$5,$6,$7,$8)", &[&s.tenant,&s.repository,&candidate_id,&bytes(digest),&lease.key().generation(),&token,&(object.length() as i64),&store.backend()]).await?;
        }
        tx.execute("UPDATE th_object_holds h SET expires_at=c.expires_at FROM th_native_candidates c WHERE c.tenant=$1 AND c.repository=$2 AND c.candidate=$3 AND h.tenant=c.tenant AND h.token=$4", &[&s.tenant,&s.repository,&candidate_id,&token]).await?;
        preparing(&tx, auth, candidate_id).await?;
        tx.commit().await?;
        Ok(lease)
    }
    /// Receipt lookup precedes state/epoch checks so response loss can be retried.
    pub async fn native_preparation_replay(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        candidate_id: Uuid,
    ) -> Result<Option<Value>> {
        match self
            .begin(prepare_request(auth, cmd, candidate_id)?)
            .await?
        {
            Begin::Replay(value) => Ok(Some(value)),
            Begin::Fresh(tx) => {
                preparing(&tx.tx, auth, candidate_id).await?;
                tx.tx.rollback().await?;
                Ok(None)
            }
        }
    }
    pub async fn finish_native_preparation_failure(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        candidate_id: Uuid,
        reason: &str,
        conflict: bool,
        details: Value,
    ) -> Result<Value> {
        crate::identifier(reason, 128)?;
        if serde_json::to_vec(&details)
            .map_err(|_| Error::Invalid("native conflict details"))?
            .len()
            > 32768
        {
            return Err(Error::Invalid("native conflict details"));
        }
        let tx = match self
            .begin(prepare_request(auth, cmd, candidate_id)?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let c = preparing(&tx.tx, auth, candidate_id).await?;
        let s = auth.scope();
        let state = if conflict { "conflicted" } else { "failed" };
        let failure = json!({"reason":reason,"details":details});
        tx.tx.execute("UPDATE th_native_candidates SET state=$4,failure=$5 WHERE tenant=$1 AND repository=$2 AND candidate=$3", &[&s.tenant,&s.repository,&candidate_id,&state,&failure]).await?;
        tx.tx.execute("UPDATE th_native_queue SET state=$4,reason=$5 WHERE tenant=$1 AND repository=$2 AND entry=$3", &[&s.tenant,&s.repository,&c.entry,&state,&reason]).await?;
        let result = candidate(&tx.tx, s, candidate_id).await?.json();
        tx.complete(result, vec![]).await
    }
    pub async fn finish_native_candidate(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        candidate_id: Uuid,
        computed: &PreparedCandidate,
        lease: &ObjectLease,
    ) -> Result<Value> {
        let tx = match self
            .begin(prepare_request(auth, cmd, candidate_id)?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let c = preparing(&tx.tx, auth, candidate_id).await?;
        if &c.input != computed.input()
            || c.profile != computed.profile()
            || lease.key().id() != computed.revision()
        {
            return Err(Error::ReceiptMismatch);
        }
        let s = auth.scope();
        let pin = format!("native-candidate:{candidate_id}");
        tx.pin_kind(&pin, lease, 5).await?;
        tx.register_base(computed.revision(), computed.commit().snapshot, lease)
            .await?;
        tx.tx.execute("UPDATE th_native_candidates SET state='checking',revision=$4,snapshot=$5,pin_holder=$6 WHERE tenant=$1 AND repository=$2 AND candidate=$3", &[&s.tenant,&s.repository,&candidate_id,&bytes(computed.revision()),&bytes(computed.commit().snapshot),&pin]).await?;
        tx.tx.execute("UPDATE th_native_queue SET state='checking' WHERE tenant=$1 AND repository=$2 AND entry=$3", &[&s.tenant,&s.repository,&c.entry]).await?;
        let result = candidate(&tx.tx, s, candidate_id).await?.json();
        tx.complete(
            result,
            vec![Event {
                kind: "native.candidate.checks".into(),
                payload: json!({"candidate":candidate_id.to_string(),"epoch":c.epoch}),
            }],
        )
        .await
    }
    pub async fn fail_native_candidate(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        candidate_id: Uuid,
        reason: &str,
        conflict: bool,
    ) -> Result<Value> {
        crate::identifier(reason, 128)?;
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.candidate.fail.v1",
                json!([1, candidate_id.to_string(), reason, conflict]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let c = candidate(&tx.tx, s, candidate_id).await?;
        if c.state != "preparing" {
            return Err(Error::IntegrationState);
        }
        let state = if conflict { "conflicted" } else { "failed" };
        tx.tx.execute("UPDATE th_native_candidates SET state=$4 WHERE tenant=$1 AND repository=$2 AND candidate=$3", &[&s.tenant,&s.repository,&candidate_id,&state]).await?;
        tx.tx.execute("UPDATE th_native_queue SET state=$4,reason=$5 WHERE tenant=$1 AND repository=$2 AND entry=$3", &[&s.tenant,&s.repository,&c.entry,&state,&reason]).await?;
        let result = candidate(&tx.tx, s, candidate_id).await?.json();
        tx.complete(result, vec![]).await
    }
}

fn prepare_request(auth: &Authorization, cmd: &Command, id: Uuid) -> Result<Request> {
    request(
        auth,
        cmd,
        "native.candidate.prepare.v1",
        json!([1, id.to_string(), treeharbor_merge::PROFILE]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservation_identity_matches_independent_vector_and_is_scope_bound() {
        let v: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/native-integration-v1.json"
        ))
        .unwrap();
        let scope = Scope::new(
            v["tenant"].as_str().unwrap(),
            v["repository"].as_str().unwrap().parse().unwrap(),
        )
        .unwrap();
        let candidate = v["candidate"].as_str().unwrap().parse().unwrap();
        let object = v["object"].as_str().unwrap().parse().unwrap();
        let token = object_token(&scope, candidate, object);
        assert_eq!(token.to_string(), v["hold_token"]);
        let other = Scope::new("other", scope.repository()).unwrap();
        assert_ne!(token, object_token(&other, candidate, object));
        assert_ne!(token, object_token(&scope, Uuid::nil(), object));
    }
}
