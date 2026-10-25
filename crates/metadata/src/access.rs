//! Repository-scoped opaque credentials and transactional authorization. Bootstrap
//! administration requires a trusted database connection, never an agent tool.
use crate::catalog::settings;
use crate::intake::CommandContext;
use crate::{Authority, Error, Metadata, Result, Scope, identifier, tenant_valid};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt};
use tokio_postgres::Transaction;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    Read,
    WorkspaceWrite,
    ChangeWrite,
    QueueRequest,
    RepositoryWrite,
    RuntimeControl,
    ValidationReport,
    HostPublish,
    HostRead,
    HostQueue,
    HostMerge,
    HostRecover,
    Review,
    PolicyWrite,
    IntegrationControl,
}
impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::WorkspaceWrite => "workspace.write",
            Self::ChangeWrite => "change.write",
            Self::QueueRequest => "queue.request",
            Self::RepositoryWrite => "repository.write",
            Self::RuntimeControl => "runtime.control",
            Self::ValidationReport => "validation.report",
            Self::HostPublish => "host.publish",
            Self::HostRead => "host.read",
            Self::HostQueue => "host.queue",
            Self::HostMerge => "host.merge",
            Self::HostRecover => "host.recover",
            Self::Review => "review",
            Self::PolicyWrite => "policy.write",
            Self::IntegrationControl => "integration.control",
        }
    }
    pub(crate) fn for_command(kind: &str) -> Result<Self> {
        match kind {
            "github.managed_check.approve.v1"
            | "github.managed_check.approve_pull.v1"
            | "github.managed_check.revoke.v1" => Ok(Self::PolicyWrite),
            "github.managed_check.prepare.v1"
            | "github.managed_check.start.v1"
            | "github.managed_check.collect.v1"
            | "github.managed_check.release.v1" => Ok(Self::ValidationReport),
            "github.policy.set.v1" => Ok(Self::PolicyWrite),
            "github.recovery.request.v1" | "github.recovery.cancel.v1" => Ok(Self::RepositoryWrite),
            "github.recovery.page.v1"
            | "github.recovery.detail.v1"
            | "github.recovery.begin.v1"
            | "github.recovery.report.v1" => Ok(Self::HostRecover),
            "github.events.create.v1" | "github.events.revoke.v1" => Ok(Self::RepositoryWrite),
            "github.dequeue.request.v1" => Ok(Self::HostQueue),
            "github.dequeue.begin.v1" | "github.dequeue.report.v1" => Ok(Self::HostMerge),
            "github.queue.request.v1" | "github.queue.cancel.v1" => Ok(Self::HostQueue),
            "github.queue.begin.v1" | "github.queue.report.v1" => Ok(Self::HostMerge),
            "github.inspection.request.v1" | "github.inspection.cancel.v1" => {
                Ok(Self::RepositoryWrite)
            }
            "github.inspection.report.v1" => Ok(Self::HostRead),
            "github.pull.request.v1" | "github.pull.cancel.v1" => Ok(Self::RepositoryWrite),
            "github.pull.begin.v1" | "github.pull.report.v1" => Ok(Self::HostPublish),
            "github.publication.request.v1" | "github.publication.cancel.v1" => {
                Ok(Self::RepositoryWrite)
            }
            "github.publication.prepare.v1"
            | "github.publication.begin.v1"
            | "github.publication.report.v1" => Ok(Self::HostPublish),
            "github.bind.v1" | "github.revoke.v1" => Ok(Self::RepositoryWrite),
            "github.import.request.v1" | "github.import.cancel.v1" => Ok(Self::RepositoryWrite),
            "github.import.begin.v1" | "github.import.finish.v1" => Ok(Self::HostRead),
            "github.projection.request.v1"
            | "github.projection.cancel.v1"
            | "github.projection.prepare.v1" => Ok(Self::RepositoryWrite),
            "github.probe.begin.v1" | "github.probe.finish.v1" => Ok(Self::HostRead),
            "native.program.put.v1" | "native.policy.put.v1" | "native.ref.policy.v1" => {
                Ok(Self::PolicyWrite)
            }
            "native.ref.create.v1" => Ok(Self::RepositoryWrite),
            "native.queue.enqueue.v1" | "native.queue.cancel.v1" => Ok(Self::QueueRequest),
            "native.candidate.start.v1"
            | "native.candidate.prepare.v1"
            | "native.candidate.fail.v1"
            | "native.check.assign.v1"
            | "native.candidate.land.v1" => Ok(Self::IntegrationControl),
            "native.check.prepare.v1"
            | "native.check.start.v1"
            | "native.check.collect.v1"
            | "native.check.release.v1"
            | "native.validation.report.v1"
            | "native.validation.revoke.v1" => Ok(Self::ValidationReport),
            "native.review.record.v1" | "native.review.revoke.v1" => Ok(Self::Review),
            "base.register.v1" | "source.begin.v1" | "source.finish.v1" | "source.abort.v1" => {
                Ok(Self::RepositoryWrite)
            }
            "workspace.create.v1"
            | "workspace.drain.v1"
            | "profile.put.v1"
            | "run.request.v1"
            | "workspace.seal.request.v1"
            | "workspace.seal.cancel.v1"
            | "run.stop.v1" => Ok(Self::WorkspaceWrite),
            "workspace.ready.v1" | "run.running.v1" | "workspace.seal.ready.v1" => {
                Ok(Self::RuntimeControl)
            }
            "change.accept.v1"
            | "change.begin.v1"
            | "change.abort.v1"
            | "change.submit.v1"
            | "workspace.seal.accept.v1"
            | "change.rebase.start.v1"
            | "change.rebase.prepare.v1"
            | "change.rebase.cancel.v1" => Ok(Self::ChangeWrite),
            _ => Err(Error::Forbidden),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrincipalKind {
    Human,
    Agent,
    Node,
    Validator,
    Bridge,
    Integrator,
}
impl PrincipalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
            Self::Node => "node",
            Self::Validator => "validator",
            Self::Bridge => "bridge",
            Self::Integrator => "integrator",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "human" => Ok(Self::Human),
            "agent" => Ok(Self::Agent),
            "node" => Ok(Self::Node),
            "validator" => Ok(Self::Validator),
            "bridge" => Ok(Self::Bridge),
            "integrator" => Ok(Self::Integrator),
            _ => Err(Error::SchemaMismatch),
        }
    }
    fn permits(self, permission: Permission) -> bool {
        match permission {
            Permission::Read => true,
            Permission::WorkspaceWrite | Permission::ChangeWrite | Permission::QueueRequest => {
                matches!(self, Self::Human | Self::Agent)
            }
            Permission::RepositoryWrite => self == Self::Human,
            Permission::RuntimeControl => self == Self::Node,
            Permission::ValidationReport => self == Self::Validator,
            Permission::HostPublish => self == Self::Bridge,
            Permission::HostRead | Permission::HostMerge | Permission::HostRecover => {
                self == Self::Bridge
            }
            Permission::HostQueue => self == Self::Human,
            Permission::Review | Permission::PolicyWrite => self == Self::Human,
            Permission::IntegrationControl => self == Self::Integrator,
        }
    }
}

