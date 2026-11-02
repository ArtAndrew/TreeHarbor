//! Retained identities for restore assessment, without reactivating any binding.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_EFFECTS: i64 = 256;
const MAX_CONTEXT: usize = 512 * 1024;

#[path = "restore_effects.rs"]
mod extended;
pub use extended::{RestoreDequeue, RestoreRecovery, RestoreRecoveryPage, RestoreRedelivery};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestoreEffectKind {
    Branch,
    Pull,
    Queue,
    Check,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreEffect {
    pub kind: RestoreEffectKind,
    pub id: Uuid,
    pub branch: Option<Uuid>,
    pub native_object_hex: Option<String>,
    pub expected_git: Option<String>,
    pub prepared_git: Option<String>,
    pub target: Option<PullTarget>,
    pub pull_number: Option<i64>,
    pub pull_node: Option<String>,
    pub attempt: Option<Uuid>,
    pub state: Option<String>,
    pub cancel_requested: bool,
    /// Digest of the entire retained row, including observation/report fields.
    pub row_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreContext {
    pub version: u8,
    pub tenant: String,
    pub repository: Uuid,
    pub epoch: i64,
    pub binding: Uuid,
    pub specification: BindingSpec,
    pub manifest_sha256: String,
    pub restored_database: String,
    pub quarantine_sha256: String,
    pub other_bindings: Vec<Uuid>,
    pub effects: Vec<RestoreEffect>,
    pub dequeues: Vec<RestoreDequeue>,
    pub recoveries: Vec<RestoreRecovery>,
    pub recovery_pages: Vec<RestoreRecoveryPage>,
    pub redeliveries: Vec<RestoreRedelivery>,
    pub reconciliation_complete: bool,
}

pub fn restore_digest<T: Serialize>(value: &T) -> Result<String> {
    // Normalize structs and maps through the workspace's sorted serde_json Map.
    // The same report decoded as JSON must hash identically to its typed value.
    let value = serde_json::to_value(value).map_err(|_| Error::SchemaMismatch)?;
    let bytes = serde_json::to_vec(&value).map_err(|_| Error::SchemaMismatch)?;
    let mut digest = Sha256::new();
    digest.update(b"treeharbor-restore-assessment\0v1\0");
    digest.update(bytes);
    Ok(digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn field<T: serde::de::DeserializeOwned>(row: &Value, name: &str) -> Result<T> {
    serde_json::from_value(row.get(name).ok_or(Error::SchemaMismatch)?.clone())
        .map_err(|_| Error::SchemaMismatch)
}
fn optional<T: serde::de::DeserializeOwned>(row: &Value, name: &str) -> Result<Option<T>> {
    match row.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|_| Error::SchemaMismatch),
    }
}

impl Metadata {
    pub async fn github_restore_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        manifest: &str,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let result = retained_context(&tx, auth, epoch, id, manifest, Permission::HostRead).await?;
        context(&tx, auth, epoch, Permission::HostRead).await?;
        tx.commit().await?;
        Ok(json!(result))
    }
}

// Caller holds the repository lock. Owner recording may inspect the same retained
// identities, while bridge assessment still requires the assigned HostRead role.
pub(super) async fn retained_context(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    id: Uuid,
    manifest: &str,
    permission: Permission,
) -> Result<RestoreContext> {
    uuid(id)?;
    digest_valid(manifest)?;
    tx.batch_execute("SET LOCAL TimeZone='UTC'; SET LOCAL bytea_output='hex'")
        .await?;
    context(tx, auth, epoch, permission).await?;
    let s = auth.scope();
    let b = binding(tx, s, id).await?;
    let spec = specification(&b)?;
    if permission == Permission::HostRead && spec.bridge != auth.principal() {
        return Err(Error::Forbidden);
    }
    if b.get::<_, i64>("epoch") != epoch || b.get::<_, String>("state") != "revoked" {
        return Err(Error::HostBindingState);
    }
    let (receipt, database) = quarantine_receipt(tx, s, manifest).await?;
    let bindings = tx.query("SELECT binding,state FROM th_github_bindings WHERE tenant=$1 AND repository=$2 ORDER BY binding LIMIT $3",
        &[&s.tenant(),&s.repository(),&(MAX_EFFECTS+1)]).await?;
    if bindings.len() > MAX_EFFECTS as usize {
        return Err(Error::RestoreContextLimit);
    }
    if bindings
        .iter()
        .any(|b| b.get::<_, String>("state") != "revoked")
    {
        return Err(Error::HostBindingState);
    }
    let other_bindings = bindings
        .iter()
        .map(|b| b.get::<_, Uuid>("binding"))
        .filter(|b| *b != id)
        .collect();
    let mut effects = Vec::new();
    // Table and identifier names below are constants, never client input.
    for (kind, table, primary) in [
        (
            RestoreEffectKind::Branch,
            "th_github_publications",
            "publication",
        ),
        (RestoreEffectKind::Pull, "th_github_pulls", "pull"),
        (RestoreEffectKind::Queue, "th_github_queue", "queue"),
        (
            RestoreEffectKind::Check,
            "th_github_check_publications",
            "publication",
        ),
    ] {
        let rows=tx.query(&format!("SELECT to_jsonb(t) FROM {table} t WHERE tenant=$1 AND repository=$2 AND binding=$3 ORDER BY {primary} LIMIT $4"),
            &[&s.tenant(),&s.repository(),&id,&(MAX_EFFECTS+1)]).await?;
        if rows.len() > MAX_EFFECTS as usize {
            return Err(Error::RestoreContextLimit);
        }
        for row in rows {
            let value: Value = row.get(0);
            if field::<i64>(&value, "epoch")? != epoch {
                return Err(Error::Fenced);
            }
            let native: Option<String> = optional(&value, "native_commit")?;
            let native_object_hex = native
                .map(|v| {
                    v.strip_prefix("\\x")
                        .map(str::to_owned)
                        .ok_or(Error::SchemaMismatch)
                })
                .transpose()?;
            effects.push(RestoreEffect {
                kind: kind.clone(),
                id: field(&value, primary)?,
                branch: optional(&value, "branch")?,
                native_object_hex,
                expected_git: optional(&value, "expected_git")?,
                prepared_git: optional(&value, "prepared_git")?,
                target: if kind == RestoreEffectKind::Pull {
                    Some(field(&value, "target")?)
                } else {
                    None
                },
                pull_number: optional(&value, "pull_number")?,
                pull_node: optional(&value, "pull_node")?,
                attempt: optional(&value, "attempt")?,
                state: optional(&value, "state")?,
                cancel_requested: field(&value, "cancel_requested")?,
                row_sha256: restore_digest(&value)?,
            });
        }
    }
    let (dequeues, recoveries, recovery_pages, redeliveries) =
        extended::load(tx, s, id, epoch).await?;
    let result = RestoreContext {
        version: 2,
        tenant: s.tenant().into(),
        repository: s.repository(),
        epoch,
        binding: id,
        specification: spec,
        manifest_sha256: manifest.into(),
        restored_database: database,
        quarantine_sha256: restore_digest(&receipt)?,
        other_bindings,
        effects,
        dequeues,
        recoveries,
        recovery_pages,
        redeliveries,
        reconciliation_complete: false,
    };
    let bytes = serde_json::to_vec(&result).map_err(|_| Error::SchemaMismatch)?;
    if bytes.len() > MAX_CONTEXT {
        return Err(Error::RestoreContextLimit);
    }
    Ok(result)
}

// Shared by discovery-only target registration and retained-binding inspection.
// A caller has already checked current authority/epoch and holds the repository lock.
pub(super) async fn quarantine_receipt(
    tx: &Transaction<'_>,
    s: &Scope,
    manifest: &str,
) -> Result<(Value, String)> {
    digest_valid(manifest)?;
    let key = format!("restore-quarantine:{manifest}");
    let journal = tx.query_opt("SELECT result,request_digest,current_database()::text AS database FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind='restore.quarantine.v1' AND principal='restore-quarantine'",
        &[&s.tenant(),&s.repository(),&key]).await?.ok_or(Error::HostRecoveryRequired)?;
    let receipt: Value = journal.get("result");
    let database: String = journal.get("database");
    let digest = Sha256::digest(format!(
        "treeharbor-restore-quarantine\0v1\0{manifest}\0{database}"
    ))
    .to_vec();
    if receipt["version"] != 1
        || receipt["status"] != "quarantined"
        || receipt["manifest_sha256"] != manifest
        || receipt["database"] != database
        || journal.get::<_, Vec<u8>>("request_digest") != digest
    {
        return Err(Error::HostRecoveryRequired);
    }
    Ok((receipt, database))
}

pub(super) fn digest_valid(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid("restore digest"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_context_v2_matches_independent_vector_and_requires_every_family() {
        let vector: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/restore-context-v2.json"))
                .unwrap();
        let c: RestoreContext = serde_json::from_value(vector["context"].clone()).unwrap();
        assert_eq!(c.version, 2);
        assert_eq!(json!(c), vector["context"]);
        assert_eq!(restore_digest(&c).unwrap(), vector["context_sha256"]);
        for key in ["dequeues", "recoveries", "recovery_pages", "redeliveries"] {
            let mut missing = vector["context"].clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(serde_json::from_value::<RestoreContext>(missing).is_err());
        }
    }
    #[test]
    fn restore_digest_matches_independent_json_vector_and_struct_field_order() {
        let vector: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/restore-digest-v1.json"))
                .unwrap();
        #[derive(Serialize)]
        struct Ordered {
            z: String,
            a: Vec<Value>,
        }
        let typed = Ordered {
            z: "x".into(),
            a: vec![json!(1), json!(true), Value::Null],
        };
        assert_eq!(
            restore_digest(&typed).unwrap(),
            vector["sha256"].as_str().unwrap()
        );
        assert_eq!(
            restore_digest(&vector["value"]).unwrap(),
            vector["sha256"].as_str().unwrap()
        );
    }
}
