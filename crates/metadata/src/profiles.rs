//! Immutable, repository-scoped source delivery profiles.
use crate::{
    Begin, Error, Metadata, Result,
    access::{Authorization, Permission, repository_lock},
    catalog::settings,
    intake::CommandContext,
};
use serde_json::{Value, json};
use treeharbor_core::{ProfileId, SourceProfile};

pub fn describe(profile: &SourceProfile) -> Value {
    match profile.paths() {
        None => json!({"kind":"full"}),
        Some(paths) => {
            json!({"kind":"sparse","paths_hex":paths.iter().map(|p|p.as_bytes().iter().map(|b|format!("{b:02x}")).collect::<String>()).collect::<Vec<_>>()})
        }
    }
}
impl Metadata {
    pub async fn put_source_profile(
        &mut self,
        ctx: &CommandContext,
        profile: &SourceProfile,
    ) -> Result<Value> {
        let encoded = profile.encode();
        let digest = profile.id();
        let request = ctx.request("profile.put.v1", json!([1, describe(profile)]))?;
        let tx = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        let scope = &ctx.scope;
        tx.tx.execute("INSERT INTO th_source_profiles(tenant,repository,digest,definition,principal) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",&[&scope.tenant,&scope.repository,&digest.as_bytes().as_slice(),&encoded,&ctx.principal]).await?;
        let row=tx.tx.query_one("SELECT definition FROM th_source_profiles WHERE tenant=$1 AND repository=$2 AND digest=$3",&[&scope.tenant,&scope.repository,&digest.as_bytes().as_slice()]).await?;
        if row.get::<_, Vec<u8>>(0) != encoded {
            return Err(Error::SchemaMismatch);
        }
        tx.complete(
            json!({"profile_digest":digest.to_string(),"profile":describe(profile)}),
            vec![],
        )
        .await
    }
    pub async fn authorized_source_profile(
        &mut self,
        auth: &Authorization,
        id: ProfileId,
    ) -> Result<SourceProfile> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let scope = auth.scope();
        repository_lock(&tx, scope, false).await?;
        auth.check_transaction(&tx, scope, auth.principal(), Permission::Read)
            .await?;
        let row=tx.query_opt("SELECT definition FROM th_source_profiles WHERE tenant=$1 AND repository=$2 AND digest=$3",&[&scope.tenant,&scope.repository,&id.as_bytes().as_slice()]).await?.ok_or(Error::NotFound)?;
        let profile = SourceProfile::decode(&row.get::<_, Vec<u8>>(0))?;
        if profile.id() != id {
            return Err(Error::SchemaMismatch);
        }
        auth.check_transaction(&tx, scope, auth.principal(), Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(profile)
    }
}
