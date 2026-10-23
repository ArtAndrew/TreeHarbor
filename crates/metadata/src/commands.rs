//! Transport-independent, bounded agent command boundary. HTTP, CLI and MCP must
//! use these authenticated operations, never deserialize a trusted CommandContext.
//! This module does not listen on a socket or confer runtime/validation authority.
use crate::access::{Permission, SecretToken};
use crate::intake::{WorkspaceFormat, WorkspaceSpec};
use crate::{Authority, Error, Metadata, Scope};
use serde::Deserialize;
use serde_json::{Value, json};
use treeharbor_core::{ObjectId, ProfileId, RepoPath, RevisionRef, SourceProfile};
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
            Self::Metadata(Error::RebaseState) => "rebase_state_changed",
            Self::Metadata(Error::UploadState) => "upload_state_changed",
            Self::Metadata(Error::UploadLimit) => "upload_limit",
            Self::Metadata(Error::UploadMembership) => "upload_membership",
            Self::Metadata(Error::ObjectBusy) => "object_busy",
            Self::Metadata(Error::StaleLease) => "lease_expired",
            Self::Metadata(Error::IntegrationState) => "integration_state_changed",
            Self::Metadata(Error::StaleCandidate) => "candidate_stale",
            Self::Metadata(Error::AdmissionBlocked) => "admission_blocked",
            Self::Metadata(Error::HostBindingState) => "host_binding_state",
            Self::Metadata(Error::HostProjectionState) => "host_projection_state",
            Self::Metadata(Error::HostPublicationState) => "host_publication_state",
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
    Github {
        request: crate::github::WireCommand,
    },
    Native {
        request: crate::integration::WireCommand,
    },
    RepositoryInfo {},
    ChangeBegin {
        key: String,
        authority: Mode,
        epoch: i64,
        upload: String,
        run: String,
        generation: i64,
        max_objects: i64,
        max_bytes: i64,
        ttl_seconds: i32,
    },
    ChangeStatus {
        upload: String,
    },
    ChangeAbort {
        key: String,
        authority: Mode,
        epoch: i64,
        upload: String,
    },
    SourceBegin {
        key: String,
        authority: Mode,
        epoch: i64,
        upload: String,
        max_objects: i64,
        max_bytes: i64,
        ttl_seconds: i32,
    },
    SourceFinish {
        key: String,
        authority: Mode,
        epoch: i64,
        upload: String,
        commit: String,
    },
    SourceAbort {
        key: String,
        authority: Mode,
        epoch: i64,
        upload: String,
    },
    SourceStatus {
        upload: String,
    },
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
    SealRequest {
        key: String,
        authority: Mode,
        epoch: i64,
        seal: String,
        run: String,
        generation: i64,
        selection: SelectionWire,
        max_objects: i64,
        max_bytes: i64,
        ttl_seconds: i32,
    },
    SealStatus {
        seal: String,
    },
    SealCancel {
        key: String,
        authority: Mode,
        epoch: i64,
        seal: String,
    },
    RunRequest {
        key: String,
        authority: Mode,
        epoch: i64,
        run: String,
        workspace: String,
        generation: i64,
        profile_digest: String,
        argv: Vec<String>,
        agent: String,
        task: String,
    },
    RunStatus {
        run: String,
    },
    RunStop {
        key: String,
        authority: Mode,
        epoch: i64,
        run: String,
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionWire {
    replace_hex: Vec<String>,
    delete_hex: Vec<String>,
    exclude_hex: Vec<String>,
}
impl SelectionWire {
    fn decode(self) -> Result<treeharbor_core::CaptureSelection, CommandError> {
        fn paths(values: Vec<String>) -> Result<Vec<RepoPath>, CommandError> {
            if values.len() > 256 {
                return Err(CommandError::InvalidRequest);
            }
            values
                .into_iter()
                .map(|v| {
                    if v.len() > 8192
                        || v.len() % 2 != 0
                        || !v
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    {
                        return Err(CommandError::InvalidRequest);
                    }
                    let bytes = (0..v.len())
                        .step_by(2)
                        .map(|i| {
                            u8::from_str_radix(&v[i..i + 2], 16)
                                .map_err(|_| CommandError::InvalidRequest)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    RepoPath::new(bytes).map_err(|_| CommandError::InvalidRequest)
                })
                .collect()
        }
        treeharbor_core::CaptureSelection::new(
            paths(self.replace_hex)?,
            paths(self.delete_hex)?,
            paths(self.exclude_hex)?,
        )
        .map_err(|_| CommandError::InvalidRequest)
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
            Self::Github { request } => request.permission(),
            Self::Native { request } => request.permission(),
            Self::ChangeBegin { .. } | Self::ChangeStatus { .. } | Self::ChangeAbort { .. } => {
                Permission::ChangeWrite
            }
            Self::SourceBegin { .. }
            | Self::SourceFinish { .. }
            | Self::SourceAbort { .. }
            | Self::SourceStatus { .. } => Permission::RepositoryWrite,
            Self::WorkspaceCreate { .. }
            | Self::WorkspaceDelete { .. }
            | Self::RunRequest { .. }
            | Self::RunStop { .. }
            | Self::SealRequest { .. }
            | Self::SealCancel { .. }
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
        self.execute_command_with_backend(credential, bytes, None)
            .await
    }
    /// Backend label comes only from verified deployment configuration.
    pub async fn execute_command_with_backend(
        &mut self,
        credential: &SecretToken,
        bytes: &[u8],
        backend: Option<&str>,
    ) -> Result<Value, CommandError> {
        let request = decode(bytes)?;
        let scope = Scope::new(request.tenant, uuid(&request.repository)?)?;
        let auth = self
            .authorize(credential, &scope, request.command.permission())
            .await?;
        let result = match request.command {
            Command::Github { request } => request.execute(self, &auth, backend).await?,
            Command::Native { request } => request.execute(self, &auth).await?,
            Command::RepositoryInfo {} => self.authorized_repository_info(&auth).await?,
            Command::ChangeBegin {
                key,
                authority,
                epoch,
                upload,
                run,
                generation,
                max_objects,
                max_bytes,
                ttl_seconds,
            } => {
                self.begin_change_upload(
                    &auth.context(key, authority.into(), epoch),
                    backend.ok_or(Error::Invalid("storage not configured"))?,
                    &crate::proposals::ChangeUploadSpec {
                        upload: uuid(&upload)?,
                        run: uuid(&run)?,
                        generation,
                        limits: crate::uploads::UploadLimits {
                            objects: max_objects,
                            bytes: max_bytes,
                            ttl_seconds,
                        },
                    },
                )
                .await?
            }
            Command::ChangeStatus { upload } => {
                self.change_upload_status(&auth, uuid(&upload)?).await?
            }
            Command::ChangeAbort {
                key,
                authority,
                epoch,
                upload,
            } => {
                self.abort_change_upload(
                    &auth.context(key, authority.into(), epoch),
                    uuid(&upload)?,
                )
                .await?
            }
            Command::SealRequest {
                key,
                authority,
                epoch,
                seal,
                run,
                generation,
                selection,
                max_objects,
                max_bytes,
                ttl_seconds,
            } => {
                self.request_workspace_seal(
                    &auth.context(key, authority.into(), epoch),
                    &crate::seals::SealRequest {
                        seal: uuid(&seal)?,
                        run: uuid(&run)?,
                        generation,
                        selection: selection.decode()?,
                        limits: crate::uploads::UploadLimits {
                            objects: max_objects,
                            bytes: max_bytes,
                            ttl_seconds,
                        },
                    },
                )
                .await?
            }
            Command::SealStatus { seal } => self.workspace_seal_status(&auth, uuid(&seal)?).await?,
            Command::SealCancel {
                key,
                authority,
                epoch,
                seal,
            } => {
                self.cancel_workspace_seal(
                    &auth.context(key, authority.into(), epoch),
                    uuid(&seal)?,
                )
                .await?
            }
            Command::RunRequest {
                key,
                authority,
                epoch,
                run,
                workspace,
                generation,
                profile_digest,
                argv,
                agent,
                task,
            } => {
                let profile: ObjectId = profile_digest
                    .parse()
                    .map_err(|_| CommandError::InvalidRequest)?;
                self.request_run(
                    &auth.context(key, authority.into(), epoch),
                    &crate::runs::RunRequest {
                        run: uuid(&run)?,
                        workspace: uuid(&workspace)?,
                        generation,
                        profile: *profile.as_bytes(),
                        argv,
                        agent,
                        task,
                    },
                )
                .await?
            }
            Command::RunStatus { run } => self.run_status(&auth, uuid(&run)?).await?,
            Command::RunStop {
                key,
                authority,
                epoch,
                run,
            } => {
                self.stop_run(&auth.context(key, authority.into(), epoch), uuid(&run)?)
                    .await?
            }
            Command::SourceBegin {
                key,
                authority,
                epoch,
                upload,
                max_objects,
                max_bytes,
                ttl_seconds,
            } => {
                self.begin_source_upload(
                    &auth.context(key, authority.into(), epoch),
                    backend.ok_or(Error::Invalid("storage not configured"))?,
                    uuid(&upload)?,
                    &crate::uploads::UploadLimits {
                        objects: max_objects,
                        bytes: max_bytes,
                        ttl_seconds,
                    },
                )
                .await?
            }
            Command::SourceFinish {
                key,
                authority,
                epoch,
                upload,
                commit,
            } => {
                let id: ObjectId = commit.parse().map_err(|_| CommandError::InvalidRequest)?;
                self.finish_source_upload(
                    &auth.context(key, authority.into(), epoch),
                    uuid(&upload)?,
                    id,
                )
                .await?
            }
            Command::SourceAbort {
                key,
                authority,
                epoch,
                upload,
            } => {
                self.abort_source_upload(
                    &auth.context(key, authority.into(), epoch),
                    uuid(&upload)?,
                )
                .await?
            }
            Command::SourceStatus { upload } => {
                self.source_upload_status(&auth, uuid(&upload)?).await?
            }
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
    fn capture_selection_rejects_ambiguous_paths_and_preserves_raw_bytes() {
        let raw: SelectionWire = serde_json::from_value(json!({
            "replace_hex":["7372632fff"], "delete_hex":[], "exclude_hex":[]
        }))
        .unwrap();
        assert_eq!(raw.decode().unwrap().replace()[0].as_bytes(), b"src/\xff");
        for value in [
            json!({"replace_hex":["2e2e2f61"],"delete_hex":[],"exclude_hex":[]}),
            json!({"replace_hex":["61","612f62"],"delete_hex":[],"exclude_hex":[]}),
            json!({"replace_hex":["61"],"delete_hex":["61"],"exclude_hex":[]}),
            json!({"replace_hex":["61"],"delete_hex":[],"exclude_hex":["62"]}),
            json!({"replace_hex":["FF"],"delete_hex":[],"exclude_hex":[]}),
            json!({"replace_hex":["f"],"delete_hex":[],"exclude_hex":[]}),
            json!({"replace_hex":["2e676974"],"delete_hex":[],"exclude_hex":[]}),
            json!({"replace_hex":["61".repeat(4097)],"delete_hex":[],"exclude_hex":[]}),
            json!({"replace_hex":vec!["61";257],"delete_hex":[],"exclude_hex":[]}),
        ] {
            assert!(
                serde_json::from_value::<SelectionWire>(value)
                    .unwrap()
                    .decode()
                    .is_err()
            );
        }
        assert!(
            serde_json::from_value::<SelectionWire>(json!({
                "replace_hex":["61"],"delete_hex":[],"exclude_hex":[],"quiescent":true
            }))
            .is_err()
        );
    }
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
