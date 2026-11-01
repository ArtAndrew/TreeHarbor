//! Append-only operator decisions; source retention is atomic with the receipt.
//! Decisions do not prove host facts or authorize reconnection/publication.
use super::*;
use crate::identifier;
use serde::{Deserialize, Serialize};
use treeharbor_core::RevisionRef;

const KIND: &str = "github.restore.decision.v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RestoreDecisionSubject {
    Retained {
        effect_kind: RestoreEffectKind,
        operation: Uuid,
    },
    RetainedDequeue {
        operation: Uuid,
    },
    RetainedRecovery {
        recovery: Uuid,
    },
    RetainedRedelivery {
        recovery: Uuid,
        provider_id: i64,
    },
    ExternalBranch {
        name: String,
    },
    ExternalPull {
        number: i64,
        node: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RestoreDisposition {
    AwaitEvidence,
    /// Operator intention, not a claim that the original operation never ran.
    DoNotReplay,
    /// Retain an already accepted commit. No unverified locator can create a pin.
    PreserveSource {
        source_holder: String,
        native_revision: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreDecisionSpec {
    pub version: u8,
    pub binding: Uuid,
    pub manifest_sha256: String,
    pub context_sha256: String,
    /// SHA-256 of the exact externally retained evidence file bytes. This is an
    /// operator assertion, not a server attestation of the file or GitHub facts.
    pub evidence_sha256: String,
    pub subject: RestoreDecisionSubject,
    pub disposition: RestoreDisposition,
    pub previous_key: Option<String>,
    pub note: String,
}
impl RestoreDecisionSpec {
    fn validate(&self) -> Result<()> {
        if !matches!(self.version, 1 | 2)
            || self.note.trim().is_empty()
            || self.note.len() > 4096
            || self
                .note
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(Error::Invalid("restore decision"));
        }
        uuid(self.binding)?;
        for digest in [
            &self.manifest_sha256,
            &self.context_sha256,
            &self.evidence_sha256,
        ] {
            restore::digest_valid(digest)?;
        }
        if let Some(key) = &self.previous_key {
            identifier(key, 128)?;
        }
        match &self.subject {
            RestoreDecisionSubject::Retained { operation, .. } => uuid(*operation)?,
            RestoreDecisionSubject::RetainedDequeue { operation } => {
                self.require_v2()?;
                uuid(*operation)?;
            }
            RestoreDecisionSubject::RetainedRecovery { recovery } => {
                self.require_v2()?;
                uuid(*recovery)?;
            }
            RestoreDecisionSubject::RetainedRedelivery {
                recovery,
                provider_id,
            } => {
                self.require_v2()?;
                uuid(*recovery)?;
                if *provider_id <= 0 {
                    return Err(Error::Invalid("restore redelivery subject"));
                }
            }
            RestoreDecisionSubject::ExternalBranch { name } => {
                // External names are recorded as data, never passed to Git or a shell.
                if name.is_empty() || name.len() > 1024 || name.chars().any(char::is_control) {
                    return Err(Error::Invalid("restore branch subject"));
                }
            }
            RestoreDecisionSubject::ExternalPull { number, node } => {
                if *number <= 0 {
                    return Err(Error::Invalid("restore pull subject"));
                }
                identifier(node, 256)?;
            }
        }
        if let RestoreDisposition::PreserveSource {
            source_holder,
            native_revision,
        } = &self.disposition
        {
            identifier(source_holder, 128)?;
            native_commit(native_revision)?;
        }
        Ok(())
    }
    fn require_v2(&self) -> Result<()> {
        if self.version != 2 {
            return Err(Error::Invalid("restore decision subject version"));
        }
        Ok(())
    }
}
fn native_commit(value: &str) -> Result<treeharbor_core::ObjectId> {
    match value.parse::<RevisionRef>() {
        Ok(RevisionRef::Native(id)) if RevisionRef::Native(id).to_string() == value => Ok(id),
        _ => Err(Error::Invalid("restore source revision")),
    }
}

impl Metadata {
    pub async fn record_github_restore_decision(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        spec: &RestoreDecisionSpec,
    ) -> Result<Value> {
        spec.validate()?;
        let request = request(auth, cmd, KIND, json!([1, spec]))?;
        let m = match self.begin(request).await? {
            Begin::Replay(receipt) => return Ok(receipt),
            Begin::Fresh(m) => m,
        };
        let ctx = restore::retained_context(
            &m.tx,
            auth,
            cmd.epoch,
            spec.binding,
            &spec.manifest_sha256,
            Permission::RepositoryWrite,
        )
        .await?;
        if restore_digest(&ctx)? != spec.context_sha256 {
            return Err(Error::RestoreContextChanged);
        }
        let present = match &spec.subject {
            RestoreDecisionSubject::Retained {
                effect_kind,
                operation,
            } => ctx
                .effects
                .iter()
                .any(|e| e.kind == *effect_kind && e.id == *operation),
            RestoreDecisionSubject::RetainedDequeue { operation } => {
                ctx.dequeues.iter().any(|e| e.id == *operation)
            }
            RestoreDecisionSubject::RetainedRecovery { recovery } => {
                ctx.recoveries.iter().any(|e| e.recovery == *recovery)
            }
            RestoreDecisionSubject::RetainedRedelivery {
                recovery,
                provider_id,
            } => ctx
                .redeliveries
                .iter()
                .any(|e| e.recovery == *recovery && e.provider_id == *provider_id),
            RestoreDecisionSubject::ExternalBranch { .. }
            | RestoreDecisionSubject::ExternalPull { .. } => true,
        };
        if !present {
            return Err(Error::NotFound);
        }
        let s = auth.scope();
        let subject_digest = restore_digest(&spec.subject)?;
        // Repository lock serializes the chain, including two different request keys.
        // Subject/manifest/binding are scoped together; context may change between
        // reviewed decisions, but the old decision remains visible and immutable.
        let previous = m.tx.query_opt("SELECT key,(result->>'sequence')::bigint AS sequence FROM th_operations WHERE tenant=$1 AND repository=$2 AND kind=$3 AND result->>'manifest_sha256'=$4 AND result->>'binding'=$5 AND result->>'subject_sha256'=$6 ORDER BY (result->>'sequence')::bigint DESC LIMIT 1",
            &[&s.tenant(), &s.repository(), &KIND, &spec.manifest_sha256, &spec.binding.to_string(), &subject_digest]).await?;
        let previous_key: Option<String> = previous.as_ref().map(|r| r.get("key"));
        if previous_key != spec.previous_key {
            return Err(Error::RestoreDecisionConflict);
        }
        let sequence = previous.map_or(Ok(1), |r| {
            r.get::<_, i64>("sequence")
                .checked_add(1)
                .ok_or(Error::RestoreContextLimit)
        })?;
        let mut preserved = Value::Null;
        if let RestoreDisposition::PreserveSource {
            source_holder,
            native_revision,
        } = &spec.disposition
        {
            let id = native_commit(native_revision)?;
            // Allocate a stable per-operation pin. Copy validates and locks the
            // original object's live generation before any durable acknowledgement.
            let pin = format!("restore-decision:{}", restore_digest(&json!([1, cmd.key]))?);
            m.copy_root_pin(source_holder, &pin).await?;
            let row = m.tx.query_one("SELECT p.digest,p.generation,o.kind,o.backend FROM th_object_pins p JOIN th_objects o ON o.tenant=p.tenant AND o.digest=p.digest AND o.generation=p.generation WHERE p.tenant=$1 AND p.repository=$2 AND p.holder=$3",
                &[&s.tenant(), &s.repository(), &pin]).await?;
            if row.get::<_, Vec<u8>>("digest") != id.as_bytes() || row.get::<_, i16>("kind") != 5 {
                return Err(Error::ReceiptMismatch);
            }
            preserved = json!({"holder":pin,"native_revision":native_revision,
                "generation":row.get::<_,i64>("generation"),"backend":row.get::<_,String>("backend"),
                "git_mapping_verified":false});
        }
        let receipt = json!({"version":1,"key":cmd.key,"sequence":sequence,
            "tenant":s.tenant(),"repository":s.repository(),"epoch":cmd.epoch,
            "binding":spec.binding,"manifest_sha256":spec.manifest_sha256,
            "subject_sha256":subject_digest,"specification":spec,"context":ctx,
            "recorded_by":auth.principal(),"source_preservation":preserved,
            "external_facts_verified":false,"reconciliation_complete":false,"publication_authorized":false});
        // No dispatchable outbox task: a decision is never an external write grant.
        m.complete(receipt, Vec::new()).await
    }

    /// Return an immutable historical receipt. It is not a live validity decision.
    pub async fn github_restore_decision(
        &mut self,
        auth: &Authorization,
        key: &str,
    ) -> Result<Value> {
        identifier(key, 128)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        let receipt: Value = tx.query_opt("SELECT result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind=$4",
            &[&s.tenant(), &s.repository(), &key, &KIND]).await?.ok_or(Error::NotFound)?.get(0);
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_v2_subjects_keep_composite_identity_and_reject_v1_encoding() {
        let vector: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/restore-context-v2.json"))
                .unwrap();
        let old: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/restore-decision-v1.json"
        ))
        .unwrap();
        let mut spec: RestoreDecisionSpec =
            serde_json::from_value(old["request"]["specification"].clone()).unwrap();
        for item in vector["subjects"].as_array().unwrap() {
            spec.subject = serde_json::from_value(item["subject"].clone()).unwrap();
            spec.version = 2;
            spec.validate().unwrap();
            assert_eq!(
                restore_digest(&spec.subject).unwrap(),
                item["subject_sha256"]
            );
            spec.version = 1;
            assert!(spec.validate().is_err());
        }
        spec.version = 2;
        for provider_id in [0, -1] {
            spec.subject = RestoreDecisionSubject::RetainedRedelivery {
                recovery: Uuid::from_bytes([5; 16]),
                provider_id,
            };
            assert!(spec.validate().is_err());
        }
    }
    #[test]
    fn decision_wire_and_pin_identity_match_independent_v1_vector() {
        let vector: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/restore-decision-v1.json"
        ))
        .unwrap();
        let wire: WireCommand = serde_json::from_value(vector["request"].clone()).unwrap();
        let WireCommand::RestoreDecisionRecord {
            key,
            epoch,
            specification,
        } = wire
        else {
            panic!()
        };
        assert_eq!(epoch, 1);
        specification.validate().unwrap();
        assert_eq!(json!(specification), vector["request"]["specification"]);
        assert_eq!(
            restore_digest(&specification.subject).unwrap(),
            vector["subject_sha256"]
        );
        assert_eq!(
            format!(
                "restore-decision:{}",
                restore_digest(&json!([1, key])).unwrap()
            ),
            vector["pin_holder"]
        );
        let mut extra = vector["request"].clone();
        extra["specification"]["publication_authorized"] = json!(true);
        assert!(serde_json::from_value::<WireCommand>(extra).is_err());
        for action in ["approve", "resume", "clear_quarantine"] {
            let mut unsupported = vector["request"].clone();
            unsupported["specification"]["disposition"] = json!({"action":action});
            assert!(serde_json::from_value::<WireCommand>(unsupported).is_err());
        }
    }
}