/// Never serialize or log this value. Exposure is explicit for a protected
/// credential file or an Authorization header. Debug always redacts it.
pub struct SecretToken(String);
impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretToken([REDACTED])")
    }
}
impl SecretToken {
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() != 69
            || !value.starts_with("thv1_")
            || !value.as_bytes()[5..]
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
        {
            return Err(Error::Unauthenticated);
        }
        Ok(Self(value.into()))
    }
    fn generate() -> Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::CredentialGeneration)?;
        let value = format!(
            "thv1_{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        Ok(Self(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    fn fingerprint(&self) -> Vec<u8> {
        let mut hash = Sha256::new();
        hash.update(b"treeharbor-access-token\0v1\0");
        hash.update(self.0.as_bytes());
        hash.finalize().to_vec()
    }
}
#[derive(Debug)]
pub struct IssuedToken {
    pub id: Uuid,
    pub secret: SecretToken,
}

/// Private identity binding, minted by a credential lookup. It is revalidated
/// inside every authoritative mutation, including replay and immediately before
/// commit. It is not an indefinitely valid cached permission or network token.
#[derive(Clone)]
pub struct Authorization {
    scope: Scope,
    principal: String,
    token: Uuid,
    fingerprint: Vec<u8>,
    permission: Permission,
}
impl fmt::Debug for Authorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Authorization")
            .field("scope", &self.scope)
            .field("principal", &self.principal)
            .field("permission", &self.permission)
            .finish_non_exhaustive()
    }
}
impl Authorization {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn principal(&self) -> &str {
        &self.principal
    }
    pub(crate) fn token_id(&self) -> Uuid {
        self.token
    }
    pub fn context(&self, key: String, authority: Authority, epoch: i64) -> CommandContext {
        CommandContext::authenticated(self.clone(), key, authority, epoch)
    }
    pub(crate) async fn check_transaction(
        &self,
        tx: &Transaction<'_>,
        scope: &Scope,
        principal: &str,
        permission: Permission,
    ) -> Result<()> {
        if self.scope.tenant != scope.tenant
            || self.scope.repository != scope.repository
            || self.principal != principal
            || self.permission != permission
        {
            return Err(Error::Forbidden);
        }
        // Lock order is repository -> principal -> grant -> token. Revocation
        // cannot return success while an already admitted mutation still commits.
        let actor = tx
            .query_opt(
                "SELECT kind,enabled FROM th_principals WHERE tenant=$1 AND principal=$2 FOR SHARE",
                &[&scope.tenant, &principal],
            )
            .await?
            .ok_or(Error::Unauthenticated)?;
        if !actor.get::<_, bool>(1) {
            return Err(Error::Unauthenticated);
        }
        if !PrincipalKind::parse(&actor.get::<_, String>(0))?.permits(permission) {
            return Err(Error::Forbidden);
        }
        let grant = tx.query_opt("SELECT permissions FROM th_repository_access WHERE tenant=$1 AND repository=$2 AND principal=$3 FOR SHARE", &[&scope.tenant,&scope.repository,&principal]).await?.ok_or(Error::Forbidden)?;
        if !grant
            .get::<_, Vec<String>>(0)
            .iter()
            .any(|p| p == permission.as_str())
        {
            return Err(Error::Forbidden);
        }
        let token = tx.query_opt("SELECT permissions,expires_at>clock_timestamp() AND revoked_at IS NULL FROM th_access_tokens WHERE tenant=$1 AND repository=$2 AND principal=$3 AND token=$4 AND fingerprint=$5 FOR SHARE", &[&scope.tenant,&scope.repository,&principal,&self.token,&self.fingerprint]).await?.ok_or(Error::Unauthenticated)?;
        if !token.get::<_, bool>(1) {
            return Err(Error::Unauthenticated);
        }
        if !token
            .get::<_, Vec<String>>(0)
            .iter()
            .any(|p| p == permission.as_str())
        {
            return Err(Error::Forbidden);
        }
        Ok(())
    }
}

