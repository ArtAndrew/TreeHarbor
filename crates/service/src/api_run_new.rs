//! Durable identities for the existing external API task creation command.
use crate::{Error, Result, client, files};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{fs::File, path::Path, time::Duration};
use treeharbor_core::{ProfileId, RevisionRef};
use treeharbor_metadata::Authority;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u8,
    tenant: String,
    repository: Uuid,
    authority: String,
    epoch: i64,
    base: String,
    profile_digest: String,
    agent: String,
    task: String,
    workspace: Uuid,
    run: Uuid,
    key: String,
}

pub struct Request<'a> {
    pub state: &'a Path,
    pub socket: &'a Path,
    pub credential: &'a client::Credential,
    pub authority: Authority,
    pub epoch: i64,
    pub base: &'a str,
    pub profile_digest: &'a str,
    pub agent: &'a str,
    pub task: &'a str,
    pub timeout: Duration,
}

fn key(workspace: Uuid, run: Uuid) -> String {
    format!("api-run-new-{workspace}-{run}")
}

fn load_or_create(r: &Request<'_>) -> Result<Intent> {
    let base: RevisionRef = r.base.parse().map_err(|_| Error::ApiRunState)?;
    let profile: ProfileId = r.profile_digest.parse().map_err(|_| Error::ApiRunState)?;
    if !matches!(base, RevisionRef::Native(_))
        || base.to_string() != r.base
        || profile.to_string() != r.profile_digest
        || r.epoch <= 0
        || !r.state.is_absolute()
        || [(r.agent, 128), (r.task, 256)].iter().any(|(value, max)| {
            value.is_empty() || value.len() > *max || !value.bytes().all(|b| b.is_ascii_graphic())
        })
    {
        return Err(Error::ApiRunState);
    }
    let workspace = Uuid::new_v4();
    let run = Uuid::new_v4();
    let proposed = Intent {
        version: 1,
        tenant: r.credential.scope.tenant().into(),
        repository: r.credential.scope.repository(),
        authority: r.authority.as_str().into(),
        epoch: r.epoch,
        base: base.to_string(),
        profile_digest: profile.to_string(),
        agent: r.agent.into(),
        task: r.task.into(),
        workspace,
        run,
        key: key(workspace, run),
    };
    let bytes = serde_json::to_vec(&proposed).map_err(|_| Error::ApiRunState)?;
    match files::write_private(r.state, &bytes) {
        Ok(()) => {}
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let intent: Intent = serde_json::from_slice(&files::read_private(r.state, 4096)?)
        .map_err(|_| Error::ApiRunState)?;
    if intent.version != 1
        || intent.tenant != proposed.tenant
        || intent.repository != proposed.repository
        || intent.authority != proposed.authority
        || intent.epoch != proposed.epoch
        || intent.base != proposed.base
        || intent.profile_digest != proposed.profile_digest
        || intent.agent != proposed.agent
        || intent.task != proposed.task
        || intent.workspace.is_nil()
        || intent.run.is_nil()
        || intent.key != key(intent.workspace, intent.run)
    {
        return Err(Error::ApiRunState);
    }
    // A competing writer may have linked the winning name before syncing its
    // parent. Every sender makes that directory entry durable before network IO.
    File::open(files::private_directory(
        r.state.parent().ok_or(Error::UnsafeFile)?,
    )?)?
    .sync_all()?;
    Ok(intent)
}

pub async fn send(r: Request<'_>) -> Result<client::Reply> {
    let i = load_or_create(&r)?;
    let command = json!({"method":"api_run_create","key":i.key,
        "authority":i.authority,"epoch":i.epoch,"workspace":i.workspace.to_string(),
        "run":i.run.to_string(),"base":i.base,"profile_digest":i.profile_digest,
        "agent":i.agent,"task":i.task});
    let reply = client::send(
        r.socket,
        &r.credential.token,
        r.credential.request(command).to_string().into_bytes(),
        r.timeout,
    )
    .await?;
    let result = &reply.body["result"];
    if reply.status.is_success()
        && (reply.body["version"] != 1
            || result["run"] != i.run.to_string()
            || result["workspace"] != i.workspace.to_string()
            || result["base"] != i.base
            || result["profile_digest"] != i.profile_digest
            || result["epoch"] != i.epoch
            || result["generation"] != 1
            || result["state"] != "recorded"
            || result["workspace_state"] != "source_ready"
            || result["origin"] != "external_api"
            || result["delivery"] != "api"
            || result["execution_observed"] != false)
    {
        return Err(Error::Http);
    }
    // Preserve the original receipt even after close, never assert current
    // readiness. Errors and unknown outcomes leave the intent intact for retry.
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};
    use treeharbor_metadata::{Scope, access::SecretToken};

    #[tokio::test]
    async fn invalid_or_changed_task_intents_fail_before_connecting_and_remain_intact() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = dir.path().join("task.json");
        let socket = dir.path().join("absent.sock");
        let credential = client::Credential {
            scope: Scope::new("agent-test", Uuid::new_v4()).unwrap(),
            token: SecretToken::parse(&format!("thv1_{}", "12".repeat(32))).unwrap(),
        };
        let base = format!("native:sha256:{}", "01".repeat(32));
        let profile = treeharbor_core::SourceProfile::full().id().to_string();
        let request = || Request {
            state: &state,
            socket: &socket,
            credential: &credential,
            authority: Authority::Connected,
            epoch: 1,
            base: &base,
            profile_digest: &profile,
            agent: "controller/1",
            task: "fix-parser",
            timeout: Duration::from_secs(1),
        };
        assert!(matches!(
            send(Request {
                task: "invalid task",
                ..request()
            })
            .await,
            Err(Error::ApiRunState)
        ));
        assert!(!state.exists());
        load_or_create(&request()).unwrap();
        let original = fs::read(&state).unwrap();
        assert!(!original.windows(5).any(|w| w == b"thv1_"));
        for request in [
            Request {
                epoch: 2,
                ..request()
            },
            Request {
                task: "other-task",
                ..request()
            },
            Request {
                agent: "other-controller",
                ..request()
            },
        ] {
            assert!(matches!(send(request).await, Err(Error::ApiRunState)));
            assert_eq!(fs::read(&state).unwrap(), original);
        }
        for (field, value) in [
            ("version", json!(2)),
            ("run", json!(Uuid::nil())),
            ("workspace", json!(Uuid::new_v4())),
            ("tenant", json!("foreign")),
            ("unexpected", json!(true)),
        ] {
            let mut bad: serde_json::Value = serde_json::from_slice(&original).unwrap();
            bad[field] = value;
            let bytes = serde_json::to_vec(&bad).unwrap();
            fs::write(&state, &bytes).unwrap();
            assert!(matches!(send(request()).await, Err(Error::ApiRunState)));
            assert_eq!(fs::read(&state).unwrap(), bytes);
        }
    }
}
