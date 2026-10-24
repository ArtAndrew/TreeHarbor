use crate::{
    API_VERSION, AppCredentials, Error, OwnerType, Probe, QueueEligibility, RepositoryBinding,
    Result, Stage, now, wire,
};
use reqwest::{Client, Method, StatusCode, Url, header};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;
pub(crate) mod deliveries;
pub(crate) mod evidence;
pub(crate) mod publisher;
pub(crate) mod pulls;
pub(crate) mod queue;

const MAX_RESPONSE: usize = 128 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(45);

/// Only this trusted bridge component holds the App key. No configurable host,
/// redirect, environment proxy, implicit retry, cookie jar or agent endpoint.
pub struct Connection {
    client: Client,
    base: Url,
    credentials: AppCredentials,
    slots: Arc<Semaphore>,
}
impl Connection {
    pub fn new(credentials: AppCredentials) -> Result<Self> {
        let base = Url::parse("https://api.github.com/").map_err(|_| Error::Configuration)?;
        Ok(Self {
            client: client(true)?,
            base,
            credentials,
            slots: Arc::new(Semaphore::new(4)),
        })
    }

    /// Reads installation/repository identity using a freshly minted read-only
    /// token narrowed to one numeric repository ID, then revokes that token.
    /// Does not import, bind, publish, change branch policy or enqueue anything.
    pub async fn probe(&self, binding: &RepositoryBinding) -> Result<Probe> {
        binding.validate()?;
        tokio::time::timeout(PROBE_TIMEOUT, async {
            let _slot = self
                .slots
                .acquire()
                .await
                .map_err(|_| Error::Configuration)?;
            self.probe_inner(binding).await
        })
        .await
        .map_err(|_| Error::Deadline)?
    }

    async fn mint(&self, binding: &RepositoryBinding) -> Result<(wire::Installation, wire::Token)> {
        self.mint_for(binding, wire::Capability::Source).await
    }
    async fn mint_for(
        &self,
        binding: &RepositoryBinding,
        capability: wire::Capability,
    ) -> Result<(wire::Installation, wire::Token)> {
        let jwt = self.credentials.jwt(now()?)?;
        let installation: wire::Installation = self
            .json(
                Stage::Installation,
                Method::GET,
                &format!("app/installations/{}", binding.installation_id),
                jwt.header()?,
                None,
                StatusCode::OK,
            )
            .await?;
        installation.check(binding, self.credentials.app_id)?;
        let token: wire::Token = self.json(
            Stage::Token, Method::POST,
            &format!("app/installations/{}/access_tokens", binding.installation_id), jwt.header()?,
            Some(json!({"repository_ids": [binding.repository_id], "permissions": capability.permissions()})),
            StatusCode::CREATED,
        ).await?;
        Ok((installation, token))
    }
    async fn revoke(&self, token: &wire::Token) -> Result<()> {
        self.request(
            Stage::RevokeToken,
            Method::DELETE,
            "installation/token",
            token.token.header()?,
            None,
            StatusCode::NO_CONTENT,
        )
        .await
        .map(|_| ())
    }
    async fn probe_inner(&self, binding: &RepositoryBinding) -> Result<Probe> {
        let (installation, token) = self.mint(binding).await?;
        let observed = self
            .observe_with_token(binding, &installation, &token)
            .await;
        // Attempt revocation even if subsequent identity/scope checks failed. Do
        // not print the token to enable manual cleanup. Cancellation/transport
        // loss can leave an unobserved token until GitHub's expiration deadline.
        let revoked = self
            .request(
                Stage::RevokeToken,
                Method::DELETE,
                "installation/token",
                token.token.header()?,
                None,
                StatusCode::NO_CONTENT,
            )
            .await;
        let result = observed?;
        revoked?;
        Ok(result)
    }

