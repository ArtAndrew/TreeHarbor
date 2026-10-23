//! Owner acceptance of immutable ready capture. Only the trusted service computes
//! and publishes the result; remote callers supply no operations or result hash.
use super::*;
use crate::{
    Authority, Request,
    catalog::{ObjectLease, lease_pin_in, live_lease, reserve_in},
    intake::ChangeSubmission,
    proposals::{RunSource, run_source_in},
    reads::{ReadBatch, ReadNode, ReadSession},
};
use treeharbor_core::{AppliedChange, ObjectId, RevisionRef};
use treeharbor_storage::{ManagedStore, PreparedObject};

#[derive(Clone)]
pub struct SealAcceptCommand {
    pub key: String,
    pub authority: Authority,
    pub epoch: i64,
    pub seal: Uuid,
    pub changeset: Uuid,
    pub previous_version: i64,
    pub ttl_seconds: i32,
}
impl SealAcceptCommand {
    fn request(&self, auth: &Authorization) -> Result<Request> {
        ttl_valid(self.ttl_seconds)?;
        if self.seal.is_nil()
            || self.changeset.is_nil()
            || self.previous_version < 0
            || self.previous_version == i64::MAX
        {
            return Err(Error::Invalid("seal acceptance identity"));
        }
        auth.context(self.key.clone(), self.authority, self.epoch)
            .request(
                "workspace.seal.accept.v1",
                json!([
                    1,
                    self.seal.to_string(),
                    self.changeset.to_string(),
                    self.previous_version,
                    self.ttl_seconds,
                    "native-change-v1"
                ]),
            )
    }
}
pub enum SealAcceptStart {
    Replay(Value),
    Ready(Box<SealAcceptRead>),
}
pub struct SealAcceptRead {
    auth: Authorization,
    command: SealAcceptCommand,
    source: RunSource,
    operations: ObjectId,
    sessions: Vec<ReadSession>,
}
impl SealAcceptRead {
    /// Source zero is the original commit; source one is the sealed operations.
    pub fn root(&self, source: usize) -> Result<ReadNode> {
        Ok(self.sessions.get(source).ok_or(Error::ReadScope)?.root())
    }
    pub fn snapshot(&self) -> ObjectId {
        self.source.snapshot
    }
    pub fn operations(&self) -> ObjectId {
        self.operations
    }
}
fn bytes(id: ObjectId) -> Vec<u8> {
    id.as_bytes().to_vec()
}
fn object(raw: Vec<u8>) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        raw.try_into().map_err(|_| Error::SchemaMismatch)?,
    ))
}
async fn owned_ready(
    tx: &Transaction<'_>,
    auth: &Authorization,
    cmd: &SealAcceptCommand,
) -> Result<(Row, RunSource)> {
    let s = auth.scope();
    repository_lock(tx, s, true).await?;
    auth.check_transaction(tx, s, auth.principal(), Permission::ChangeWrite)
        .await?;
    let repo = tx
        .query_one(
            "SELECT authority,epoch,state FROM th_repositories WHERE tenant=$1 AND repository=$2",
            &[&s.tenant(), &s.repository()],
        )
        .await?;
    if repo.get::<_, String>(0) != cmd.authority.as_str()
        || repo.get::<_, i64>(1) != cmd.epoch
        || repo.get::<_, String>(2) != "active"
    {
        return Err(Error::Fenced);
    }
    let r = record(tx, s, cmd.seal).await?;
    if r.get::<_, String>("principal") != auth.principal() {
        return Err(Error::NotFound);
    }
    if r.get::<_, String>("state") != "ready" {
        return Err(Error::ExecutionState);
    }
    if r.get::<_, i64>("epoch") != cmd.epoch
        || r.get::<_, String>("authority") != cmd.authority.as_str()
    {
        return Err(Error::Fenced);
    }
    let source = run_source_in(
        tx,
        s,
        auth.principal(),
        r.get("run"),
        r.get("workspace_generation"),
        cmd.epoch,
    )
    .await?;
    Ok((r, source))
}
async fn active(
    tx: &Transaction<'_>,
    auth: &Authorization,
    cmd: &SealAcceptCommand,
) -> Result<Row> {
    let (seal, _) = owned_ready(tx, auth, cmd).await?;
    let s = auth.scope();
    let r=tx.query_opt("SELECT *,expires_at>clock_timestamp() AS unexpired FROM th_seal_acceptances WHERE tenant=$1 AND repository=$2 AND operation_key=$3",&[&s.tenant(),&s.repository(),&cmd.key]).await?.ok_or(Error::NotFound)?;
    let request = cmd.request(auth)?;
    if r.get::<_, Vec<u8>>("request_digest") != request.digest
        || r.get::<_, String>("principal") != auth.principal()
    {
        return Err(Error::IdempotencyConflict);
    }
    if r.get::<_, String>("state") != "preparing" || !r.get::<_, bool>("unexpired") {
        return Err(Error::UploadState);
    }
    let latest=tx.query_opt("SELECT principal,latest_version FROM th_changesets WHERE tenant=$1 AND repository=$2 AND changeset=$3",&[&s.tenant(),&s.repository(),&cmd.changeset]).await?;
    match latest {
        None if cmd.previous_version == 0 => (),
        Some(v)
            if v.get::<_, String>(0) == auth.principal()
                && v.get::<_, i64>(1) == cmd.previous_version => {}
        _ => return Err(Error::VersionConflict),
    }
    // Retain explicit source identity even if a future lifecycle implementation
    // changes which workspace pin run_source_in returns.
    if r.get::<_, Uuid>("seal") != seal.get::<_, Uuid>("seal") {
        return Err(Error::SchemaMismatch);
    }
    Ok(r)
}
fn derived_token(s: &Scope, key: &str, id: ObjectId) -> Uuid {
    let mut h = Sha256::new();
    h.update(b"treeharbor-seal-accept-object\0v1\0");
    h.update((s.tenant().len() as u64).to_be_bytes());
    h.update(s.tenant().as_bytes());
    h.update(s.repository().as_bytes());
    h.update((key.len() as u64).to_be_bytes());
    h.update(key.as_bytes());
    h.update(id.as_bytes());
    Uuid::from_bytes(h.finalize()[..16].try_into().expect("digest length"))
}
async fn member(tx: &Transaction<'_>, read: &SealAcceptRead, lease: &ObjectLease) -> Result<()> {
    let s = read.auth.scope();
    let row=tx.query_opt("SELECT o.generation,o.hold_token,a.backend FROM th_seal_accept_objects o JOIN th_seal_acceptances a USING(tenant,repository,operation_key) WHERE o.tenant=$1 AND o.repository=$2 AND o.operation_key=$3 AND o.digest=$4",
        &[&s.tenant(),&s.repository(),&read.command.key,&bytes(lease.key().id())]).await?.ok_or(Error::UploadMembership)?;
    if lease.key().tenant() != s.tenant()
        || row.get::<_, i64>(0) != lease.key().generation()
        || row.get::<_, Uuid>(1) != lease.token()
        || row.get::<_, Option<String>>(2).as_deref() != Some(lease.key().backend())
    {
        return Err(Error::ReceiptMismatch);
    }
    live_lease(tx, lease).await
}
impl Metadata {
    pub async fn open_seal_acceptance(
        &mut self,
        auth: &Authorization,
        cmd: &SealAcceptCommand,
    ) -> Result<SealAcceptStart> {
        let mutation = match self.begin(cmd.request(auth)?).await? {
            Begin::Replay(v) => return Ok(SealAcceptStart::Replay(v)),
            Begin::Fresh(m) => m,
        };
        let tx = &mutation.tx;
        let s = auth.scope();
        let (seal, source) = owned_ready(tx, auth, cmd).await?;
        tx.execute("INSERT INTO th_seal_acceptances(tenant,repository,operation_key,request_digest,principal,seal,changeset,previous_version,authority,epoch,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,clock_timestamp()+($11::integer*interval '1 second')) ON CONFLICT DO NOTHING",
            &[&s.tenant(),&s.repository(),&cmd.key,&mutation.request.digest,&auth.principal(),&cmd.seal,&cmd.changeset,&cmd.previous_version,&cmd.authority.as_str(),&cmd.epoch,&cmd.ttl_seconds]).await?;
        active(tx, auth, cmd).await?;
        let pins = vec![
            seal.get::<_, String>("base_pin"),
            seal.get::<_, String>("operations_pin"),
        ];
        tx.query("SELECT o.digest FROM th_objects o JOIN th_object_pins p ON p.tenant=o.tenant AND p.digest=o.digest WHERE p.tenant=$1 AND p.repository=$2 AND p.holder=ANY($3::text[]) ORDER BY o.digest FOR UPDATE OF o",&[&s.tenant(),&s.repository(),&pins]).await?;
        let operations = object(seal.get("operations"))?;
        let mut sessions = Vec::new();
        for (index, pin) in pins.iter().enumerate() {
            let lease = lease_pin_in(tx, s, pin, Uuid::new_v4(), cmd.ttl_seconds).await?;
            if (index == 0 && source.revision != RevisionRef::Native(lease.key().id()))
                || (index == 1 && lease.key().id() != operations)
            {
                return Err(Error::ReadScope);
            }
            tx.execute("UPDATE th_object_holds h SET expires_at=a.expires_at FROM th_seal_acceptances a WHERE a.tenant=$1 AND a.repository=$2 AND a.operation_key=$3 AND h.tenant=a.tenant AND h.token=$4",&[&s.tenant(),&s.repository(),&cmd.key,&lease.token()]).await?;
            sessions.push(ReadSession::from_lease(lease));
        }
        active(tx, auth, cmd).await?;
        mutation.tx.commit().await?;
        Ok(SealAcceptStart::Ready(Box::new(SealAcceptRead {
            auth: auth.clone(),
            command: cmd.clone(),
            source,
            operations,
            sessions,
        })))
    }
    pub async fn read_seal_acceptance(
        &mut self,
        read: &SealAcceptRead,
        source: usize,
        store: &ManagedStore,
        nodes: &[ReadNode],
        budget: u64,
    ) -> Result<ReadBatch> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        active(&tx, &read.auth, &read.command).await?;
        tx.commit().await?;
        self.open_read_batch(
            read.sessions.get(source).ok_or(Error::ReadScope)?,
            store,
            nodes,
            budget,
        )
        .await
    }
    pub async fn close_seal_acceptance(&mut self, read: SealAcceptRead) -> Result<()> {
        let mut error = None;
        for session in read.sessions {
            if let Err(e) = self.close_read_session(session).await {
                error.get_or_insert(e);
            }
        }
        error.map_or(Ok(()), Err)
    }
    /// Trusted coordinator only: derived frames come from shared prepare_change
    /// over the protected read sessions. Never expose this as a remote upload API.
    pub async fn reserve_seal_accept_object(
        &mut self,
        read: &SealAcceptRead,
        store: &ManagedStore,
        frame: &PreparedObject,
    ) -> Result<ObjectLease> {
        if !matches!(frame.kind(), 2..=4) || (frame.kind() == 4 && frame.id() != read.operations) {
            return Err(Error::ReceiptMismatch);
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let r = active(&tx, &read.auth, &read.command).await?;
        let s = read.auth.scope();
        let key = &read.command.key;
        if store.tenant() != s.tenant()
            || r.get::<_, Option<String>>("backend")
                .is_some_and(|b| b != store.backend())
        {
            return Err(Error::ReceiptMismatch);
        }
        let old=tx.query_opt("SELECT generation,hold_token,kind,length FROM th_seal_accept_objects WHERE tenant=$1 AND repository=$2 AND operation_key=$3 AND digest=$4",&[&s.tenant(),&s.repository(),&key,&bytes(frame.id())]).await?;
        if old.is_none()
            && (r.get::<_, i64>("reserved_objects") >= 4096
                || frame.length() as i64 > 134217728 - r.get::<_, i64>("reserved_bytes"))
        {
            return Err(Error::UploadLimit);
        }
        let token = derived_token(s, key, frame.id());
        let lease = reserve_in(&tx, store, frame, token, read.command.ttl_seconds).await?;
        if let Some(old) = old {
            if old.get::<_, i64>(0) != lease.key().generation()
                || old.get::<_, Uuid>(1) != token
                || old.get::<_, i16>(2) != i16::from(frame.kind())
                || old.get::<_, i64>(3) != frame.length() as i64
            {
                return Err(Error::ReceiptMismatch);
            }
        } else {
            tx.execute("INSERT INTO th_seal_accept_objects(tenant,repository,operation_key,digest,generation,hold_token,kind,length) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",&[&s.tenant(),&s.repository(),&key,&bytes(frame.id()),&lease.key().generation(),&token,&i16::from(frame.kind()),&(frame.length() as i64)]).await?;
            tx.execute("UPDATE th_seal_acceptances SET backend=$4,reserved_objects=reserved_objects+1,reserved_bytes=reserved_bytes+$5 WHERE tenant=$1 AND repository=$2 AND operation_key=$3",&[&s.tenant(),&s.repository(),&key,&store.backend(),&(frame.length() as i64)]).await?;
        }
        tx.execute("UPDATE th_object_holds h SET expires_at=a.expires_at FROM th_seal_acceptances a WHERE a.tenant=$1 AND a.repository=$2 AND a.operation_key=$3 AND h.tenant=a.tenant AND h.token=$4",&[&s.tenant(),&s.repository(),&key,&token]).await?;
        active(&tx, &read.auth, &read.command).await?;
        tx.commit().await?;
        Ok(lease)
    }
    pub async fn finish_seal_acceptance(
        &mut self,
        read: &SealAcceptRead,
        applied: &AppliedChange,
        operations: &ObjectLease,
        result: &ObjectLease,
    ) -> Result<Value> {
        let cmd = &read.command;
        let auth = &read.auth;
        let mutation = match self.begin(cmd.request(auth)?).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        active(&mutation.tx, auth, cmd).await?;
        if applied.base_snapshot() != read.source.snapshot
            || applied.operations() != read.operations
        {
            return Err(Error::ReceiptMismatch);
        }
        let (seal, source) = owned_ready(&mutation.tx, auth, cmd).await?;
        if source != read.source {
            return Err(Error::ExecutionState);
        }
        member(&mutation.tx, read, operations).await?;
        member(&mutation.tx, read, result).await?;
        let mut value = mutation
            .accept_change_version(&ChangeSubmission {
                id: cmd.changeset,
                previous_version: cmd.previous_version,
                run: seal.get("run"),
                workspace_generation: seal.get("workspace_generation"),
                applied,
                operations,
                result,
            })
            .await?;
        value["seal"] = json!(cmd.seal.to_string());
        value["manifest_digest"] = json!(hex(&seal.get::<_, Vec<u8>>("manifest_digest")));
        let s = auth.scope();
        mutation.tx.execute("UPDATE th_seal_acceptances SET state='completed',completed_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND operation_key=$3",&[&s.tenant(),&s.repository(),&cmd.key]).await?;
        mutation
            .write_completion(
                &value,
                &[Event {
                    kind: "change.accepted".into(),
                    payload: value.clone(),
                }],
            )
            .await?;
        owned_ready(&mutation.tx, auth, cmd).await?;
        let unexpired:bool=mutation.tx.query_one("SELECT expires_at>clock_timestamp() FROM th_seal_acceptances WHERE tenant=$1 AND repository=$2 AND operation_key=$3",&[&s.tenant(),&s.repository(),&cmd.key]).await?.get(0);
        if !unexpired {
            return Err(Error::UploadState);
        }
        mutation.tx.commit().await?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceptance_reservation_matches_independent_vector_and_separates_scopes() {
        let scope = Scope::new("test-tenant", Uuid::from_u128(1)).unwrap();
        let object = ObjectId::from_bytes([17; 32]);
        let token = derived_token(&scope, "accept-1", object);
        assert_eq!(
            token.to_string(),
            include_str!("../../tests/fixtures/seal-accept-reservation-v1.txt").trim()
        );
        for (scope, key, object) in [
            (
                Scope::new("other", Uuid::from_u128(1)).unwrap(),
                "accept-1",
                object,
            ),
            (
                Scope::new("test-tenant", Uuid::from_u128(2)).unwrap(),
                "accept-1",
                object,
            ),
            (scope.clone(), "accept-2", object),
            (scope, "accept-1", ObjectId::from_bytes([18; 32])),
        ] {
            assert_ne!(token, derived_token(&scope, key, object));
        }
    }
}
