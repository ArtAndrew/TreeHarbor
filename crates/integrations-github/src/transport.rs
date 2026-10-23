use crate::{
    API_VERSION, AppCredentials, Error, OwnerType, Probe, QueueEligibility, RepositoryBinding,
    Result, Stage, now, wire,
};
use reqwest::{Client, Method, StatusCode, Url, header};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

const MAX_RESPONSE: usize = 128 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(45);

/// Only this trusted bridge component holds the App key. No configurable host,
/// redirect, environment proxy, implicit retry, cookie jar or agent endpoint.
pub struct Connection {
    client: Client,
    base: Url,
    credentials: AppCredentials,
    slots: Semaphore,
}
impl Connection {
    pub fn new(credentials: AppCredentials) -> Result<Self> {
        let base = Url::parse("https://api.github.com/").map_err(|_| Error::Configuration)?;
        Ok(Self {
            client: client(true)?,
            base,
            credentials,
            slots: Semaphore::new(4),
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

    async fn probe_inner(&self, binding: &RepositoryBinding) -> Result<Probe> {
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
            Some(json!({"repository_ids": [binding.repository_id], "permissions": {"contents":"read", "metadata":"read"}})),
            StatusCode::CREATED,
        ).await?;
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

    async fn observe_with_token(
        &self,
        binding: &RepositoryBinding,
        installation: &wire::Installation,
        token: &wire::Token,
    ) -> Result<Probe> {
        let expiry = token.check(binding.repository_id, now()?)?;
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
        if response.status() != expected {
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
        if response
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE as u64)
        {
            return Err(Error::Size { stage });
        }
        if expected != StatusCode::NO_CONTENT {
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
            if chunk.len() > MAX_RESPONSE - bytes.len() {
                return Err(Error::Size { stage });
            }
            bytes.extend_from_slice(&chunk);
        }
        if expected == StatusCode::NO_CONTENT && !bytes.is_empty() {
            return Err(Error::Response { stage });
        }
        Ok(bytes)
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
