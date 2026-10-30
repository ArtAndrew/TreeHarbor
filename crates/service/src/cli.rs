//! Trusted local administration is deliberately separate from agent HTTP commands.
use crate::{
    Error, Result,
    client::{self, Credential},
    config::Config,
    database::Database,
    files,
    server::Server,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::{ffi::OsString, os::unix::ffi::OsStrExt, path::PathBuf, time::Duration};
use treeharbor_metadata::{
    Authority, Scope,
    access::{Permission, PrincipalKind},
};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "treeharbor",
    version,
    about = "TreeHarbor development transport (local Unix socket)"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Create private local service configuration/storage directories, without starting anything.
    Init(crate::init::Options),
    /// Trusted operator cold backup. Requires explicitly stopped writers.
    Backup {
        #[command(subcommand)]
        action: crate::backup::Action,
    },
    /// Inspect local source-service prerequisites without installing or migrating anything.
    Doctor {
        #[arg(long)]
        config: PathBuf,
    },
    /// Serve agent tools over stdio using a fixed repository-scoped credential.
    Mcp {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long, default_value_t=15_000, value_parser=clap::value_parser!(u64).range(100..=60_000))]
        timeout_ms: u64,
    },
    /// Receive signed GitHub callbacks on a separate private Unix socket.
    WebhookServe {
        #[arg(long)]
        config: PathBuf,
    },
    #[command(hide = true)]
    InternalGitWorker,
    /// Trusted GitHub App connection; never run inside an agent workspace/runtime.
    Github {
        #[command(subcommand)]
        action: crate::github::Action,
    },
    /// Import the full ancestry and trees of one exact Git commit from a complete bundle.
    GitImport {
        #[arg(long)]
        bundle: PathBuf,
        /// Exact git:sha1:<40 hex> or git:sha256:<64 hex> revision, never a branch name.
        #[arg(long)]
        commit: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "/usr/bin/git")]
        git: PathBuf,
        #[arg(long, default_value_t=300, value_parser=clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
        #[arg(long, default_value_t = 100_000)]
        max_objects: usize,
        #[arg(long, default_value_t = 4_294_967_296)]
        max_bytes: u64,
    },
    /// Project a completed local native pack into a self-contained Git bundle.
    /// This exports source history, not server reviews, checks or authority.
    GitExport {
        #[arg(long)]
        pack: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "/usr/bin/git")]
        git: PathBuf,
        #[arg(long, default_value = "main")]
        branch: String,
        #[arg(long, default_value = "sha1", value_parser = ["sha1", "sha256"])]
        object_format: String,
        #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },
    /// Trusted local preparation and bounded runtime node.
    Node {
        #[command(subcommand)]
        action: crate::node_cli::Action,
    },
    /// Capture explicitly selected files from a stopped native source directory.
    SourcePack {
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long = "path", required = true)]
        paths: Vec<OsString>,
        #[arg(long)]
        author_name: String,
        #[arg(long)]
        author_email: String,
        #[arg(long, default_value = "Native source import")]
        message: String,
        #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
        timestamp_seconds: i64,
        #[arg(long, default_value_t = 100_000)]
        max_objects: u64,
        #[arg(long, default_value_t = 4_294_967_296)]
        max_bytes: u64,
    },
    /// Serve authenticated commands. Requires an explicitly migrated database.
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
    /// Trusted database administration; never expose this command to an agent.
    Admin {
        #[arg(long)]
        config: PathBuf,
        #[command(subcommand)]
        action: Admin,
    },
    /// Use a repository-scoped credential without database access.
    Client {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long, default_value_t=15_000, value_parser=clap::value_parser!(u64).range(100..=60_000))]
        timeout_ms: u64,
        #[command(subcommand)]
        action: Agent,
    },
}
#[derive(Args)]
struct Repository {
    #[arg(long)]
    tenant: String,
    #[arg(long)]
    repository: Uuid,
}
impl Repository {
    fn scope(&self) -> Result<Scope> {
        Scope::new(&self.tenant, self.repository).map_err(|_| Error::Config)
    }
}
#[derive(Clone, Copy, ValueEnum)]
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
#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    Human,
    Agent,
    Node,
    Validator,
    Bridge,
    Integrator,
}
impl From<Kind> for PrincipalKind {
    fn from(value: Kind) -> Self {
        match value {
            Kind::Human => Self::Human,
            Kind::Agent => Self::Agent,
            Kind::Node => Self::Node,
            Kind::Validator => Self::Validator,
            Kind::Bridge => Self::Bridge,
            Kind::Integrator => Self::Integrator,
        }
    }
}
#[derive(Clone, Copy, ValueEnum)]
enum Grant {
    Read,
    #[value(name = "workspace.write")]
    WorkspaceWrite,
    #[value(name = "change.write")]
    ChangeWrite,
    #[value(name = "queue.request")]
    QueueRequest,
    #[value(name = "repository.write")]
    RepositoryWrite,
    #[value(name = "runtime.control")]
    RuntimeControl,
    #[value(name = "validation.report")]
    ValidationReport,
    #[value(name = "host.publish")]
    HostPublish,
    #[value(name = "host.read")]
    HostRead,
    #[value(name = "host.queue")]
    HostQueue,
    #[value(name = "host.merge")]
    HostMerge,
    #[value(name = "host.recover")]
    HostRecover,
    #[value(name = "host.check")]
    HostCheck,
    Review,
    #[value(name = "policy.write")]
    PolicyWrite,
    #[value(name = "integration.control")]
    IntegrationControl,
}
impl From<Grant> for Permission {
    fn from(value: Grant) -> Self {
        match value {
            Grant::Read => Self::Read,
            Grant::WorkspaceWrite => Self::WorkspaceWrite,
            Grant::ChangeWrite => Self::ChangeWrite,
            Grant::QueueRequest => Self::QueueRequest,
            Grant::RepositoryWrite => Self::RepositoryWrite,
            Grant::RuntimeControl => Self::RuntimeControl,
            Grant::ValidationReport => Self::ValidationReport,
            Grant::HostPublish => Self::HostPublish,
            Grant::HostRead => Self::HostRead,
            Grant::HostQueue => Self::HostQueue,
            Grant::HostMerge => Self::HostMerge,
            Grant::HostRecover => Self::HostRecover,
            Grant::HostCheck => Self::HostCheck,
            Grant::Review => Self::Review,
            Grant::PolicyWrite => Self::PolicyWrite,
            Grant::IntegrationControl => Self::IntegrationControl,
        }
    }
}
#[derive(Subcommand)]
enum Admin {
    /// Approve an immutable runtime image and resource policy for a repository.
    RuntimeProfileRegister {
        #[command(flatten)]
        scope: Repository,
        #[arg(long)]
        file: PathBuf,
    },
    /// Fence future starts and request cleanup of runs using this profile.
    RuntimeProfileRevoke {
        #[command(flatten)]
        scope: Repository,
        #[arg(long)]
        profile_digest: String,
    },
    /// Bind an existing Node principal to its initialized storage UUID.
    NodeRegister {
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        principal: String,
        #[arg(long)]
        storage: Uuid,
    },
    /// Install/upgrade the checked schema in the configured, existing schema.
    Migrate,
    RepositoryCreate {
        #[command(flatten)]
        scope: Repository,
        #[arg(long, value_enum)]
        authority: Mode,
    },
    PrincipalCreate {
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        principal: String,
        #[arg(long, value_enum)]
        kind: Kind,
    },
    /// Replace permissions. Revoking all requires an explicit flag.
    AccessSet {
        #[command(flatten)]
        scope: Repository,
        #[arg(long)]
        principal: String,
        #[arg(
            long,
            value_enum,
            required_unless_present = "revoke_all",
            conflicts_with = "revoke_all"
        )]
        permission: Vec<Grant>,
        #[arg(long)]
        revoke_all: bool,
    },
    /// Write a new credential file (0600), never a secret on stdout or argv.
    TokenIssue {
        #[command(flatten)]
        scope: Repository,
        #[arg(long)]
        principal: String,
        #[arg(long, value_enum, required = true)]
        permission: Vec<Grant>,
        #[arg(long, default_value_t=3600, value_parser=clap::value_parser!(i32).range(1..=7_776_000))]
        ttl_seconds: i32,
        #[arg(long)]
        output: PathBuf,
    },
    TokenRevoke {
        #[command(flatten)]
        scope: Repository,
        #[arg(long)]
        token_id: Uuid,
    },
    PrincipalDisable {
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        principal: String,
    },
}
#[derive(Args)]
struct Mutation {
    /// Preserve this key on retry, including after a timeout or restart.
    #[arg(long)]
    key: String,
    #[arg(long, value_enum)]
    authority: Mode,
    #[arg(long, value_parser=clap::value_parser!(i64).range(1..))]
    epoch: i64,
}
#[derive(Args)]
struct SourceContext {
    #[arg(long)]
    workspace: Uuid,
    #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
    generation: i64,
    #[arg(long, value_enum)]
    authority: Mode,
    #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
    epoch: i64,
}
impl SourceContext {
    fn fence(self) -> treeharbor_metadata::workspace_reads::SourceFence {
        treeharbor_metadata::workspace_reads::SourceFence {
            workspace: self.workspace,
            generation: self.generation,
            authority: self.authority.into(),
            epoch: self.epoch,
        }
    }
}
#[derive(Subcommand)]
enum HistoryQuery {
    /// Complete ancestry, paginated in deterministic child-before-parent order.
    Log {
        #[arg(long)]
        revision: String,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t=100, value_parser=clap::value_parser!(u16).range(1..=256))]
        limit: u16,
        /// Display only the first parent chain; ancestry queries still use all parents.
        #[arg(long)]
        first_parent: bool,
    },
    /// Read a commit proven reachable from the registered revision.
    Show {
        #[arg(long)]
        revision: String,
        #[arg(long)]
        commit: String,
    },
    IsAncestor {
        #[arg(long)]
        ancestor: String,
        #[arg(long)]
        head: String,
    },
    /// Return every best common ancestor, including criss-cross histories.
    MergeBases {
        #[arg(long)]
        left: String,
        #[arg(long)]
        right: String,
    },
}
impl HistoryQuery {
    fn wire(self) -> Value {
        match self {
            Self::Log {
                revision,
                after,
                limit,
                first_parent,
            } => {
                json!({"kind":"log","revision":revision,"after":after,"limit":limit,"first_parent":first_parent})
            }
            Self::Show { revision, commit } => {
                json!({"kind":"show","revision":revision,"commit":commit})
            }
            Self::IsAncestor { ancestor, head } => {
                json!({"kind":"is_ancestor","ancestor":ancestor,"head":head})
            }
            Self::MergeBases { left, right } => {
                json!({"kind":"merge_bases","left":left,"right":right})
            }
        }
    }
}
#[derive(Subcommand)]
enum DiffQuery {
    /// Compare a prepared integration candidate against its captured target head.
    Candidate {
        #[arg(long)]
        candidate: Uuid,
    },
    Change {
        #[arg(long)]
        changeset: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        version: i64,
    },
    Revisions {
        #[arg(long)]
        before: String,
        #[arg(long)]
        after: String,
    },
}
impl DiffQuery {
    fn wire(self) -> Value {
        match self {
            Self::Candidate { candidate } => {
                json!({"kind":"candidate","candidate":candidate.to_string()})
            }
            Self::Change { changeset, version } => {
                json!({"kind":"change","changeset":changeset.to_string(),"version":version})
            }
            Self::Revisions { before, after } => {
                json!({"kind":"revisions","before":before,"after":after})
            }
        }
    }
}
#[derive(Subcommand)]
enum Agent {
    /// Review an exact proposal, integration candidate or revision pair; no mutations.
    Diff {
        #[arg(long, value_enum)]
        authority: Mode,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[arg(long,default_value_t=20,value_parser=clap::value_parser!(u16).range(1..=100))]
        limit: u16,
        #[arg(long)]
        after: Option<String>,
        #[arg(long)]
        metadata_only: bool,
        #[command(subcommand)]
        query: DiffQuery,
    },
    /// Reapply an owned accepted version onto an exact registered native revision.
    RebaseStart {
        #[arg(long)]
        key: String,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[arg(long)]
        rebase: Uuid,
        #[arg(long)]
        changeset: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        source_version: i64,
        #[arg(long)]
        onto: String,
    },
    /// Compute and durably accept the rebased version. Never moves a branch.
    RebasePrepare {
        #[arg(long)]
        key: String,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[arg(long)]
        rebase: Uuid,
    },
    RebaseStatus {
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[arg(long)]
        rebase: Uuid,
    },
    RebaseCancel {
        #[arg(long)]
        key: String,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[arg(long)]
        rebase: Uuid,
    },
    /// Query native history with scoped read permission; never changes refs.
    History {
        #[arg(long, value_enum)]
        authority: Mode,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[command(subcommand)]
        query: HistoryQuery,
    },
    /// Download a complete registered native revision from the live service.
    SourceExport {
        #[arg(long)]
        revision: String,
        #[arg(long, value_enum)]
        authority: Mode,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[arg(long)]
        output: PathBuf,
    },
    /// Journal and resume preparation of one pending native queue entry.
    NativeAdvance {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        target: String,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
    },
    /// Compute a durable merge candidate from server-owned, immutable inputs.
    NativePrepare {
        #[arg(long)]
        key: String,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[arg(long)]
        candidate: Uuid,
    },
    RepositoryInfo,
    /// Configure or inspect a Connected GitHub binding using private JSON.
    Github {
        #[arg(long)]
        request: PathBuf,
    },
    /// Execute one native policy/ref/queue/review/check operation from private JSON.
    Native {
        #[arg(long)]
        request: PathBuf,
    },
    /// Save generated task/workspace identities before creation; reuse --state on retry.
    ApiRunNew {
        #[arg(long)]
        state: PathBuf,
        #[arg(long, value_enum)]
        authority: Mode,
        #[arg(long, value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[arg(long)]
        base: String,
        #[arg(long)]
        profile_digest: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        task: String,
    },
    /// Record an external task and pin its exact source; does not execute code.
    ApiRunCreate {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        workspace: Uuid,
        #[arg(long)]
        run: Uuid,
        #[arg(long)]
        base: String,
        #[arg(long)]
        profile_digest: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        task: String,
    },
    ApiRunStatus {
        #[arg(long)]
        run: Uuid,
    },
    /// Close the server context; does not stop any external process.
    ApiRunClose {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        run: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        generation: i64,
    },
    /// Request a run from exactly prepared files and an operator-approved profile.
    RunRequest {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        run: Uuid,
        #[arg(long)]
        workspace: Uuid,
        #[arg(long, value_parser=clap::value_parser!(i64).range(1..))]
        generation: i64,
        #[arg(long)]
        profile_digest: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        task: String,
        /// Explicit command after --; evaluated only inside the approved sandbox.
        #[arg(last=true, required=true, num_args=1..=128)]
        argv: Vec<String>,
    },
    RunStatus {
        #[arg(long)]
        run: Uuid,
    },
    /// Request cancellation; files remain pinned until runtime removal is observed.
    RunStop {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        run: Uuid,
    },
    /// Read a baseline file (or link bytes) into a new private regular file.
    SourceFile {
        #[command(flatten)]
        source: SourceContext,
        #[arg(long)]
        path: OsString,
        #[arg(long)]
        output: PathBuf,
    },
    /// List one baseline directory page; empty path selects the root.
    SourceList {
        #[command(flatten)]
        source: SourceContext,
        #[arg(long, default_value = "")]
        path: OsString,
        #[arg(long)]
        after_hex: Option<String>,
        #[arg(long,default_value_t=128,value_parser=clap::value_parser!(u16).range(1..=256))]
        limit: u16,
    },
    /// Import a completed source pack; keep the same state file to resume.
    SourceImport {
        #[arg(long)]
        pack: PathBuf,
        #[arg(long)]
        state: PathBuf,
        #[arg(long, value_enum)]
        authority: Mode,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[arg(long,default_value_t=3600,value_parser=clap::value_parser!(i32).range(1..=3600))]
        ttl_seconds: i32,
    },
    /// Stop a managed run and request capture of explicit file/subtree changes.
    SealRequest {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        seal: Uuid,
        #[arg(long)]
        run: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        generation: i64,
        #[arg(long)]
        replace: Vec<OsString>,
        #[arg(long)]
        delete: Vec<OsString>,
        #[arg(long)]
        exclude: Vec<OsString>,
        #[arg(long, default_value_t = 100_000)]
        max_objects: i64,
        #[arg(long, default_value_t = 268_435_456)]
        max_bytes: i64,
        #[arg(long, default_value_t = 3600)]
        ttl_seconds: i32,
    },
    SealStatus {
        #[arg(long)]
        seal: Uuid,
    },
    SealCancel {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        seal: Uuid,
    },
    /// Accept an immutable ready capture as a new ChangeSet version; no CI reuse.
    SealAccept {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        seal: Uuid,
        #[arg(long)]
        changeset: Uuid,
        #[arg(long,default_value_t=0,value_parser=clap::value_parser!(i64).range(0..))]
        previous_version: i64,
        #[arg(long,default_value_t=300,value_parser=clap::value_parser!(i32).range(1..=3600))]
        ttl_seconds: i32,
    },
    /// Reserve a change upload for an exact run and workspace generation.
    ChangeBegin {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        upload: Uuid,
        #[arg(long)]
        run: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        generation: i64,
        #[arg(long, default_value_t = 10_000)]
        max_objects: i64,
        #[arg(long, default_value_t = 268_435_456)]
        max_bytes: i64,
        #[arg(long, default_value_t = 3600)]
        ttl_seconds: i32,
    },
    /// Upload replacement bytes from a private regular file; prints its object ID.
    ChangePut {
        #[arg(long)]
        upload: Uuid,
        #[arg(long)]
        file: PathBuf,
    },
    /// Submit explicit operations; the server computes and retains the result.
    ChangeSubmit {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        upload: Uuid,
        #[arg(long)]
        changeset: Uuid,
        #[arg(long,default_value_t=0,value_parser=clap::value_parser!(i64).range(0..))]
        previous_version: i64,
        #[arg(long)]
        operations: PathBuf,
    },
    ChangeStatus {
        #[arg(long)]
        upload: Uuid,
    },
    ChangeAbort {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        upload: Uuid,
    },
    /// Begin a resumable native source upload; does not publish a branch.
    SourceBegin {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        upload: Uuid,
        #[arg(long, default_value_t = 100_000)]
        max_objects: i64,
        #[arg(long, default_value_t = 4_294_967_296)]
        max_bytes: i64,
        #[arg(long, default_value_t = 3600)]
        ttl_seconds: i32,
    },
    /// Send one canonical object frame, children first.
    SourcePut {
        #[arg(long)]
        upload: Uuid,
        #[arg(long)]
        object: String,
        #[arg(long)]
        file: PathBuf,
    },
    SourceFinish {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        upload: Uuid,
        #[arg(long)]
        commit: String,
    },
    SourceAbort {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        upload: Uuid,
    },
    SourceStatus {
        #[arg(long)]
        upload: Uuid,
    },
    /// Store a full or explicit sparse source-delivery profile.
    ProfilePut {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long, required_unless_present = "paths", conflicts_with = "paths")]
        full: bool,
        #[arg(long = "path", required_unless_present = "full")]
        paths: Vec<OsString>,
    },
    ProfileGet {
        #[arg(long)]
        profile_digest: String,
    },
    /// Save a generated workspace identity before requesting preparation; reuse --state on retry.
    WorkspaceNew {
        #[arg(long)]
        state: PathBuf,
        #[arg(long, value_enum)]
        authority: Mode,
        #[arg(long, value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[arg(long)]
        base: String,
        #[arg(long)]
        profile_digest: String,
    },
    /// Enqueue preparation from a registered native base; does not imply READY.
    WorkspaceCreate {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        workspace: Uuid,
        #[arg(long)]
        base: String,
        #[arg(long)]
        profile_digest: String,
    },
    WorkspaceStatus {
        #[arg(long)]
        workspace: Uuid,
    },
    /// Begin draining; the assigned node must separately confirm physical cleanup.
    WorkspaceDelete {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        workspace: Uuid,
        #[arg(long, value_parser=clap::value_parser!(i64).range(1..))]
        generation: i64,
    },
    OperationStatus {
        #[arg(long)]
        key: String,
    },
}
impl Agent {
    fn json(self) -> Result<Value> {
        Ok(match self {
            Self::RebaseStart {
                key,
                epoch,
                rebase,
                changeset,
                source_version,
                onto,
            } => {
                json!({"method":"native","request":{"action":"rebase_start","key":key,"epoch":epoch,"rebase":rebase.to_string(),"changeset":changeset.to_string(),"source_version":source_version,"onto":onto}})
            }
            Self::RebaseStatus { epoch, rebase } => {
                json!({"method":"native","request":{"action":"rebase_status","epoch":epoch,"rebase":rebase.to_string()}})
            }
            Self::RebaseCancel { key, epoch, rebase } => {
                json!({"method":"native","request":{"action":"rebase_cancel","key":key,"epoch":epoch,"rebase":rebase.to_string()}})
            }
            Self::Native { request } => {
                let bytes = files::read_private(
                    &request,
                    treeharbor_metadata::commands::MAX_REQUEST_BYTES,
                )?;
                let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Config)?;
                let _: treeharbor_metadata::integration::WireCommand =
                    serde_json::from_slice(&bytes).map_err(|_| Error::Config)?;
                json!({"method":"native","request":value})
            }
            Self::Github { request } => {
                let bytes = files::read_private(
                    &request,
                    treeharbor_metadata::commands::MAX_REQUEST_BYTES,
                )?;
                let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Config)?;
                let _: treeharbor_metadata::github::WireCommand =
                    serde_json::from_slice(&bytes).map_err(|_| Error::Config)?;
                json!({"method":"github","request":value})
            }
            Self::RepositoryInfo => json!({"method":"repository_info"}),
            Self::SealRequest {
                mutation,
                seal,
                run,
                generation,
                replace,
                delete,
                exclude,
                max_objects,
                max_bytes,
                ttl_seconds,
            } => {
                let decode = |paths: Vec<OsString>| {
                    paths
                        .into_iter()
                        .map(|p| {
                            treeharbor_core::RepoPath::new(p.as_bytes().to_vec())
                                .map_err(|_| Error::Config)
                        })
                        .collect::<Result<Vec<_>>>()
                };
                let selection = treeharbor_core::CaptureSelection::new(
                    decode(replace)?,
                    decode(delete)?,
                    decode(exclude)?,
                )
                .map_err(|_| Error::Config)?;
                let hex = |paths: &[treeharbor_core::RepoPath]| {
                    paths
                        .iter()
                        .map(|p| crate::pack::hex(p.as_bytes()))
                        .collect::<Vec<_>>()
                };
                json!({"method":"seal_request","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"seal":seal.to_string(),"run":run.to_string(),"generation":generation,
                    "selection":{"replace_hex":hex(selection.replace()),"delete_hex":hex(selection.delete()),"exclude_hex":hex(selection.exclude())},"max_objects":max_objects,"max_bytes":max_bytes,"ttl_seconds":ttl_seconds})
            }
            Self::SealStatus { seal } => json!({"method":"seal_status","seal":seal.to_string()}),
            Self::SealCancel { mutation, seal } => {
                json!({"method":"seal_cancel","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"seal":seal.to_string()})
            }
            Self::ChangeBegin {
                mutation,
                upload,
                run,
                generation,
                max_objects,
                max_bytes,
                ttl_seconds,
            } => {
                json!({"method":"change_begin","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,
                    "upload":upload.to_string(),"run":run.to_string(),"generation":generation,"max_objects":max_objects,"max_bytes":max_bytes,"ttl_seconds":ttl_seconds})
            }
            Self::ChangeStatus { upload } => {
                json!({"method":"change_status","upload":upload.to_string()})
            }
            Self::ChangeAbort { mutation, upload } => {
                json!({"method":"change_abort","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"upload":upload.to_string()})
            }
            Self::SourceExport { .. }
            | Self::History { .. }
            | Self::Diff { .. }
            | Self::NativeAdvance { .. }
            | Self::NativePrepare { .. }
            | Self::RebasePrepare { .. }
            | Self::SealAccept { .. }
            | Self::ChangePut { .. }
            | Self::ChangeSubmit { .. } => {
                return Err(Error::Config);
            }
            Self::ApiRunCreate {
                mutation,
                workspace,
                run,
                base,
                profile_digest,
                agent,
                task,
            } => {
                json!({"method":"api_run_create","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,
                    "workspace":workspace.to_string(),"run":run.to_string(),"base":base,"profile_digest":profile_digest,"agent":agent,"task":task})
            }
            Self::ApiRunStatus { run } => json!({"method":"api_run_status","run":run.to_string()}),
            Self::ApiRunClose {
                mutation,
                run,
                generation,
            } => {
                json!({"method":"api_run_close","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"run":run.to_string(),"generation":generation})
            }
            Self::RunRequest {
                mutation,
                run,
                workspace,
                generation,
                profile_digest,
                agent,
                task,
                argv,
            } => {
                json!({"method":"run_request","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,
                    "run":run.to_string(),"workspace":workspace.to_string(),"generation":generation,
                    "profile_digest":profile_digest,"agent":agent,"task":task,"argv":argv})
            }
            Self::RunStatus { run } => json!({"method":"run_status","run":run.to_string()}),
            Self::RunStop { mutation, run } => {
                json!({"method":"run_stop","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"run":run.to_string()})
            }
            Self::SourceBegin {
                mutation,
                upload,
                max_objects,
                max_bytes,
                ttl_seconds,
            } => {
                json!({"method":"source_begin","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"upload":upload.to_string(),"max_objects":max_objects,"max_bytes":max_bytes,"ttl_seconds":ttl_seconds})
            }
            Self::SourceFinish {
                mutation,
                upload,
                commit,
            } => {
                json!({"method":"source_finish","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"upload":upload.to_string(),"commit":commit})
            }
            Self::SourceAbort { mutation, upload } => {
                json!({"method":"source_abort","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"upload":upload.to_string()})
            }
            Self::SourceStatus { upload } => {
                json!({"method":"source_status","upload":upload.to_string()})
            }
            Self::ApiRunNew { .. }
            | Self::WorkspaceNew { .. }
            | Self::SourcePut { .. }
            | Self::SourceImport { .. }
            | Self::SourceFile { .. }
            | Self::SourceList { .. } => return Err(Error::Config), // separate data path below
            Self::ProfilePut {
                mutation,
                full,
                paths,
            } => {
                let profile = if full {
                    treeharbor_core::SourceProfile::full()
                } else {
                    let paths = paths
                        .into_iter()
                        .map(|path| treeharbor_core::RepoPath::new(path.as_bytes().to_vec()))
                        .collect::<treeharbor_core::Result<Vec<_>>>()
                        .map_err(|_| Error::Profile)?;
                    treeharbor_core::SourceProfile::sparse(paths).map_err(|_| Error::Profile)?
                };
                json!({"method":"source_profile_put","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"profile":treeharbor_metadata::profiles::describe(&profile)})
            }
            Self::ProfileGet { profile_digest } => {
                json!({"method":"source_profile_get","profile_digest":profile_digest})
            }
            Self::WorkspaceStatus { workspace } => {
                json!({"method":"workspace_status","workspace":workspace.to_string()})
            }
            Self::OperationStatus { key } => json!({"method":"operation_status","key":key}),
            Self::WorkspaceCreate {
                mutation,
                workspace,
                base,
                profile_digest,
            } => {
                json!({"method":"workspace_create","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"workspace":workspace.to_string(),"base":base,"profile_digest":profile_digest})
            }
            Self::WorkspaceDelete {
                mutation,
                workspace,
                generation,
            } => {
                json!({"method":"workspace_delete","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"workspace":workspace.to_string(),"generation":generation})
            }
        })
    }
}
pub async fn run(cli: Cli) -> Result<bool> {
    match cli.command {
        Command::Init(options) => {
            let report = crate::init::create(options)?;
            println!("{report}");
            Ok(true)
        }
        Command::Backup { action } => crate::backup::run(action).await,
        Command::Doctor { config } => {
            let report = crate::doctor::inspect(&config).await;
            let passed = report["passed"] == true;
            println!("{report}");
            Ok(passed)
        }
        Command::InternalGitWorker => {
            treeharbor_git_bridge::download::worker()?;
            Ok(true)
        }
        Command::Github { action } => crate::github::run(action).await,
        Command::GitImport {
            bundle,
            commit,
            output,
            git,
            timeout_seconds,
            max_objects,
            max_bytes,
        } => {
            let treeharbor_core::RevisionRef::Git(commit) =
                commit.parse().map_err(|_| Error::Config)?
            else {
                return Err(Error::Config);
            };
            let result = tokio::task::spawn_blocking(move || {
                crate::pack::capture_git(crate::pack::GitCapture {
                    git: &git,
                    bundle: &bundle,
                    commit,
                    output: &output,
                    limits: treeharbor_git_bridge::Limits {
                        objects: max_objects,
                        source_bytes: max_bytes,
                        timeout: Duration::from_secs(timeout_seconds),
                        ..Default::default()
                    },
                })
            })
            .await
            .map_err(|_| Error::ImportWorker)??;
            println!(
                "{}",
                json!({"revision":treeharbor_core::RevisionRef::Native(result.commit()).to_string(),"snapshot":result.snapshot().to_string(),"objects":result.object_count(),"encoded_bytes":result.encoded_bytes(),"filesystem_entries":result.filesystem_entries()})
            );
            Ok(true)
        }
        Command::GitExport {
            pack,
            output,
            git,
            branch,
            object_format,
            timeout_seconds,
        } => {
            let manifest = tokio::task::spawn_blocking(move || -> Result<_> {
                let pack = crate::pack::Pack::open(&pack)?;
                Ok(treeharbor_git_bridge::export(
                    pack.store(),
                    treeharbor_git_bridge::Export {
                        git: &git,
                        output: &output,
                        refs: std::collections::BTreeMap::from([(
                            format!("refs/heads/{branch}"),
                            pack.commit(),
                        )]),
                        algorithm: if object_format == "sha256" {
                            treeharbor_git_bridge::HashAlgorithm::Sha256
                        } else {
                            treeharbor_git_bridge::HashAlgorithm::Sha1
                        },
                        limits: treeharbor_git_bridge::Limits {
                            timeout: Duration::from_secs(timeout_seconds),
                            ..Default::default()
                        },
                    },
                )?)
            })
            .await
            .map_err(|_| Error::ExportWorker)??;
            println!(
                "{}",
                serde_json::to_string(&manifest).map_err(|_| Error::Config)?
            );
            Ok(true)
        }
        Command::Node { action } => crate::node_cli::run(action).await,
        Command::SourcePack {
            directory,
            output,
            paths,
            author_name,
            author_email,
            message,
            timestamp_seconds,
            max_objects,
            max_bytes,
        } => {
            let paths = paths
                .into_iter()
                .map(|p| treeharbor_core::RepoPath::new(p.as_bytes().to_vec()))
                .collect::<treeharbor_core::Result<Vec<_>>>()
                .map_err(|_| Error::Profile)?;
            let result = tokio::task::spawn_blocking(move || {
                crate::pack::capture(crate::pack::Capture {
                    directory: &directory,
                    output: &output,
                    paths,
                    author_name,
                    author_email,
                    message,
                    timestamp_seconds,
                    limits: crate::pack::Limits {
                        objects: max_objects,
                        bytes: max_bytes,
                    },
                })
            })
            .await
            .map_err(|_| Error::ImportWorker)??;
            println!(
                "{}",
                json!({"pack":result.directory.to_str(),"pack_path_hex":crate::pack::hex(result.directory.as_os_str().as_bytes()),"commit":result.commit().to_string(),"snapshot":result.snapshot().to_string(),"objects":result.object_count(),"encoded_bytes":result.encoded_bytes(),"filesystem_entries":result.filesystem_entries()})
            );
            Ok(true)
        }
        Command::Mcp {
            socket,
            credential_file,
            timeout_ms,
        } => {
            crate::mcp::serve(socket, credential_file, Duration::from_millis(timeout_ms)).await?;
            Ok(true)
        }
        Command::WebhookServe { config } => {
            crate::webhooks::serve(&config).await?;
            Ok(true)
        }
        Command::Serve { config } => {
            let config = Config::load(&config)?;
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            let mut interrupt =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
            let server = Server::bind(config).await?;
            println!("{}", json!({"status":"listening","socket":server.socket()}));
            server
                .run(async {
                    tokio::select! { _=terminate.recv()=>{}, _=interrupt.recv()=>{} }
                })
                .await?;
            Ok(true)
        }
        Command::Client {
            socket,
            credential_file,
            timeout_ms,
            action,
        } => {
            let credential = Credential::load(&credential_file)?;
            let action = match action {
                Agent::ApiRunNew {
                    state,
                    authority,
                    epoch,
                    base,
                    profile_digest,
                    agent,
                    task,
                } => {
                    let reply = crate::api_run_new::send(crate::api_run_new::Request {
                        state: &state,
                        socket: &socket,
                        credential: &credential,
                        authority: authority.into(),
                        epoch,
                        base: &base,
                        profile_digest: &profile_digest,
                        agent: &agent,
                        task: &task,
                        timeout: Duration::from_millis(timeout_ms),
                    })
                    .await?;
                    println!("{}", reply.body);
                    return Ok(reply.status.is_success());
                }
                Agent::NativeAdvance {
                    state,
                    target,
                    epoch,
                } => {
                    let reply = crate::native_advance::send(crate::native_advance::Request {
                        state: &state,
                        socket: &socket,
                        credential: &credential,
                        target: &target,
                        epoch,
                        timeout: Duration::from_millis(timeout_ms),
                    })
                    .await?;
                    println!("{}", reply.body);
                    return Ok(reply.status.is_success());
                }
                Agent::WorkspaceNew {
                    state,
                    authority,
                    epoch,
                    base,
                    profile_digest,
                } => {
                    let reply = crate::workspace_new::send(crate::workspace_new::Request {
                        state: &state,
                        socket: &socket,
                        credential: &credential,
                        authority: authority.into(),
                        epoch,
                        base: &base,
                        profile_digest: &profile_digest,
                        timeout: Duration::from_millis(timeout_ms),
                    })
                    .await?;
                    println!("{}", reply.body);
                    return Ok(reply.status.is_success());
                }
                Agent::Diff {
                    authority,
                    epoch,
                    limit,
                    after,
                    metadata_only,
                    query,
                } => {
                    let reply=client::review(&socket,&credential,json!({"authority":Authority::from(authority).as_str(),"epoch":epoch,"query":query.wire(),"limit":limit,"after":after,"text":!metadata_only}),Duration::from_millis(timeout_ms)).await?;
                    println!("{}", reply.body);
                    return Ok(reply.status.is_success());
                }
                Agent::History {
                    authority,
                    epoch,
                    query,
                } => {
                    let reply=client::history(&socket,&credential,json!({"authority":Authority::from(authority).as_str(),"epoch":epoch,"query":query.wire()}),Duration::from_millis(timeout_ms)).await?;
                    println!("{}", reply.body);
                    return Ok(reply.status.is_success());
                }
                Agent::SourceExport {
                    revision,
                    authority,
                    epoch,
                    output,
                } => {
                    let treeharbor_core::RevisionRef::Native(revision) =
                        revision.parse().map_err(|_| Error::Config)?
                    else {
                        return Err(Error::Config);
                    };
                    let reply = crate::archive::download(
                        &socket,
                        &credential,
                        treeharbor_metadata::exports::ExportFence {
                            authority: authority.into(),
                            epoch,
                            revision,
                        },
                        &output,
                        Duration::from_millis(timeout_ms),
                    )
                    .await?;
                    println!("{}", reply.body);
                    return Ok(reply.status.is_success());
                }
                Agent::SourceFile {
                    source,
                    path,
                    output,
                } => {
                    let path = treeharbor_core::RepoPath::new(path.as_bytes().to_vec())
                        .map_err(|_| Error::Config)?;
                    // Validate the destination before fetching; publication still
                    // uses atomic no-clobber, including a concurrent file creator.
                    files::private_directory(output.parent().ok_or(Error::UnsafeFile)?)?;
                    if output.try_exists()? {
                        return Err(Error::Config);
                    }
                    let reply = crate::download::read(
                        &socket,
                        &credential,
                        source.fence(),
                        json!({"kind":"file","path_hex":crate::pack::hex(path.as_bytes())}),
                        Duration::from_millis(timeout_ms),
                    )
                    .await?;
                    if let Some(bytes) = reply.content {
                        tokio::task::spawn_blocking(move || files::write_private(&output, &bytes))
                            .await
                            .map_err(|_| Error::Http)??;
                    }
                    println!("{}", reply.metadata);
                    return Ok(reply.status.is_success());
                }
                Agent::SourceList {
                    source,
                    path,
                    after_hex,
                    limit,
                } => {
                    if !path.is_empty() {
                        treeharbor_core::RepoPath::new(path.as_bytes().to_vec())
                            .map_err(|_| Error::Config)?;
                    }
                    let reply=crate::download::read(&socket,&credential,source.fence(),json!({"kind":"list","path_hex":crate::pack::hex(path.as_bytes()),"after_hex":after_hex,"limit":limit}),Duration::from_millis(timeout_ms)).await?;
                    println!("{}", reply.metadata);
                    return Ok(reply.status.is_success());
                }
                other => other,
            };
            if let Agent::SourceImport {
                pack,
                state,
                authority,
                epoch,
                ttl_seconds,
            } = action
            {
                let pack = tokio::task::spawn_blocking(move || crate::pack::Pack::open(&pack))
                    .await
                    .map_err(|_| Error::ImportWorker)??;
                let reply = crate::import::send(crate::import::Request {
                    socket: &socket,
                    credential: &credential,
                    pack,
                    state: &state,
                    authority: authority.into(),
                    epoch,
                    ttl_seconds,
                    timeout: Duration::from_millis(timeout_ms),
                })
                .await?;
                println!("{}", reply.body);
                return Ok(reply.status.is_success());
            }
            if let Agent::SealAccept {
                mutation,
                seal,
                changeset,
                previous_version,
                ttl_seconds,
            } = action
            {
                let body=json!({"version":1,"tenant":credential.scope.tenant(),"repository":credential.scope.repository().to_string(),"key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,
                    "seal":seal.to_string(),"changeset":changeset.to_string(),"previous_version":previous_version,"ttl_seconds":ttl_seconds}).to_string().into_bytes();
                let reply = client::send_seal_accept(
                    &socket,
                    &credential.token,
                    body,
                    Duration::from_millis(timeout_ms),
                )
                .await?;
                println!("{}", reply.body);
                return Ok(reply.status.is_success());
            }
            if let Agent::RebasePrepare { key, epoch, rebase } = action {
                let body=json!({"version":1,"tenant":credential.scope.tenant(),"repository":credential.scope.repository().to_string(),"key":key,"epoch":epoch,"rebase":rebase.to_string()}).to_string().into_bytes();
                let reply = client::send_rebase_prepare(
                    &socket,
                    &credential.token,
                    body,
                    Duration::from_millis(timeout_ms),
                )
                .await?;
                println!("{}", reply.body);
                return Ok(reply.status.is_success());
            }
            if let Agent::NativePrepare {
                key,
                epoch,
                candidate,
            } = action
            {
                let body=json!({"version":1,"tenant":credential.scope.tenant(),"repository":credential.scope.repository().to_string(),"key":key,"epoch":epoch,"candidate":candidate.to_string()}).to_string().into_bytes();
                let reply = client::send_native_prepare(
                    &socket,
                    &credential.token,
                    body,
                    Duration::from_millis(timeout_ms),
                )
                .await?;
                println!("{}", reply.body);
                return Ok(reply.status.is_success());
            }
            if let Agent::ChangePut { upload, file } = action {
                let bytes = files::read_private(&file, treeharbor_core::MAX_PAYLOAD)?;
                let object = treeharbor_core::Object::Blob(bytes);
                let frame = object.encode().map_err(|_| Error::Size)?;
                let id = treeharbor_core::ObjectId::hash_frame(&frame);
                let reply = client::send_change_blob(
                    &socket,
                    &credential,
                    upload,
                    id,
                    frame,
                    Duration::from_millis(timeout_ms),
                )
                .await?;
                println!("{}", reply.body);
                return Ok(reply.status.is_success());
            }
            if let Agent::ChangeSubmit {
                mutation,
                upload,
                changeset,
                previous_version,
                operations,
            } = action
            {
                let bytes = files::read_private(
                    &operations,
                    treeharbor_metadata::commands::MAX_REQUEST_BYTES,
                )?;
                let operations: Value =
                    serde_json::from_slice(&bytes).map_err(|_| Error::Config)?;
                let body=json!({"version":1,"tenant":credential.scope.tenant(),"repository":credential.scope.repository().to_string(),
                    "key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,
                    "upload":upload.to_string(),"changeset":changeset.to_string(),"previous_version":previous_version,"operations":operations}).to_string().into_bytes();
                let reply = client::send_proposal(
                    &socket,
                    &credential.token,
                    body,
                    Duration::from_millis(timeout_ms),
                )
                .await?;
                println!("{}", reply.body);
                return Ok(reply.status.is_success());
            }
            if let Agent::SourcePut {
                upload,
                object,
                file,
            } = action
            {
                let id: treeharbor_core::ObjectId = object.parse().map_err(|_| Error::Config)?;
                let bytes = files::read_private(
                    &file,
                    treeharbor_core::FRAME_HEADER + treeharbor_core::MAX_PAYLOAD,
                )?;
                let reply = client::send_object(
                    &socket,
                    &credential,
                    upload,
                    id,
                    bytes,
                    Duration::from_millis(timeout_ms),
                )
                .await?;
                println!("{}", reply.body);
                return Ok(reply.status.is_success());
            }
            let body = credential.request(action.json()?).to_string().into_bytes();
            let reply = client::send(
                &socket,
                &credential.token,
                body,
                Duration::from_millis(timeout_ms),
            )
            .await?;
            println!("{}", reply.body);
            Ok(reply.status.is_success())
        }
        Command::Admin { config, action } => {
            let database = Database::new(Config::load(&config)?)?;
            let mut session = database.connect(!matches!(&action, Admin::Migrate)).await?;
            // The DB role and private config are the trust boundary; this audit
            // label records the local OS uid, not an authenticated human name.
            let operator = format!("local-uid:{}", rustix::process::geteuid().as_raw());
            let db = &mut session.metadata;
            let value = match action {
                Admin::RuntimeProfileRegister { scope, file } => {
                    let bytes = files::read_private(&file, 4096)?;
                    let definition = serde_json::from_slice(&bytes).map_err(|_| Error::Config)?;
                    let profile =
                        treeharbor_runtime::Profile::new(definition).map_err(|_| Error::Config)?;
                    db.register_runtime_profile(&scope.scope()?, &profile, &operator)
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"registered","profile_digest":treeharbor_core::ObjectId::from_bytes(profile.digest()).to_string()})
                }
                Admin::RuntimeProfileRevoke {
                    scope,
                    profile_digest,
                } => {
                    let profile: treeharbor_core::ObjectId =
                        profile_digest.parse().map_err(|_| Error::Config)?;
                    db.revoke_runtime_profile(&scope.scope()?, *profile.as_bytes(), &operator)
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"revoked","profile_digest":profile.to_string()})
                }
                Admin::NodeRegister {
                    tenant,
                    principal,
                    storage,
                } => {
                    db.register_node_storage(&tenant, &principal, storage, &operator)
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"registered","storage":storage.to_string(),"principal":principal})
                }
                Admin::Migrate => {
                    db.migrate().await.map_err(|_| Error::Administration)?;
                    json!({"status":"migrated"})
                }
                Admin::RepositoryCreate { scope, authority } => {
                    db.create_repository(&scope.scope()?, authority.into())
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"provisioned","repository":scope.repository.to_string()})
                }
                Admin::PrincipalCreate {
                    tenant,
                    principal,
                    kind,
                } => {
                    db.provision_principal(&tenant, &principal, kind.into(), &operator)
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"provisioned","principal":principal})
                }
                Admin::AccessSet {
                    scope,
                    principal,
                    permission,
                    ..
                } => {
                    let permissions: Vec<_> = permission.into_iter().map(Into::into).collect();
                    db.set_repository_access(&scope.scope()?, &principal, &permissions, &operator)
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"access_replaced"})
                }
                Admin::TokenIssue {
                    scope,
                    principal,
                    permission,
                    ttl_seconds,
                    output,
                } => {
                    // Validate the destination before minting; atomic no-clobber
                    // publication still handles a racing creation safely.
                    files::private_directory(output.parent().ok_or(Error::UnsafeFile)?)?;
                    match std::fs::symlink_metadata(&output) {
                        Ok(_) => return Err(Error::UnsafeFile),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                    let scope = scope.scope()?;
                    let permissions: Vec<_> = permission.into_iter().map(Into::into).collect();
                    let issued = db
                        .issue_repository_token(
                            &scope,
                            &principal,
                            &permissions,
                            ttl_seconds,
                            &operator,
                        )
                        .await
                        .map_err(|_| Error::Administration)?;
                    let document = json!({"version":1,"tenant":scope.tenant(),"repository":scope.repository().to_string(),"token_id":issued.id.to_string(),"token":issued.secret.expose()});
                    if files::write_private(&output, document.to_string().as_bytes()).is_err() {
                        return Err(
                            if db
                                .revoke_repository_token(&scope, issued.id, &operator)
                                .await
                                .is_ok()
                            {
                                Error::CredentialWriteRevoked { token: issued.id }
                            } else {
                                Error::CredentialWriteUncertain { token: issued.id }
                            },
                        );
                    }
                    json!({"status":"issued","token_id":issued.id.to_string(),"credential_file":output})
                }
                Admin::TokenRevoke { scope, token_id } => {
                    db.revoke_repository_token(&scope.scope()?, token_id, &operator)
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"revoked","token_id":token_id.to_string()})
                }
                Admin::PrincipalDisable { tenant, principal } => {
                    db.disable_principal(&tenant, &principal, &operator)
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"disabled","principal":principal})
                }
            };
            println!("{value}");
            Ok(true)
        }
    }
}
