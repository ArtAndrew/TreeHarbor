use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Operator-supplied definition. Construct a validated Profile before using it.
/// The immutable image must contain the trusted static protocol-1 init at /init.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub version: u8,
    pub image: String,
    pub uid: u32,
    pub gid: u32,
    pub memory_bytes: u64,
    pub nano_cpus: u64,
    pub max_processes: u32,
    pub timeout_ms: u64,
    pub workspace_bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile(Definition);
impl Profile {
    pub fn new(value: Definition) -> Result<Self> {
        if value.version != 1
            || !sha256_id(&value.image)
            || [value.uid, value.gid]
                .into_iter()
                .any(|id| !(1..=2_147_483_647).contains(&id) || id == 65534)
            || !(128 * 1024 * 1024..=16 * 1024 * 1024 * 1024).contains(&value.memory_bytes)
            || !(100_000_000..=16_000_000_000).contains(&value.nano_cpus)
            || !(4..=4096).contains(&value.max_processes)
            || !(100..=86_400_000).contains(&value.timeout_ms)
            || !(64 * 1024 * 1024..=64 * 1024 * 1024 * 1024).contains(&value.workspace_bytes)
        {
            return Err(Error::Invalid);
        }
        Ok(Self(value))
    }
    pub fn definition(&self) -> &Definition {
        &self.0
    }
    pub fn encode(&self) -> Vec<u8> {
        // A fixed versioned struct of strings and integers is infallible here.
        serde_json::to_vec(&self.0).expect("runtime profile serialization")
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 4096 {
            return Err(Error::Limit);
        }
        let profile = Self::new(serde_json::from_slice(bytes)?)?;
        if profile.encode() != bytes {
            return Err(Error::Invalid);
        }
        Ok(profile)
    }
    pub fn digest(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"treeharbor-runtime-profile\0v1\0");
        hash.update(self.encode());
        hash.finalize().into()
    }
}

/// An explicit UTF-8 argv, never a host shell command or Docker option vector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task(Vec<String>);
impl Task {
    pub fn new(argv: Vec<String>) -> Result<Self> {
        let bytes = argv
            .iter()
            .try_fold(0usize, |sum, arg| sum.checked_add(arg.len()));
        if argv.is_empty()
            || argv.len() > 128
            || bytes.is_none_or(|n| n > 60 * 1024)
            || argv.iter().any(|s| s.contains('\0'))
            || !argv[0].starts_with('/')
            || argv[0].len() < 2
        {
            return Err(Error::Invalid);
        }
        Ok(Self(argv))
    }
    pub fn argv(&self) -> &[String] {
        &self.0
    }
    pub fn init_arguments(&self, profile: &Profile) -> Vec<String> {
        let p = profile.definition();
        let mut args = vec![
            "--protocol".into(),
            "1".into(),
            "--timeout-ms".into(),
            p.timeout_ms.to_string(),
            "--max-processes".into(),
            p.max_processes.to_string(),
            "--uid".into(),
            p.uid.to_string(),
            "--gid".into(),
            p.gid.to_string(),
            "--".into(),
        ];
        args.extend(self.0.iter().cloned());
        args
    }
}
pub(crate) fn sha256_id(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value.as_bytes()[7..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_profile_vector_and_noncanonical_rejection() {
        let bytes = include_bytes!("../tests/fixtures/profile-v1.json");
        let profile = Profile::decode(bytes).unwrap();
        assert_eq!(
            hex(&profile.digest()),
            include_str!("../tests/fixtures/profile-v1.sha256").trim()
        );
        let mut whitespace = bytes.to_vec();
        whitespace.push(b'\n');
        assert!(Profile::decode(&whitespace).is_err());
        let mut unknown: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        unknown["extra"] = serde_json::Value::Bool(true);
        assert!(Profile::decode(&serde_json::to_vec(&unknown).unwrap()).is_err());
    }
    #[test]
    fn profile_and_task_reject_unbounded_or_ambiguous_execution() {
        let valid = Definition {
            version: 1,
            image: format!("sha256:{}", "11".repeat(32)),
            uid: 1001,
            gid: 1001,
            memory_bytes: 268435456,
            nano_cpus: 1_000_000_000,
            max_processes: 64,
            timeout_ms: 10000,
            workspace_bytes: 134217728,
        };
        let profile = Profile::new(valid.clone()).unwrap();
        assert_eq!(Profile::decode(&profile.encode()).unwrap(), profile);
        let mut bad = valid.clone();
        bad.image = "example:latest".into();
        assert!(Profile::new(bad).is_err());
        for uid in [0, 65534, u32::MAX] {
            let mut bad = valid.clone();
            bad.uid = uid;
            assert!(Profile::new(bad).is_err());
        }
        let mut bad = valid;
        bad.version = 2;
        assert!(Profile::new(bad).is_err());
        assert!(Task::new(vec!["relative".into()]).is_err());
        assert!(Task::new(vec!["/bin/tool".into(), "x\0y".into()]).is_err());
        assert!(Task::new(vec!["/bin/tool".into(); 129]).is_err());
        let task = Task::new(vec!["/bin/tool".into(), "$(never-a-host-shell)".into()]).unwrap();
        assert_eq!(
            task.init_arguments(&profile).last().unwrap(),
            "$(never-a-host-shell)"
        );
    }
}
