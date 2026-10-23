use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireCommand {
    PublicationDispatchContext {
        epoch: i64,
        publication: String,
        attempt: String,
    },
    PublicationRequest {
        key: String,
        epoch: i64,
        publication: String,
        specification: PublicationSpec,
    },
    PublicationPrepare {
        key: String,
        epoch: i64,
        publication: String,
        mapping: PublicationMapping,
    },
    PublicationBegin {
        key: String,
        epoch: i64,
        publication: String,
        attempt: String,
    },
    PublicationReport {
        key: String,
        epoch: i64,
        publication: String,
        report: PublicationReport,
    },
    PublicationCancel {
        key: String,
        epoch: i64,
        publication: String,
    },
    PublicationStatus {
        epoch: i64,
        publication: String,
    },
    PublicationContext {
        epoch: i64,
        publication: String,
    },
    ProjectionRequest {
        key: String,
        epoch: i64,
        projection: String,
        binding: String,
        specification: ProjectionSpec,
    },
    ProjectionCancel {
        key: String,
        epoch: i64,
        projection: String,
    },
    ProjectionStatus {
        epoch: i64,
        projection: String,
    },
    ProjectionContext {
        epoch: i64,
        projection: String,
    },
    ImportRequest {
        key: String,
        epoch: i64,
        import: String,
        binding: String,
        specification: ImportSpec,
    },
    ImportCancel {
        key: String,
        epoch: i64,
        import: String,
    },
    ImportStatus {
        epoch: i64,
        import: String,
    },
    ImportContext {
        epoch: i64,
        import: String,
    },
    ImportBegin {
        key: String,
        epoch: i64,
        import: String,
        native_commit: String,
    },
    ImportFinish {
        key: String,
        epoch: i64,
        import: String,
    },
    Bind {
        key: String,
        epoch: i64,
        binding: String,
        specification: BindingSpec,
    },
    Revoke {
        key: String,
        epoch: i64,
        binding: String,
    },
    Status {
        epoch: i64,
        binding: String,
    },
    ProbeBegin {
        key: String,
        epoch: i64,
        binding: String,
        probe: String,
    },
    ProbeContext {
        epoch: i64,
        binding: String,
        probe: String,
    },
    ProbeFinish {
        key: String,
        epoch: i64,
        binding: String,
        probe: String,
        observation: Observation,
    },
}
fn id(s: &str) -> Result<Uuid> {
    let id: Uuid = s
        .parse()
        .map_err(|_| Error::Invalid("GitHub operation id"))?;
    if id.is_nil() || id.to_string() != s {
        return Err(Error::Invalid("GitHub operation id"));
    }
    Ok(id)
}
impl WireCommand {
    pub fn permission(&self) -> Permission {
        match self {
            Self::PublicationRequest { .. } | Self::PublicationCancel { .. } => {
                Permission::RepositoryWrite
            }
            Self::PublicationStatus { .. } => Permission::Read,
            Self::PublicationPrepare { .. }
            | Self::PublicationDispatchContext { .. }
            | Self::PublicationBegin { .. }
            | Self::PublicationReport { .. }
            | Self::PublicationContext { .. } => Permission::HostPublish,
            Self::Bind { .. }
            | Self::Revoke { .. }
            | Self::ImportRequest { .. }
            | Self::ImportCancel { .. } => Permission::RepositoryWrite,
            Self::ProjectionRequest { .. } | Self::ProjectionCancel { .. } => {
                Permission::RepositoryWrite
            }
            Self::ProjectionStatus { .. } => Permission::Read,
            Self::ProjectionContext { .. } => Permission::HostPublish,
            Self::Status { .. } | Self::ImportStatus { .. } => Permission::Read,
            _ => Permission::HostRead,
        }
    }
    pub async fn execute(
        self,
        db: &mut Metadata,
        auth: &Authorization,
        backend: Option<&str>,
    ) -> Result<Value> {
        match self {
            Self::PublicationDispatchContext {
                epoch,
                publication,
                attempt,
            } => {
                db.github_publication_dispatch_context(
                    auth,
                    epoch,
                    id(&publication)?,
                    id(&attempt)?,
                )
                .await
            }
            Self::PublicationRequest {
                key,
                epoch,
                publication,
                specification,
            } => {
                db.request_github_publication(
                    auth,
                    &Command { key, epoch },
                    id(&publication)?,
                    &specification,
                )
                .await
            }
            Self::PublicationPrepare {
                key,
                epoch,
                publication,
                mapping,
            } => {
                db.prepare_github_publication(
                    auth,
                    &Command { key, epoch },
                    id(&publication)?,
                    &mapping,
                )
                .await
            }
            Self::PublicationBegin {
                key,
                epoch,
                publication,
                attempt,
            } => {
                db.begin_github_publication(
                    auth,
                    &Command { key, epoch },
                    id(&publication)?,
                    id(&attempt)?,
                )
                .await
            }
            Self::PublicationReport {
                key,
                epoch,
                publication,
                report,
            } => {
                db.report_github_publication(
                    auth,
                    &Command { key, epoch },
                    id(&publication)?,
                    &report,
                )
                .await
            }
            Self::PublicationCancel {
                key,
                epoch,
                publication,
            } => {
                db.cancel_github_publication(auth, &Command { key, epoch }, id(&publication)?)
                    .await
            }
            Self::PublicationStatus { epoch, publication } => {
                db.github_publication_status(auth, epoch, id(&publication)?, false)
                    .await
            }
            Self::PublicationContext { epoch, publication } => {
                db.github_publication_status(auth, epoch, id(&publication)?, true)
                    .await
            }
            Self::ProjectionRequest {
                key,
                epoch,
                projection,
                binding,
                specification,
            } => {
                db.request_github_projection(
                    auth,
                    &Command { key, epoch },
                    id(&projection)?,
                    id(&binding)?,
                    &specification,
                )
                .await
            }
            Self::ProjectionCancel {
                key,
                epoch,
                projection,
            } => {
                db.cancel_github_projection(auth, &Command { key, epoch }, id(&projection)?)
                    .await
            }
            Self::ProjectionStatus { epoch, projection } => {
                db.github_projection_status(auth, epoch, id(&projection)?)
                    .await
            }
            Self::ProjectionContext { epoch, projection } => {
                db.github_projection_context(auth, epoch, id(&projection)?)
                    .await
            }
            Self::ImportRequest {
                key,
                epoch,
                import,
                binding,
                specification,
            } => {
                db.request_github_import(
                    auth,
                    &Command { key, epoch },
                    id(&import)?,
                    id(&binding)?,
                    &specification,
                )
                .await
            }
            Self::ImportCancel { key, epoch, import } => {
                db.cancel_github_import(auth, &Command { key, epoch }, id(&import)?)
                    .await
            }
            Self::ImportStatus { epoch, import } => {
                db.github_import_status(auth, epoch, id(&import)?).await
            }
            Self::ImportContext { epoch, import } => {
                db.github_import_context(auth, epoch, id(&import)?).await
            }
            Self::ImportBegin {
                key,
                epoch,
                import,
                native_commit,
            } => {
                db.begin_github_import_upload(
                    auth,
                    &Command { key, epoch },
                    id(&import)?,
                    backend.ok_or(Error::Invalid("storage not configured"))?,
                    native_commit
                        .parse()
                        .map_err(|_| Error::Invalid("native import commit"))?,
                )
                .await
            }
            Self::ImportFinish { key, epoch, import } => {
                db.finish_github_import(auth, &Command { key, epoch }, id(&import)?)
                    .await
            }
            Self::Bind {
                key,
                epoch,
                binding,
                specification,
            } => {
                db.bind_github(auth, &Command { key, epoch }, id(&binding)?, &specification)
                    .await
            }
            Self::Revoke {
                key,
                epoch,
                binding,
            } => {
                db.revoke_github(auth, &Command { key, epoch }, id(&binding)?)
                    .await
            }
            Self::Status { epoch, binding } => {
                db.github_binding_status(auth, epoch, id(&binding)?).await
            }
            Self::ProbeBegin {
                key,
                epoch,
                binding,
                probe,
            } => {
                db.begin_github_probe(auth, &Command { key, epoch }, id(&binding)?, id(&probe)?)
                    .await
            }
            Self::ProbeContext {
                epoch,
                binding,
                probe,
            } => {
                db.github_probe_context(auth, epoch, id(&binding)?, id(&probe)?)
                    .await
            }
            Self::ProbeFinish {
                key,
                epoch,
                binding,
                probe,
                observation,
            } => {
                db.finish_github_probe(
                    auth,
                    &Command { key, epoch },
                    id(&binding)?,
                    id(&probe)?,
                    &observation,
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_wire_requires_exact_git_revision_and_owner_bridge_separation() {
        let value: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/github-import-v1.json"))
                .unwrap();
        let command: WireCommand = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(command.permission(), Permission::RepositoryWrite);
        let WireCommand::ImportRequest {
            import,
            binding,
            specification,
            ..
        } = command
        else {
            panic!()
        };
        id(&import).unwrap();
        id(&binding).unwrap();
        specification.validate().unwrap();
        for revision in [
            "main",
            "HEAD",
            "git:sha1:123",
            "native:sha256:7777777777777777777777777777777777777777777777777777777777777777",
        ] {
            let mut spec = specification.clone();
            spec.git_revision = revision.into();
            assert!(spec.validate().is_err());
        }
        for field in ["token", "url", "path", "backend", "force"] {
            let mut invalid = value.clone();
            invalid["specification"][field] = json!("forbidden");
            assert!(serde_json::from_value::<WireCommand>(invalid).is_err());
        }
        for (action, permission) in [
            ("import_cancel", Permission::RepositoryWrite),
            ("import_status", Permission::Read),
            ("import_context", Permission::HostRead),
            ("import_begin", Permission::HostRead),
            ("import_finish", Permission::HostRead),
        ] {
            let mut value = json!({"action":action,"epoch":1,"import":import});
            if !matches!(action, "import_status" | "import_context") {
                value["key"] = json!("request");
            }
            if action == "import_begin" {
                value["native_commit"] =
                    json!(treeharbor_core::ObjectId::from_bytes([7; 32]).to_string());
            }
            assert_eq!(
                serde_json::from_value::<WireCommand>(value)
                    .unwrap()
                    .permission(),
                permission
            );
        }
    }

    #[test]
    fn binding_wire_rejects_secret_fields_and_noncanonical_ids() {
        let fixture = include_str!("../../tests/fixtures/github-bind-v1.json");
        let command: WireCommand = serde_json::from_str(fixture).unwrap();
        assert_eq!(command.permission(), Permission::RepositoryWrite);
        let WireCommand::Bind {
            binding,
            specification,
            ..
        } = command
        else {
            panic!("wrong fixture action")
        };
        id(&binding).unwrap();
        specification.validate().unwrap();
        let observation: Observation = serde_json::from_slice(include_bytes!(
            "../../../integrations-github/tests/fixtures/probe-v1.json"
        ))
        .unwrap();
        observation.validate(&specification).unwrap();
        for invalid in [
            "00000000-0000-0000-0000-000000000000",
            "aaaaaaaaaaaa4aaa8aaaaaaaaaaaaaaa",
            "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
        ] {
            assert!(id(invalid).is_err());
        }
        for field in ["token", "private_key", "api_url"] {
            let mut value: Value = serde_json::from_str(fixture).unwrap();
            value["specification"][field] = json!("never accepted");
            assert!(serde_json::from_value::<WireCommand>(value).is_err());
        }
        let mut value: Value = serde_json::from_str(fixture).unwrap();
        value["permission"] = json!("host.publish");
        assert!(serde_json::from_value::<WireCommand>(value).is_err());
        for action in ["probe_begin", "probe_context", "probe_finish"] {
            let mut value = json!({"action":action,"epoch":1,"binding":binding,"probe":binding});
            if action != "probe_context" {
                value["key"] = json!("request");
            }
            if action == "probe_finish" {
                value["observation"] = json!(observation);
            }
            assert_eq!(
                serde_json::from_value::<WireCommand>(value)
                    .unwrap()
                    .permission(),
                Permission::HostRead
            );
        }
    }
}
