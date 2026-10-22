use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireCommand {
    ProgramPut {
        key: String,
        epoch: i64,
        program: CheckProgram,
    },
    CheckPrepare {
        key: String,
        epoch: i64,
        run: String,
    },
    CheckStart {
        key: String,
        epoch: i64,
        run: String,
    },
    CheckCollect {
        key: String,
        epoch: i64,
        run: String,
    },
    CheckStatus {
        run: String,
    },
    PolicyPut {
        key: String,
        epoch: i64,
        policy: Policy,
    },
    RefCreate {
        key: String,
        epoch: i64,
        name: String,
        head: String,
        policy: String,
    },
    RefPolicy {
        key: String,
        epoch: i64,
        name: String,
        generation: i64,
        policy: String,
    },
    RefResolve {
        name: String,
    },
    Enqueue {
        key: String,
        epoch: i64,
        entry: String,
        target: String,
        changeset: String,
        version: i64,
    },
    QueueStatus {
        entry: String,
    },
    Cancel {
        key: String,
        epoch: i64,
        entry: String,
    },
    CandidateStart {
        key: String,
        epoch: i64,
        entry: String,
        candidate: String,
    },
    CandidateStatus {
        candidate: String,
    },
    CandidateFail {
        key: String,
        epoch: i64,
        candidate: String,
        reason: String,
        conflict: bool,
    },
    CheckAssign {
        key: String,
        epoch: i64,
        candidate: String,
        name: String,
        run: String,
        validator: String,
    },
    ValidationReport {
        key: String,
        epoch: i64,
        run: String,
        receipt: String,
        outcome: String,
        evidence: String,
    },
    ValidationRevoke {
        key: String,
        epoch: i64,
        run: String,
    },
    Review {
        key: String,
        epoch: i64,
        candidate: String,
        review: String,
        decision: String,
    },
    ReviewRevoke {
        key: String,
        epoch: i64,
        review: String,
    },
    Land {
        key: String,
        epoch: i64,
        candidate: String,
    },
}
fn uuid(s: &str) -> Result<Uuid> {
    let id: Uuid = s.parse().map_err(|_| Error::Invalid("native id"))?;
    if id.to_string() != s {
        return Err(Error::Invalid("native id"));
    }
    Ok(id)
}
fn native(s: &str) -> Result<ObjectId> {
    match s.parse().map_err(|_| Error::Invalid("native revision"))? {
        RevisionRef::Native(id) => Ok(id),
        _ => Err(Error::Invalid("native revision")),
    }
}
impl WireCommand {
    pub fn permission(&self) -> Permission {
        match self {
            Self::ProgramPut { .. } | Self::PolicyPut { .. } | Self::RefPolicy { .. } => {
                Permission::PolicyWrite
            }
            Self::RefCreate { .. } => Permission::RepositoryWrite,
            Self::Enqueue { .. } | Self::Cancel { .. } => Permission::QueueRequest,
            Self::CandidateStart { .. }
            | Self::CandidateFail { .. }
            | Self::CheckAssign { .. }
            | Self::Land { .. } => Permission::IntegrationControl,
            Self::CheckPrepare { .. }
            | Self::CheckStart { .. }
            | Self::CheckCollect { .. }
            | Self::ValidationReport { .. }
            | Self::ValidationRevoke { .. } => Permission::ValidationReport,
            Self::Review { .. } | Self::ReviewRevoke { .. } => Permission::Review,
            _ => Permission::Read,
        }
    }
    pub async fn execute(self, db: &mut Metadata, auth: &Authorization) -> Result<Value> {
        match self {
            Self::ProgramPut {
                key,
                epoch,
                program,
            } => {
                db.put_native_check_program(auth, &Command { key, epoch }, &program)
                    .await
            }
            Self::CheckPrepare { key, epoch, run } => {
                db.prepare_native_check(auth, &Command { key, epoch }, uuid(&run)?)
                    .await
            }
            Self::CheckStart { key, epoch, run } => {
                db.start_native_check(auth, &Command { key, epoch }, uuid(&run)?)
                    .await
            }
            Self::CheckCollect { key, epoch, run } => {
                db.collect_native_check(auth, &Command { key, epoch }, uuid(&run)?)
                    .await
            }
            Self::CheckStatus { run } => db.native_check_execution_status(auth, uuid(&run)?).await,
            Self::PolicyPut { key, epoch, policy } => {
                db.put_native_policy(auth, &Command { key, epoch }, &policy)
                    .await
            }
            Self::RefCreate {
                key,
                epoch,
                name,
                head,
                policy,
            } => {
                db.create_native_ref(
                    auth,
                    &Command { key, epoch },
                    &name,
                    native(&head)?,
                    object(&policy)?,
                )
                .await
            }
            Self::RefPolicy {
                key,
                epoch,
                name,
                generation,
                policy,
            } => {
                db.change_native_policy(
                    auth,
                    &Command { key, epoch },
                    &name,
                    generation,
                    object(&policy)?,
                )
                .await
            }
            Self::RefResolve { name } => db.resolve_native_ref(auth, &name).await,
            Self::Enqueue {
                key,
                epoch,
                entry,
                target,
                changeset,
                version,
            } => {
                db.enqueue_native_change(
                    auth,
                    &Command { key, epoch },
                    uuid(&entry)?,
                    &target,
                    uuid(&changeset)?,
                    version,
                )
                .await
            }
            Self::QueueStatus { entry } => db.native_queue_status(auth, uuid(&entry)?).await,
            Self::Cancel { key, epoch, entry } => {
                db.cancel_native_entry(auth, &Command { key, epoch }, uuid(&entry)?)
                    .await
            }
            Self::CandidateStart {
                key,
                epoch,
                entry,
                candidate,
            } => {
                db.start_native_candidate(
                    auth,
                    &Command { key, epoch },
                    uuid(&entry)?,
                    uuid(&candidate)?,
                )
                .await
            }
            Self::CandidateStatus { candidate } => {
                db.native_candidate_status(auth, uuid(&candidate)?).await
            }
            Self::CandidateFail {
                key,
                epoch,
                candidate,
                reason,
                conflict,
            } => {
                db.fail_native_candidate(
                    auth,
                    &Command { key, epoch },
                    uuid(&candidate)?,
                    &reason,
                    conflict,
                )
                .await
            }
            Self::CheckAssign {
                key,
                epoch,
                candidate,
                name,
                run,
                validator,
            } => {
                db.assign_native_check(
                    auth,
                    &Command { key, epoch },
                    uuid(&candidate)?,
                    &name,
                    uuid(&run)?,
                    &validator,
                )
                .await
            }
            Self::ValidationReport {
                key,
                epoch,
                run,
                receipt,
                outcome,
                evidence,
            } => {
                db.report_native_validation(
                    auth,
                    &Command { key, epoch },
                    uuid(&run)?,
                    uuid(&receipt)?,
                    &outcome,
                    object(&evidence)?,
                )
                .await
            }
            Self::ValidationRevoke { key, epoch, run } => {
                db.revoke_native_validation(auth, &Command { key, epoch }, uuid(&run)?)
                    .await
            }
            Self::Review {
                key,
                epoch,
                candidate,
                review,
                decision,
            } => {
                db.record_native_review(
                    auth,
                    &Command { key, epoch },
                    uuid(&candidate)?,
                    uuid(&review)?,
                    &decision,
                )
                .await
            }
            Self::ReviewRevoke { key, epoch, review } => {
                db.revoke_native_review(auth, &Command { key, epoch }, uuid(&review)?)
                    .await
            }
            Self::Land {
                key,
                epoch,
                candidate,
            } => {
                db.land_native_candidate(auth, &Command { key, epoch }, uuid(&candidate)?)
                    .await
            }
        }
    }
}

fn object(s: &str) -> Result<ObjectId> {
    s.parse().map_err(|_| Error::Invalid("object id"))
}
