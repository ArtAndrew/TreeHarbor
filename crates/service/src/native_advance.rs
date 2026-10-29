//! One journaled native queue preparation, never a review/check/merge grant.
use crate::{Error, Result, client, files};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs::File, path::Path, time::Duration};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u8,
    tenant: String,
    repository: Uuid,
    target: String,
    epoch: i64,
    entry: Uuid,
    candidate: Uuid,
}
pub struct Request<'a> {
    pub state: &'a Path,
    pub socket: &'a Path,
    pub credential: &'a client::Credential,
    pub target: &'a str,
    pub epoch: i64,
    pub timeout: Duration,
}
fn load(r: &Request<'_>) -> Result<Option<Intent>> {
    match std::fs::symlink_metadata(r.state) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    let v: Intent = serde_json::from_slice(&files::read_private(r.state, 4096)?)
        .map_err(|_| Error::NativeAdvanceState)?;
    if v.version != 1
        || v.tenant != r.credential.scope.tenant()
        || v.repository != r.credential.scope.repository()
        || v.target != r.target
        || v.epoch != r.epoch
        || v.entry.is_nil()
        || v.candidate.is_nil()
    {
        return Err(Error::NativeAdvanceState);
    }
    File::open(files::private_directory(
        r.state.parent().ok_or(Error::UnsafeFile)?,
    )?)?
    .sync_all()?;
    Ok(Some(v))
}
async fn command(r: &Request<'_>, request: Value) -> Result<client::Reply> {
    client::send(
        r.socket,
        &r.credential.token,
        r.credential
            .request(json!({"method":"native","request":request}))
            .to_string()
            .into_bytes(),
        r.timeout,
    )
    .await
}
fn result(reply: &client::Reply) -> Result<&Value> {
    if reply.body["version"] != 1 || !reply.body["result"].is_object() {
        return Err(Error::Http);
    }
    Ok(&reply.body["result"])
}
pub async fn send(r: Request<'_>) -> Result<client::Reply> {
    if r.epoch <= 0 || !r.state.is_absolute() || r.target.is_empty() || r.target.len() > 256 {
        return Err(Error::NativeAdvanceState);
    }
    files::private_directory(r.state.parent().ok_or(Error::UnsafeFile)?)?;
    let intent = match load(&r)? {
        Some(v) => v,
        None => {
            let page = command(
                &r,
                json!({"action":"queue_pending","target":r.target,"after":0,"limit":1}),
            )
            .await?;
            if !page.status.is_success() {
                return Ok(page);
            }
            let value = result(&page)?;
            if value["target"] != r.target {
                return Err(Error::Http);
            }
            let entries = value["entries"].as_array().ok_or(Error::Http)?;
            if entries.len() > 1 {
                return Err(Error::Http);
            }
            let Some(first) = entries.first() else {
                return Ok(page);
            };
            // Another candidate already owns the head; report it without adopting it.
            if first["state"] != "queued" {
                return Ok(page);
            }
            if first["epoch"] != r.epoch {
                return Err(Error::NativeAdvanceState);
            }
            let entry: Uuid = first["entry"]
                .as_str()
                .ok_or(Error::Http)?
                .parse()
                .map_err(|_| Error::Http)?;
            if entry.is_nil() {
                return Err(Error::Http);
            }
            let proposed = Intent {
                version: 1,
                tenant: r.credential.scope.tenant().into(),
                repository: r.credential.scope.repository(),
                target: r.target.into(),
                epoch: r.epoch,
                entry,
                candidate: Uuid::new_v4(),
            };
            let bytes = serde_json::to_vec(&proposed).map_err(|_| Error::NativeAdvanceState)?;
            match files::write_private(r.state, &bytes) {
                Ok(()) => {}
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            load(&r)?.ok_or(Error::NativeAdvanceState)?
        }
    };
    let start = command(&r, json!({"action":"candidate_start", "key":format!("native-advance-start-{}",intent.candidate),
        "epoch":intent.epoch,"entry":intent.entry.to_string(),"candidate":intent.candidate.to_string()})).await?;
    if !start.status.is_success() {
        return Ok(start);
    }
    let v = result(&start)?;
    if v["entry"] != intent.entry.to_string()
        || v["target"] != intent.target
        || v["candidate"] != intent.candidate.to_string()
        || v["epoch"] != intent.epoch
    {
        return Err(Error::Http);
    }
    // Start/prepare replay receipts can describe a past state. Always read now.
    let status = command(
        &r,
        json!({"action":"candidate_status","candidate":intent.candidate.to_string()}),
    )
    .await?;
    if !status.status.is_success() {
        return Ok(status);
    }
    let v = checked_status(&status, &intent)?;
    if v["state"] != "preparing" {
        return Ok(status);
    }
    let body =
        json!({"version":1,"tenant":intent.tenant,"repository":intent.repository.to_string(),
        "key":format!("native-advance-prepare-{}",intent.candidate),"epoch":intent.epoch,
        "candidate":intent.candidate.to_string()})
        .to_string()
        .into_bytes();
    let prepared =
        client::send_native_prepare(r.socket, &r.credential.token, body, r.timeout).await?;
    if !prepared.status.is_success() {
        return Ok(prepared);
    }
    checked_status(&prepared, &intent)?;
    let current = command(
        &r,
        json!({"action":"candidate_status","candidate":intent.candidate.to_string()}),
    )
    .await?;
    if current.status.is_success() {
        checked_status(&current, &intent)?;
    }
    Ok(current)
}
fn checked_status<'a>(reply: &'a client::Reply, intent: &Intent) -> Result<&'a Value> {
    let v = result(reply)?;
    if v["candidate"] != intent.candidate.to_string()
        || v["entry"] != intent.entry.to_string()
        || v["target"] != intent.target
        || v["epoch"] != intent.epoch
    {
        return Err(Error::Http);
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};
    use treeharbor_metadata::{Scope, access::SecretToken};

    #[tokio::test]
    async fn altered_or_corrupt_journal_fails_before_connect_and_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = directory.path().join("advance.json");
        let socket = directory.path().join("absent.sock");
        let credential = client::Credential {
            scope: Scope::new("test", Uuid::new_v4()).unwrap(),
            token: SecretToken::parse(&format!("thv1_{}", "34".repeat(32))).unwrap(),
        };
        let intent = Intent {
            version: 1,
            tenant: "test".into(),
            repository: credential.scope.repository(),
            target: "main".into(),
            epoch: 1,
            entry: Uuid::new_v4(),
            candidate: Uuid::new_v4(),
        };
        let bytes = serde_json::to_vec(&intent).unwrap();
        files::write_private(&state, &bytes).unwrap();
        for (field, value) in [
            ("version", json!(2)),
            ("tenant", json!("other")),
            ("repository", json!(Uuid::new_v4())),
            ("epoch", json!(2)),
            ("target", json!("other")),
            ("entry", json!(Uuid::nil())),
            ("candidate", json!(Uuid::nil())),
            ("unknown", json!(true)),
        ] {
            let mut corrupt: Value = serde_json::from_slice(&bytes).unwrap();
            corrupt[field] = value;
            let corrupt = serde_json::to_vec(&corrupt).unwrap();
            fs::write(&state, &corrupt).unwrap();
            assert!(matches!(
                send(Request {
                    state: &state,
                    socket: &socket,
                    credential: &credential,
                    target: "main",
                    epoch: 1,
                    timeout: Duration::from_secs(1)
                })
                .await,
                Err(Error::NativeAdvanceState)
            ));
            assert_eq!(fs::read(&state).unwrap(), corrupt);
        }
    }
}