    /// Fresh App access for one exact Git commit. A probe receipt cannot be
    /// substituted. The blocking worker retains the slot after caller cancellation.
    pub async fn download_git(
        &self,
        binding: &RepositoryBinding,
        request: crate::GitDownload,
    ) -> Result<treeharbor_git_bridge::download::Manifest> {
        binding.validate()?;
        if request.limits.timeout.is_zero() || request.limits.timeout > Duration::from_secs(300) {
            return Err(Error::Configuration);
        }
        let slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.clone().acquire_owned())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let (installation, token) = tokio::time::timeout(PROBE_TIMEOUT, self.mint(binding))
            .await
            .map_err(|_| Error::Deadline)??;
        let observed = tokio::time::timeout(
            PROBE_TIMEOUT,
            self.observe_with_token(binding, &installation, &token),
        )
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let result = match observed {
            Err(error) => Err(error),
            Ok(_) => {
                let enough = now()
                    .ok()
                    .and_then(|n| {
                        token.check(binding.repository_id, n).ok().map(|expiry| {
                            n.saturating_add(request.limits.timeout.as_secs() + 30) < expiry
                        })
                    })
                    .unwrap_or(false);
                if !enough {
                    Err(Error::TokenLifetime)
                } else {
                    let secret = token.token.copy_for_git();
                    let binding = binding.clone();
                    tokio::task::spawn_blocking(move || {
                        let _slot = slot;
                        treeharbor_git_bridge::download::fetch(
                            treeharbor_git_bridge::download::Request {
                                worker: &request.worker,
                                git: &request.git,
                                output: &request.output,
                                owner: &binding.owner,
                                name: &binding.name,
                                token: &secret,
                                commit: request.commit,
                                limits: request.limits,
                            },
                        )
                    })
                    .await
                    .map_err(|_| Error::GitDownload)
                    .and_then(|r| r.map_err(|_| Error::GitDownload))
                }
            }
        };
        let revoked = self.revoke(&token).await;
        let value = result?;
        revoked?;
        Ok(value)
    }

    async fn observe_with_token(
        &self,
        binding: &RepositoryBinding,
        installation: &wire::Installation,
        token: &wire::Token,
    ) -> Result<Probe> {
        self.observe_for(binding, installation, token, wire::Capability::Source)
            .await
    }
    async fn observe_for(
        &self,
        binding: &RepositoryBinding,
        installation: &wire::Installation,
        token: &wire::Token,
        capability: wire::Capability,
    ) -> Result<Probe> {
        let expiry = token.check_for(binding.repository_id, now()?, capability)?;
        let repos: wire::TokenRepositories = self
            .json(
                Stage::TokenRepositories,
                Method::GET,
                "installation/repositories?per_page=100",
                token.token.header()?,
                None,
                StatusCode::OK,
            )
            .await?;
        repos.check(binding.repository_id)?;
        if now()?.saturating_add(30) >= expiry {
            return Err(Error::TokenLifetime);
        }
        let repo: wire::Repository = self
            .json(
                Stage::Repository,
                Method::GET,
                &format!("repos/{}/{}", binding.owner, binding.name),
                token.token.header()?,
                None,
                StatusCode::OK,
            )
            .await?;
        repo.check(binding, installation.account.kind)?;
        if now()?.saturating_add(15) >= expiry {
            return Err(Error::TokenLifetime);
        }
        Ok(Probe {
            version: 1,
            api_version: API_VERSION,
            observed_at_unix: now()?,
            app_id: self.credentials.app_id,
            binding: binding.clone(),
            repository_node_id: repo.node_id,
            owner_type: repo.owner.kind,
            private: repo.private,
            default_branch: repo.default_branch,
            archived: repo.archived,
            disabled: repo.disabled,
            read_connection_verified: true,
            probe_token_revoked: true,
            queue_eligibility: match (repo.owner.kind, repo.private) {
                (OwnerType::User, _) => QueueEligibility::PersonalRepositoryUnsupported,
                (OwnerType::Organization, false) => QueueEligibility::PublicOrganizationEligible,
                (OwnerType::Organization, true) => {
                    QueueEligibility::PrivateOrganizationPlanRequired
                }
            },
            queue_configuration_verified: false,
            publication_ready: false,
        })
    }

    async fn json<T: DeserializeOwned>(
        &self,
        stage: Stage,
        method: Method,
        path: &str,
        authorization: header::HeaderValue,
        body: Option<Value>,
        expected: StatusCode,
    ) -> Result<T> {
        let bytes = self
            .request(stage, method, path, authorization, body, expected)
            .await?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Response { stage })
    }

    async fn request(
        &self,
        stage: Stage,
        method: Method,
        path: &str,
        authorization: header::HeaderValue,
        body: Option<Value>,
        expected: StatusCode,
    ) -> Result<Zeroizing<Vec<u8>>> {
        self.request_status(stage, method, path, authorization, body, &[expected])
            .await
            .map(|(_, bytes)| bytes)
    }
    async fn request_status(
        &self,
        stage: Stage,
        method: Method,
        path: &str,
        authorization: header::HeaderValue,
        body: Option<Value>,
        accepted: &[StatusCode],
    ) -> Result<(StatusCode, Zeroizing<Vec<u8>>)> {
        self.request_headers(stage, method, path, authorization, body, accepted)
            .await
            .map(|(status, _, bytes)| (status, bytes))
    }
    async fn request_headers(
        &self,
        stage: Stage,
        method: Method,
        path: &str,
        authorization: header::HeaderValue,
        body: Option<Value>,
        accepted: &[StatusCode],
    ) -> Result<(StatusCode, header::HeaderMap, Zeroizing<Vec<u8>>)> {
        let url = self.base.join(path).map_err(|_| Error::Configuration)?;
        // Defense in depth if later routes become dynamic. Never trust URLs
        // returned by GitHub, including pagination and API error links.
        if url.origin() != self.base.origin()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::Configuration);
        }
        let mut request = self
            .client
            .request(method, url)
            .header(header::AUTHORIZATION, authorization)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| Error::Transport { stage })?;
        let status = response.status();
        if !accepted.contains(&status) {
            let retry_after_seconds = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            return Err(Error::Http {
                stage,
                status: response.status().as_u16(),
                retry_after_seconds,
            });
        }
        let max_response = if stage == Stage::WebhookDeliveryDetail {
            8 * 1024 * 1024
        } else {
            MAX_RESPONSE
        };
        if response
            .content_length()
            .is_some_and(|n| n > max_response as u64)
        {
            return Err(Error::Size { stage });
        }
        let headers = response.headers().clone();
        let empty_receipt = status == StatusCode::ACCEPTED && stage == Stage::WebhookRedelivery;
        if status != StatusCode::NO_CONTENT && !empty_receipt {
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').next())
                .map(str::trim);
            if !matches!(
                content_type,
                Some("application/json" | "application/vnd.github+json")
            ) || response
                .headers()
                .get(header::CONTENT_ENCODING)
                .is_some_and(|v| v != "identity")
            {
                return Err(Error::Response { stage });
            }
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Error::Transport { stage })?
        {
            if chunk.len() > max_response - bytes.len() {
                return Err(Error::Size { stage });
            }
            bytes.extend_from_slice(&chunk);
        }
        if status == StatusCode::NO_CONTENT && !bytes.is_empty() {
            return Err(Error::Response { stage });
        }
        // The redelivery endpoint documents an empty 202. Never interpret an
        // unexpected body as an authenticated callback or recovery receipt.
        if empty_receipt && (!bytes.is_empty() || headers.contains_key(header::CONTENT_ENCODING)) {
            return Err(Error::Response { stage });
        }
        Ok((status, headers, bytes))
    }
}

fn client(https_only: bool) -> Result<Client> {
    Client::builder()
        .user_agent(concat!("TreeHarbor/", env!("CARGO_PKG_VERSION")))
        .https_only(https_only)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .no_gzip()
        .no_brotli()
        .no_zstd()
        .no_deflate()
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .pool_max_idle_per_host(4)
        .http1_only()
        .http1_max_headers(64)
        .build()
        .map_err(|_| Error::Configuration)
}

#[cfg(test)]
mod tests;
