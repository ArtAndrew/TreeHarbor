//! Fresh membership observations, never settlement of a previous enqueue/dequeue.
use super::*;
use serde::{Deserialize, Serialize};
use treeharbor_github_contract::{queue::QueueTarget, restore_queue::RestoreQueueReport};
const KIND: &str = "github.restore.queue.v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RestoreQueueSubject {
    Queue { operation: Uuid },
    Dequeue { operation: Uuid },
}
impl RestoreQueueSubject {
    fn validate(&self) -> Result<()> {
        let (Self::Queue { operation } | Self::Dequeue { operation }) = self;
        uuid(*operation)
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreQueueContext {
    pub version: u8,
    pub context: RestoreContext,
    pub context_sha256: String,
    pub subject: RestoreQueueSubject,
    pub queue: Uuid,
    pub target: QueueTarget,
    pub publication_authorized: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreQueueEvidence {
    pub version: u8,
    pub binding: Uuid,
    pub manifest_sha256: String,
    pub context_sha256: String,
    pub subject: RestoreQueueSubject,
    pub report: RestoreQueueReport,
}
impl RestoreQueueEvidence {
    fn validate(&self) -> Result<()> {
        uuid(self.binding)?;
        self.subject.validate()?;
        restore::digest_valid(&self.manifest_sha256)?;
        restore::digest_valid(&self.context_sha256)?;
        if self.version != 1 {
            return Err(Error::Invalid("restore queue evidence"));
        }
        Ok(())
    }
}
async fn retained(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    binding: Uuid,
    manifest: &str,
    subject: &RestoreQueueSubject,
) -> Result<RestoreQueueContext> {
    subject.validate()?;
    let ctx =
        restore::retained_context(tx, auth, epoch, binding, manifest, Permission::HostRead).await?;
    let id = match subject {
        RestoreQueueSubject::Queue { operation } => *operation,
        RestoreQueueSubject::Dequeue { operation } => {
            ctx.dequeues
                .iter()
                .find(|d| d.id == *operation)
                .ok_or(Error::NotFound)?
                .queue
        }
    };
    if !ctx
        .effects
        .iter()
        .any(|e| e.kind == RestoreEffectKind::Queue && e.id == id)
    {
        return Err(Error::NotFound);
    }
    let row = queue::load(tx, auth.scope(), id).await?;
    if row.get::<_, Uuid>("binding") != binding || row.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    let target = queue::target(&row)?;
    target.validate().map_err(|_| Error::SchemaMismatch)?;
    if target.pull.repository_id != ctx.specification.upstream.repository_id {
        return Err(Error::SchemaMismatch);
    }
    Ok(RestoreQueueContext {
        version: 1,
        context_sha256: restore_digest(&ctx)?,
        context: ctx,
        subject: subject.clone(),
        queue: id,
        target,
        publication_authorized: false,
    })
}
impl Metadata {
    pub async fn github_restore_queue_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        binding: Uuid,
        manifest: &str,
        subject: &RestoreQueueSubject,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let result = retained(&tx, auth, epoch, binding, manifest, subject).await?;
        context(&tx, auth, epoch, Permission::HostRead).await?;
        tx.commit().await?;
        Ok(json!(result))
    }
    pub async fn record_github_restore_queue(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        spec: &RestoreQueueEvidence,
    ) -> Result<Value> {
        spec.validate()?;
        let m = match self
            .begin(request(auth, cmd, KIND, json!([1, spec]))?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let ctx = retained(
            &m.tx,
            auth,
            cmd.epoch,
            spec.binding,
            &spec.manifest_sha256,
            &spec.subject,
        )
        .await?;
        if ctx.context_sha256 != spec.context_sha256 {
            return Err(Error::RestoreContextChanged);
        }
        let r = &spec.report;
        let classification = r
            .classification(&ctx.target)
            .map_err(|_| Error::Invalid("restore queue report"))?;
        if r.app_id != ctx.context.specification.app_id
            || r.installation_id != ctx.context.specification.upstream.installation_id
        {
            return Err(Error::HostPullState);
        }
        let now: i64 =
            m.tx.query_one(
                "SELECT (extract(epoch FROM clock_timestamp())*1000)::bigint",
                &[],
            )
            .await?
            .get(0);
        if r.finished_at_unix_ms > now.saturating_add(5000)
            || r.finished_at_unix_ms < now.saturating_sub(120_000)
        {
            return Err(Error::Invalid("restore queue capture time"));
        }
        let receipt = json!({"version":1,"key":cmd.key,"tenant":auth.scope().tenant(),"repository":auth.scope().repository(),
            "epoch":cmd.epoch,"recorded_by":auth.principal(),"specification":spec,"queue":ctx.queue,"retained_target":ctx.target,
            "capture_consistent":r.before==r.after,"classification":classification,
            "prior_mutation_settled":false,"source_mapping_verified":false,"reconciliation_complete":false,"publication_authorized":false});
        m.complete(receipt, vec![]).await
    }
    pub async fn github_restore_queue(&mut self, auth: &Authorization, key: &str) -> Result<Value> {
        crate::identifier(key, 128)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        let value:Value=tx.query_opt("SELECT result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind=$4",&[&s.tenant(),&s.repository(),&key,&KIND]).await?.ok_or(Error::NotFound)?.get(0);
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_queue_wire_matches_independent_digest_and_refuses_supplied_settlement() {
        let v: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/restore-queue-v1.json"))
                .unwrap();
        let wire: WireCommand = serde_json::from_value(v["request"].clone()).unwrap();
        assert_eq!(wire.permission(), Permission::HostRead);
        let WireCommand::RestoreQueueRecord { specification, .. } = wire else {
            panic!()
        };
        specification.validate().unwrap();
        assert_eq!(json!(specification), v["request"]["specification"]);
        assert_eq!(
            restore_digest(&specification).unwrap(),
            v["specification_sha256"]
        );
        for field in [
            "prior_mutation_settled",
            "publication_authorized",
            "reconciliation_complete",
        ] {
            let mut bad = v["request"].clone();
            bad["specification"][field] = json!(true);
            assert!(serde_json::from_value::<WireCommand>(bad).is_err());
        }
    }
}