fn permissions(values: &[Permission]) -> Result<Vec<&'static str>> {
    let unique: BTreeSet<_> = values.iter().copied().collect();
    if unique.len() != values.len() {
        return Err(Error::Invalid("duplicate permission"));
    }
    Ok(unique.into_iter().map(Permission::as_str).collect())
}
pub(crate) async fn repository_lock(
    tx: &Transaction<'_>,
    scope: &Scope,
    write: bool,
) -> Result<()> {
    let sql = if write {
        "SELECT repository FROM th_repositories WHERE tenant=$1 AND repository=$2 FOR UPDATE"
    } else {
        "SELECT repository FROM th_repositories WHERE tenant=$1 AND repository=$2 FOR SHARE"
    };
    tx.query_opt(sql, &[&scope.tenant, &scope.repository])
        .await?
        .ok_or(Error::NotFound)?;
    Ok(())
}
async fn audit(
    tx: &Transaction<'_>,
    scope: (&str, Option<Uuid>),
    operator: &str,
    action: &str,
    principal: &str,
    token: Option<Uuid>,
    permissions: Option<Vec<&str>>,
) -> Result<()> {
    let (tenant, repo) = scope;
    tx.execute("INSERT INTO th_security_audit (tenant,repository,operator,action,principal,token,permissions) VALUES ($1,$2,$3,$4,$5,$6,$7)", &[&tenant,&repo,&operator,&action,&principal,&token,&permissions]).await?;
    Ok(())
}

