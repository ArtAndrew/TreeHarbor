//! Recover selected post-backup source without activating a GitHub binding.
use super::publish::journal::Journal;
use super::*;
use crate::pack::{GitCapture, Pack};
use treeharbor_core::{Object, ObjectStore, RevisionRef};
use treeharbor_metadata::{
    Authority,
    github::{RestoreAcquisitionContext, RestoreDecisionSpec, RestoreDisposition, restore_digest},
};

#[derive(clap::Args)]
pub struct Options {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    credential_file: PathBuf,
    #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
    epoch: i64,
    #[arg(long)]
    decision_key: String,
    /// Existing private directory retained across interruption and retries.
    #[arg(long)]
    state: PathBuf,
    #[arg(long, default_value = "/usr/bin/git")]
    git: PathBuf,
}
fn keys(key: &str) -> Result<(String, String)> {
    let digest = restore_digest(&json!([1, key])).map_err(|_| Error::RestoreAssessment)?;
    Ok((
        format!("restore-acquire-preserve:{digest}"),
        format!("restore-acquire-map:{digest}"),
    ))
}

async fn current(
    o: &Options,
    c: &Credential,
    receipt: &Value,
) -> Result<RestoreAcquisitionContext> {
    let value = command(&o.socket, c, json!({"action":"restore_acquisition_context","epoch":o.epoch,"decision_key":o.decision_key})).await?;
    let ctx: RestoreAcquisitionContext =
        serde_json::from_value(value).map_err(|_| Error::RestoreAssessment)?;
    if ctx.version != 1
        || ctx.tenant != c.scope.tenant()
        || ctx.repository != c.scope.repository()
        || ctx.epoch != o.epoch
        || ctx.decision_key != o.decision_key
        || ctx.publication_authorized
        || ctx.decision_sha256 != restore_digest(receipt).map_err(|_| Error::RestoreAssessment)?
        || json!(ctx.decision) != receipt["specification"]
    {
        return Err(Error::RestoreAssessment);
    }
    Ok(ctx)
}
async fn verified(
    o: &Options,
    c: &Credential,
    decision: &str,
    key: &str,
    git_revision: &str,
    acquisition: &RestoreDecisionSpec,
) -> Result<Option<Value>> {
    let reply = client::restore_verify_source(
        &o.socket,
        c,
        json!({"key":key,"epoch":o.epoch,"decision_key":decision,"git_revision":git_revision}),
        Duration::from_secs(120),
    )
    .await?;
    if reply.status.as_u16() == 404 && reply.body["error"]["code"] == "not_found" {
        return Ok(None);
    }
    if !reply.status.is_success() {
        return Err(Error::GithubBinding {
            status: reply.status.as_u16(),
        });
    }
    let r = &reply.body["result"];
    if reply.body["version"] != 1
        || r["version"] != 1
        || r["key"] != key
        || r["decision_key"] != decision
        || r["tenant"] != c.scope.tenant()
        || r["repository"] != json!(c.scope.repository())
        || r["epoch"] != o.epoch
        || r["git_revision"] != git_revision
        || r["git_mapping_verified"] != true
        || r["host_state_verified"] != false
        || r["reconciliation_complete"] != false
        || r["publication_authorized"] != false
    {
        return Err(Error::RestoreAssessment);
    }
    let preserved = command(
        &o.socket,
        c,
        json!({"action":"restore_decision_status","key":decision}),
    )
    .await?;
    let native = r["native_revision"]
        .as_str()
        .ok_or(Error::RestoreAssessment)?;
    let RevisionRef::Native(root) = native.parse().map_err(|_| Error::RestoreAssessment)? else {
        return Err(Error::RestoreAssessment);
    };
    let mut expected = acquisition.clone();
    expected.disposition = RestoreDisposition::PreserveSource {
        source_holder: format!("base:{}", root.hex()),
        native_revision: native.into(),
    };
    expected.previous_key = Some(o.decision_key.clone());
    if preserved["specification"] != json!(expected)
        || r["decision_sha256"]
            != restore_digest(&preserved).map_err(|_| Error::RestoreAssessment)?
    {
        return Err(Error::RestoreAssessment);
    }
    Ok(Some(r.clone()))
}
fn finish(j: &Journal, receipt: Value) -> Result<bool> {
    j.bind("mapping.json", &receipt)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&receipt).map_err(|_| Error::Http)?
    );
    Ok(true)
}
pub(super) async fn run(o: Options) -> Result<bool> {
    let c = Credential::load(&o.credential_file)?;
    let j = Journal::open(
        &o.state,
        &json!({"version":1,"kind":"restore_acquire_source","tenant":c.scope.tenant(),"repository":c.scope.repository(),"epoch":o.epoch,"decision_key":o.decision_key}),
    )?;
    let historical = command(
        &o.socket,
        &c,
        json!({"action":"restore_decision_status","key":o.decision_key}),
    )
    .await?;
    if historical["version"] != 1
        || historical["key"] != o.decision_key
        || historical["epoch"] != o.epoch
        || historical["tenant"] != c.scope.tenant()
        || historical["repository"] != json!(c.scope.repository())
    {
        return Err(Error::RestoreAssessment);
    }
    let spec: RestoreDecisionSpec = serde_json::from_value(historical["specification"].clone())
        .map_err(|_| Error::RestoreAssessment)?;
    let RestoreDisposition::AcquireSource {
        git_revision,
        max_objects,
        max_bytes,
    } = &spec.disposition
    else {
        return Err(Error::RestoreAssessment);
    };
    if spec.version != 3 {
        return Err(Error::RestoreAssessment);
    }
    let (preserve_key, mapping_key) = keys(&o.decision_key)?;
    // Recover a committed proof or finish verification of an already preserved
    // root before opening local source, App files or invoking Git/network.
    if let Some(r) = verified(&o, &c, &preserve_key, &mapping_key, git_revision, &spec).await? {
        return finish(&j, r);
    }
    let ctx = current(&o, &c, &historical).await?;
    j.bind("source.json", &json!(ctx))?;
    let RevisionRef::Git(commit) = git_revision.parse().map_err(|_| Error::RestoreAssessment)?
    else {
        return Err(Error::RestoreAssessment);
    };
    let limits = treeharbor_git_bridge::Limits {
        objects: usize::try_from(*max_objects).map_err(|_| Error::Size)?,
        source_bytes: u64::try_from(*max_bytes).map_err(|_| Error::Size)?,
        stream_bytes: u64::try_from(*max_bytes)
            .map_err(|_| Error::Size)?
            .saturating_mul(2)
            .min(128 << 30),
        ..Default::default()
    };
    let destination = j.root.join("pack");
    let pack = if destination.try_exists()? {
        // Private retained source is input, not a fresh host observation. Actual
        // server projection below, not the manifest claim, establishes its OID.
        tokio::task::spawn_blocking(move || Pack::open(&destination))
            .await
            .map_err(|_| Error::ImportWorker)??
    } else {
        let downloaded = j.root.join("download");
        if !downloaded.try_exists()? {
            let (app, config) = load(&o.config)?;
            matches_config(&config, &ctx.binding_specification)?;
            Connection::new(app)?
                .download_git(
                    &config.repository,
                    treeharbor_github::GitDownload {
                        worker: std::env::current_exe()?,
                        git: o.git.clone(),
                        output: downloaded.clone(),
                        commit,
                        limits,
                    },
                )
                .await?;
        }
        let git = o.git.clone();
        let upstream = ctx.binding_specification.upstream.clone();
        let revision = git_revision.clone();
        tokio::task::spawn_blocking(move || {
            let manifest = treeharbor_git_bridge::download::open(&downloaded, limits)?;
            if manifest.owner != upstream.owner
                || manifest.name != upstream.name
                || manifest.revision != revision
            {
                return Err(Error::ImportState);
            }
            Ok(crate::pack::capture_git(GitCapture {
                git: &git,
                bundle: &downloaded.join("history.bundle"),
                commit,
                output: &destination,
                limits,
            })?)
        })
        .await
        .map_err(|_| Error::ImportWorker)??
    };
    if pack.object_count() > limits.objects || pack.encoded_bytes() > limits.source_bytes {
        return Err(Error::Size);
    }
    let Object::NativeCommit(native) = pack
        .store()
        .get(pack.commit())
        .map_err(crate::pack::Error::from)?
    else {
        return Err(Error::ImportState);
    };
    if native.git_origin.as_ref().map(|o| o.oid) != Some(commit) {
        return Err(Error::ImportState);
    }
    j.bind("pack.json", &json!({"version":1,"fingerprint":pack.fingerprint.to_string(),"revision":RevisionRef::Native(pack.commit()).to_string()}))?;
    if current(&o, &c, &historical).await? != ctx {
        return Err(Error::RestoreAssessment);
    }
    let root = pack.commit();
    let reply = crate::import::send(crate::import::Request {
        socket: &o.socket,
        credential: &c,
        pack,
        state: &j.root.join("upload.json"),
        authority: Authority::Connected,
        epoch: o.epoch,
        ttl_seconds: 3600,
        timeout: Duration::from_secs(30),
    })
    .await?;
    if !reply.status.is_success() {
        return Err(Error::GithubBinding {
            status: reply.status.as_u16(),
        });
    }
    if reply.body["result"]["revision"] != RevisionRef::Native(root).to_string() {
        return Err(Error::Http);
    }
    if current(&o, &c, &historical).await? != ctx {
        return Err(Error::RestoreAssessment);
    }
    let mut preservation = spec.clone();
    preservation.disposition = RestoreDisposition::PreserveSource {
        source_holder: format!("base:{}", root.hex()),
        native_revision: RevisionRef::Native(root).to_string(),
    };
    preservation.previous_key = Some(o.decision_key.clone());
    command(&o.socket, &c, json!({"action":"restore_decision_record","key":preserve_key,"epoch":o.epoch,"specification":preservation})).await?;
    let result = verified(
        &o,
        &c,
        &preserve_key,
        &mapping_key,
        &RevisionRef::Git(commit).to_string(),
        &spec,
    )
    .await?
    .ok_or(Error::RestoreAssessment)?;
    finish(&j, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acquisition_phase_keys_match_independent_vector() {
        let v: Value = serde_json::from_str(include_str!(
            "../../../metadata/tests/fixtures/restore-acquire-v3.json"
        ))
        .unwrap();
        let (preserve, mapping) = keys(v["request"]["key"].as_str().unwrap()).unwrap();
        assert_eq!(preserve, v["preserve_key"]);
        assert_eq!(mapping, v["mapping_key"]);
        assert_ne!(
            keys("a-different-acquisition").unwrap(),
            (preserve, mapping)
        );
    }
}
