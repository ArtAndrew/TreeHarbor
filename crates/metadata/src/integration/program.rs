use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use treeharbor_runtime::{Profile, Task};

/// Offline check: all external tooling/dependencies are in the immutable image;
/// repository inputs come from the exact candidate. No host environment or secrets.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckProgram {
    pub version: u8,
    pub runtime_profile: String,
    pub argv: Vec<String>,
}
impl CheckProgram {
    pub fn canonical(&self) -> Result<Vec<u8>> {
        if self.version != 1 {
            return Err(Error::Invalid("check program version"));
        }
        self.runtime_profile
            .parse::<ObjectId>()
            .map_err(|_| Error::Invalid("runtime profile digest"))?;
        Task::new(self.argv.clone()).map_err(|_| Error::Invalid("check argv"))?;
        let bytes = serde_json::to_vec(self).map_err(|_| Error::Invalid("check program"))?;
        if bytes.len() > 16384 {
            return Err(Error::Invalid("check program size"));
        }
        Ok(bytes)
    }
    pub fn digest(&self) -> Result<ObjectId> {
        let mut h = Sha256::new();
        h.update(b"treeharbor-check-program\0v1\0");
        h.update(self.canonical()?);
        Ok(ObjectId::from_bytes(h.finalize().into()))
    }
    pub(super) fn decode(bytes: &[u8], id: ObjectId) -> Result<Self> {
        let p: Self = serde_json::from_slice(bytes).map_err(|_| Error::SchemaMismatch)?;
        if p.canonical()? != bytes || p.digest()? != id {
            return Err(Error::SchemaMismatch);
        }
        Ok(p)
    }
    pub(super) fn matches(&self, check: &CheckDefinition, p: &Profile) -> Result<()> {
        // Immutable image identity is the v1 toolchain and external-dependency
        // bundle. Repository manifests are already bound by candidate.snapshot.
        if check.definition != self.digest()?.to_string()
            || check.environment != self.runtime_profile
            || check.toolchain != p.definition().image
            || check.dependencies != p.definition().image
            || self.runtime_profile != ObjectId::from_bytes(p.digest()).to_string()
        {
            return Err(Error::Invalid("check execution context"));
        }
        Ok(())
    }
}
pub(crate) async fn load_program(
    tx: &Transaction<'_>,
    s: &Scope,
    id: ObjectId,
) -> Result<(CheckProgram, Profile)> {
    let row=tx.query_opt("SELECT c.definition,r.definition FROM th_check_programs c JOIN th_runtime_profiles r ON r.tenant=c.tenant AND r.repository=c.repository AND r.digest=c.profile WHERE c.tenant=$1 AND c.repository=$2 AND c.digest=$3",&[&s.tenant,&s.repository,&bytes(id)]).await?.ok_or(Error::NotFound)?;
    let program = CheckProgram::decode(&row.get::<_, Vec<u8>>(0), id)?;
    let profile = Profile::decode(&row.get::<_, Vec<u8>>(1)).map_err(|_| Error::SchemaMismatch)?;
    if program.runtime_profile != ObjectId::from_bytes(profile.digest()).to_string() {
        return Err(Error::SchemaMismatch);
    }
    Ok((program, profile))
}
pub(super) async fn known_program(tx: &Transaction<'_>, s: &Scope, id: &[u8]) -> Result<bool> {
    Ok(tx
        .query_opt(
            "SELECT 1 FROM th_check_programs WHERE tenant=$1 AND repository=$2 AND digest=$3",
            &[&s.tenant, &s.repository, &id],
        )
        .await?
        .is_some())
}
impl Metadata {
    pub async fn put_native_check_program(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        program: &CheckProgram,
    ) -> Result<Value> {
        let definition = program.canonical()?;
        let digest = program.digest()?;
        let profile: ObjectId = program.runtime_profile.parse()?;
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.program.put.v1",
                json!([1, digest.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        if tx
            .tx
            .query_opt(
                "SELECT 1 FROM th_runtime_profiles WHERE tenant=$1 AND repository=$2 AND digest=$3",
                &[&s.tenant, &s.repository, &bytes(profile)],
            )
            .await?
            .is_none()
        {
            return Err(Error::NotFound);
        }
        tx.tx.execute("INSERT INTO th_check_programs(tenant,repository,digest,definition,profile,principal) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING",&[&s.tenant,&s.repository,&bytes(digest),&definition,&bytes(profile),&auth.principal()]).await?;
        let (_, profile) = load_program(&tx.tx, s, digest).await?;
        if tx.tx.query_opt("SELECT 1 FROM th_runtime_profile_revocations WHERE tenant=$1 AND repository=$2 AND digest=$3",&[&s.tenant,&s.repository,&profile.digest().as_slice()]).await?.is_some(){return Err(Error::ExecutionState);}
        tx.complete(json!({"definition":digest.to_string(),"environment":program.runtime_profile,"toolchain":profile.definition().image,"dependencies":profile.definition().image}),vec![]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn check_program_matches_independent_vector_and_rejects_ambiguous_input() {
        let vector: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/check-program-v1.json"))
                .unwrap();
        let p: CheckProgram = serde_json::from_value(vector["program"].clone()).unwrap();
        assert_eq!(
            p.canonical().unwrap(),
            vector["canonical"].as_str().unwrap().as_bytes()
        );
        assert_eq!(p.digest().unwrap().hex(), vector["digest"]);
        CheckProgram::decode(&p.canonical().unwrap(), p.digest().unwrap()).unwrap();
        assert!(
            CheckProgram::decode(&p.canonical().unwrap(), ObjectId::from_bytes([0; 32])).is_err()
        );
        let mut bad = p.clone();
        bad.argv.clear();
        assert!(bad.canonical().is_err());
        let mut bad = p.clone();
        bad.argv[0] = "relative".into();
        assert!(bad.canonical().is_err());
        let mut bad = p;
        bad.argv.push("x".repeat(17000));
        assert!(bad.canonical().is_err());
        assert!(
            serde_json::from_str::<CheckProgram>(
                r#"{"version":1,"version":1,"runtime_profile":"x","argv":[]}"#
            )
            .is_err()
        );
    }
}
