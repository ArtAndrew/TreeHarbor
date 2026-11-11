//! Retain the exact source set for a later write-authority handoff.
use super::*;
use crate::{
    catalog::{ObjectKey, ObjectLease},
    integration::ref_name,
    uploads::{self, UploadLimits},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use treeharbor_core::{GitOid, ObjectId, RevisionRef};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchiveBranchTip {
    pub git_revision: String,
    pub native_revision: String,
    pub snapshot: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchiveStageSpec {
    pub version: u8,
    pub spool_fingerprint: String,
    pub app_id: u64,
    pub upstream: Upstream,
    pub default_branch: String,
    pub branches: BTreeMap<String, ArchiveBranchTip>,
    pub object_count: i64,
    pub encoded_bytes: i64,
}

pub(super) struct Root {
    pub(super) snapshot: ObjectId,
    git_revision: String,
}

fn invalid() -> Error {
    Error::Invalid("archived source set")
}

impl ArchiveStageSpec {
    pub(super) fn roots(&self) -> Result<BTreeMap<ObjectId, Root>> {
        self.upstream.validate()?;
        let fingerprint: ObjectId = self.spool_fingerprint.parse().map_err(|_| invalid())?;
        ref_name(&self.default_branch)?;
        if self.version != 1
            || self.spool_fingerprint != fingerprint.to_string()
            || self.app_id == 0
            || self.app_id > i64::MAX as u64
            || self.branches.is_empty()
            || self.branches.len() > 128
            || !self
                .branches
                .contains_key(&format!("refs/heads/{}", self.default_branch))
        {
            return Err(invalid());
        }
        UploadLimits {
            objects: self.object_count,
            bytes: self.encoded_bytes,
            ttl_seconds: 1,
        }
        .validate()?;
        let mut roots = BTreeMap::new();
        for (name, tip) in &self.branches {
            ref_name(name.strip_prefix("refs/heads/").ok_or_else(invalid)?)?;
            let RevisionRef::Native(native) = tip.native_revision.parse().map_err(|_| invalid())?
            else {
                return Err(invalid());
            };
            let git @ RevisionRef::Git(GitOid::Sha1(git_bytes)) =
                tip.git_revision.parse().map_err(|_| invalid())?
            else {
                return Err(invalid());
            };
            let snapshot: ObjectId = tip.snapshot.parse().map_err(|_| invalid())?;
            if tip.native_revision != RevisionRef::Native(native).to_string()
                || tip.git_revision != git.to_string()
                || git_bytes == [0; 20]
                || tip.snapshot != snapshot.to_string()
            {
                return Err(invalid());
            }
            let root = Root {
                snapshot,
                git_revision: tip.git_revision.clone(),
            };
            if roots.insert(native, root).is_some_and(|previous: Root| {
                previous.snapshot != snapshot || previous.git_revision != tip.git_revision
            }) {
                return Err(invalid());
            }
        }
        if roots.len() as i64 > self.object_count {
            return Err(invalid());
        }
        Ok(roots)
    }

    pub fn validate(&self) -> Result<()> {
        self.roots().map(|_| ())
    }
}

pub(super) async fn check_archive_binding(
    tx: &Transaction<'_>,
    scope: &Scope,
    binding_id: Uuid,
    epoch: i64,
    spec: &ArchiveStageSpec,
) -> Result<()> {
    let selected = binding(tx, scope, binding_id)
        .await
        .map_err(|error| match error {
            Error::NotFound => Error::HostBindingState,
            other => other,
        })?;
    let selected_spec = specification(&selected)?;
    if selected.get::<_, i64>("epoch") != epoch
        || selected.get::<_, String>("state") != "active"
        || selected_spec.app_id != spec.app_id
        || selected_spec.upstream != spec.upstream
    {
        return Err(Error::HostBindingState);
    }
    Ok(())
}

impl Metadata {
    /// Commit all roots, the completion receipt and the closed upload together.
    /// The receipt identifies retained data. Host fencing is a separate operation.
    pub async fn stage_archived_source(
        &mut self,
        auth: &Authorization,
        command: &Command,
        binding_id: Uuid,
        upload: Uuid,
        spec: &ArchiveStageSpec,
    ) -> Result<Value> {
        uuid(binding_id)?;
        uuid(upload)?;
        let roots = spec.roots()?;
        let request = request(
            auth,
            command,
            "github.archive.stage.v1",
            json!([1, binding_id, upload, spec]),
        )?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(receipt) => return Ok(receipt),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let scope = auth.scope();
        let tenant = scope.tenant();
        let repository = scope.repository();
        check_archive_binding(tx, scope, binding_id, command.epoch, spec).await?;
        let session = uploads::active(tx, scope, auth.principal(), upload).await?;
        let ordinary: bool = tx
            .query_one(
                "SELECT github_import IS NULL FROM th_source_uploads WHERE tenant=$1 AND repository=$2 AND upload=$3",
                &[&tenant, &repository, &upload],
            )
            .await?
            .get(0);
        if !ordinary
            || session.get::<_, i64>("max_objects") != spec.object_count
            || session.get::<_, i64>("max_bytes") != spec.encoded_bytes
        {
            return Err(Error::UploadState);
        }
        let totals = tx
            .query_one(
                r#"
            SELECT count(*), COALESCE(sum(i.length),0)::bigint
            FROM th_source_upload_items i JOIN th_objects o
              ON o.tenant=i.tenant AND o.digest=i.digest AND o.generation=i.generation
            WHERE i.tenant=$1 AND i.repository=$2 AND i.upload=$3 AND i.complete AND o.state='live'
        "#,
                &[&tenant, &repository, &upload],
            )
            .await?;
        if totals.get::<_, i64>(0) != spec.object_count
            || totals.get::<_, i64>(1) != spec.encoded_bytes
        {
            return Err(Error::UploadMembership);
        }
        let root_digests: Vec<_> = roots.keys().map(|id| id.as_bytes().to_vec()).collect();
        // Object publication already requires complete typed children from the
        // same upload. In that DAG, every object belongs to a selected root's
        // closure exactly when every unselected object has an uploaded parent.
        let invalid_membership: bool = tx
            .query_one(
                r#"
            SELECT EXISTS (
              SELECT 1 FROM th_source_upload_items i
              WHERE i.tenant=$1 AND i.repository=$2 AND i.upload=$3 AND i.digest<>ALL($4::bytea[])
                AND NOT EXISTS (
                  SELECT 1 FROM th_object_edges e JOIN th_source_upload_items p
                    ON p.tenant=e.tenant AND p.repository=$2 AND p.upload=$3
                    AND p.digest=e.parent AND p.generation=e.parent_generation AND p.complete
                  WHERE e.tenant=i.tenant AND e.child=i.digest AND e.child_generation=i.generation
                )
            ) OR EXISTS (
              SELECT 1 FROM th_source_upload_items p JOIN th_object_edges e
                ON e.tenant=p.tenant AND e.parent=p.digest AND e.parent_generation=p.generation
              WHERE p.tenant=$1 AND p.repository=$2 AND p.upload=$3
                AND NOT EXISTS (
                  SELECT 1 FROM th_source_upload_items c
                  WHERE c.tenant=p.tenant AND c.repository=p.repository AND c.upload=p.upload
                    AND c.digest=e.child AND c.generation=e.child_generation AND c.complete
                )
            )
        "#,
                &[&tenant, &repository, &upload, &root_digests],
            )
            .await?
            .get(0);
        if invalid_membership {
            return Err(Error::UploadMembership);
        }
        let rows = tx.query(r#"
            SELECT digest,generation,hold_token,snapshot,git_origin FROM th_source_upload_items
            WHERE tenant=$1 AND repository=$2 AND upload=$3 AND digest=ANY($4::bytea[]) AND kind=5 AND complete
            ORDER BY digest
        "#, &[&tenant,&repository,&upload,&root_digests]).await?;
        if rows.len() != roots.len() {
            return Err(Error::UploadMembership);
        }
        for row in rows {
            let id = uploads::oid(row.get("digest"))?;
            let root = roots.get(&id).ok_or(Error::UploadMembership)?;
            let snapshot = uploads::oid(row.get("snapshot"))?;
            if snapshot != root.snapshot
                || row.get::<_, Option<String>>("git_origin").as_deref()
                    != Some(root.git_revision.as_str())
            {
                return Err(Error::UploadMembership);
            }
            let lease = ObjectLease {
                key: ObjectKey {
                    tenant: tenant.to_owned(),
                    backend: session.get("backend"),
                    id,
                    generation: row.get("generation"),
                },
                token: row.get("hold_token"),
                upload: false,
            };
            mutation.register_base(id, snapshot, &lease).await?;
        }
        let changed = tx.execute(r#"
            UPDATE th_source_uploads SET state='completed'
            WHERE tenant=$1 AND repository=$2 AND upload=$3 AND state='active' AND expires_at>clock_timestamp()
        "#, &[&tenant,&repository,&upload]).await?;
        if changed != 1 {
            return Err(Error::UploadState);
        }
        mutation.complete(json!({"version":1,"tenant":tenant,"repository":repository,"authority":"connected",
            "epoch":command.epoch,"binding":binding_id,"upload":upload,"state":"staged","specification":spec}),vec![]).await
    }
}
