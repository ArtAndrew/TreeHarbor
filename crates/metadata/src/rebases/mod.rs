//! Owner-authorized immutable ChangeSet rebasing. This never moves refs or
//! manufactures CI evidence. Accepted versions remain independent retention roots.
mod objects;
use crate::{
    Authority, Begin, Error, Event, Metadata, Request, Result, Scope,
    access::{Authorization, Permission, repository_lock},
    catalog::{ObjectLease, settings},
    integration::Command,
};
pub use objects::RebaseRead;
use serde_json::{Value, json};
use tokio_postgres::Transaction;
use treeharbor_core::{ObjectId, RevisionRef};
use treeharbor_merge::{CandidateInput, PreparedRebase, REBASE_PROFILE};
use uuid::Uuid;

fn bytes(id: ObjectId) -> Vec<u8> {
    id.as_bytes().to_vec()
}
fn object(raw: Vec<u8>) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        raw.try_into().map_err(|_| Error::SchemaMismatch)?,
    ))
}
fn native(text: &str) -> Result<ObjectId> {
    match text.parse()? {
        RevisionRef::Native(id) => Ok(id),
        _ => Err(Error::Invalid("native rebase revision")),
    }
}
fn request(auth: &Authorization, cmd: &Command, kind: &str, data: Value) -> Result<Request> {
    auth.context(cmd.key.clone(), Authority::Standalone, cmd.epoch)
        .request(kind, data)
}
fn preparation(auth: &Authorization, cmd: &Command, id: Uuid) -> Result<Request> {
    request(
        auth,
        cmd,
        "change.rebase.prepare.v1",
        json!([1, id.to_string(), REBASE_PROFILE]),
    )
}
struct Record {
    rebase: Uuid,
    changeset: Uuid,
    source_version: i64,
    result_version: Option<i64>,
    run: Uuid,
    principal: String,
    epoch: i64,
    state: String,
    profile: String,
    revision: Option<ObjectId>,
    input: CandidateInput,
    source_pins: Vec<String>,
    unexpired: bool,
    failure: Option<Value>,
}
impl Record {
    fn json(&self) -> Value {
        json!({"rebase":self.rebase.to_string(),"changeset":self.changeset.to_string(),
            "source_version":self.source_version,"result_version":self.result_version,
            "run":self.run.to_string(),"onto":RevisionRef::Native(self.input.current_revision).to_string(),
            "epoch":self.epoch,"state":self.state,"unexpired":self.unexpired,
            "profile":self.profile,"revision":self.revision.map(|r|RevisionRef::Native(r).to_string()),
            "failure":self.failure})
    }
}
async fn record(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Record> {
    let row=tx.query_opt(r#"SELECT r.*,v.run,v.base_revision,v.base_snapshot,v.operations,v.result_snapshot,
        v.base_pin,v.operations_pin,v.result_pin,b.snapshot AS onto_snapshot,b.pin_holder AS onto_pin,
        (r.expires_at>clock_timestamp()) AS unexpired
        FROM th_change_rebases r JOIN th_change_versions v
        ON v.tenant=r.tenant AND v.repository=r.repository AND v.changeset=r.changeset AND v.version=r.source_version
        JOIN th_bases b ON b.tenant=r.tenant AND b.repository=r.repository AND b.revision=r.onto
        WHERE r.tenant=$1 AND r.repository=$2 AND r.rebase=$3"#,
        &[&s.tenant,&s.repository,&id]).await?.ok_or(Error::NotFound)?;
    let changeset: Uuid = row.get("changeset");
    let source_version: i64 = row.get("source_version");
    let run: Uuid = row.get("run");
    Ok(Record {
        rebase: id,
        changeset,
        source_version,
        run,
        result_version: row.get("result_version"),
        principal: row.get("principal"),
        epoch: row.get("epoch"),
        state: row.get("state"),
        profile: row.get("profile"),
        revision: row
            .get::<_, Option<Vec<u8>>>("revision")
            .map(object)
            .transpose()?,
        unexpired: row.get("unexpired"),
        failure: row.get("failure"),
        input: CandidateInput {
            base_revision: native(&row.get::<_, String>("base_revision"))?,
            base_snapshot: object(row.get("base_snapshot"))?,
            original_operations: object(row.get("operations"))?,
            proposed_snapshot: object(row.get("result_snapshot"))?,
            current_revision: native(&row.get::<_, String>("onto"))?,
            current_snapshot: object(row.get("onto_snapshot"))?,
            timestamp_seconds: row.get("timestamp_seconds"),
            label: format!("{id}/{changeset}/{source_version}/{run}"),
        },
        source_pins: vec![
            row.get("base_pin"),
            row.get("onto_pin"),
            row.get("operations_pin"),
            row.get("result_pin"),
        ],
    })
}
async fn authorized(tx: &Transaction<'_>, auth: &Authorization, epoch: i64) -> Result<()> {
    let s = auth.scope();
    repository_lock(tx, s, true).await?;
    auth.check_transaction(tx, s, auth.principal(), Permission::ChangeWrite)
        .await?;
    let row = tx
        .query_one(
            "SELECT authority,state,epoch FROM th_repositories WHERE tenant=$1 AND repository=$2",
            &[&s.tenant, &s.repository],
        )
        .await?;
    if epoch <= 0
        || row.get::<_, String>(0) != "standalone"
        || row.get::<_, String>(1) != "active"
        || row.get::<_, i64>(2) != epoch
    {
        return Err(Error::Fenced);
    }
    Ok(())
}
async fn owned(tx: &Transaction<'_>, auth: &Authorization, id: Uuid, epoch: i64) -> Result<Record> {
    authorized(tx, auth, epoch).await?;
    let r = record(tx, auth.scope(), id).await?;
    if r.principal != auth.principal() {
        return Err(Error::Forbidden);
    }
    if r.epoch != epoch {
        return Err(Error::Fenced);
    }
    Ok(r)
}
async fn preparing(
    tx: &Transaction<'_>,
    auth: &Authorization,
    id: Uuid,
    epoch: i64,
) -> Result<Record> {
    let r = owned(tx, auth, id, epoch).await?;
    if r.state != "preparing" || !r.unexpired || r.profile != REBASE_PROFILE {
        return Err(Error::RebaseState);
    }
    let s = auth.scope();
    let latest:i64=tx.query_one("SELECT latest_version FROM th_changesets WHERE tenant=$1 AND repository=$2 AND changeset=$3 AND principal=$4", &[&s.tenant,&s.repository,&r.changeset,&auth.principal()]).await?.get(0);
    if latest != r.source_version {
        return Err(Error::VersionConflict);
    }
    Ok(r)
}

pub struct RebaseStart {
    pub rebase: Uuid,
    pub changeset: Uuid,
    pub source_version: i64,
    pub onto: ObjectId,
}

impl Metadata {
    pub async fn start_change_rebase(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        spec: &RebaseStart,
    ) -> Result<Value> {
        if spec.source_version <= 0 || spec.source_version == i64::MAX {
            return Err(Error::Invalid("source version"));
        }
        let onto = RevisionRef::Native(spec.onto).to_string();
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "change.rebase.start.v1",
                json!([
                    1,
                    spec.rebase.to_string(),
                    spec.changeset.to_string(),
                    spec.source_version,
                    onto,
                    REBASE_PROFILE
                ]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        authorized(&tx.tx, auth, cmd.epoch).await?;
        let s = auth.scope();
        let row=tx.tx.query_opt("SELECT principal,latest_version FROM th_changesets WHERE tenant=$1 AND repository=$2 AND changeset=$3",&[&s.tenant,&s.repository,&spec.changeset]).await?.ok_or(Error::NotFound)?;
        if row.get::<_, String>(0) != auth.principal() {
            return Err(Error::Forbidden);
        }
        if row.get::<_, i64>(1) != spec.source_version {
            return Err(Error::VersionConflict);
        }
        if tx
            .tx
            .query_opt(
                "SELECT 1 FROM th_bases WHERE tenant=$1 AND repository=$2 AND revision=$3",
                &[&s.tenant, &s.repository, &onto],
            )
            .await?
            .is_none()
        {
            return Err(Error::NotFound);
        }
        let inserted=tx.tx.execute("INSERT INTO th_change_rebases(tenant,repository,rebase,principal,changeset,source_version,onto,epoch,profile,timestamp_seconds,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,floor(extract(epoch from clock_timestamp()))::bigint,'preparing',clock_timestamp()+interval '300 seconds') ON CONFLICT DO NOTHING", &[&s.tenant,&s.repository,&spec.rebase,&auth.principal(),&spec.changeset,&spec.source_version,&onto,&cmd.epoch,&REBASE_PROFILE]).await?;
        if inserted != 1 {
            return Err(Error::RebaseState);
        }
        let result = preparing(&tx.tx, auth, spec.rebase, cmd.epoch)
            .await?
            .json();
        tx.complete(result, vec![]).await
    }
    pub async fn change_rebase_status(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let value = owned(&tx, auth, id, epoch).await?.json();
        authorized(&tx, auth, epoch).await?;
        tx.commit().await?;
        Ok(value)
    }
    pub async fn cancel_change_rebase(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
    ) -> Result<Value> {
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "change.rebase.cancel.v1",
                json!([1, id.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        // Cancellation can close an expired or superseded preparation too.
        let r = owned(&tx.tx, auth, id, cmd.epoch).await?;
        if r.state != "preparing" {
            return Err(Error::RebaseState);
        }
        let s = auth.scope();
        tx.tx.execute("UPDATE th_change_rebases SET state='cancelled' WHERE tenant=$1 AND repository=$2 AND rebase=$3",&[&s.tenant,&s.repository,&id]).await?;
        let result = record(&tx.tx, s, id).await?.json();
        tx.complete(result, vec![]).await
    }
    pub async fn change_rebase_replay(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
    ) -> Result<Option<Value>> {
        match self.begin(preparation(auth, cmd, id)?).await? {
            Begin::Replay(v) => Ok(Some(v)),
            Begin::Fresh(tx) => {
                preparing(&tx.tx, auth, id, cmd.epoch).await?;
                tx.tx.rollback().await?;
                Ok(None)
            }
        }
    }
    pub async fn finish_change_rebase_failure(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        reason: &str,
        conflict: bool,
        details: Value,
    ) -> Result<Value> {
        crate::identifier(reason, 128)?;
        if serde_json::to_vec(&details)
            .map_err(|_| Error::Invalid("rebase details"))?
            .len()
            > 32768
        {
            return Err(Error::Invalid("rebase details"));
        }
        let tx = match self.begin(preparation(auth, cmd, id)?).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        preparing(&tx.tx, auth, id, cmd.epoch).await?;
        let s = auth.scope();
        let state = if conflict { "conflicted" } else { "failed" };
        let failure = json!({"reason":reason,"details":details});
        tx.tx.execute("UPDATE th_change_rebases SET state=$4,failure=$5 WHERE tenant=$1 AND repository=$2 AND rebase=$3",&[&s.tenant,&s.repository,&id,&state,&failure]).await?;
        let result = record(&tx.tx, s, id).await?.json();
        tx.complete(result, vec![]).await
    }
    pub async fn finish_change_rebase(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        computed: &PreparedRebase,
        leases: &RebaseLeases<'_>,
    ) -> Result<Value> {
        let tx = match self.begin(preparation(auth, cmd, id)?).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let r = preparing(&tx.tx, auth, id, cmd.epoch).await?;
        let applied = computed.applied();
        if computed.input() != &r.input
            || computed.profile() != r.profile
            || leases.revision.key().id() != computed.revision()
            || leases.operations.key().id() != applied.operations()
            || leases.result.key().id() != applied.result_snapshot()
        {
            return Err(Error::ReceiptMismatch);
        }
        let s = auth.scope();
        for lease in [leases.revision, leases.operations, leases.result] {
            if lease.key().tenant() != s.tenant {
                return Err(Error::Forbidden);
            }
        }
        let digests = vec![
            bytes(r.input.current_revision),
            bytes(computed.revision()),
            bytes(applied.operations()),
            bytes(applied.result_snapshot()),
        ];
        tx.tx.query("SELECT digest FROM th_objects WHERE tenant=$1 AND digest=ANY($2::bytea[]) ORDER BY digest FOR UPDATE",&[&s.tenant,&digests]).await?;
        let version = r.source_version + 1;
        let prefix = format!("change:{}:{version}", r.changeset);
        let base_pin = format!("{prefix}:base");
        let operations_pin = format!("{prefix}:operations");
        let result_pin = format!("{prefix}:result");
        let revision_pin = format!("change-rebase:{id}");
        tx.copy_root_pin(&r.source_pins[1], &base_pin).await?;
        tx.pin_kind(&operations_pin, leases.operations, 4).await?;
        tx.pin_kind(&result_pin, leases.result, 3).await?;
        tx.pin_kind(&revision_pin, leases.revision, 5).await?;
        tx.register_base(
            computed.revision(),
            applied.result_snapshot(),
            leases.revision,
        )
        .await?;
        let changed=tx.tx.execute("UPDATE th_changesets SET latest_version=$4 WHERE tenant=$1 AND repository=$2 AND changeset=$3 AND principal=$5 AND latest_version=$6",&[&s.tenant,&s.repository,&r.changeset,&version,&auth.principal(),&r.source_version]).await?;
        if changed != 1 {
            return Err(Error::VersionConflict);
        }
        let onto = RevisionRef::Native(r.input.current_revision).to_string();
        tx.tx.execute("INSERT INTO th_change_versions(tenant,repository,changeset,version,run,base_revision,base_snapshot,operations,result_snapshot,base_pin,operations_pin,result_pin,operation_key) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",&[&s.tenant,&s.repository,&r.changeset,&version,&r.run,&onto,&bytes(applied.base_snapshot()),&bytes(applied.operations()),&bytes(applied.result_snapshot()),&base_pin,&operations_pin,&result_pin,&cmd.key]).await?;
        tx.tx.execute("INSERT INTO th_change_rebase_lineage(tenant,repository,changeset,result_version,source_version,rebase,revision,pin_holder) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",&[&s.tenant,&s.repository,&r.changeset,&version,&r.source_version,&id,&bytes(computed.revision()),&revision_pin]).await?;
        tx.tx.execute("UPDATE th_change_rebases SET state='ready',result_version=$4,revision=$5 WHERE tenant=$1 AND repository=$2 AND rebase=$3",&[&s.tenant,&s.repository,&id,&version,&bytes(computed.revision())]).await?;
        // Only this owner's superseded proposals are invalidated. Already landed
        // history, immutable checks/reviews and their audit trail are retained.
        tx.tx.execute("UPDATE th_native_candidates c SET state='stale' FROM th_native_queue q WHERE c.tenant=$1 AND c.repository=$2 AND q.tenant=c.tenant AND q.repository=c.repository AND q.entry=c.entry AND q.changeset=$3 AND q.version<$4 AND c.state IN ('preparing','checking')",&[&s.tenant,&s.repository,&r.changeset,&version]).await?;
        tx.tx.execute("UPDATE th_native_queue SET state='stale',reason='change_rebased' WHERE tenant=$1 AND repository=$2 AND changeset=$3 AND version<$4 AND state IN ('queued','preparing','checking')",&[&s.tenant,&s.repository,&r.changeset,&version]).await?;
        // Recheck expiry after pin/row work, before committing a fresh result.
        authorized(&tx.tx, auth, cmd.epoch).await?;
        let result = record(&tx.tx, s, id).await?;
        if !result.unexpired {
            return Err(Error::RebaseState);
        }
        tx.complete(result.json(),vec![Event {kind:"change.rebased".into(),payload:json!({"rebase":id.to_string(),"changeset":r.changeset.to_string(),"version":version,"epoch":r.epoch})}]).await
    }
}
pub struct RebaseLeases<'a> {
    pub operations: &'a ObjectLease,
    pub result: &'a ObjectLease,
    pub revision: &'a ObjectLease,
}
