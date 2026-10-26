use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireCommand {
    CheckDispatchPolicy {
        epoch: i64,
        publication: String,
    },
    QueueHostPolicyContext {
        epoch: i64,
        queue: String,
        membership_sequence: i64,
    },
    QueueHostPolicies {
        epoch: i64,
        queue: String,
        after: i64,
    },
    QueueAdmissionEvidence {
        epoch: i64,
        queue: String,
        check_sequence: i64,
        publication_sequence: Option<i64>,
        host_policy_sequence: Option<i64>,
    },
    AdmissionPolicyPlan {
        epoch: i64,
        binding: String,
        base_ref: String,
        expected_generation: i64,
        definition: AdmissionPolicy,
    },
    AdmissionPolicySet {
        key: String,
        epoch: i64,
        binding: String,
        base_ref: String,
        expected_generation: i64,
        definition: Option<AdmissionPolicy>,
    },
    AdmissionPolicy {
        epoch: i64,
        binding: String,
        base_ref: String,
    },
    CheckPublicationRequest {
        key: String,
        epoch: i64,
        publication: String,
        specification: CheckPublicationSpec,
    },
    CheckPublicationBegin {
        key: String,
        epoch: i64,
        publication: String,
        attempt: String,
    },
    CheckPublicationDispatch {
        key: String,
        epoch: i64,
        publication: String,
        attempt: String,
        preflight: Box<treeharbor_github_contract::check_publication::Preflight>,
    },
    CheckPublicationReport {
        key: String,
        epoch: i64,
        publication: String,
        attempt: String,
        report: Box<treeharbor_github_contract::check_publication::Report>,
    },
    CheckPublicationStatus {
        epoch: i64,
        publication: String,
    },
    CheckPublicationContext {
        epoch: i64,
        publication: String,
    },
    CheckPublicationCancel {
        key: String,
        epoch: i64,
        publication: String,
    },
    ManagedCheckApprovePull {
        key: String,
        epoch: i64,
        check_run: String,
        specification: ManagedPullCheckSpec,
    },
    ManagedCheckApprove {
        key: String,
        epoch: i64,
        check_run: String,
        specification: ManagedCheckSpec,
    },
    ManagedCheckStatus {
        epoch: i64,
        check_run: String,
    },
    ManagedCheckRevoke {
        key: String,
        epoch: i64,
        check_run: String,
    },
    ManagedCheckPrepare {
        key: String,
        epoch: i64,
        check_run: String,
    },
    ManagedCheckStart {
        key: String,
        epoch: i64,
        check_run: String,
    },
    ManagedCheckCollect {
        key: String,
        epoch: i64,
        check_run: String,
    },
    ManagedCheckRelease {
        key: String,
        epoch: i64,
        check_run: String,
    },

    RecoveryRequest {
        key: String,
        epoch: i64,
        recovery: String,
        specification: RecoverySpec,
    },
    RecoveryCancel {
        key: String,
        epoch: i64,
        recovery: String,
    },
    RecoveryStatus {
        epoch: i64,
        recovery: String,
        after: i64,
    },
    RecoveryContext {
        epoch: i64,
        recovery: String,
        after: i64,
    },
    RecoveryPage {
        key: String,
        epoch: i64,
        recovery: String,
        report: Box<treeharbor_github_contract::deliveries::DeliveryPageReport>,
    },
    RecoveryDetails {
        key: String,
        epoch: i64,
        recovery: String,
        details: Box<treeharbor_github_contract::deliveries::DeliveryDetails>,
    },
    RedeliveryBegin {
        key: String,
        epoch: i64,
        recovery: String,
        provider_id: i64,
        dispatch: String,
    },
    RedeliveryContext {
        epoch: i64,
        recovery: String,
        provider_id: i64,
        dispatch: String,
    },
    RedeliveryReport {
        key: String,
        epoch: i64,
        recovery: String,
        provider_id: i64,
        dispatch: String,
        report: RedeliveryReport,
    },
    EventChannelCreate {
        key: String,
        epoch: i64,
        channel: String,
        specification: EventChannelSpec,
    },
    EventChannelRevoke {
        key: String,
        epoch: i64,
        channel: String,
    },
    EventPage {
        epoch: i64,
        channel: String,
        after: i64,
    },
    DequeueRequest {
        key: String,
        epoch: i64,
        dequeue: String,
        specification: DequeueSpec,
    },
    DequeueBegin {
        key: String,
        epoch: i64,
        dequeue: String,
        attempt: String,
    },
    DequeueReport {
        key: String,
        epoch: i64,
        dequeue: String,
        attempt: String,
        report: Box<treeharbor_github_contract::dequeue::DequeueObservation>,
    },
    DequeueStatus {
        epoch: i64,
        dequeue: String,
    },
    DequeueContext {
        epoch: i64,
        dequeue: String,
    },
    DequeueDispatchContext {
        epoch: i64,
        dequeue: String,
        attempt: String,
    },
    CheckPolicySet {
        key: String,
        epoch: i64,
        binding: String,
        base_ref: String,
        expected_generation: i64,
        definition: Option<treeharbor_github_contract::workflow_definitions::Policy>,
    },
    CheckPolicy {
        epoch: i64,
        binding: String,
        base_ref: String,
    },
    QueuePublicationContext {
        epoch: i64,
        queue: String,
        membership_sequence: i64,
    },
    QueuePublications {
        epoch: i64,
        queue: String,
        after: i64,
    },
    QueueDefinitionContext {
        epoch: i64,
        queue: String,
        attempt_sequence: i64,
    },
    QueueDefinitions {
        epoch: i64,
        queue: String,
        after: i64,
    },
    QueueRequest {
        key: String,
        epoch: i64,
        queue: String,
        specification: QueueSpec,
    },
    QueueBegin {
        key: String,
        epoch: i64,
        queue: String,
        attempt: String,
    },
    QueueReport {
        key: String,
        epoch: i64,
        queue: String,
        attempt: String,
        report: Box<treeharbor_github_contract::queue::QueueObservation>,
    },
    QueueCancel {
        key: String,
        epoch: i64,
        queue: String,
    },
    QueueStatus {
        epoch: i64,
        queue: String,
    },
    QueueGroups {
        epoch: i64,
        queue: String,
        after: i64,
    },
    QueueCheckContext {
        epoch: i64,
        queue: String,
        membership_sequence: i64,
        side: treeharbor_github_contract::group_checks::Side,
    },
    QueueChecks {
        epoch: i64,
        queue: String,
        after: i64,
    },
    QueueAttemptContext {
        epoch: i64,
        queue: String,
        selection: treeharbor_github_contract::group_attempts::Selection,
    },
    QueueAttempts {
        epoch: i64,
        queue: String,
        after: i64,
    },
    QueueContext {
        epoch: i64,
        queue: String,
    },
    QueueDispatchContext {
        epoch: i64,
        queue: String,
        attempt: String,
    },
    InspectionRequest {
        key: String,
        epoch: i64,
        inspection: String,
        specification: InspectionSpec,
    },
    InspectionReport {
        key: String,
        epoch: i64,
        inspection: String,
        report: Box<treeharbor_github_contract::InspectionReport>,
    },
    InspectionCancel {
        key: String,
        epoch: i64,
        inspection: String,
    },
    InspectionStatus {
        epoch: i64,
        inspection: String,
    },
    InspectionContext {
        epoch: i64,
        inspection: String,
    },
    InspectionCollectContext {
        epoch: i64,
        inspection: String,
    },
    PullRequest {
        key: String,
        epoch: i64,
        pull: String,
        specification: PullSpec,
    },
    PullBegin {
        key: String,
        epoch: i64,
        pull: String,
        attempt: String,
    },
    PullReport {
        key: String,
        epoch: i64,
        pull: String,
        report: PullReport,
    },
    PullCancel {
        key: String,
        epoch: i64,
        pull: String,
    },
    PullStatus {
        epoch: i64,
        pull: String,
    },
    PullContext {
        epoch: i64,
        pull: String,
    },
    PullDispatchContext {
        epoch: i64,
        pull: String,
        attempt: String,
    },
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
pub(super) fn id(s: &str) -> Result<Uuid> {
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
            Self::AdmissionPolicySet { .. } | Self::AdmissionPolicyPlan { .. } => {
                Permission::PolicyWrite
            }
            Self::AdmissionPolicy { .. } | Self::QueueAdmissionEvidence { .. } => Permission::Read,
            Self::CheckPublicationRequest { .. } | Self::CheckPublicationCancel { .. } => {
                Permission::HostQueue
            }
            Self::CheckPublicationStatus { .. } | Self::CheckDispatchPolicy { .. } => {
                Permission::Read
            }
            Self::CheckPublicationContext { .. }
            | Self::CheckPublicationBegin { .. }
            | Self::CheckPublicationDispatch { .. }
            | Self::CheckPublicationReport { .. } => Permission::HostCheck,
            Self::ManagedCheckApprove { .. }
            | Self::ManagedCheckApprovePull { .. }
            | Self::ManagedCheckRevoke { .. } => Permission::PolicyWrite,
            Self::ManagedCheckStatus { .. } => Permission::Read,
            Self::ManagedCheckPrepare { .. }
            | Self::ManagedCheckStart { .. }
            | Self::ManagedCheckCollect { .. }
            | Self::ManagedCheckRelease { .. } => Permission::ValidationReport,
            Self::RecoveryRequest { .. } | Self::RecoveryCancel { .. } => {
                Permission::RepositoryWrite
            }
            Self::RecoveryStatus { .. } => Permission::Read,
            Self::RecoveryContext { .. }
            | Self::RecoveryPage { .. }
            | Self::RecoveryDetails { .. }
            | Self::RedeliveryBegin { .. }
            | Self::RedeliveryContext { .. }
            | Self::RedeliveryReport { .. } => Permission::HostRecover,
            Self::EventChannelCreate { .. } | Self::EventChannelRevoke { .. } => {
                Permission::RepositoryWrite
            }
            Self::EventPage { .. } => Permission::Read,
            Self::DequeueRequest { .. } => Permission::HostQueue,
            Self::DequeueStatus { .. } => Permission::Read,
            Self::DequeueBegin { .. }
            | Self::DequeueReport { .. }
            | Self::DequeueContext { .. }
            | Self::DequeueDispatchContext { .. } => Permission::HostMerge,
            Self::CheckPolicySet { .. } => Permission::PolicyWrite,
            Self::QueueHostPolicies { .. }
            | Self::CheckPolicy { .. }
            | Self::QueueDefinitions { .. }
            | Self::QueuePublications { .. } => Permission::Read,
            Self::QueueHostPolicyContext { .. }
            | Self::QueueDefinitionContext { .. }
            | Self::QueuePublicationContext { .. } => Permission::HostMerge,
            Self::QueueRequest { .. } | Self::QueueCancel { .. } => Permission::HostQueue,
            Self::QueueBegin { .. }
            | Self::QueueReport { .. }
            | Self::QueueContext { .. }
            | Self::QueueDispatchContext { .. }
            | Self::QueueCheckContext { .. }
            | Self::QueueAttemptContext { .. } => Permission::HostMerge,
            Self::QueueStatus { .. }
            | Self::QueueGroups { .. }
            | Self::QueueChecks { .. }
            | Self::QueueAttempts { .. } => Permission::Read,
            Self::InspectionRequest { .. } | Self::InspectionCancel { .. } => {
                Permission::RepositoryWrite
            }
            Self::InspectionStatus { .. } => Permission::Read,
            Self::InspectionReport { .. }
            | Self::InspectionContext { .. }
            | Self::InspectionCollectContext { .. } => Permission::HostRead,
            Self::PullRequest { .. } | Self::PullCancel { .. } => Permission::RepositoryWrite,
            Self::PullStatus { .. } => Permission::Read,
            Self::PullBegin { .. }
            | Self::PullReport { .. }
            | Self::PullContext { .. }
            | Self::PullDispatchContext { .. } => Permission::HostPublish,
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
            Self::QueueHostPolicyContext {
                epoch,
                queue,
                membership_sequence,
            } => {
                db.github_queue_host_policy_context(auth, epoch, id(&queue)?, membership_sequence)
                    .await
            }
            Self::QueueHostPolicies {
                epoch,
                queue,
                after,
            } => {
                db.github_queue_host_policies(auth, epoch, id(&queue)?, after)
                    .await
            }
            Self::QueueAdmissionEvidence {
                epoch,
                queue,
                check_sequence,
                publication_sequence,
                host_policy_sequence,
            } => {
                db.github_queue_admission_with_host_policy(
                    auth,
                    epoch,
                    id(&queue)?,
                    check_sequence,
                    publication_sequence,
                    host_policy_sequence,
                )
                .await
            }
            Self::AdmissionPolicyPlan {
                epoch,
                binding,
                base_ref,
                expected_generation,
                definition,
            } => {
                db.plan_github_admission_policy(
                    auth,
                    epoch,
                    id(&binding)?,
                    &base_ref,
                    expected_generation,
                    &definition,
                )
                .await
            }
            Self::AdmissionPolicySet {
                key,
                epoch,
                binding,
                base_ref,
                expected_generation,
                definition,
            } => {
                db.set_github_admission_policy(
                    auth,
                    &Command { key, epoch },
                    id(&binding)?,
                    &base_ref,
                    expected_generation,
                    definition.as_ref(),
                )
                .await
            }
            Self::AdmissionPolicy {
                epoch,
                binding,
                base_ref,
            } => {
                db.github_admission_policy(auth, epoch, id(&binding)?, &base_ref)
                    .await
            }
            Self::CheckPublicationRequest {
                key,
                epoch,
                publication,
                specification,
            } => {
                db.request_github_check_publication(
                    auth,
                    &Command { key, epoch },
                    id(&publication)?,
                    &specification,
                )
                .await
            }
            Self::CheckPublicationBegin {
                key,
                epoch,
                publication,
                attempt,
            } => {
                db.begin_github_check_publication(
                    auth,
                    &Command { key, epoch },
                    id(&publication)?,
                    id(&attempt)?,
                )
                .await
            }
            Self::CheckPublicationDispatch {
                key,
                epoch,
                publication,
                attempt,
                preflight,
            } => {
                db.dispatch_github_check_publication(
                    auth,
                    &Command { key, epoch },
                    id(&publication)?,
                    id(&attempt)?,
                    &preflight,
                )
                .await
            }
            Self::CheckPublicationReport {
                key,
                epoch,
                publication,
                attempt,
                report,
            } => {
                db.report_github_check_publication(
                    auth,
                    &Command { key, epoch },
                    id(&publication)?,
                    id(&attempt)?,
                    &report,
                )
                .await
            }
            Self::CheckPublicationStatus { epoch, publication } => {
                db.github_check_publication_status(auth, epoch, id(&publication)?, false)
                    .await
            }
            Self::CheckDispatchPolicy { epoch, publication } => {
                db.github_check_dispatch_policy(auth, epoch, id(&publication)?)
                    .await
            }
            Self::CheckPublicationContext { epoch, publication } => {
                db.github_check_publication_status(auth, epoch, id(&publication)?, true)
                    .await
            }
            Self::CheckPublicationCancel {
                key,
                epoch,
                publication,
            } => {
                db.cancel_github_check_publication(auth, &Command { key, epoch }, id(&publication)?)
                    .await
            }
            Self::ManagedCheckApprovePull {
                key,
                epoch,
                check_run,
                specification,
            } => {
                db.approve_github_managed_pull_check(
                    auth,
                    &Command { key, epoch },
                    id(&check_run)?,
                    &specification,
                )
                .await
            }
            Self::ManagedCheckApprove {
                key,
                epoch,
                check_run,
                specification,
            } => {
                db.approve_github_managed_check(
                    auth,
                    &Command { key, epoch },
                    id(&check_run)?,
                    &specification,
                )
                .await
            }
            Self::ManagedCheckStatus { epoch, check_run } => {
                db.github_managed_check_status(auth, epoch, id(&check_run)?)
                    .await
            }
            Self::ManagedCheckRevoke {
                key,
                epoch,
                check_run,
            } => {
                db.revoke_github_managed_check(auth, &Command { key, epoch }, id(&check_run)?)
                    .await
            }
            Self::ManagedCheckPrepare {
                key,
                epoch,
                check_run,
            } => {
                db.prepare_github_managed_check(auth, &Command { key, epoch }, id(&check_run)?)
                    .await
            }
            Self::ManagedCheckStart {
                key,
                epoch,
                check_run,
            } => {
                db.start_github_managed_check(auth, &Command { key, epoch }, id(&check_run)?)
                    .await
            }
            Self::ManagedCheckCollect {
                key,
                epoch,
                check_run,
            } => {
                db.collect_github_managed_check(auth, &Command { key, epoch }, id(&check_run)?)
                    .await
            }
            Self::ManagedCheckRelease {
                key,
                epoch,
                check_run,
            } => {
                db.release_github_managed_check(auth, &Command { key, epoch }, id(&check_run)?)
                    .await
            }

            Self::RecoveryRequest {
                key,
                epoch,
                recovery,
                specification,
            } => {
                db.request_github_recovery(
                    auth,
                    &Command { key, epoch },
                    id(&recovery)?,
                    &specification,
                )
                .await
            }
            Self::RecoveryCancel {
                key,
                epoch,
                recovery,
            } => {
                db.cancel_github_recovery(auth, &Command { key, epoch }, id(&recovery)?)
                    .await
            }
            Self::RecoveryStatus {
                epoch,
                recovery,
                after,
            } => {
                db.github_recovery_context(auth, epoch, id(&recovery)?, after, false)
                    .await
            }
            Self::RecoveryContext {
                epoch,
                recovery,
                after,
            } => {
                db.github_recovery_context(auth, epoch, id(&recovery)?, after, true)
                    .await
            }
            Self::RecoveryPage {
                key,
                epoch,
                recovery,
                report,
            } => {
                db.record_github_recovery_page(
                    auth,
                    &Command { key, epoch },
                    id(&recovery)?,
                    &report,
                )
                .await
            }
            Self::RecoveryDetails {
                key,
                epoch,
                recovery,
                details,
            } => {
                db.record_github_recovery_details(
                    auth,
                    &Command { key, epoch },
                    id(&recovery)?,
                    &details,
                )
                .await
            }
            Self::RedeliveryBegin {
                key,
                epoch,
                recovery,
                provider_id,
                dispatch,
            } => {
                db.begin_github_redelivery(
                    auth,
                    &Command { key, epoch },
                    id(&recovery)?,
                    provider_id,
                    id(&dispatch)?,
                )
                .await
            }
            Self::RedeliveryContext {
                epoch,
                recovery,
                provider_id,
                dispatch,
            } => {
                db.github_redelivery_context(
                    auth,
                    epoch,
                    id(&recovery)?,
                    provider_id,
                    id(&dispatch)?,
                )
                .await
            }
            Self::RedeliveryReport {
                key,
                epoch,
                recovery,
                provider_id,
                dispatch,
                report,
            } => {
                db.report_github_redelivery(
                    auth,
                    &Command { key, epoch },
                    id(&recovery)?,
                    provider_id,
                    id(&dispatch)?,
                    &report,
                )
                .await
            }
            Self::EventChannelCreate {
                key,
                epoch,
                channel,
                specification,
            } => {
                db.create_github_event_channel(
                    auth,
                    &Command { key, epoch },
                    id(&channel)?,
                    &specification,
                )
                .await
            }
            Self::EventChannelRevoke {
                key,
                epoch,
                channel,
            } => {
                db.revoke_github_event_channel(auth, &Command { key, epoch }, id(&channel)?)
                    .await
            }
            Self::EventPage {
                epoch,
                channel,
                after,
            } => {
                db.github_event_page(auth, epoch, id(&channel)?, after)
                    .await
            }
            Self::DequeueRequest {
                key,
                epoch,
                dequeue,
                specification,
            } => {
                db.request_github_dequeue(
                    auth,
                    &Command { key, epoch },
                    id(&dequeue)?,
                    &specification,
                )
                .await
            }
            Self::DequeueBegin {
                key,
                epoch,
                dequeue,
                attempt,
            } => {
                db.begin_github_dequeue(auth, &Command { key, epoch }, id(&dequeue)?, id(&attempt)?)
                    .await
            }
            Self::DequeueReport {
                key,
                epoch,
                dequeue,
                attempt,
                report,
            } => {
                db.report_github_dequeue(
                    auth,
                    &Command { key, epoch },
                    id(&dequeue)?,
                    id(&attempt)?,
                    &report,
                )
                .await
            }
            Self::DequeueStatus { epoch, dequeue } => {
                db.github_dequeue_context(auth, epoch, id(&dequeue)?, false, None)
                    .await
            }
            Self::DequeueContext { epoch, dequeue } => {
                db.github_dequeue_context(auth, epoch, id(&dequeue)?, true, None)
                    .await
            }
            Self::DequeueDispatchContext {
                epoch,
                dequeue,
                attempt,
            } => {
                db.github_dequeue_context(auth, epoch, id(&dequeue)?, true, Some(id(&attempt)?))
                    .await
            }
            Self::QueueRequest {
                key,
                epoch,
                queue,
                specification,
            } => {
                db.request_github_queue(auth, &Command { key, epoch }, id(&queue)?, &specification)
                    .await
            }
            Self::QueueBegin {
                key,
                epoch,
                queue,
                attempt,
            } => {
                db.begin_github_queue(auth, &Command { key, epoch }, id(&queue)?, id(&attempt)?)
                    .await
            }
            Self::QueueReport {
                key,
                epoch,
                queue,
                attempt,
                report,
            } => {
                db.report_github_queue(
                    auth,
                    &Command { key, epoch },
                    id(&queue)?,
                    id(&attempt)?,
                    &report,
                )
                .await
            }
            Self::QueueCancel { key, epoch, queue } => {
                db.cancel_github_queue(auth, &Command { key, epoch }, id(&queue)?)
                    .await
            }
            Self::QueueStatus { epoch, queue } => {
                db.github_queue_context(auth, epoch, id(&queue)?, false, None)
                    .await
            }
            Self::QueueGroups {
                epoch,
                queue,
                after,
            } => {
                db.github_queue_groups(auth, epoch, id(&queue)?, after)
                    .await
            }
            Self::QueueCheckContext {
                epoch,
                queue,
                membership_sequence,
                side,
            } => {
                db.github_queue_check_context(auth, epoch, id(&queue)?, membership_sequence, side)
                    .await
            }
            Self::QueueChecks {
                epoch,
                queue,
                after,
            } => {
                db.github_queue_checks(auth, epoch, id(&queue)?, after)
                    .await
            }
            Self::QueueAttemptContext {
                epoch,
                queue,
                selection,
            } => {
                db.github_queue_attempt_context(auth, epoch, id(&queue)?, &selection)
                    .await
            }
            Self::QueueAttempts {
                epoch,
                queue,
                after,
            } => {
                db.github_queue_attempts(auth, epoch, id(&queue)?, after)
                    .await
            }
            Self::CheckPolicySet {
                key,
                epoch,
                binding,
                base_ref,
                expected_generation,
                definition,
            } => {
                db.set_github_check_policy(
                    auth,
                    &Command { key, epoch },
                    id(&binding)?,
                    &base_ref,
                    expected_generation,
                    definition.as_ref(),
                )
                .await
            }
            Self::CheckPolicy {
                epoch,
                binding,
                base_ref,
            } => {
                db.github_check_policy(auth, epoch, id(&binding)?, &base_ref)
                    .await
            }
            Self::QueuePublicationContext {
                epoch,
                queue,
                membership_sequence,
            } => {
                db.github_queue_publication_context(auth, epoch, id(&queue)?, membership_sequence)
                    .await
            }
            Self::QueuePublications {
                epoch,
                queue,
                after,
            } => {
                db.github_queue_publications(auth, epoch, id(&queue)?, after)
                    .await
            }
            Self::QueueDefinitionContext {
                epoch,
                queue,
                attempt_sequence,
            } => {
                db.github_queue_definition_context(auth, epoch, id(&queue)?, attempt_sequence)
                    .await
            }
            Self::QueueDefinitions {
                epoch,
                queue,
                after,
            } => {
                db.github_queue_definitions(auth, epoch, id(&queue)?, after)
                    .await
            }
            Self::QueueContext { epoch, queue } => {
                db.github_queue_context(auth, epoch, id(&queue)?, true, None)
                    .await
            }
            Self::QueueDispatchContext {
                epoch,
                queue,
                attempt,
            } => {
                db.github_queue_context(auth, epoch, id(&queue)?, true, Some(id(&attempt)?))
                    .await
            }
            Self::InspectionRequest {
                key,
                epoch,
                inspection,
                specification,
            } => {
                db.request_github_inspection(
                    auth,
                    &Command { key, epoch },
                    id(&inspection)?,
                    &specification,
                )
                .await
            }
            Self::InspectionReport {
                key,
                epoch,
                inspection,
                report,
            } => {
                db.report_github_inspection(
                    auth,
                    &Command { key, epoch },
                    id(&inspection)?,
                    &report,
                )
                .await
            }
            Self::InspectionCancel {
                key,
                epoch,
                inspection,
            } => {
                db.cancel_github_inspection(auth, &Command { key, epoch }, id(&inspection)?)
                    .await
            }
            Self::InspectionStatus { epoch, inspection } => {
                db.github_inspection_context(auth, epoch, id(&inspection)?, false, false)
                    .await
            }
            Self::InspectionContext { epoch, inspection } => {
                db.github_inspection_context(auth, epoch, id(&inspection)?, true, false)
                    .await
            }
            Self::InspectionCollectContext { epoch, inspection } => {
                db.github_inspection_context(auth, epoch, id(&inspection)?, true, true)
                    .await
            }
            Self::PullRequest {
                key,
                epoch,
                pull,
                specification,
            } => {
                db.request_github_pull(auth, &Command { key, epoch }, id(&pull)?, &specification)
                    .await
            }
            Self::PullBegin {
                key,
                epoch,
                pull,
                attempt,
            } => {
                db.begin_github_pull(auth, &Command { key, epoch }, id(&pull)?, id(&attempt)?)
                    .await
            }
            Self::PullReport {
                key,
                epoch,
                pull,
                report,
            } => {
                db.report_github_pull(auth, &Command { key, epoch }, id(&pull)?, &report)
                    .await
            }
            Self::PullCancel { key, epoch, pull } => {
                db.cancel_github_pull(auth, &Command { key, epoch }, id(&pull)?)
                    .await
            }
            Self::PullStatus { epoch, pull } => {
                db.github_pull_context(auth, epoch, id(&pull)?, false, None)
                    .await
            }
            Self::PullContext { epoch, pull } => {
                db.github_pull_context(auth, epoch, id(&pull)?, true, None)
                    .await
            }
            Self::PullDispatchContext {
                epoch,
                pull,
                attempt,
            } => {
                db.github_pull_context(auth, epoch, id(&pull)?, true, Some(id(&attempt)?))
                    .await
            }
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
    fn event_channel_wire_is_owner_scoped_and_cannot_forge_authenticated_delivery() {
        let value: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/github-event-channel-v1.json"
        ))
        .unwrap();
        let c: WireCommand = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(c.permission(), Permission::RepositoryWrite);
        for field in ["verified", "body", "signature", "tenant", "app_id"] {
            let mut bad = value.clone();
            bad[field] = json!(true);
            assert!(serde_json::from_value::<WireCommand>(bad).is_err());
        }
        assert!(
            serde_json::from_value::<WireCommand>(
                json!({"action":"event_receive","verified":true})
            )
            .is_err()
        );
        let c: WireCommand = serde_json::from_value(
            json!({"action":"event_page","epoch":1,"channel":value["channel"],"after":0}),
        )
        .unwrap();
        assert_eq!(c.permission(), Permission::Read);
    }
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
