//! Transport-independent, bounded agent command boundary. HTTP, CLI and MCP must
//! use these authenticated operations, never deserialize a trusted CommandContext.
//! This module does not listen on a socket or confer runtime/validation authority.
use crate::access::{Permission, SecretToken};
use crate::intake::{WorkspaceFormat, WorkspaceSpec};
use crate::{Authority, Error, Metadata, Scope};
use serde::Deserialize;
use serde_json::{Value, json};
use treeharbor_core::{ProfileId, RepoPath, RevisionRef, SourceProfile};
use uuid::Uuid;

pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("request exceeds the command size limit")]
    TooLarge,
    #[error("invalid or unsupported command request")]
    InvalidRequest,
    // Deliberately do not format the underlying database error for a client.
    #[error("command could not be completed")]
    Metadata(#[from] Error),
}
impl CommandError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => "request_too_large",
            Self::InvalidRequest => "invalid_request",
            Self::Metadata(Error::Unauthenticated) => "unauthenticated",
            Self::Metadata(Error::Forbidden) => "forbidden",
            Self::Metadata(Error::NotFound) => "not_found",
            Self::Metadata(Error::Fenced) => "repository_fenced",
            Self::Metadata(Error::ExecutionState) => "execution_state_changed",
            Self::Metadata(Error::IdempotencyConflict) => "idempotency_conflict",
            Self::Metadata(Error::VersionConflict) => "version_conflict",
            Self::Metadata(Error::Invalid(_)) => "invalid_request",
            Self::Metadata(_) => "internal_error",
        }
    }
    /// Only this bounded public representation belongs in a protocol response.
    /// Raw error Debug/source may contain database details; do not return it.
    pub fn response(&self) -> Value {
        json!({"error":{"code":self.code()}})
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u8,
    tenant: String,
    repository: String,
    command: Command,
}
#[derive(Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    RepositoryInfo {},
    SourceProfilePut {
        key: String,
        authority: Mode,
        epoch: i64,
        profile: ProfileWire,
    },
    SourceProfileGet {
        profile_digest: String,
    },
    WorkspaceCreate {
        key: String,
        authority: Mode,
        epoch: i64,
        workspace: String,
        base: String,
        profile_digest: String,
    },
    WorkspaceStatus {
        workspace: String,
    },
    WorkspaceDelete {
        key: String,
        authority: Mode,
        epoch: i64,
        workspace: String,
        generation: i64,
    },
    OperationStatus {
        key: String,
    },
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum ProfileWire {
    Full {},
    Sparse { paths_hex: Vec<String> },
}
impl ProfileWire {
    fn decode(self) -> Result<SourceProfile, CommandError> {
        match self {
            Self::Full {} => Ok(SourceProfile::full()),
            Self::Sparse { paths_hex } => {
                if paths_hex.len() > treeharbor_core::MAX_PROFILE_PATHS {
                    return Err(CommandError::InvalidRequest);
                }
                let mut paths = Vec::with_capacity(paths_hex.len());
                for value in paths_hex {
                    if value.len() % 2 != 0
                        || !value
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    {
                        return Err(CommandError::InvalidRequest);
                    }
                    let bytes = (0..value.len())
                        .step_by(2)
                        .map(|i| {
                            u8::from_str_radix(&value[i..i + 2], 16)
                                .map_err(|_| CommandError::InvalidRequest)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    paths.push(RepoPath::new(bytes).map_err(|_| CommandError::InvalidRequest)?);
                }
                SourceProfile::sparse(paths).map_err(|_| CommandError::InvalidRequest)
            }
        }
    }
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Connected,
    Standalone,
}
impl From<Mode> for Authority {
    fn from(value: Mode) -> Self {
        match value {
            Mode::Connected => Self::Connected,
            Mode::Standalone => Self::Standalone,
        }
    }
}
impl Command {
    fn permission(&self) -> Permission {
        match self {
            Self::WorkspaceCreate { .. }
            | Self::WorkspaceDelete { .. }
            | Self::SourceProfilePut { .. } => Permission::WorkspaceWrite,
            _ => Permission::Read,
        }
    }
}
fn uuid(value: &str) -> Result<Uuid, CommandError> {
    let id = Uuid::parse_str(value).map_err(|_| CommandError::InvalidRequest)?;
    if id.to_string() != value {
        return Err(CommandError::InvalidRequest);
    }
    Ok(id)
}
fn decode(bytes: &[u8]) -> Result<Envelope, CommandError> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(CommandError::TooLarge);
    }
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| CommandError::InvalidRequest)?;
    if envelope.version != 1 {
        return Err(CommandError::InvalidRequest);
    }
    Ok(envelope)
}
impl Metadata {
    /// Repository identities select a scope; they never establish authority.
    /// The credential is obtained by the adapter, not from model/agent JSON.
    /// All mutation authorization is checked again in the committing transaction.
    pub async fn execute_agent_command(
        &mut self,
        credential: &SecretToken,
        bytes: &[u8],
    ) -> Result<Value, CommandError> {
        let request = decode(bytes)?;
        let scope = Scope::new(request.tenant, uuid(&request.repository)?)?;
        let auth = self
            .authorize(credential, &scope, request.command.permission())
            .await?;
        let result = match request.command {
            Command::RepositoryInfo {} => self.authorized_repository_info(&auth).await?,
            Command::SourceProfilePut {
                key,
                authority,
                epoch,
                profile,
            } => {
                self.put_source_profile(
                    &auth.context(key, authority.into(), epoch),
                    &profile.decode()?,
                )
                .await?
            }
            Command::SourceProfileGet { profile_digest } => {
                let id: ProfileId = profile_digest
                    .parse()
                    .map_err(|_| CommandError::InvalidRequest)?;
                let profile = self.authorized_source_profile(&auth, id).await?;
                json!({"profile_digest":id.to_string(),"profile":crate::profiles::describe(&profile)})
            }
            Command::WorkspaceStatus { workspace } => {
                self.authorized_workspace_status(&auth, uuid(&workspace)?)
                    .await?
            }
            Command::OperationStatus { key } => {
                self.authorized_operation_status(&auth, &key).await?
            }
            Command::WorkspaceCreate {
                key,
                authority,
                epoch,
                workspace,
                base,
                profile_digest,
            } => {
                let base: RevisionRef = base.parse().map_err(|_| CommandError::InvalidRequest)?;
                // Git import and Git-compatible materialization are not available
                // through this boundary yet; never accept an unimplemented mode.
                if !matches!(base, RevisionRef::Native(_)) {
                    return Err(CommandError::InvalidRequest);
                }
                let profile: ProfileId = profile_digest
                    .parse()
                    .map_err(|_| CommandError::InvalidRequest)?;
                let spec = WorkspaceSpec {
                    id: uuid(&workspace)?,
                    base,
                    profile: *profile.as_bytes(),
                    format: WorkspaceFormat::Native,
                };
                self.create_workspace(&auth.context(key, authority.into(), epoch), &spec)
                    .await?
            }
            Command::WorkspaceDelete {
                key,
                authority,
                epoch,
                workspace,
                generation,
            } => {
                self.drain_workspace(
                    &auth.context(key, authority.into(), epoch),
                    uuid(&workspace)?,
                    generation,
                )
                .await?
            }
        };
        Ok(json!({"version":1,"result":result}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_rejects_identity_injection_unsupported_commands_and_ambiguous_json() {
        let prefix = r#"{"version":1,"tenant":"test","repository":"00000000-0000-0000-0000-000000000000","command":"#;
        for command in [
            r#"{"method":"repository_info","principal":"admin"}"#,
            r#"{"method":"repository_info","credential":"secret"}"#,
            r#"{"method":"mark_validated"}"#,
            r#"{"method":"workspace_ready"}"#,
            r#"{"method":"repository_info","method":"repository_info"}"#,
        ] {
            assert!(matches!(
                decode(format!("{prefix}{command}}}").as_bytes()),
                Err(CommandError::InvalidRequest)
            ));
        }
        let valid = format!(r#"{prefix}{{"method":"repository_info"}}}}"#);
        assert!(decode(valid.as_bytes()).is_ok());
        assert!(decode(valid.replace("\"version\":1", "\"version\":2").as_bytes()).is_err());
        assert!(
            decode(
                valid
                    .replace("\"version\":1", "\"version\":1,\"principal\":\"admin\"")
                    .as_bytes()
            )
            .is_err()
        );
        assert!(decode(format!("{valid}{{}}").as_bytes()).is_err());
        assert!(matches!(
            decode(&vec![b' '; MAX_REQUEST_BYTES + 1]),
            Err(CommandError::TooLarge)
        ));
        assert_eq!(
            CommandError::Metadata(Error::Invalid("private detail")).response(),
            json!({"error":{"code":"invalid_request"}})
        );
    }
}
