//! Complete the capture-to-server path. Only `capture_seal` can supply the local
//! artifact here; object publication never treats arbitrary agent bytes as proof
//! that the runtime was stopped.
use crate::{Error, Node, Result};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};
use treeharbor_core::{Object, RepoPath};
use treeharbor_materializer::{CaptureArtifact, Limits};
use treeharbor_metadata::{
    Metadata,
    access::Authorization,
    seals::{SealPlan, SealPublication},
};
use treeharbor_storage::{ManagedStore, PreparedObject};

impl Node {
    #[allow(clippy::too_many_arguments)]
    pub async fn seal_workspace(
        &self,
        db: &mut Metadata,
        heartbeat: &mut Metadata,
        auth: &Authorization,
        plan: &SealPlan,
        socket: &Path,
        store: &ManagedStore,
        limits: Limits,
    ) -> Result<serde_json::Value> {
        self.check_auth(auth)?;
        if plan.context().prepared().node != self.identity().node()
            || plan.context().prepared().slot.storage != self.identity().storage()
        {
            return Err(Error::Identity);
        }
        if let Some(receipt) = db.completed_workspace_seal(auth, plan).await? {
            return Ok(receipt);
        }
        let artifact = Arc::new(
            self.capture_seal(db, heartbeat, auth, plan, socket, limits)
                .await?,
        );
        let work = async {
            let input = SealPublication::new(
                artifact.manifest_digest(),
                artifact.operations().clone(),
                artifact.objects().to_vec(),
                artifact.baseline_dependencies().clone(),
                artifact.encoded_bytes(),
            )?;
            db.begin_seal_publication(auth, plan, store, &input).await?;
            prove_dependencies(db, auth, plan, store, &artifact, limits).await?;
            for id in artifact.objects().iter().copied() {
                let owned = artifact.clone();
                let frame = tokio::task::spawn_blocking(move || {
                    let object = owned.object(id)?;
                    PreparedObject::new(&object).map_err(treeharbor_materializer::Error::from)
                })
                .await??;
                let ticket = db.reserve_seal_object(auth, plan, store, &frame).await?;
                let generation = ticket.generation();
                let backend = store.clone();
                let receipt = tokio::task::spawn_blocking(move || backend.put(generation, &frame))
                    .await?
                    .map_err(treeharbor_materializer::Error::from)?;
                db.publish_seal_object(auth, plan, &ticket, &receipt)
                    .await?;
            }
            Ok(db
                .finish_seal_publication(auth, plan, artifact.manifest_digest())
                .await?)
        };
        let renew = async {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Err(error) = heartbeat.renew_workspace_seal(auth, plan, 60).await {
                    return Err::<serde_json::Value, Error>(error.into());
                }
            }
        };
        tokio::select! { biased; result=work=>result, failure=renew=>failure }
    }
}
async fn prove_dependencies(
    db: &mut Metadata,
    auth: &Authorization,
    plan: &SealPlan,
    store: &ManagedStore,
    artifact: &CaptureArtifact,
    limits: Limits,
) -> Result<()> {
    let mut needed = artifact.baseline_dependencies().clone();
    if needed.is_empty() {
        return Ok(());
    }
    let source = db.open_seal_source(auth, plan).await?;
    let result = async {
        let paths: Vec<_> = plan
            .selection()
            .replace()
            .iter()
            .chain(plan.selection().delete())
            .chain(plan.selection().exclude())
            .collect();
        // Follow only selected paths and exclusion ancestors, never all commit
        // parents, unrelated trees or every blob in a large original repository.
        let mut pending = vec![(source.root(), Vec::new())];
        let mut bytes = 0u64;
        let mut entries = 0usize;
        let mut path_bytes = 0u64;
        while !needed.is_empty() {
            let (node, path) = pending.pop().ok_or(Error::Identity)?;
            let budget = limits
                .max_source_bytes
                .saturating_sub(bytes)
                .min(treeharbor_metadata::reads::MAX_READ_BYTES);
            if budget == 0 {
                return Err(treeharbor_materializer::Error::Limit.into());
            }
            let batch = db
                .read_seal_source(auth, plan, &source, store, &[node], budget)
                .await?;
            let object = tokio::task::spawn_blocking(move || batch.read())
                .await??
                .pop()
                .ok_or(Error::Identity)?;
            bytes = bytes
                .checked_add(object.encoded_length())
                .ok_or(Error::Configuration)?;
            if needed.remove(&object.id()).is_some() {
                db.prove_seal_dependency(auth, plan, &source, &object)
                    .await?;
            }
            if needed.is_empty() {
                break;
            }
            let children: BTreeMap<_, _> = object.children().map(|n| (n.id(), n)).collect();
            match object.object() {
                Object::Snapshot { root } => {
                    pending.push((children.get(root).ok_or(Error::Identity)?.clone(), path))
                }
                Object::Directory(dir) => {
                    entries = entries
                        .checked_add(dir.entries().len())
                        .ok_or(Error::Configuration)?;
                    if entries > limits.max_entries {
                        return Err(treeharbor_materializer::Error::Limit.into());
                    }
                    for entry in dir.entries() {
                        let mut child_path = path.clone();
                        if !child_path.is_empty() {
                            child_path.push(b'/');
                        }
                        child_path.extend_from_slice(entry.name.as_bytes());
                        let child = RepoPath::new(child_path.clone())
                            .map_err(treeharbor_materializer::Error::from)?;
                        if needed.contains_key(&entry.value.target)
                            || paths
                                .iter()
                                .any(|p| **p == child || child.is_ancestor_of(p))
                        {
                            path_bytes = path_bytes
                                .checked_add(child_path.len() as u64)
                                .ok_or(Error::Configuration)?;
                            if path_bytes > limits.max_path_bytes {
                                return Err(treeharbor_materializer::Error::Limit.into());
                            }
                            pending.push((
                                children
                                    .get(&entry.value.target)
                                    .ok_or(Error::Identity)?
                                    .clone(),
                                child_path,
                            ));
                        }
                    }
                }
                Object::Blob(_) => (),
                _ => return Err(Error::Identity),
            }
        }
        Ok(())
    }
    .await;
    let close = db.close_seal_source(source).await;
    result?;
    close?;
    Ok(())
}
