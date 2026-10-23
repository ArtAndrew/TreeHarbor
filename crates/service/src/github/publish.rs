//! Trusted bridge driver. An existing attempt is recoverable only by observation.
use super::*;
use crate::pack::Pack;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    time::{SystemTime, UNIX_EPOCH},
};
use treeharbor_core::{GitOid, ObjectId, RevisionRef};
use treeharbor_git_bridge::publish::{self, Prepared};
use treeharbor_metadata::github::{PublicationMapping, PublicationOutcome, PublicationReport};
mod journal;
use journal::Journal;

pub(super) struct Request {
    pub config: PathBuf,
    pub socket: PathBuf,
    pub credential_file: PathBuf,
    pub id: Uuid,
    pub epoch: i64,
    pub state: PathBuf,
    pub git: PathBuf,
}
struct Session<'a> {
    request: &'a Request,
    credential: &'a Credential,
    identity: Value,
}
fn uuid(v: &Value) -> Result<Uuid> {
    let s = v.as_str().ok_or(Error::PublicationState)?;
    let id: Uuid = s.parse().map_err(|_| Error::PublicationState)?;
    if id.is_nil() || id.to_string() != s {
        return Err(Error::PublicationState);
    }
    Ok(id)
}
fn native(v: &Value) -> Result<ObjectId> {
    let s = v.as_str().ok_or(Error::PublicationState)?;
    match s.parse::<RevisionRef>() {
        Ok(RevisionRef::Native(id)) if RevisionRef::Native(id).to_string() == s => Ok(id),
        _ => Err(Error::PublicationState),
    }
}
fn git(v: &Value) -> Result<GitOid> {
    let s = v.as_str().ok_or(Error::PublicationState)?;
    match s.parse::<RevisionRef>() {
        Ok(RevisionRef::Git(id @ GitOid::Sha1(bytes)))
            if bytes != [0; 20] && RevisionRef::Git(id).to_string() == s =>
        {
            Ok(id)
        }
        _ => Err(Error::PublicationState),
    }
}
fn now() -> Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|v| i64::try_from(v.as_millis()).ok())
        .ok_or(Error::PublicationState)
}
fn terminal(c: &Value) -> bool {
    matches!(
        c["state"].as_str(),
        Some("observed" | "not_sent" | "cancelled")
    )
}
fn identity(c: &Value, r: &Request, credential: &Credential) -> Result<Value> {
    if c["version"] != 1
        || uuid(&c["publication"])? != r.id
        || c["epoch"] != r.epoch
        || c["merge_authorized"] != false
    {
        return Err(Error::PublicationState);
    }
    let branch = uuid(&c["branch_id"])?;
    if c["branch"] != publish::branch(branch)? {
        return Err(Error::PublicationState);
    }
    uuid(&c["projection"])?;
    uuid(&c["binding"])?;
    native(&c["native_revision"])?;
    if !c["expected_head"].is_null() {
        git(&c["expected_head"])?;
    }
    if c["expires_at_unix_ms"].as_i64().is_none() {
        return Err(Error::PublicationState);
    }
    let spec: treeharbor_metadata::github::BindingSpec =
        serde_json::from_value(c["specification"].clone()).map_err(|_| Error::PublicationState)?;
    spec.validate().map_err(|_| Error::PublicationState)?;
    Ok(
        json!({"version":1,"tenant":credential.scope.tenant(),"repository":credential.scope.repository().to_string(),
        "publication":r.id.to_string(),"epoch":r.epoch,"projection":c["projection"],"binding":c["binding"],
        "specification":spec,"branch_id":c["branch_id"],"branch":c["branch"],"native_revision":c["native_revision"],
        "expected_head":c["expected_head"],"expires_at_unix_ms":c["expires_at_unix_ms"]}),
    )
}
impl Session<'_> {
    async fn command(&self, mut value: Value) -> Result<Value> {
        value["epoch"] = json!(self.request.epoch);
        value["publication"] = json!(self.request.id.to_string());
        command(&self.request.socket, self.credential, value).await
    }
    async fn context(&self) -> Result<Value> {
        let c = self
            .command(json!({"action":"publication_context"}))
            .await?;
        if identity(&c, self.request, self.credential)? != self.identity {
            return Err(Error::PublicationState);
        }
        Ok(c)
    }
    fn matches(&self, c: &Value, mapping: &PublicationMapping, attempt: Uuid) -> Result<()> {
        for key in [
            "publication",
            "projection",
            "binding",
            "epoch",
            "branch_id",
            "branch",
            "native_revision",
            "expected_head",
            "expires_at_unix_ms",
        ] {
            if c[key] != self.identity[key] {
                return Err(Error::PublicationState);
            }
        }
        if c["version"] != 1
            || c["merge_authorized"] != false
            || c["attempt"] != attempt.to_string()
            || c["prepared_git"] != mapping.git_revision
            || c["branch_id"] != mapping.branch_id
            || c["native_revision"] != mapping.native_revision
        {
            return Err(Error::PublicationState);
        }
        Ok(())
    }
    async fn eligible(&self, mapping: &PublicationMapping, attempt: Uuid) -> Result<()> {
        let c = self
            .command(json!({"action":"publication_dispatch_context","attempt":attempt.to_string()}))
            .await?;
        self.matches(&c, mapping, attempt)?;
        if c["state"] != "attempting"
            || c["unexpired"] != true
            || c["cancel_requested"] != false
            || c["expires_at_unix_ms"]
                .as_i64()
                .ok_or(Error::PublicationState)?
                <= now()?
        {
            return Err(Error::PublicationState);
        }
        Ok(())
    }
    fn report(
        &self,
        mapping: &PublicationMapping,
        attempt: Uuid,
        remote: Option<String>,
        token_revoked: bool,
    ) -> Result<PublicationReport> {
        if let Some(ref oid) = remote {
            git(&json!(oid))?;
        }
        Ok(PublicationReport {
            attempt: attempt.to_string(),
            branch_id: mapping.branch_id.clone(),
            binding: self.identity["binding"]
                .as_str()
                .ok_or(Error::PublicationState)?
                .into(),
            native_revision: mapping.native_revision.clone(),
            prepared_git: mapping.git_revision.clone(),
            outcome: if remote.is_some() {
                PublicationOutcome::Observed
            } else {
                PublicationOutcome::Unknown
            },
            remote_revision: remote,
            observed_at_unix_ms: now()?,
            token_revoked,
        })
    }
    async fn pending(&self, journal: &Journal, c: &Value) -> Result<()> {
        if let Some((index, record)) = journal.pending()? {
            let mapping = mapping(c)?;
            self.matches(c, &mapping, uuid(&json!(record.report.attempt))?)?;
            if record.report.binding != self.identity["binding"]
                || record.report.branch_id != mapping.branch_id
                || record.report.native_revision != mapping.native_revision
                || record.report.prepared_git != mapping.git_revision
            {
                return Err(Error::PublicationState);
            }
            if c["observation"]["observed_at_unix_ms"]
                .as_i64()
                .is_some_and(|time| time > record.report.observed_at_unix_ms)
            {
                journal.ack(index, &json!({"superseded_by":c["observation"]}))?;
                return Ok(());
            }
            let result = self
                .command(
                    json!({"action":"publication_report","key":record.key,"report":record.report}),
                )
                .await?;
            self.matches(&result, &mapping, uuid(&json!(record.report.attempt))?)?;
            journal.ack(index, &result)?;
        }
        Ok(())
    }
}
fn mapping(c: &Value) -> Result<PublicationMapping> {
    git(&c["prepared_git"])?;
    Ok(PublicationMapping {
        branch_id: uuid(&c["branch_id"])?.to_string(),
        native_revision: RevisionRef::Native(native(&c["native_revision"])?).to_string(),
        git_revision: c["prepared_git"]
            .as_str()
            .ok_or(Error::PublicationState)?
            .into(),
    })
}
fn print(c: &Value) -> Result<bool> {
    println!(
        "{}",
        serde_json::to_string_pretty(c).map_err(|_| Error::Http)?
    );
    Ok(c["state"] == "observed")
}
pub(super) async fn run(r: Request) -> Result<bool> {
    let credential = Credential::load(&r.credential_file)?;
    let mut c = command(
        &r.socket,
        &credential,
        json!({"action":"publication_context","epoch":r.epoch,"publication":r.id.to_string()}),
    )
    .await?;
    let s = Session {
        identity: identity(&c, &r, &credential)?,
        request: &r,
        credential: &credential,
    };
    // Historical server evidence requires neither local files nor App credentials.
    if terminal(&c) {
        return print(&c);
    }
    let journal = Journal::open(&r.state, &s.identity)?;
    s.pending(&journal, &c).await?;
    c = s.context().await?;
    if terminal(&c) {
        return print(&c);
    }
    if !cfg!(target_os = "linux") && matches!(c["state"].as_str(), Some("requested" | "prepared")) {
        return Err(treeharbor_git_bridge::Error::Unsupported("new GitHub publications currently require Linux; observation recovery also supports macOS").into());
    }
    let spec =
        serde_json::from_value(c["specification"].clone()).map_err(|_| Error::PublicationState)?;
    let (app, config) = load(&r.config)?;
    matches_config(&config, &spec)?;
    let host = Connection::new(app)?;
    let prepared = if matches!(c["state"].as_str(), Some("requested" | "prepared")) {
        Some(prepare(&s, &journal, &c).await?)
    } else if matches!(c["state"].as_str(), Some("attempting" | "uncertain")) {
        None
    } else {
        return Err(Error::PublicationState);
    };
    c = s.context().await?;
    if terminal(&c) {
        return print(&c);
    }
    let mut dispatch = false;
    let target = mapping(&c)?;
    let attempt = if c["attempt"].is_null() {
        if c["state"] != "prepared" || prepared.is_none() {
            return Err(Error::PublicationState);
        }
        let attempt = journal.attempt()?;
        let response = s.command(json!({"action":"publication_begin","key":format!("github-publish-begin-{}",r.id),"attempt":attempt.to_string()})).await?;
        s.matches(&response["receipt"], &target, attempt)?;
        if response["receipt"]["state"] != "attempting" {
            return Err(Error::PublicationState);
        }
        dispatch = response["dispatch_granted"]
            .as_bool()
            .ok_or(Error::PublicationState)?;
        attempt
    } else {
        uuid(&c["attempt"])?
    };
    // A previous dispatcher may still be running. No observed state or replay
    // of its receipt allows this process to call the write adapter again.
    let record = if dispatch {
        let prepared = prepared.ok_or(Error::PublicationState)?;
        if prepared.native() != native(&c["native_revision"])?
            || prepared.branch() != c["branch"]
            || RevisionRef::Git(prepared.git_oid()).to_string() != target.git_revision
        {
            return Err(Error::PublicationState);
        }
        let expected = if c["expected_head"].is_null() {
            None
        } else {
            Some(git(&c["expected_head"])?)
        };
        let outcome = host
            .publish_branch_guarded(
                &config.repository,
                std::env::current_exe()?,
                prepared,
                expected,
                || async {
                    s.eligible(&target, attempt)
                        .await
                        .map_err(|_| treeharbor_github::Error::PublicationStopped)
                },
            )
            .await;
        match outcome {
            Ok(v) => {
                if v.version != 1
                    || v.binding != config.repository
                    || v.native_revision != target.native_revision
                    || v.branch != s.identity["branch"]
                    || v.git_revision != target.git_revision
                {
                    return Err(Error::PublicationState);
                }
                journal::Record::new(
                    s.report(&target, attempt, Some(v.git_revision), v.token_revoked)?,
                    Some(v.token_expires_at),
                )
            }
            // An adapter error can follow an external effect or lost token reply.
            // Preserve uncertainty, including unknown credential cleanup.
            Err(error) => {
                eprintln!("{error}");
                journal::Record::new(s.report(&target, attempt, None, false)?, None)
            }
        }
    } else {
        match host
            .observe_branch(&config.repository, uuid(&c["branch_id"])?)
            .await
        {
            Ok(v) => {
                if v.version != 1
                    || v.binding != config.repository
                    || v.branch != s.identity["branch"]
                {
                    return Err(Error::PublicationState);
                }
                journal::Record::new(
                    // Revoking this read token cannot prove cleanup of the
                    // preceding dispatcher's write token.
                    s.report(
                        &target,
                        attempt,
                        v.git_revision,
                        c["observation"]["token_revoked"] == true && v.token_revoked,
                    )?,
                    Some(v.token_expires_at),
                )
            }
            Err(error) => {
                eprintln!("{error}");
                journal::Record::new(s.report(&target, attempt, None, false)?, None)
            }
        }
    };
    journal.append(&record)?;
    c = s.context().await?;
    if !terminal(&c) {
        s.pending(&journal, &c).await?;
    }
    print(&s.context().await?)
}

