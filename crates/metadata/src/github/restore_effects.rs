//! Retained removal and webhook-delivery identities. No dispatch or host proof.
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreDequeue {
    pub id: Uuid,
    pub queue: Uuid,
    pub attempt: Option<Uuid>,
    pub row_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreRecovery {
    pub recovery: Uuid,
    pub channel: Uuid,
    pub cancelled: bool,
    pub row_sha256: String,
    pub channel_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreRecoveryPage {
    pub recovery: Uuid,
    pub sequence: i32,
    pub row_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreRedelivery {
    pub recovery: Uuid,
    pub provider_id: i64,
    pub delivery: Uuid,
    pub dispatch: Option<Uuid>,
    pub row_sha256: String,
}

fn bounded(rows: Vec<Row>) -> Result<Vec<Row>> {
    if rows.len() > MAX_EFFECTS as usize {
        return Err(Error::RestoreContextLimit);
    }
    Ok(rows)
}
fn epoch_matches(row: &Value, epoch: i64) -> Result<()> {
    if field::<i64>(row, "epoch")? != epoch {
        return Err(Error::Fenced);
    }
    Ok(())
}

pub(super) async fn load(
    tx: &Transaction<'_>,
    s: &Scope,
    binding: Uuid,
    epoch: i64,
) -> Result<(
    Vec<RestoreDequeue>,
    Vec<RestoreRecovery>,
    Vec<RestoreRecoveryPage>,
    Vec<RestoreRedelivery>,
)> {
    let limit = MAX_EFFECTS + 1;
    let params: &[&(dyn tokio_postgres::types::ToSql + Sync)] =
        &[&s.tenant(), &s.repository(), &binding, &limit];
    let mut dequeues = Vec::new();
    let rows = bounded(tx.query("SELECT to_jsonb(d),q.epoch FROM th_github_dequeues d JOIN th_github_queue q ON q.tenant=d.tenant AND q.repository=d.repository AND q.queue=d.queue WHERE q.tenant=$1 AND q.repository=$2 AND q.binding=$3 ORDER BY d.dequeue LIMIT $4", params).await?)?;
    for row in rows {
        let value: Value = row.get(0);
        epoch_matches(&value, epoch)?;
        if row.get::<_, i64>(1) != epoch {
            return Err(Error::Fenced);
        }
        dequeues.push(RestoreDequeue {
            id: field(&value, "dequeue")?,
            queue: field(&value, "queue")?,
            attempt: optional(&value, "attempt")?,
            row_sha256: restore_digest(&value)?,
        });
    }
    let mut recoveries = Vec::new();
    let rows = bounded(tx.query("SELECT to_jsonb(r),to_jsonb(c) FROM th_github_event_recoveries r JOIN th_github_event_channels c ON c.tenant=r.tenant AND c.repository=r.repository AND c.channel=r.channel WHERE r.tenant=$1 AND r.repository=$2 AND r.binding=$3 ORDER BY r.recovery LIMIT $4", params).await?)?;
    for row in rows {
        let value: Value = row.get(0);
        let channel: Value = row.get(1);
        epoch_matches(&value, epoch)?;
        epoch_matches(&channel, epoch)?;
        if field::<Uuid>(&channel, "binding")? != binding {
            return Err(Error::SchemaMismatch);
        }
        recoveries.push(RestoreRecovery {
            recovery: field(&value, "recovery")?,
            channel: field(&value, "channel")?,
            cancelled: field(&value, "cancelled")?,
            row_sha256: restore_digest(&value)?,
            channel_sha256: restore_digest(&channel)?,
        });
    }
    let mut pages = Vec::new();
    let rows = bounded(tx.query("SELECT to_jsonb(p) FROM th_github_event_recovery_pages p JOIN th_github_event_recoveries r ON r.tenant=p.tenant AND r.repository=p.repository AND r.recovery=p.recovery WHERE r.tenant=$1 AND r.repository=$2 AND r.binding=$3 ORDER BY p.recovery,p.sequence LIMIT $4", params).await?)?;
    for row in rows {
        let value: Value = row.get(0);
        pages.push(RestoreRecoveryPage {
            recovery: field(&value, "recovery")?,
            sequence: field(&value, "sequence")?,
            row_sha256: restore_digest(&value)?,
        });
    }
    let mut attempts = Vec::new();
    let rows = bounded(tx.query("SELECT to_jsonb(a) FROM th_github_event_recovery_attempts a JOIN th_github_event_recoveries r ON r.tenant=a.tenant AND r.repository=a.repository AND r.recovery=a.recovery WHERE r.tenant=$1 AND r.repository=$2 AND r.binding=$3 ORDER BY a.recovery,a.provider_id LIMIT $4", params).await?)?;
    for row in rows {
        let value: Value = row.get(0);
        attempts.push(RestoreRedelivery {
            recovery: field(&value, "recovery")?,
            provider_id: field(&value, "provider_id")?,
            delivery: field(&value, "delivery")?,
            dispatch: optional(&value, "dispatch")?,
            row_sha256: restore_digest(&value)?,
        });
    }
    Ok((dequeues, recoveries, pages, attempts))
}
