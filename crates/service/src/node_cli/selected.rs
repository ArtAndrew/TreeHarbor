//! Bounded operator selections share one locked node. Only existing owner
//! requests can become work; this worker cannot create, accept or drain them.
use super::*;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) struct Options {
    pub selection: PathBuf,
    pub engine_socket: PathBuf,
    pub cow_driver: Option<PathBuf>,
    pub wait_seconds: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    version: u8,
    runs: Vec<Uuid>,
    seals: Vec<Uuid>,
    cleanups: Vec<Uuid>,
}

#[derive(Clone, Copy)]
enum Operation {
    Run,
    Seal,
    Cleanup,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Seal => "seal",
            Self::Cleanup => "cleanup",
        }
    }
}

impl Selection {
    fn operations(self) -> Result<Vec<(Operation, Uuid)>> {
        let count = self.runs.len() + self.seals.len() + self.cleanups.len();
        if self.version != 1 || !(1..=16).contains(&count) {
            return Err(Error::Config);
        }
        let mut operations = Vec::with_capacity(count);
        for (kind, ids) in [
            (Operation::Run, self.runs),
            (Operation::Seal, self.seals),
            (Operation::Cleanup, self.cleanups),
        ] {
            let mut seen = HashSet::new();
            for id in ids {
                if id.is_nil() || !seen.insert(id) {
                    return Err(Error::Config);
                }
                operations.push((kind, id));
            }
        }
        Ok(operations)
    }
}

struct Worker {
    node: Node,
    database: Database,
    auth: Authorization,
    store: Option<ManagedStore>,
    socket: PathBuf,
    driver: Option<PathBuf>,
    deadline: Instant,
}

pub(super) async fn run(
    node: Node,
    database: Database,
    auth: Authorization,
    config: &Config,
    options: Options,
) -> Result<()> {
    let selection: Selection =
        serde_json::from_slice(&files::read_private(&options.selection, 4096)?)
            .map_err(|_| Error::Config)?;
    let operations = selection.operations()?;
    let store = operations
        .iter()
        .any(|(kind, _)| matches!(kind, Operation::Seal))
        .then(|| preparation_store(config, &auth))
        .transpose()?;
    let worker = Arc::new(Worker {
        node,
        database,
        auth,
        store,
        socket: options.engine_socket,
        driver: options.cow_driver,
        deadline: Instant::now() + Duration::from_secs(options.wait_seconds),
    });
    let mut tasks = tokio::task::JoinSet::new();
    for (kind, id) in operations {
        let worker = Arc::clone(&worker);
        tasks.spawn(async move {
            let outcome = worker.perform(kind, id).await;
            let failed = outcome.is_err();
            let result =
                outcome.unwrap_or_else(|error| json!({"state":"error","error":error.to_string()}));
            (failed, json!({"kind":kind.name(),"id":id,"result":result}))
        });
    }
    let mut results = Vec::new();
    let mut failed = false;
    while let Some(joined) = tasks.join_next().await {
        // Even an unexpected task failure must not drop the JoinSet and abort
        // another task's runtime supervision or in-flight durable publication.
        match joined {
            Ok((error, result)) => {
                failed |= error;
                results.push(result);
            }
            Err(_) => {
                failed = true;
                results.push(json!({"state":"error","error":"worker task failed"}));
            }
        }
    }
    println!("{}", json!(results));
    if failed {
        Err(Error::Administration)
    } else {
        Ok(())
    }
}

impl Worker {
    async fn perform(&self, kind: Operation, id: Uuid) -> Result<Value> {
        let mut session = self.database.connect(true).await?;
        loop {
            if let Some(result) = self.perform_once(&mut session.metadata, kind, id).await? {
                return Ok(result);
            }
            if Instant::now() >= self.deadline {
                return Ok(json!({"state":"idle"}));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn perform_once(
        &self,
        db: &mut Metadata,
        kind: Operation,
        id: Uuid,
    ) -> Result<Option<Value>> {
        match kind {
            Operation::Run => {
                let Some(plan) = self.node.claim_run_for(db, &self.auth, id).await? else {
                    return Ok(None);
                };
                let mut heartbeat = self.database.connect(true).await?;
                let limits = Limits {
                    max_workspace_bytes: plan.profile().definition().workspace_bytes,
                    ..Limits::default()
                };
                Ok(Some(
                    self.node
                        .execute_run(
                            db,
                            &mut heartbeat.metadata,
                            &self.auth,
                            &plan,
                            &self.socket,
                            limits,
                        )
                        .await?,
                ))
            }
            Operation::Seal => {
                let Some(plan) = self.node.claim_seal_for(db, &self.auth, id).await? else {
                    return Ok(None);
                };
                let mut heartbeat = self.database.connect(true).await?;
                let limits = Limits {
                    max_workspace_bytes: plan.context().profile().definition().workspace_bytes,
                    ..Limits::default()
                };
                Ok(Some(
                    self.node
                        .seal_workspace(
                            db,
                            &mut heartbeat.metadata,
                            &self.auth,
                            &plan,
                            &self.socket,
                            self.store.as_ref().ok_or(Error::Config)?,
                            limits,
                        )
                        .await?,
                ))
            }
            Operation::Cleanup => {
                let Some(plan) = self.node.claim_ready_cleanup(db, &self.auth, id).await? else {
                    return db
                        .completed_workspace_cleanup(&self.auth, id, self.node.identity().storage())
                        .await
                        .map_err(|_| Error::Administration);
                };
                let mut heartbeat = self.database.connect(true).await?;
                Ok(Some(
                    self.node
                        .cleanup_workspace_with_driver(
                            db,
                            &mut heartbeat.metadata,
                            &self.auth,
                            &plan,
                            Some(&self.socket),
                            self.driver.as_deref(),
                        )
                        .await?,
                ))
            }
        }
    }
}