impl Metadata {
    /// Trusted bootstrap/administration only; never route agent JSON to this method.
    pub async fn provision_principal(
        &mut self,
        tenant: &str,
        principal: &str,
        kind: PrincipalKind,
        operator: &str,
    ) -> Result<()> {
        tenant_valid(tenant)?;
        identifier(principal, 128)?;
        identifier(operator, 128)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let inserted = tx.execute("INSERT INTO th_principals(tenant,principal,kind) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING", &[&tenant,&principal,&kind.as_str()]).await?;
        let row = tx.query_one("SELECT kind,enabled FROM th_principals WHERE tenant=$1 AND principal=$2 FOR UPDATE", &[&tenant,&principal]).await?;
        if row.get::<_, String>(0) != kind.as_str() || !row.get::<_, bool>(1) {
            return Err(Error::Forbidden);
        }
        if inserted == 1 {
            audit(
                &tx,
                (tenant, None),
                operator,
                "principal.created",
                principal,
                None,
                None,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Replace the current grant; an empty list revokes access without deleting
    /// token/audit history. Role classes never grant permissions implicitly.
    pub async fn set_repository_access(
        &mut self,
        scope: &Scope,
        principal: &str,
        allowed: &[Permission],
        operator: &str,
    ) -> Result<()> {
        identifier(principal, 128)?;
        identifier(operator, 128)?;
        let names = permissions(allowed)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, scope, true).await?;
        let actor = tx
            .query_opt(
                "SELECT kind,enabled FROM th_principals WHERE tenant=$1 AND principal=$2 FOR SHARE",
                &[&scope.tenant, &principal],
            )
            .await?
            .ok_or(Error::NotFound)?;
        let kind = PrincipalKind::parse(&actor.get::<_, String>(0))?;
        if (!actor.get::<_, bool>(1) && !allowed.is_empty())
            || allowed.iter().any(|p| !kind.permits(*p))
        {
            return Err(Error::Forbidden);
        }
        tx.execute("INSERT INTO th_repository_access(tenant,repository,principal,permissions) VALUES ($1,$2,$3,$4) ON CONFLICT(tenant,repository,principal) DO UPDATE SET permissions=EXCLUDED.permissions", &[&scope.tenant,&scope.repository,&principal,&names]).await?;
        audit(
            &tx,
            (&scope.tenant, Some(scope.repository)),
            operator,
            "access.replaced",
            principal,
            None,
            Some(names),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// A high-entropy token scoped to one repository and a subset of its current
    /// grant. The database/audit retain a fingerprint, never the credential bytes.
    pub async fn issue_repository_token(
        &mut self,
        scope: &Scope,
        principal: &str,
        allowed: &[Permission],
        ttl_seconds: i32,
        operator: &str,
    ) -> Result<IssuedToken> {
        identifier(principal, 128)?;
        identifier(operator, 128)?;
        if allowed.is_empty() || !(1..=7_776_000).contains(&ttl_seconds) {
            return Err(Error::Invalid("token permissions or lifetime"));
        }
        let names = permissions(allowed)?;
        let secret = SecretToken::generate()?;
        let token = Uuid::new_v4();
        let fingerprint = secret.fingerprint();
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, scope, true).await?;
        let actor = tx
            .query_opt(
                "SELECT kind,enabled FROM th_principals WHERE tenant=$1 AND principal=$2 FOR SHARE",
                &[&scope.tenant, &principal],
            )
            .await?
            .ok_or(Error::Forbidden)?;
        let kind = PrincipalKind::parse(&actor.get::<_, String>(0))?;
        if !actor.get::<_, bool>(1) || allowed.iter().any(|p| !kind.permits(*p)) {
            return Err(Error::Forbidden);
        }
        let grant = tx.query_opt("SELECT permissions FROM th_repository_access WHERE tenant=$1 AND repository=$2 AND principal=$3 FOR SHARE", &[&scope.tenant,&scope.repository,&principal]).await?.ok_or(Error::Forbidden)?;
        let granted: Vec<String> = grant.get(0);
        if names.iter().any(|name| !granted.iter().any(|p| p == name)) {
            return Err(Error::Forbidden);
        }
        tx.execute("INSERT INTO th_access_tokens(tenant,token,repository,principal,fingerprint,permissions,expires_at) VALUES ($1,$2,$3,$4,$5,$6,clock_timestamp()+($7::integer*interval '1 second'))", &[&scope.tenant,&token,&scope.repository,&principal,&fingerprint,&names,&ttl_seconds]).await?;
        audit(
            &tx,
            (&scope.tenant, Some(scope.repository)),
            operator,
            "token.issued",
            principal,
            Some(token),
            Some(names),
        )
        .await?;
        tx.commit().await?;
        Ok(IssuedToken { id: token, secret })
    }

    pub async fn revoke_repository_token(
        &mut self,
        scope: &Scope,
        token: Uuid,
        operator: &str,
    ) -> Result<()> {
        identifier(operator, 128)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, scope, true).await?;
        let row = tx.query_opt("SELECT principal,revoked_at IS NOT NULL FROM th_access_tokens WHERE tenant=$1 AND repository=$2 AND token=$3 FOR UPDATE", &[&scope.tenant,&scope.repository,&token]).await?.ok_or(Error::NotFound)?;
        if !row.get::<_, bool>(1) {
            tx.execute("UPDATE th_access_tokens SET revoked_at=clock_timestamp() WHERE tenant=$1 AND token=$2", &[&scope.tenant,&token]).await?;
            audit(
                &tx,
                (&scope.tenant, Some(scope.repository)),
                operator,
                "token.revoked",
                &row.get::<_, String>(0),
                Some(token),
                None,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn disable_principal(
        &mut self,
        tenant: &str,
        principal: &str,
        operator: &str,
    ) -> Result<()> {
        tenant_valid(tenant)?;
        identifier(principal, 128)?;
        identifier(operator, 128)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let row = tx
            .query_opt(
                "SELECT enabled FROM th_principals WHERE tenant=$1 AND principal=$2 FOR UPDATE",
                &[&tenant, &principal],
            )
            .await?
            .ok_or(Error::NotFound)?;
        if row.get::<_, bool>(0) {
            tx.execute(
                "UPDATE th_principals SET enabled=false WHERE tenant=$1 AND principal=$2",
                &[&tenant, &principal],
            )
            .await?;
            audit(
                &tx,
                (tenant, None),
                operator,
                "principal.disabled",
                principal,
                None,
                None,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn authorize(
        &mut self,
        secret: &SecretToken,
        scope: &Scope,
        permission: Permission,
    ) -> Result<Authorization> {
        let fingerprint = secret.fingerprint();
        let row = self.client.query_opt("SELECT tenant,repository,principal,token FROM th_access_tokens WHERE fingerprint=$1", &[&fingerprint]).await?.ok_or(Error::Unauthenticated)?;
        if row.get::<_, String>(0) != scope.tenant || row.get::<_, Uuid>(1) != scope.repository {
            return Err(Error::Forbidden);
        }
        let authorization = Authorization {
            scope: scope.clone(),
            principal: row.get(2),
            token: row.get(3),
            fingerprint,
            permission,
        };
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, scope, false).await?;
        authorization
            .check_transaction(&tx, scope, &authorization.principal, permission)
            .await?;
        tx.commit().await?;
        Ok(authorization)
    }

    /// A shared authenticated service read. Cached Authorization is checked anew;
    /// callers cannot turn a successful earlier lookup into perpetual access.
    pub async fn authorized_repository_info(
        &mut self,
        auth: &Authorization,
    ) -> Result<serde_json::Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, &auth.scope, false).await?;
        auth.check_transaction(&tx, &auth.scope, &auth.principal, Permission::Read)
            .await?;
        let row = tx.query_one("SELECT authority,state,epoch FROM th_repositories WHERE tenant=$1 AND repository=$2", &[&auth.scope.tenant,&auth.scope.repository]).await?;
        let result = serde_json::json!({"tenant":auth.scope.tenant,"repository":auth.scope.repository.to_string(),"authority":row.get::<_,String>(0),"state":row.get::<_,String>(1),"epoch":row.get::<_,i64>(2)});
        auth.check_transaction(&tx, &auth.scope, &auth.principal, Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn authorized_operation_status(
        &mut self,
        auth: &Authorization,
        key: &str,
    ) -> Result<serde_json::Value> {
        identifier(key, 128)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, &auth.scope, false).await?;
        auth.check_transaction(&tx, &auth.scope, &auth.principal, Permission::Read)
            .await?;
        let row = tx.query_opt("SELECT kind,result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND principal=$4", &[&auth.scope.tenant,&auth.scope.repository,&key,&auth.principal]).await?.ok_or(Error::NotFound)?;
        let result = serde_json::json!({"key":key,"state":"committed","kind":row.get::<_,String>(0),"result":row.get::<_,serde_json::Value>(1)});
        auth.check_transaction(&tx, &auth.scope, &auth.principal, Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn authorized_workspace_status(
        &mut self,
        auth: &Authorization,
        workspace: Uuid,
    ) -> Result<serde_json::Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, &auth.scope, false).await?;
        auth.check_transaction(&tx, &auth.scope, &auth.principal, Permission::Read)
            .await?;
        let row = tx.query_opt("SELECT base_revision,state,generation,epoch,format,profile,resolved_profile IS NOT NULL FROM th_workspaces WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND principal=$4", &[&auth.scope.tenant,&auth.scope.repository,&workspace,&auth.principal]).await?.ok_or(Error::NotFound)?;
        let cleanup = tx.query_opt("SELECT cleanup,state FROM th_workspace_cleanups WHERE tenant=$1 AND repository=$2 AND workspace=$3", &[&auth.scope.tenant,&auth.scope.repository,&workspace]).await?
            .map(|r| serde_json::json!({"id":r.get::<_,Uuid>(0).to_string(),"state":r.get::<_,String>(1)}));
        let result = serde_json::json!({"workspace":workspace.to_string(),"base":row.get::<_,String>(0),"state":row.get::<_,String>(1),"generation":row.get::<_,i64>(2),"epoch":row.get::<_,i64>(3),"format":row.get::<_,String>(4),"profile_digest":treeharbor_core::ObjectId::from_bytes(row.get::<_,Vec<u8>>(5).try_into().map_err(|_|Error::SchemaMismatch)?).to_string(),"profile_resolved":row.get::<_,bool>(6),"cleanup":cleanup});
        auth.check_transaction(&tx, &auth.scope, &auth.principal, Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_format_and_fingerprint_match_independent_vector() {
        let token = SecretToken::parse(
            "thv1_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        // Generated independently with Python hashlib, not the production helper.
        let actual = token
            .fingerprint()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(
            actual,
            "65a8b5315c3dc3656091475f305a7186808abf0086910a20e9232b123edb52d8"
        );
        for invalid in [
            "".into(),
            "thv1_".into(),
            format!("thv1_{}", "A".repeat(64)),
            format!("thv1_{}", "g".repeat(64)),
            format!("thv2_{}", "0".repeat(64)),
            format!("thv1_{}\n", "0".repeat(64)),
        ] {
            assert!(matches!(
                SecretToken::parse(&invalid),
                Err(Error::Unauthenticated)
            ));
        }
        assert_eq!(format!("{token:?}"), "SecretToken([REDACTED])");
    }
}