async fn prepare(s: &Session<'_>, journal: &Journal, c: &Value) -> Result<Prepared> {
    if c["unexpired"] != true || c["cancel_requested"] != false {
        return Err(Error::PublicationState);
    }
    let r = s.request;
    let p = uuid(&c["projection"])?;
    let live = command(
        &r.socket,
        s.credential,
        json!({"action":"projection_context","epoch":r.epoch,"projection":p.to_string()}),
    )
    .await?;
    if live["projection"] != c["projection"]
        || live["binding"] != c["binding"]
        || live["epoch"] != r.epoch
        || live["state"] != "ready"
        || live["is_current_version"] != true
        || live["revision"] != c["native_revision"]
        || live["specification"] != c["specification"]
    {
        return Err(Error::PublicationState);
    }
    let path = journal.root.join("pack");
    if !path.try_exists()? {
        let reply = crate::archive::download_projection(
            &r.socket,
            s.credential,
            p,
            r.epoch,
            native(&c["native_revision"])?,
            &path,
            Duration::from_secs(120),
        )
        .await?;
        if !reply.status.is_success() {
            return Err(Error::GithubBinding {
                status: reply.status.as_u16(),
            });
        }
    }
    let root = journal.root.clone();
    let git = r.git.clone();
    let branch = uuid(&c["branch_id"])?;
    let native = native(&c["native_revision"])?;
    let prepared = tokio::task::spawn_blocking(move || -> Result<Prepared> {
        let pack = Pack::open(&path)?;
        if pack.commit() != native {
            return Err(Error::PublicationState);
        }
        Ok(publish::prepare(
            pack.store(),
            &git,
            &root,
            branch,
            native,
            Default::default(),
        )?)
    })
    .await
    .map_err(|_| Error::ExportWorker)??;
    let m = PublicationMapping {
        branch_id: branch.to_string(),
        native_revision: RevisionRef::Native(native).to_string(),
        git_revision: RevisionRef::Git(prepared.git_oid()).to_string(),
    };
    journal.bind("mapping.json", &json!(m))?;
    if c["state"] == "requested" {
        let mapped=s.command(json!({"action":"publication_prepare","key":format!("github-publish-prepare-{}",r.id),"mapping":m})).await?;
        if mapped["prepared_git"] != m.git_revision
            || mapped["native_revision"] != m.native_revision
            || mapped["branch_id"] != m.branch_id
        {
            return Err(Error::PublicationState);
        }
    } else if c["prepared_git"] != m.git_revision {
        return Err(Error::PublicationState);
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn different_remote_head_is_an_observation_not_a_malformed_unknown_report() {
        let r = Request {
            config: PathBuf::new(),
            socket: PathBuf::new(),
            credential_file: PathBuf::new(),
            id: Uuid::new_v4(),
            epoch: 1,
            state: PathBuf::new(),
            git: PathBuf::new(),
        };
        let credential = Credential {
            scope: treeharbor_metadata::Scope::new("fixture", Uuid::new_v4()).unwrap(),
            token: treeharbor_metadata::access::SecretToken::parse(&format!(
                "thv1_{}",
                "0".repeat(64)
            ))
            .unwrap(),
        };
        let session = Session {
            request: &r,
            credential: &credential,
            identity: json!({"binding":Uuid::new_v4().to_string()}),
        };
        let mapping = PublicationMapping {
            branch_id: Uuid::new_v4().to_string(),
            native_revision: format!("native:sha256:{}", "11".repeat(32)),
            git_revision: format!("git:sha1:{}", "22".repeat(20)),
        };
        let remote = format!("git:sha1:{}", "33".repeat(20));
        let observed = session
            .report(&mapping, Uuid::new_v4(), Some(remote.clone()), false)
            .unwrap();
        assert_eq!(observed.outcome, PublicationOutcome::Observed);
        assert_eq!(observed.remote_revision, Some(remote));
        assert_ne!(
            observed.remote_revision.as_ref(),
            Some(&observed.prepared_git)
        );
        let absent = session
            .report(&mapping, Uuid::new_v4(), None, false)
            .unwrap();
        assert_eq!(absent.outcome, PublicationOutcome::Unknown);
        assert!(absent.remote_revision.is_none());
        assert!(
            session
                .report(&mapping, Uuid::new_v4(), Some("main".into()), true)
                .is_err()
        );
    }
}
