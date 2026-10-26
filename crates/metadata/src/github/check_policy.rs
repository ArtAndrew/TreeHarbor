use super::*;
use serde::{Deserialize, Serialize};
use treeharbor_core::ObjectId;
use treeharbor_github_contract::{queue::QueueTarget, workflow_definitions::Policy};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyPin {
    pub digest: String,
    pub generation: i64,
}
impl PolicyPin {
    pub(super) fn validate(&self) -> Result<ObjectId> {
        if self.generation <= 0 {
            return Err(Error::Invalid("policy generation"));
        }
        self.digest
            .parse()
            .map_err(|_| Error::Invalid("policy digest"))
    }
}
// GitHub API branch name (e.g. main), matching InspectionTarget.base_ref.
pub(super) fn base(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || value.split('/').any(|p| {
            p.is_empty()
                || p.len() > 64
                || !p.as_bytes()[0].is_ascii_alphanumeric()
                || p.ends_with('.')
                || p.ends_with(".lock")
                || p.contains("..")
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
    {
        return Err(Error::Invalid("policy base ref"));
    }
    Ok(())
}
async fn head(
    tx: &Transaction<'_>,
    s: &Scope,
    b: Uuid,
    epoch: i64,
    base: &str,
) -> Result<Option<Row>> {
    Ok(tx.query_opt("SELECT generation,policy FROM th_github_policy_refs WHERE tenant=$1 AND repository=$2 AND binding=$3 AND epoch=$4 AND base_ref=$5",&[&s.tenant(),&s.repository(),&b,&epoch,&base]).await?)
}
async fn policy(
    tx: &Transaction<'_>,
    s: &Scope,
    b: Uuid,
    epoch: i64,
    id: ObjectId,
) -> Result<Policy> {
    let r=tx.query_opt("SELECT definition FROM th_github_check_policies WHERE tenant=$1 AND repository=$2 AND binding=$3 AND epoch=$4 AND digest=$5",&[&s.tenant(),&s.repository(),&b,&epoch,&id.as_bytes().as_slice()]).await?.ok_or(Error::NotFound)?;
    Policy::decode(&r.get::<_, Vec<u8>>(0), id).map_err(|_| Error::SchemaMismatch)
}
pub(super) async fn pin(
    tx: &Transaction<'_>,
    s: &Scope,
    q: &Row,
    t: &QueueTarget,
    p: &PolicyPin,
) -> Result<()> {
    let id = p.validate()?;
    let b = q.get("binding");
    let epoch = q.get("epoch");
    let h = head(tx, s, b, epoch, &t.pull.base_ref)
        .await?
        .ok_or(Error::HostQueueState)?;
    if h.get::<_, i64>("generation") != p.generation
        || h.get::<_, Option<Vec<u8>>>("policy").as_deref() != Some(id.as_bytes())
    {
        return Err(Error::HostQueueState);
    }
    policy(tx, s, b, epoch, id)
        .await?
        .matches_required(t)
        .map_err(|_| Error::Invalid("policy required checks"))?;
    tx.execute("INSERT INTO th_github_queue_policies(tenant,repository,queue,binding,epoch,base_ref,generation,policy) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",&[&s.tenant(),&s.repository(),&q.get::<_,Uuid>("queue"),&b,&epoch,&t.pull.base_ref,&p.generation,&id.as_bytes().as_slice()]).await?;
    Ok(())
}
pub(super) async fn pinned(
    tx: &Transaction<'_>,
    s: &Scope,
    q: &Row,
) -> Result<Option<(PolicyPin, Policy, bool)>> {
    let id = q.get::<_, Uuid>("queue");
    let Some(r)=tx.query_opt("SELECT binding,epoch,base_ref,generation,policy FROM th_github_queue_policies WHERE tenant=$1 AND repository=$2 AND queue=$3",&[&s.tenant(),&s.repository(),&id]).await? else {return Ok(None);};
    let b = r.get::<_, Uuid>("binding");
    let epoch = r.get::<_, i64>("epoch");
    let base = r.get::<_, String>("base_ref");
    if b != q.get::<_, Uuid>("binding")
        || epoch != q.get::<_, i64>("epoch")
        || base != queue::target(q)?.pull.base_ref
    {
        return Err(Error::SchemaMismatch);
    }
    let bytes = r.get::<_, Vec<u8>>("policy");
    let id = ObjectId::from_bytes(
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| Error::SchemaMismatch)?,
    );
    let pin = PolicyPin {
        digest: id.to_string(),
        generation: r.get("generation"),
    };
    let current = head(tx, s, b, epoch, &base).await?.is_some_and(|h| {
        h.get::<_, i64>("generation") == pin.generation
            && h.get::<_, Option<Vec<u8>>>("policy").as_deref() == Some(bytes.as_slice())
    });
    Ok(Some((pin, policy(tx, s, b, epoch, id).await?, current)))
}
pub(super) async fn fresh(tx: &Transaction<'_>, s: &Scope, q: &Row) -> Result<()> {
    if pinned(tx, s, q)
        .await?
        .is_some_and(|(_, _, current)| !current)
    {
        return Err(Error::HostQueueState);
    }
    Ok(())
}
impl Metadata {
    pub async fn set_github_check_policy(
        &mut self,
        auth: &Authorization,
        c: &Command,
        b: Uuid,
        base_ref: &str,
        expected_generation: i64,
        value: Option<&Policy>,
    ) -> Result<Value> {
        uuid(b)?;
        base(base_ref)?;
        if !(0..i64::MAX).contains(&expected_generation) {
            return Err(Error::Invalid("policy generation"));
        }
        let encoded = value
            .map(|p| p.canonical().map_err(|_| Error::Invalid("check policy")))
            .transpose()?;
        let id = value
            .map(|p| p.digest().map_err(|_| Error::Invalid("check policy")))
            .transpose()?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.policy.set.v1",
                json!([
                    1,
                    b,
                    base_ref,
                    expected_generation,
                    id.map(|i| i.to_string())
                ]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::PolicyWrite).await?;
        let s = auth.scope();
        projections::active_binding(&m.tx, s, b, c.epoch).await?;
        let old = head(&m.tx, s, b, c.epoch, base_ref).await?;
        if old.as_ref().map_or(0, |r| r.get("generation")) != expected_generation
            || (old.is_none() && value.is_none())
        {
            return Err(Error::HostQueueState);
        }
        let bytes = id.map(|id| id.as_bytes().to_vec());
        if let Some(encoded) = encoded {
            m.tx.execute("INSERT INTO th_github_check_policies(tenant,repository,binding,epoch,digest,definition,principal) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING",&[&s.tenant(),&s.repository(),&b,&c.epoch,&bytes,&encoded,&auth.principal()]).await?;
            if policy(&m.tx, s, b, c.epoch, id.ok_or(Error::SchemaMismatch)?)
                .await?
                .canonical()
                .map_err(|_| Error::SchemaMismatch)?
                != encoded
            {
                return Err(Error::SchemaMismatch);
            }
        }
        let generation = expected_generation + 1;
        m.tx.execute("INSERT INTO th_github_policy_refs(tenant,repository,binding,epoch,base_ref,generation,policy) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(tenant,repository,binding,epoch,base_ref) DO UPDATE SET generation=EXCLUDED.generation,policy=EXCLUDED.policy",&[&s.tenant(),&s.repository(),&b,&c.epoch,&base_ref,&generation,&bytes]).await?;
        let result = json!({"version":1,"binding":b,"epoch":c.epoch,"base_ref":base_ref,"generation":generation,"policy":id.map(|v|v.to_string())});
        m.write_completion(
            &result,
            &[Event {
                kind: "github.policy.changed".into(),
                payload: result.clone(),
            }],
        )
        .await?;
        context(&m.tx, auth, c.epoch, Permission::PolicyWrite).await?;
        m.tx.commit().await?;
        Ok(result)
    }
    pub async fn github_check_policy(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        b: Uuid,
        base_ref: &str,
    ) -> Result<Value> {
        uuid(b)?;
        base(base_ref)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        context(&tx, auth, epoch, Permission::Read).await?;
        let binding = binding(&tx, s, b).await?;
        if binding.get::<_, i64>("epoch") != epoch {
            return Err(Error::Fenced);
        }
        let row = head(&tx, s, b, epoch, base_ref).await?;
        let generation = row.as_ref().map_or(0, |r| r.get::<_, i64>("generation"));
        let bytes = row.and_then(|r| r.get::<_, Option<Vec<u8>>>("policy"));
        let id = bytes
            .as_ref()
            .map(|b| {
                b.as_slice()
                    .try_into()
                    .map(ObjectId::from_bytes)
                    .map_err(|_| Error::SchemaMismatch)
            })
            .transpose()?;
        let policy = if let Some(id) = id {
            Some(policy(&tx, s, b, epoch, id).await?)
        } else {
            None
        };
        context(&tx, auth, epoch, Permission::Read).await?;
        tx.commit().await?;
        Ok(
            json!({"version":1,"binding":b,"epoch":epoch,"base_ref":base_ref,"generation":generation,"policy":id.map(|i|i.to_string()),"definition":policy}),
        )
    }
}
