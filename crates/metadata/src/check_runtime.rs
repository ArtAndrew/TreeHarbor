//! Shared trusted-runtime evidence checks. Repository authority is an input;
//! importing a Git candidate never creates a native authoritative ref.
use crate::integration::{CheckProgram, trusted};
use crate::{Error, Result, Scope};
use serde_json::{Value, json};
use tokio_postgres::{Row, Transaction};
use treeharbor_core::{ObjectId, RevisionRef, SourceProfile};
use treeharbor_runtime::Profile;
use uuid::Uuid;

pub(crate) struct Subject<'a> {
    pub run: Uuid,
    pub workspace: Uuid,
    pub validator: &'a str,
    pub epoch: i64,
    pub authority: &'a str,
    pub revision: RevisionRef,
    pub snapshot: ObjectId,
}
pub(crate) async fn observe(
    tx: &Transaction<'_>,
    s: &Scope,
    subject: &Subject<'_>,
    program: &CheckProgram,
    profile: &Profile,
) -> Result<(Row, String)> {
    let row=tx.query_opt(r#"SELECT m.*,r.state AS run_state,r.principal,r.workspace,r.workspace_generation,r.epoch,
        p.base_revision,p.snapshot,p.profile AS source_profile,p.manifest_digest,p.node AS prepared_node,
        p.workspace AS prepared_workspace,p.generation AS prepared_generation,p.epoch AS prepared_epoch,
        p.storage_instance AS prepared_storage,p.slot AS prepared_slot
        FROM th_managed_runs m JOIN th_agent_runs r USING(tenant,repository,run)
        JOIN th_workspace_preparations p ON p.attempt=m.preparation
        WHERE m.tenant=$1 AND m.repository=$2 AND m.run=$3 AND p.tenant=m.tenant AND p.repository=m.repository"#,&[&s.tenant,&s.repository,&subject.run]).await?.ok_or(Error::ExecutionState)?;
    let r = &row;
    let argv = serde_json::to_vec(&program.argv).map_err(|_| Error::SchemaMismatch)?;
    let node: String = r.get("node");
    if !r.get::<_, bool>("removed")
        || r.get::<_, String>("principal") != subject.validator
        || r.get::<_, Uuid>("workspace") != subject.workspace
        || r.get::<_, i64>("workspace_generation") != 1
        || r.get::<_, i64>("epoch") != subject.epoch
        || r.get::<_, String>("authority") != subject.authority
        || r.get::<_, String>("base_revision") != subject.revision.to_string()
        || r.get::<_, Vec<u8>>("snapshot") != subject.snapshot.as_bytes().as_slice()
        || r.get::<_, Vec<u8>>("source_profile") != SourceProfile::full().id().as_bytes()
        || r.get::<_, Option<Vec<u8>>>("manifest_digest").is_none()
        || r.get::<_, Vec<u8>>("profile") != profile.digest()
        || r.get::<_, Vec<u8>>("argv") != argv
        || r.get::<_, String>("prepared_node") != node
        || r.get::<_, Uuid>("prepared_workspace") != subject.workspace
        || r.get::<_, i64>("prepared_generation") != 1
        || r.get::<_, i64>("prepared_epoch") != subject.epoch
        || r.get::<_, Uuid>("prepared_storage") != r.get::<_, Uuid>("storage_instance")
        || r.get::<_, Uuid>("prepared_slot") != r.get::<_, Uuid>("slot")
    {
        return Err(Error::ExecutionState);
    }
    if !trusted(tx, s, &node, "node", "runtime.control", None).await? {
        return Err(Error::Forbidden);
    }
    let state: String = r.get("run_state");
    let exit: Option<i64> = r.get("exit_code");
    let oom: Option<bool> = r.get("oom_killed");
    if state == "succeeded"
        && ["runtime_started_at", "runtime_finished_at"]
            .iter()
            .any(|field| {
                r.get::<_, Option<String>>(*field)
                    .is_none_or(|v| v.is_empty() || v == "0001-01-01T00:00:00Z")
            })
    {
        return Err(Error::ExecutionState);
    }
    let outcome = match state.as_str() {
        "succeeded"
            if exit == Some(0) && oom == Some(false) && !r.get::<_, bool>("stop_requested") =>
        {
            "passed"
        }
        "failed" if exit.is_some() => "failed",
        "cancelled" => "cancelled",
        "lost" => "error",
        _ => return Err(Error::ExecutionState),
    }
    .to_owned();
    Ok((row, outcome))
}
/// Canonical, bounded facts from the trusted node, not user-supplied outcomes.
pub(crate) fn facts(r: &Row) -> Value {
    json!({
        "attempt":r.get::<_,Uuid>("attempt"), "preparation":r.get::<_,Uuid>("preparation"),
        "binding":hex(r.get("binding")), "manifest":hex(r.get("manifest_digest")),
        "node":r.get::<_,String>("node"), "storage":r.get::<_,Uuid>("storage_instance"),
        "slot":r.get::<_,Uuid>("slot"), "volume":hex(r.get("volume_digest")),
        "engine":r.get::<_,Option<String>>("engine"), "container":r.get::<_,Option<String>>("container"),
        "state":r.get::<_,String>("run_state"), "exit_code":r.get::<_,Option<i64>>("exit_code"),
        "oom_killed":r.get::<_,Option<bool>>("oom_killed"),
        "started_at":r.get::<_,Option<String>>("runtime_started_at"),
        "finished_at":r.get::<_,Option<String>>("runtime_finished_at"),
        "stop_requested":r.get::<_,bool>("stop_requested"),
        "finished_generation":r.get::<_,Option<i64>>("finished_generation"),
        "finished_worker":r.get::<_,Option<Uuid>>("finished_worker")
    })
}
fn hex(v: Vec<u8>) -> String {
    v.iter().map(|v| format!("{v:02x}")).collect()
}
