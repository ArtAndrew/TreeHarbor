use crate::{Error, Node, Result, private};
use serde_json::Value;
use std::{path::Path, time::Duration};
use treeharbor_materializer::Limits;
use treeharbor_metadata::{Metadata, access::Authorization, runs::RunPlan};
use treeharbor_runtime::{Controller, Engine, Outcome, RuntimeState, Specification};
use uuid::Uuid;

impl Node {
    pub async fn claim_run(
        &self,
        db: &mut Metadata,
        auth: &Authorization,
    ) -> Result<Option<RunPlan>> {
        self.check_auth(auth)?;
        self.storage.verify()?;
        Ok(db
            .claim_run(auth, self.identity().storage(), Uuid::new_v4(), 60)
            .await?)
    }
    /// Execute a persisted request using only the registered placement/profile.
    /// The second connection renews ownership while an Engine RPC is outstanding.
    /// A lost caller does not repeat execution; the supervisor still owns deadline.
    pub async fn execute_run(
        &self,
        db: &mut Metadata,
        heartbeat: &mut Metadata,
        auth: &Authorization,
        plan: &RunPlan,
        socket: &Path,
        limits: Limits,
    ) -> Result<Value> {
        self.check_auth(auth)?;
        if let Some(result) = db.completed_run(auth, plan).await? {
            return Ok(result);
        }
        let operation = self.operation(plan.prepared().slot.slot).await?;
        if operation.volume_digest != Some(plan.volume_digest()) {
            return Err(Error::Identity);
        }
        db.renew_run(auth, plan, 60).await?;
        let tree = self
            .open_run_files(operation.clone(), plan.prepared(), limits)
            .await?;
        let volume = operation
            .volume
            .as_ref()
            .ok_or(Error::Identity)?
            .for_files(&tree.path())?;
        let engine = Engine::open(socket).await?;
        let spec = Specification::new(
            plan.attempt(),
            plan.binding(),
            plan.profile().clone(),
            plan.task().clone(),
            volume,
        )?;
        let journals = private::make_directory(&self.storage.root.join("runtime"))?;
        let controller =
            Controller::open(&journals.join(plan.attempt().to_string()), &engine, spec)?;
        let work = async {
            let stop = db.renew_run(auth, plan, 60).await?;
            if stop {
                controller.remove().await?;
                return Ok(db.finish_run(auth, plan).await?);
            }
            let observed = match controller.status().await? {
                Outcome::NotCreated => controller.create().await?,
                Outcome::Observed(i) => i,
                Outcome::Removed => return Ok(db.finish_run(auth, plan).await?),
            };
            let stop = db
                .observe_run(auth, plan, engine.identity(), &observed)
                .await?;
            if stop {
                controller.remove().await?;
                return Ok(db.finish_run(auth, plan).await?);
            }
            if observed.state == RuntimeState::Created {
                if plan.is_check() {
                    let fresh = tree.clone();
                    let guard = operation.clone();
                    tokio::task::spawn_blocking(move || {
                        let _guard = guard;
                        fresh.verify_pristine(limits)
                    })
                    .await??;
                }
                db.admit_run_start(auth, plan).await?;
                let actual = controller.start().await?;
                db.observe_run(auth, plan, engine.identity(), &actual)
                    .await?;
            }
            loop {
                operation.verify()?;
                let stop = db.renew_run(auth, plan, 60).await?;
                if stop {
                    controller.remove().await?;
                    return Ok(db.finish_run(auth, plan).await?);
                }
                match controller.status().await? {
                    Outcome::Observed(i) => {
                        db.observe_run(auth, plan, engine.identity(), &i).await?;
                        if matches!(
                            i.state,
                            RuntimeState::Exited | RuntimeState::Dead | RuntimeState::Removing
                        ) {
                            controller.remove().await?;
                            return Ok(db.finish_run(auth, plan).await?);
                        }
                    }
                    Outcome::Removed => return Ok(db.finish_run(auth, plan).await?),
                    Outcome::NotCreated => return Err(Error::Incomplete),
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        };
        let renew = async {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Err(e) = heartbeat.renew_run(auth, plan, 60).await {
                    return Err::<Value, Error>(e.into());
                }
            }
        };
        let result = tokio::select! {biased;result=work=>result,failed=renew=>failed};
        match result {
            Ok(value) => Ok(value),
            Err(original) => {
                // Never let a revoked/failed worker become a new execution owner.
                // Exact owned removal is safe; uncertainty retains all reservations.
                if controller.remove().await.is_ok()
                    && let Ok(value) = db.finish_run(auth, plan).await
                {
                    return Ok(value);
                }
                Err(original)
            }
        }
    }
}
