//! Persist a workspace creation identity before issuing the existing command.
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
    workspace: Uuid,
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
    pub timeout: Duration,
}

fn load_or_create(request: &Request<'_>) -> Result<Intent> {
    let base: RevisionRef = request.base.parse().map_err(|_| Error::WorkspaceState)?;
    let profile: ProfileId = request
        .profile_digest
        .parse()
        .map_err(|_| Error::WorkspaceState)?;
    if !matches!(base, RevisionRef::Native(_))
        || base.to_string() != request.base
        || profile.to_string() != request.profile_digest
        || request.epoch <= 0
        || !request.state.is_absolute()
    {
        return Err(Error::WorkspaceState);
    }
    let workspace = Uuid::new_v4();
    let proposed = Intent {
        version: 1,
        tenant: request.credential.scope.tenant().into(),
        repository: request.credential.scope.repository(),
        authority: request.authority.as_str().into(),
        epoch: request.epoch,
        base: base.to_string(),
        profile_digest: profile.to_string(),
        workspace,
        key: format!("workspace-new-{workspace}"),
    };
    let bytes = serde_json::to_vec(&proposed).map_err(|_| Error::WorkspaceState)?;
    match files::write_private(request.state, &bytes) {
        Ok(()) => {}
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let intent: Intent = serde_json::from_slice(&files::read_private(request.state, 4096)?)
        .map_err(|_| Error::WorkspaceState)?;
    if intent.version != 1
        || intent.tenant != proposed.tenant
        || intent.repository != proposed.repository
        || intent.authority != proposed.authority
        || intent.epoch != proposed.epoch
        || intent.base != proposed.base
        || intent.profile_digest != proposed.profile_digest
        || intent.workspace.is_nil()
        || intent.key != format!("workspace-new-{}", intent.workspace)
    {
        return Err(Error::WorkspaceState);
    }
    // A concurrent writer may have linked the winning intent before syncing its
    // directory. Every caller makes that name durable before any network write.
    File::open(files::private_directory(
        request.state.parent().ok_or(Error::UnsafeFile)?,
    )?)?
    .sync_all()?;
    Ok(intent)
}

pub async fn send(request: Request<'_>) -> Result<client::Reply> {
    let intent = load_or_create(&request)?;
    let command = json!({"method":"workspace_create","key":intent.key,
        "authority":intent.authority,"epoch":intent.epoch,
        "workspace":intent.workspace.to_string(),"base":intent.base,
        "profile_digest":intent.profile_digest});
    let reply = client::send(
        request.socket,
        &request.credential.token,
        request.credential.request(command).to_string().into_bytes(),
        request.timeout,
    )
    .await?;
    // A replay is the original reservation receipt, never a fresh readiness
    // assertion. Keep all server errors and the durable intent for inspection.
    if reply.status.is_success()
        && (reply.body["version"] != 1
            || reply.body["result"]["workspace"] != intent.workspace.to_string()
            || reply.body["result"]["base"] != intent.base
            || reply.body["result"]["generation"] != 1
            || reply.body["result"]["state"] != "requested")
    {
        return Err(Error::Http);
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::{Arc, Barrier},
    };
    use treeharbor_metadata::{Scope, access::SecretToken};

    #[tokio::test]
    async fn corrupt_or_changed_intents_are_preserved_before_connecting() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = directory.path().join("task.json");
        let credential = client::Credential {
            scope: Scope::new("pair", Uuid::new_v4()).unwrap(),
            token: SecretToken::parse(&format!("thv1_{}", "34".repeat(32))).unwrap(),
        };
        let base = format!("native:sha256:{}", "01".repeat(32));
        let profile = treeharbor_core::SourceProfile::full().id().to_string();
        let mut request = Request {
            state: &state,
            socket: &directory.path().join("absent.sock"),
            credential: &credential,
            authority: Authority::Connected,
            epoch: 1,
            base: &base,
            profile_digest: &profile,
            timeout: Duration::from_secs(1),
        };
        load_or_create(&request).unwrap();
        let original = fs::read(&state).unwrap();
        request.epoch = 2;
        assert!(matches!(send(request).await, Err(Error::WorkspaceState)));
        assert_eq!(fs::read(&state).unwrap(), original);
        for (field, value) in [
            ("version", json!(2)),
            ("workspace", json!(Uuid::nil().to_string())),
            ("key", json!("another-operation")),
            ("unexpected", json!(true)),
        ] {
            let mut corrupt: serde_json::Value = serde_json::from_slice(&original).unwrap();
            corrupt[field] = value;
            let bytes = serde_json::to_vec(&corrupt).unwrap();
            fs::write(&state, &bytes).unwrap();
            assert!(matches!(
                send(Request {
                    state: &state,
                    socket: &directory.path().join("absent.sock"),
                    credential: &credential,
                    authority: Authority::Connected,
                    epoch: 1,
                    base: &base,
                    profile_digest: &profile,
                    timeout: Duration::from_secs(1),
                })
                .await,
                Err(Error::WorkspaceState)
            ));
            assert_eq!(fs::read(&state).unwrap(), bytes);
        }
    }

    #[test]
    fn concurrent_first_writers_share_one_intent_and_reject_changed_scope() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = directory.path().join("task.json");
        let scope = Scope::new("pair", Uuid::new_v4()).unwrap();
        let barrier = Arc::new(Barrier::new(4));
        let identities = std::thread::scope(|threads| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let state = &state;
                    let scope = scope.clone();
                    let barrier = barrier.clone();
                    threads.spawn(move || {
                        let credential = client::Credential {
                            scope,
                            token: SecretToken::parse(&format!("thv1_{}", "12".repeat(32)))
                                .unwrap(),
                        };
                        let base = format!("native:sha256:{}", "01".repeat(32));
                        let profile = treeharbor_core::SourceProfile::full().id().to_string();
                        barrier.wait();
                        load_or_create(&Request {
                            state,
                            socket: Path::new("/unused"),
                            credential: &credential,
                            authority: Authority::Connected,
                            epoch: 1,
                            base: &base,
                            profile_digest: &profile,
                            timeout: Duration::from_secs(1),
                        })
                        .unwrap()
                        .workspace
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(identities.iter().all(|id| *id == identities[0]));
        let original = fs::read(&state).unwrap();
        let credential = client::Credential {
            scope: Scope::new("other", scope.repository()).unwrap(),
            token: SecretToken::parse(&format!("thv1_{}", "12".repeat(32))).unwrap(),
        };
        let base = format!("native:sha256:{}", "01".repeat(32));
        let profile = treeharbor_core::SourceProfile::full().id().to_string();
        assert!(matches!(
            load_or_create(&Request {
                state: &state,
                socket: Path::new("/unused"),
                credential: &credential,
                authority: Authority::Connected,
                epoch: 1,
                base: &base,
                profile_digest: &profile,
                timeout: Duration::from_secs(1),
            }),
            Err(Error::WorkspaceState)
        ));
        assert_eq!(fs::read(&state).unwrap(), original);
        assert!(!original.windows(5).any(|w| w == b"thv1_"));
    }
}
