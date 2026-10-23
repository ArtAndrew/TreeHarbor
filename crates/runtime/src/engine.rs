use crate::{Error, Profile, Result, Task, Volume, profile::hex};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::UnixStream;
use uuid::Uuid;

/// A trusted operator connection to one local rootful Linux Engine. Never expose
/// this API, its socket or arbitrary Specifications to an agent.
pub struct Engine {
    socket: PathBuf,
    device: u64,
    inode: u64,
    identity: String,
}
pub struct Specification {
    attempt: Uuid,
    binding: [u8; 32],
    profile: Profile,
    task: Task,
    volume: Volume,
}
impl Specification {
    pub fn new(
        attempt: Uuid,
        binding: [u8; 32],
        profile: Profile,
        task: Task,
        volume: Volume,
    ) -> Result<Self> {
        if attempt.is_nil()
            || profile.definition().workspace_bytes != volume.identity().limit_bytes
            || profile.definition().uid != rustix::process::geteuid().as_raw()
            || profile.definition().gid != rustix::process::getegid().as_raw()
        {
            return Err(Error::Mismatch);
        }
        volume.verify()?;
        Ok(Self {
            attempt,
            binding,
            profile,
            task,
            volume,
        })
    }
    pub fn name(&self) -> String {
        format!("treeharbor-{}", self.attempt)
    }
    pub fn profile(&self) -> &Profile {
        &self.profile
    }
    pub fn task(&self) -> &Task {
        &self.task
    }
    pub fn volume(&self) -> &Volume {
        &self.volume
    }
    pub fn binding(&self) -> [u8; 32] {
        self.binding
    }
    pub fn attempt(&self) -> Uuid {
        self.attempt
    }
    fn body(&self) -> Value {
        let p = self.profile.definition();
        json!({
            "Image":p.image,"User":"0:0","Entrypoint":["/init"],"Cmd":self.task.init_arguments(&self.profile),
            "WorkingDir":"/","Env":[],"Tty":false,"OpenStdin":false,"StdinOnce":false,
            "AttachStdin":false,"AttachStdout":false,"AttachStderr":false,"NetworkDisabled":true,
            "Healthcheck":{"Test":["NONE"]},"Volumes":{},"ExposedPorts":{},"StopSignal":"SIGKILL",
            "Labels":{"io.treeharbor.attempt":self.attempt.to_string(),"io.treeharbor.binding":hex(&self.binding),
                "io.treeharbor.profile":hex(&self.profile.digest()),"io.treeharbor.protocol":"1"},
            "HostConfig":{
                "Runtime":"runsc","NetworkMode":"none","ReadonlyRootfs":true,"Privileged":false,
                "CapDrop":["ALL"],"CapAdd":["SETUID","SETGID"],"SecurityOpt":["no-new-privileges"],
                "CgroupnsMode":"private","PidMode":"","IpcMode":"private","UTSMode":"","UsernsMode":"",
                "Memory":p.memory_bytes,"MemorySwap":p.memory_bytes,"NanoCpus":p.nano_cpus,
                "PidsLimit":256,"ShmSize":8*1024*1024,"OomKillDisable":false,
                "Ulimits":[{"Name":"nofile","Soft":1024,"Hard":1024},{"Name":"core","Soft":0,"Hard":0}],
                "Tmpfs":{"/tmp":"rw,noexec,nodev,nosuid,size=16777216,mode=1777"},
                "Mounts":[{"Type":"bind","Source":self.volume.files(),"Target":"/workspace","ReadOnly":false,
                    "BindOptions":{"Propagation":"rprivate","NonRecursive":true}}],
                "RestartPolicy":{"Name":"no","MaximumRetryCount":0},"AutoRemove":false,"PublishAllPorts":false,
                "LogConfig":{"Type":"local","Config":{"max-size":"1m","max-file":"1","compress":"false"}}
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    id: String,
    binding: [u8; 32],
}
impl Container {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn binding(&self) -> [u8; 32] {
        self.binding
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeState {
    Created,
    Running,
    Exited,
    Dead,
    Removing,
}
#[derive(Clone, Debug)]
pub struct Inspection {
    pub container: Container,
    pub state: RuntimeState,
    pub started_at: String,
    pub finished_at: String,
    pub exit_code: i64,
    pub oom_killed: bool,
}
impl Inspection {
    pub fn never_started(&self) -> bool {
        self.state == RuntimeState::Created && self.started_at == "0001-01-01T00:00:00Z"
    }
}

struct Driver(tokio::task::JoinHandle<()>);
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl Engine {
    pub async fn open(socket: &Path) -> Result<Self> {
        if !cfg!(target_os = "linux") || !socket.is_absolute() {
            return Err(Error::Unsupported);
        }
        let meta = fs::symlink_metadata(socket)?;
        if !meta.file_type().is_socket() || meta.uid() != 0 || meta.mode() & 0o002 != 0 {
            return Err(Error::Unsupported);
        }
        let mut engine = Self {
            socket: socket.to_owned(),
            device: meta.dev(),
            inode: meta.ino(),
            identity: String::new(),
        };
        let (status, value) = engine.request("GET", "/info", None).await?;
        if status != StatusCode::OK
            || value["OSType"] != "linux"
            || value["Architecture"] != "x86_64"
            || value["CgroupVersion"] != "2"
            || !value["Runtimes"]["runsc"].is_object()
        {
            return Err(Error::Unsupported);
        }
        let id = value["ID"].as_str().ok_or(Error::Unsupported)?;
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_:".contains(&c))
        {
            return Err(Error::Unsupported);
        }
        engine.identity = id.into();
        Ok(engine)
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value)> {
        let operation = async {
            let meta = fs::symlink_metadata(&self.socket)?;
            if !meta.file_type().is_socket()
                || meta.dev() != self.device
                || meta.ino() != self.inode
                || meta.uid() != 0
                || meta.mode() & 0o002 != 0
            {
                return Err(Error::Mismatch);
            }
            let stream = UnixStream::connect(&self.socket).await?;
            if stream.peer_cred()?.uid() != 0 {
                return Err(Error::Unsupported);
            }
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(stream))
                    .await
                    .map_err(|_| Error::Unknown)?;
            let _driver = Driver(tokio::spawn(async move {
                let _ = connection.await;
            }));
            let bytes = body
                .map(|b| serde_json::to_vec(&b))
                .transpose()?
                .unwrap_or_default();
            if bytes.len() > 128 * 1024 {
                return Err(Error::Limit);
            }
            let request = Request::builder()
                .method(method)
                .uri(format!("/v1.47{path}"))
                .header("Host", "docker.local")
                .header("Connection", "close")
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(bytes)))
                .map_err(|_| Error::Invalid)?;
            let response = sender
                .send_request(request)
                .await
                .map_err(|_| Error::Unknown)?;
            let status = response.status();
            let bytes = Limited::new(response.into_body(), 1024 * 1024)
                .collect()
                .await
                .map_err(|_| Error::Unknown)?
                .to_bytes();
            let value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes)?
            };
            Ok((status, value))
        };
        tokio::time::timeout(Duration::from_secs(30), operation)
            .await
            .map_err(|_| Error::Unknown)?
    }
    /// Must be preceded by a durable create intent. A lost response is unknown;
    /// inspect the deterministic name, never choose a new name or auto-delete it.
    pub(crate) async fn create(&self, spec: &Specification) -> Result<Inspection> {
        spec.volume.verify()?;
        let (status, image) = self
            .request(
                "GET",
                &format!("/images/{}/json", spec.profile.definition().image),
                None,
            )
            .await?;
        if status != StatusCode::OK {
            return Err(Error::EngineStatus(status.as_u16()));
        }
        if image["Id"] != spec.profile.definition().image
            || image["Os"] != "linux"
            || image["Architecture"] != "amd64"
            || !empty(&image["Config"]["Volumes"])
        {
            return Err(Error::Mismatch);
        }
        let (status, response) = self
            .request(
                "POST",
                &format!(
                    "/containers/create?name={}&platform=linux%2Famd64",
                    spec.name()
                ),
                Some(spec.body()),
            )
            .await?;
        if status != StatusCode::CREATED {
            return Err(Error::EngineStatus(status.as_u16()));
        }
        if !empty(&response["Warnings"]) {
            return Err(Error::Unknown);
        }
        let id = response["Id"]
            .as_str()
            .filter(|id| container_id(id))
            .ok_or(Error::Mismatch)?;
        self.inspect_id(spec, id).await?.ok_or(Error::Unknown)
    }
    pub async fn discover(&self, spec: &Specification) -> Result<Option<Inspection>> {
        self.inspect_name(spec, &spec.name()).await
    }
    /// Cleanup recovery cannot require deleted agent files. Only absence is
    /// accepted here: any existing ID/name, even with different labels, blocks
    /// deletion. This never starts, adopts or removes a container.
    pub(crate) async fn require_absent(&self, attempt: Uuid, id: Option<&str>) -> Result<()> {
        if attempt.is_nil() || id.is_some_and(|id| !container_id(id)) {
            return Err(Error::Invalid);
        }
        let name = format!("treeharbor-{attempt}");
        for target in id.into_iter().chain(std::iter::once(name.as_str())) {
            let (status, _) = self
                .request("GET", &format!("/containers/{target}/json"), None)
                .await?;
            match status {
                StatusCode::NOT_FOUND => (),
                StatusCode::OK => return Err(Error::Unknown),
                _ => return Err(Error::EngineStatus(status.as_u16())),
            }
        }
        Ok(())
    }
    pub async fn inspect(
        &self,
        spec: &Specification,
        container: &Container,
    ) -> Result<Option<Inspection>> {
        if container.binding != spec.binding {
            return Err(Error::Mismatch);
        }
        self.inspect_id(spec, &container.id).await
    }
    pub async fn inspect_id(&self, spec: &Specification, id: &str) -> Result<Option<Inspection>> {
        if !container_id(id) {
            return Err(Error::Invalid);
        }
        self.inspect_name(spec, id).await
    }
    async fn inspect_name(&self, spec: &Specification, name: &str) -> Result<Option<Inspection>> {
        spec.volume.verify()?;
        let (status, value) = self
            .request("GET", &format!("/containers/{name}/json"), None)
            .await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status != StatusCode::OK {
            return Err(Error::EngineStatus(status.as_u16()));
        }
        let observed = verify(spec, &value)?;
        if container_id(name) && observed.container.id != name {
            return Err(Error::Mismatch);
        }
        Ok(Some(observed))
    }
    /// Caller MUST durably mark start-issued first, and never repeat an uncertain
    /// start across restart. Created state alone does not fence an in-flight call.
    pub(crate) async fn start_once(
        &self,
        spec: &Specification,
        container: &Container,
    ) -> Result<()> {
        let observed = self.inspect(spec, container).await?.ok_or(Error::Unknown)?;
        if !observed.never_started() {
            return Err(Error::State);
        }
        let (status, _) = self
            .request("POST", &format!("/containers/{}/start", container.id), None)
            .await?;
        if status != StatusCode::NO_CONTENT {
            return Err(Error::EngineStatus(status.as_u16()));
        }
        Ok(())
    }
    /// Exact-ID removal after ownership verification. Unknown outcomes retain
    /// files/reservations; callers must observe absence before reusing a volume.
    pub(crate) async fn remove(&self, spec: &Specification, container: &Container) -> Result<()> {
        if self.inspect(spec, container).await?.is_none() {
            return Ok(());
        }
        let (status, _) = self
            .request(
                "DELETE",
                &format!("/containers/{}?force=true&v=true", container.id),
                None,
            )
            .await?;
        if status != StatusCode::NO_CONTENT && status != StatusCode::NOT_FOUND {
            return Err(Error::EngineStatus(status.as_u16()));
        }
        if self.inspect(spec, container).await?.is_some() {
            return Err(Error::Unknown);
        }
        Ok(())
    }
}
fn empty(value: &Value) -> bool {
    value.is_null()
        || value.as_array().is_some_and(Vec::is_empty)
        || value.as_object().is_some_and(serde_json::Map::is_empty)
}
fn container_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn same(value: &Value, expected: &Value, fields: &[&str]) -> Result<()> {
    if fields.iter().any(|field| value[*field] != expected[*field]) {
        return Err(Error::Mismatch);
    }
    Ok(())
}
fn declared_mount(value: &Value, expected: &Value) -> Result<()> {
    // Engine 28/API 1.47 omits ReadOnly when false. The independently observed
    // effective mount must still have RW=true; do not normalize other fields.
    if !matches!(value.get("ReadOnly"), None | Some(Value::Bool(false))) {
        return Err(Error::Mismatch);
    }
    same(
        value,
        expected,
        &["Type", "Source", "Target", "BindOptions"],
    )
}
fn oom_policy(value: &Value) -> Result<()> {
    // Only cgroup v2 is supported. Docker discards this v1 setting at start and
    // reports null; the kernel's OOM handling remains enabled. Never accept true.
    if !matches!(value, Value::Null | Value::Bool(false)) {
        return Err(Error::Mismatch);
    }
    Ok(())
}
fn verify(spec: &Specification, v: &Value) -> Result<Inspection> {
    let expected = spec.body();
    let h = &v["HostConfig"];
    let e = &expected["HostConfig"];
    let c = &v["Config"];
    let id = v["Id"]
        .as_str()
        .filter(|id| container_id(id))
        .ok_or(Error::Mismatch)?;
    if v["Image"] != spec.profile.definition().image
        || v["Name"] != format!("/{}", spec.name())
        || v["RestartCount"] != 0
    {
        return Err(Error::Mismatch);
    }
    same(
        c,
        &expected,
        &[
            "Image",
            "User",
            "Entrypoint",
            "Cmd",
            "WorkingDir",
            "Tty",
            "OpenStdin",
            "StdinOnce",
            "NetworkDisabled",
            "Healthcheck",
            "StopSignal",
        ],
    )?;
    for key in [
        "io.treeharbor.attempt",
        "io.treeharbor.binding",
        "io.treeharbor.profile",
        "io.treeharbor.protocol",
    ] {
        if c["Labels"][key] != expected["Labels"][key] {
            return Err(Error::Mismatch);
        }
    }
    if !empty(&c["Volumes"]) || !empty(&c["ExposedPorts"]) {
        return Err(Error::Mismatch);
    }
    same(
        h,
        e,
        &[
            "Runtime",
            "NetworkMode",
            "ReadonlyRootfs",
            "Privileged",
            "CgroupnsMode",
            "PidMode",
            "IpcMode",
            "UTSMode",
            "UsernsMode",
            "Memory",
            "MemorySwap",
            "NanoCpus",
            "PidsLimit",
            "ShmSize",
            "Tmpfs",
            "RestartPolicy",
            "AutoRemove",
            "PublishAllPorts",
            "LogConfig",
        ],
    )?;
    oom_policy(&h["OomKillDisable"])?;
    let caps = h["CapAdd"].as_array().ok_or(Error::Mismatch)?;
    let mut caps = caps
        .iter()
        .map(|v| v.as_str().map(|s| s.strip_prefix("CAP_").unwrap_or(s)))
        .collect::<Option<Vec<_>>>()
        .ok_or(Error::Mismatch)?;
    caps.sort_unstable();
    if caps != ["SETGID", "SETUID"] || h["CapDrop"] != json!(["ALL"]) {
        return Err(Error::Mismatch);
    }
    if h["SecurityOpt"] != json!(["no-new-privileges"])
        && h["SecurityOpt"] != json!(["no-new-privileges=true"])
    {
        return Err(Error::Mismatch);
    }
    for key in [
        "Binds",
        "VolumesFrom",
        "Devices",
        "DeviceRequests",
        "DeviceCgroupRules",
        "GroupAdd",
        "PortBindings",
        "Links",
        "ExtraHosts",
    ] {
        if !empty(&h[key]) {
            return Err(Error::Mismatch);
        }
    }
    for key in [
        "CpuShares",
        "CpuPeriod",
        "CpuQuota",
        "CpuRealtimePeriod",
        "CpuRealtimeRuntime",
        "MemoryReservation",
    ] {
        if h[key].as_i64().is_some_and(|v| v != 0) {
            return Err(Error::Mismatch);
        }
    }
    for key in ["CpusetCpus", "CpusetMems", "CgroupParent"] {
        if h[key].as_str().is_some_and(|v| !v.is_empty()) {
            return Err(Error::Mismatch);
        }
    }
    if h["Init"].as_bool().unwrap_or(false) {
        return Err(Error::Mismatch);
    }
    let mut ulimits = h["Ulimits"].as_array().ok_or(Error::Mismatch)?.clone();
    ulimits.sort_by_key(|v| v["Name"].as_str().unwrap_or("").to_owned());
    if ulimits
        != vec![
            json!({"Name":"core","Soft":0,"Hard":0}),
            json!({"Name":"nofile","Soft":1024,"Hard":1024}),
        ]
    {
        return Err(Error::Mismatch);
    }
    let mounts = v["Mounts"].as_array().ok_or(Error::Mismatch)?;
    if mounts.len() != 1
        || mounts[0]["Type"] != "bind"
        || mounts[0]["Source"] != spec.volume.files().to_str().ok_or(Error::Invalid)?
        || mounts[0]["Destination"] != "/workspace"
        || mounts[0]["RW"] != true
        || mounts[0]["Propagation"] != "rprivate"
    {
        return Err(Error::Mismatch);
    }
    let declared = h["Mounts"].as_array().ok_or(Error::Mismatch)?;
    if declared.len() != 1 {
        return Err(Error::Mismatch);
    }
    declared_mount(&declared[0], &e["Mounts"][0])?;
    let state = &v["State"];
    if state["Paused"] != false || state["Restarting"] != false {
        return Err(Error::State);
    }
    let runtime_state = match state["Status"].as_str() {
        Some("created") => RuntimeState::Created,
        Some("running") => RuntimeState::Running,
        Some("exited") => RuntimeState::Exited,
        Some("dead") => RuntimeState::Dead,
        Some("removing") => RuntimeState::Removing,
        _ => return Err(Error::State),
    };
    if (runtime_state == RuntimeState::Running) != (state["Running"] == true)
        || (runtime_state != RuntimeState::Running && state["Pid"].as_i64() != Some(0))
    {
        return Err(Error::State);
    }
    let timestamp = |key: &str| {
        state[key]
            .as_str()
            .filter(|s| s.len() <= 64)
            .map(str::to_owned)
            .ok_or(Error::Mismatch)
    };
    Ok(Inspection {
        container: Container {
            id: id.into(),
            binding: spec.binding,
        },
        state: runtime_state,
        started_at: timestamp("StartedAt")?,
        finished_at: timestamp("FinishedAt")?,
        exit_code: state["ExitCode"].as_i64().ok_or(Error::Mismatch)?,
        oom_killed: state["OOMKilled"].as_bool().ok_or(Error::Mismatch)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observed_engine_omission_of_false_does_not_hide_mount_drift() {
        // Shape captured from the actual API 1.47 CI response, not generated by body().
        let observed = json!({"Type":"bind", "Source":"/slot/files", "Target":"/workspace",
            "BindOptions":{"Propagation":"rprivate", "NonRecursive":true}});
        let mut requested = observed.clone();
        requested["ReadOnly"] = Value::Bool(false);
        declared_mount(&observed, &requested).unwrap();
        declared_mount(&requested, &requested).unwrap();
        for invalid in [Value::Bool(true), Value::Null, json!("false"), json!(0)] {
            let mut changed = observed.clone();
            changed["ReadOnly"] = invalid;
            assert!(declared_mount(&changed, &requested).is_err());
        }
        let mut recursive = observed.clone();
        recursive["BindOptions"]["NonRecursive"] = Value::Bool(false);
        assert!(declared_mount(&recursive, &requested).is_err());
        let mut neighbor = observed;
        neighbor["Source"] = json!("/other/files");
        assert!(declared_mount(&neighbor, &requested).is_err());
        oom_policy(&Value::Null).unwrap();
        oom_policy(&Value::Bool(false)).unwrap();
        assert!(oom_policy(&Value::Bool(true)).is_err());
        assert!(oom_policy(&json!("false")).is_err());
    }
}
