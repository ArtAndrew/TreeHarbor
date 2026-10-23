//! Exact generated-branch transport. Durable owner authorization/intents live in
//! the service; callers must persist them before invoking this trusted adapter.
use super::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use treeharbor_core::{GitOid, RevisionRef};
use treeharbor_git_bridge::publish::{self, Prepared, Push};
use uuid::Uuid;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BranchOutcome {
    AlreadyPresent,
    Pushed,
    ObservedAfterUncertainPush,
}
#[derive(Debug, Serialize)]
pub struct BranchResult {
    pub version: u8,
    pub binding: RepositoryBinding,
    pub native_revision: String,
    pub branch: String,
    pub git_revision: String,
    pub observed_at_unix: u64,
    pub outcome: BranchOutcome,
    /// A false value requires credential cleanup; it does not undo an observed
    /// remote update. Never discard the observation because revocation failed.
    pub token_revoked: bool,
    pub token_expires_at: String,
}
#[derive(Debug, Serialize)]
pub struct BranchObservation {
    pub version: u8,
    pub binding: RepositoryBinding,
    pub branch: String,
    pub git_revision: Option<String>,
    pub observed_at_unix: u64,
    pub token_revoked: bool,
    pub token_expires_at: String,
}
#[derive(Deserialize)]
struct Ref {
    #[serde(rename = "ref")]
    name: String,
    object: RefObject,
}
#[derive(Deserialize)]
struct RefObject {
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}
fn sha(oid: GitOid) -> Result<String> {
    match oid {
        GitOid::Sha1(bytes) if bytes != [0; 20] => Ok(RevisionRef::Git(oid)
            .to_string()
            .trim_start_matches("git:sha1:")
            .to_owned()),
        _ => Err(Error::Configuration),
    }
}
impl Connection {
    pub(super) async fn reference(
        &self,
        binding: &RepositoryBinding,
        token: &wire::Token,
        branch: &str,
    ) -> Result<Option<String>> {
        let relative = branch
            .strip_prefix("refs/heads/treeharbor/")
            .ok_or(Error::Configuration)?;
        if relative.len() != 32
            || !relative
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Configuration);
        }
        let response = self
            .json::<Ref>(
                Stage::Reference,
                Method::GET,
                &format!(
                    "repos/{}/{}/git/ref/heads/treeharbor/{relative}",
                    binding.owner, binding.name
                ),
                token.token.header()?,
                None,
                StatusCode::OK,
            )
            .await;
        match response {
            Err(Error::Http {
                stage: Stage::Reference,
                status: 404,
                ..
            }) => Ok(None),
            Err(e) => Err(e),
            Ok(r) => {
                if r.name != branch || r.object.kind != "commit" {
                    return Err(Error::Identity);
                }
                let oid: RevisionRef =
                    format!("git:sha1:{}", r.object.sha)
                        .parse()
                        .map_err(|_| Error::Response {
                            stage: Stage::Reference,
                        })?;
                let RevisionRef::Git(oid) = oid else {
                    return Err(Error::Response {
                        stage: Stage::Reference,
                    });
                };
                if sha(oid)? != r.object.sha {
                    return Err(Error::Response {
                        stage: Stage::Reference,
                    });
                }
                Ok(Some(r.object.sha))
            }
        }
    }
    /// Read-only recovery observation. Absence is not proof that an earlier
    /// timed-out request cannot still complete; it never authorizes a new write.
    pub async fn observe_branch(
        &self,
        binding: &RepositoryBinding,
        identity: Uuid,
    ) -> Result<BranchObservation> {
        binding.validate()?;
        let branch = publish::branch(identity).map_err(|_| Error::Configuration)?;
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let (installation, token) = tokio::time::timeout(PROBE_TIMEOUT, self.mint(binding))
            .await
            .map_err(|_| Error::Deadline)??;
        let read = tokio::time::timeout(PROBE_TIMEOUT, async {
            self.observe_with_token(binding, &installation, &token)
                .await?;
            self.reference(binding, &token, &branch).await
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let revoked = self.revoke(&token).await;
        let head = read?;
        Ok(BranchObservation {
            version: 1,
            binding: binding.clone(),
            branch,
            git_revision: head.map(|s| format!("git:sha1:{s}")),
            observed_at_unix: now()?,
            token_revoked: revoked.is_ok(),
            token_expires_at: token.expires_at.clone(),
        })
    }
    /// Requires a process-local verified native projection and an explicit old
    /// head. This performs one push only; it never writes main, PRs or checks.
    pub async fn publish_branch(
        &self,
        binding: &RepositoryBinding,
        worker: PathBuf,
        prepared: Prepared,
        expected: Option<GitOid>,
    ) -> Result<BranchResult> {
        self.publish_branch_guarded(binding, worker, prepared, expected, || async { Ok(()) })
            .await
    }
    /// Recheck the service's current fences before minting and before push. The
    /// callback does not replace the caller's durable, one-shot dispatch grant.
    pub async fn publish_branch_guarded<G, F>(
        &self,
        binding: &RepositoryBinding,
        worker: PathBuf,
        prepared: Prepared,
        expected: Option<GitOid>,
        mut eligible: G,
    ) -> Result<BranchResult>
    where
        G: FnMut() -> F,
        F: std::future::Future<Output = Result<()>>,
    {
        binding.validate()?;
        let expected_sha = expected.map(sha).transpose()?;
        let new_sha = sha(prepared.git_oid())?;
        let branch = prepared.branch().to_owned();
        let native_revision = RevisionRef::Native(prepared.native()).to_string();
        let timeout = prepared.timeout();
        if timeout.is_zero() || timeout > Duration::from_secs(300) {
            return Err(Error::Configuration);
        }
        let slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.clone().acquire_owned())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let (installation, token) = tokio::time::timeout(PROBE_TIMEOUT, async {
            eligible().await?;
            self.mint_for(binding, wire::Capability::Branch).await
        })
        .await
        .map_err(|_| Error::Deadline)??;
        let attempt = async {
            let probe = tokio::time::timeout(
                PROBE_TIMEOUT,
                self.observe_for(binding, &installation, &token, wire::Capability::Branch),
            )
            .await
            .map_err(|_| Error::Deadline)??;
            if probe.archived || probe.disabled {
                return Err(Error::RepositoryUnavailable);
            }
            let current = self.reference(binding, &token, &branch).await?;
            if current.as_deref() == Some(&new_sha) {
                return Ok(BranchOutcome::AlreadyPresent);
            }
            if current != expected_sha {
                return Err(Error::RefChanged);
            }
            let time = now()?;
            if time.saturating_add(timeout.as_secs() + 60)
                >= token.check_for(binding.repository_id, time, wire::Capability::Branch)?
            {
                return Err(Error::TokenLifetime);
            }
            let secret = token.token.copy_for_git();
            let target = binding.clone();
            eligible().await?;
            let push = tokio::task::spawn_blocking(move || {
                let _slot = slot;
                prepared.push(Push {
                    worker: &worker,
                    owner: &target.owner,
                    name: &target.name,
                    token: &secret,
                    expected,
                })
            })
            .await;
            // Even an error can follow an applied remote ref update. Read it
            // independently before returning; never automatically resend here.
            match self.reference(binding, &token, &branch).await {
                Ok(Some(head)) if head == new_sha => Ok(if matches!(push, Ok(Ok(()))) {
                    BranchOutcome::Pushed
                } else {
                    BranchOutcome::ObservedAfterUncertainPush
                }),
                _ => Err(Error::PublicationUnknown),
            }
        }
        .await;
        let revoked = self.revoke(&token).await;
        let outcome = attempt?;
        Ok(BranchResult {
            version: 1,
            binding: binding.clone(),
            native_revision,
            branch,
            git_revision: format!("git:sha1:{new_sha}"),
            observed_at_unix: now()?,
            outcome,
            token_revoked: revoked.is_ok(),
            token_expires_at: token.expires_at.clone(),
        })
    }
}
