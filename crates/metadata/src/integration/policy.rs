use crate::{Error, Result, identifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use treeharbor_core::ObjectId;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckDefinition {
    pub name: String,
    pub definition: String,
    pub environment: String,
    pub toolchain: String,
    pub dependencies: String,
    pub validators: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub version: u8,
    pub checks: Vec<CheckDefinition>,
    pub reviewers: BTreeSet<String>,
    pub approvals: u16,
}

impl Policy {
    pub fn canonical(&self) -> Result<Vec<u8>> {
        if self.version != 1
            || self.checks.is_empty()
            || self.checks.len() > 32
            || self.reviewers.len() > 32
            || self.approvals == 0
            || usize::from(self.approvals) > self.reviewers.len()
        {
            return Err(Error::Invalid("native policy bounds"));
        }
        let mut policy = self.clone();
        policy.checks.sort_by(|a, b| a.name.cmp(&b.name));
        if policy.checks.windows(2).any(|w| w[0].name == w[1].name) {
            return Err(Error::Invalid("duplicate native check"));
        }
        for reviewer in &self.reviewers {
            identifier(reviewer, 128)?;
        }
        for check in &self.checks {
            identifier(&check.name, 128)?;
            if check.validators.is_empty() || check.validators.len() > 16 {
                return Err(Error::Invalid("native validators"));
            }
            for value in [
                &check.definition,
                &check.environment,
                &check.toolchain,
                &check.dependencies,
            ] {
                value
                    .parse::<ObjectId>()
                    .map_err(|_| Error::Invalid("policy digest"))?;
            }
            for validator in &check.validators {
                identifier(validator, 128)?;
            }
        }
        let bytes = serde_json::to_vec(&policy).map_err(|_| Error::Invalid("native policy"))?;
        if bytes.len() > 65536 {
            return Err(Error::Invalid("native policy size"));
        }
        Ok(bytes)
    }
    pub fn digest(&self) -> Result<[u8; 32]> {
        let mut hash = Sha256::new();
        hash.update(b"treeharbor-native-policy\0v1\0");
        hash.update(self.canonical()?);
        Ok(hash.finalize().into())
    }
    pub(crate) fn decode(digest: &[u8], bytes: &[u8]) -> Result<Self> {
        let policy: Self = serde_json::from_slice(bytes).map_err(|_| Error::SchemaMismatch)?;
        if policy.canonical()? != bytes || policy.digest()?.as_slice() != digest {
            return Err(Error::SchemaMismatch);
        }
        Ok(policy)
    }
    pub(crate) fn check(&self, name: &str, run: &str) -> Result<treeharbor_validation::Check> {
        let definition = self
            .checks
            .iter()
            .find(|c| c.name == name)
            .ok_or(Error::NotFound)?;
        let digest = |s: &str| -> Result<[u8; 32]> { Ok(*s.parse::<ObjectId>()?.as_bytes()) };
        Ok(treeharbor_validation::Check {
            name: name.into(),
            run_id: run.into(),
            definition_digest: digest(&definition.definition)?,
            environment_digest: digest(&definition.environment)?,
            toolchain_digest: digest(&definition.toolchain)?,
            dependency_digest: digest(&definition.dependencies)?,
            trusted_validators: definition.validators.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_matches_independent_canonical_vector_and_rejects_ambiguous_checks() {
        let vector: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/native-integration-v1.json"
        ))
        .unwrap();
        let policy: Policy = serde_json::from_value(vector["policy"].clone()).unwrap();
        assert_eq!(
            policy.canonical().unwrap(),
            vector["canonical"].as_str().unwrap().as_bytes()
        );
        assert_eq!(
            ObjectId::from_bytes(policy.digest().unwrap()).hex(),
            vector["policy_digest"]
        );
        Policy::decode(&policy.digest().unwrap(), &policy.canonical().unwrap()).unwrap();
        assert!(Policy::decode(&[0; 32], &policy.canonical().unwrap()).is_err());
        let mut duplicate = policy.clone();
        duplicate.checks.push(duplicate.checks[0].clone());
        assert!(duplicate.canonical().is_err());
        let mut invalid = policy.clone();
        invalid.approvals = 0;
        assert!(invalid.canonical().is_err());
        let mut invalid = policy;
        invalid.checks[0].validators.clear();
        assert!(invalid.canonical().is_err());
    }
}
