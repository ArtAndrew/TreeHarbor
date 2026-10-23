//! Owner-selected PR delivery. Observations do not approve checks or merging.
use super::*;
use crate::Mutation;
use treeharbor_core::{ObjectId, RevisionRef};
mod types;
pub use types::{ExpectedPull, PullIdentity, PullReport, PullSnapshot, PullSpec, PullTarget};

pub(super) async fn load(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt("SELECT *,expires_at>clock_timestamp() AS unexpired,(extract(epoch from expires_at)*1000)::bigint AS deadline,(extract(epoch from started_at)*1000)::bigint AS started,(extract(epoch from clock_timestamp())*1000)::bigint AS now_ms FROM th_github_pulls WHERE tenant=$1 AND repository=$2 AND pull=$3",&[&s.tenant(),&s.repository(),&id]).await?.ok_or(Error::NotFound)
}
fn target(row: &Row) -> Result<PullTarget> {
    serde_json::from_value(row.get("target")).map_err(|_| Error::SchemaMismatch)
}
fn native(row: &Row) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        row.get::<_, Vec<u8>>("native_commit")
            .try_into()
            .map_err(|_| Error::SchemaMismatch)?,
    ))
}
fn describe(row: &Row) -> Result<Value> {
    Ok(
        json!({"version":1,"pull":row.get::<_,Uuid>("pull").to_string(),"publication":row.get::<_,Uuid>("publication").to_string(),
        "projection":row.get::<_,Uuid>("projection").to_string(),"binding":row.get::<_,Uuid>("binding").to_string(),"epoch":row.get::<_,i64>("epoch"),
        "native_revision":RevisionRef::Native(native(row)?).to_string(),"target":target(row)?,"previous":row.get::<_,Option<Uuid>>("previous_pull").map(|id|id.to_string()),
        "state":row.get::<_,String>("state"),"cancel_requested":row.get::<_,bool>("cancel_requested"),"unexpired":row.get::<_,bool>("unexpired"),
        "expires_at_unix_ms":row.get::<_,i64>("deadline"),"attempt":row.get::<_,Option<Uuid>>("attempt").map(|id|id.to_string()),
        "observation":row.get::<_,Option<Value>>("observation"),"merge_authorized":false}),
    )
}
async fn assigned(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    row: &Row,
) -> Result<BindingSpec> {
    context(tx, auth, epoch, Permission::HostPublish).await?;
    if row.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    let spec = specification(&binding(tx, auth.scope(), row.get("binding")).await?)?;
    if spec.bridge != auth.principal() || row.get::<_, String>("bridge") != auth.principal() {
        return Err(Error::Forbidden);
    }
    Ok(spec)
}
async fn fresh(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    id: Uuid,
    permission: Permission,
) -> Result<Row> {
    context(tx, auth, epoch, permission).await?;
    let r = load(tx, auth.scope(), id).await?;
    if r.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    if !r.get::<_, bool>("unexpired") || r.get::<_, bool>("cancel_requested") {
        return Err(Error::HostPullState);
    }
    let p = projections::load(tx, auth.scope(), r.get("projection")).await?;
    projections::live(&p)?;
    if p.row.get::<_, String>("state") != "ready" || p.object.id() != native(&r)? {
        return Err(Error::HostProjectionState);
    }
    projections::active_binding(tx, auth.scope(), r.get("binding"), epoch).await?;
    if permission == Permission::HostPublish {
        assigned(tx, auth, epoch, &r).await?;
    }
    Ok(r)
}
async fn finish(
    m: Mutation<'_>,
    auth: &Authorization,
    c: &Command,
    id: Uuid,
    permission: Permission,
    kind: &str,
) -> Result<Value> {
    let result = describe(&load(&m.tx, auth.scope(), id).await?)?;
    m.write_completion(
        &result,
        &[Event {
            kind: kind.into(),
            payload: json!({"pull":id.to_string()}),
        }],
    )
    .await?;
    fresh(&m.tx, auth, c.epoch, id, permission).await?;
    m.tx.commit().await?;
    Ok(result)
}
impl Metadata {
    pub async fn request_github_pull(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        spec: &PullSpec,
    ) -> Result<Value> {
        uuid(id)?;
        let (publication, previous) = spec.validate()?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.pull.request.v1",
                json!([1, id.to_string(), spec]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        let p=m.tx.query_opt("SELECT * FROM th_github_publications WHERE tenant=$1 AND repository=$2 AND publication=$3",&[&s.tenant(),&s.repository(),&publication]).await?.ok_or(Error::NotFound)?;
        if p.get::<_, i64>("epoch") != c.epoch {
            return Err(Error::Fenced);
        }
        if p.get::<_, String>("state") != "observed" {
            return Err(Error::HostPublicationState);
        }
        let projection: Uuid = p.get("projection");
        let binding_id: Uuid = p.get("binding");
        let branch: Uuid = p.get("branch");
        let binding = projections::active_binding(&m.tx, s, binding_id, c.epoch).await?;
        let pr = projections::load(&m.tx, s, projection).await?;
        projections::live(&pr)?;
        if pr.row.get::<_, String>("state") != "ready" {
            return Err(Error::HostProjectionState);
        }
        if m.tx.query_opt("SELECT pull FROM th_github_pulls WHERE tenant=$1 AND repository=$2 AND (pull=$3 OR (branch=$4 AND state IN ('requested','attempting','uncertain'))) LIMIT 1",&[&s.tenant(),&s.repository(),&id,&branch]).await?.is_some(){return Err(Error::HostPullState);}
        let previous_target = if let Some(previous) = previous {
            let old = load(&m.tx, s, previous).await?;
            if old.get::<_, String>("state") != "observed"
                || old.get::<_, Uuid>("branch") != branch
                || old.get::<_, Uuid>("binding") != binding_id
                || old.get::<_, i64>("epoch") != c.epoch
            {
                return Err(Error::HostPullState);
            }
            let report: PullReport = serde_json::from_value(old.get("observation"))
                .map_err(|_| Error::SchemaMismatch)?;
            let observed = report.pull.ok_or(Error::SchemaMismatch)?;
            if observed.state != "open"
                || observed.merged
                || observed.base_ref != spec.base
                || observed.draft != spec.draft
            {
                return Err(Error::HostPullState);
            }
            Some(ExpectedPull {
                identity: observed.identity,
                title: observed.title,
                body: observed.body,
                draft: observed.draft,
            })
        } else {
            None
        };
        let git_revision = p
            .get::<_, Option<String>>("prepared_git")
            .ok_or(Error::SchemaMismatch)?;
        types::git(&git_revision)?;
        let t = PullTarget {
            operation: id,
            branch_id: branch,
            git_revision: git_revision.clone(),
            base: spec.base.clone(),
            title: spec.title.clone(),
            body: format!("{}\n\n<!-- treeharbor-pull:{id} -->", spec.body),
            draft: spec.draft,
            previous: previous_target,
        };
        if t.base == t.head() {
            return Err(Error::Invalid("pull base equals head"));
        }
        let number = t.previous.as_ref().map(|p| p.identity.number as i64);
        let node = t.previous.as_ref().map(|p| p.identity.node_id.clone());
        let pin = format!("github-pull:{id}");
        m.copy_root_pin(&p.get::<_, String>("pin_holder"), &pin)
            .await?;
        m.tx.execute("INSERT INTO th_github_pulls(tenant,repository,pull,publication,projection,binding,epoch,principal,bridge,branch,native_commit,git_revision,target,previous_pull,state,expires_at,pin_holder,pull_number,pull_node) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,'requested',LEAST(clock_timestamp()+($15::integer*interval '1 second'),(SELECT expires_at FROM th_github_projections WHERE tenant=$1 AND repository=$2 AND projection=$5)),$16,$17,$18)",&[&s.tenant(),&s.repository(),&id,&publication,&projection,&binding_id,&c.epoch,&auth.principal(),&binding.bridge,&branch,&p.get::<_,Vec<u8>>("native_commit"),&git_revision,&json!(t),&previous,&spec.ttl_seconds,&pin,&number,&node]).await?;
        finish(
            m,
            auth,
            c,
            id,
            Permission::RepositoryWrite,
            "github.pull.requested",
        )
        .await
    }
    pub async fn github_pull_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        bridge: bool,
        attempt: Option<Uuid>,
    ) -> Result<Value> {
        uuid(id)?;
        if attempt.is_some() && !bridge {
            return Err(Error::Forbidden);
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let row = load(&tx, auth.scope(), id).await?;
        let mut v = describe(&row)?;
        if bridge {
            v["specification"] = json!(assigned(&tx, auth, epoch, &row).await?);
        } else {
            context(&tx, auth, epoch, Permission::Read).await?;
            if row.get::<_, i64>("epoch") != epoch {
                return Err(Error::Fenced);
            }
        }
        if let Some(attempt) = attempt {
            uuid(attempt)?;
            if row.get::<_, String>("state") != "attempting"
                || row.get::<_, Option<Uuid>>("attempt") != Some(attempt)
            {
                return Err(Error::HostPullState);
            }
            fresh(&tx, auth, epoch, id, Permission::HostPublish).await?;
        }
        if bridge {
            assigned(&tx, auth, epoch, &row).await?;
        } else {
            context(&tx, auth, epoch, Permission::Read).await?;
        }
        tx.commit().await?;
        Ok(v)
    }
    pub async fn begin_github_pull(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        attempt: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(attempt)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.pull.begin.v1",
                json!([1, id.to_string(), attempt.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(json!({"receipt":v,"dispatch_granted":false})),
            Begin::Fresh(m) => m,
        };
        let row = fresh(&m.tx, auth, c.epoch, id, Permission::HostPublish).await?;
        if row.get::<_, String>("state") != "requested" {
            return Err(Error::HostPullState);
        }
        let s = auth.scope();
        m.tx.execute("UPDATE th_github_pulls SET attempt=$4,started_at=clock_timestamp(),state='attempting' WHERE tenant=$1 AND repository=$2 AND pull=$3",&[&s.tenant(),&s.repository(),&id,&attempt]).await?;
        let receipt = finish(
            m,
            auth,
            c,
            id,
            Permission::HostPublish,
            "github.pull.attempting",
        )
        .await?;
        Ok(json!({"receipt":receipt,"dispatch_granted":true}))
    }
    pub async fn cancel_github_pull(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.pull.cancel.v1",
                json!([1, id.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        let row = load(&m.tx, s, id).await?;
        if row.get::<_, i64>("epoch") != c.epoch {
            return Err(Error::Fenced);
        }
        if matches!(
            row.get::<_, String>("state").as_str(),
            "requested" | "attempting" | "uncertain"
        ) {
            m.tx.execute("UPDATE th_github_pulls SET cancel_requested=true,state=CASE WHEN attempt IS NULL THEN 'cancelled' ELSE state END WHERE tenant=$1 AND repository=$2 AND pull=$3",&[&s.tenant(),&s.repository(),&id]).await?;
        }
        let result = describe(&load(&m.tx, s, id).await?)?;
        m.write_completion(
            &result,
            &[Event {
                kind: "github.pull.cancelled".into(),
                payload: json!({"pull":id.to_string()}),
            }],
        )
        .await?;
        context(&m.tx, auth, c.epoch, Permission::RepositoryWrite).await?;
        m.tx.commit().await?;
        Ok(result)
    }
    pub async fn report_github_pull(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        report: &PullReport,
    ) -> Result<Value> {
        uuid(id)?;
        let attempt = types::id(&report.attempt)?;
        let binding_id = types::id(&report.binding)?;
        types::git(&report.git_revision)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.pull.report.v1",
                json!([1, id.to_string(), report]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let s = auth.scope();
        let row = load(&m.tx, s, id).await?;
        let spec = assigned(&m.tx, auth, c.epoch, &row).await?;
        let t = target(&row)?;
        if !matches!(
            row.get::<_, String>("state").as_str(),
            "attempting" | "uncertain"
        ) || row.get::<_, Option<Uuid>>("attempt") != Some(attempt)
            || row.get::<_, Uuid>("binding") != binding_id
            || t.git_revision != report.git_revision
        {
            return Err(Error::HostPullState);
        }
        let previous = row
            .get::<_, Option<Value>>("observation")
            .and_then(|v| v["observed_at_unix_ms"].as_i64())
            .unwrap_or(
                row.get::<_, Option<i64>>("started")
                    .ok_or(Error::SchemaMismatch)?
                    .saturating_sub(15000),
            );
        if report.observed_at_unix_ms <= 0
            || report.observed_at_unix_ms < previous
            || report.observed_at_unix_ms > row.get::<_, i64>("now_ms").saturating_add(15000)
        {
            return Err(Error::Invalid("pull observation time"));
        }
        let mut state = "uncertain";
        let mut number = row.get::<_, Option<i64>>("pull_number");
        let mut node = row.get::<_, Option<String>>("pull_node");
        if let Some(p) = &report.pull {
            p.validate(&t, &spec)?;
            if number.is_some_and(|n| n != p.identity.number as i64)
                || node.as_ref().is_some_and(|n| n != &p.identity.node_id)
            {
                return Err(Error::HostPullState);
            }
            number = Some(p.identity.number as i64);
            node = Some(p.identity.node_id.clone());
            if t.matches(p) {
                state = "observed";
            }
        }
        m.tx.execute("UPDATE th_github_pulls SET state=$4,observation=$5,pull_number=$6,pull_node=$7 WHERE tenant=$1 AND repository=$2 AND pull=$3",&[&s.tenant(),&s.repository(),&id,&state,&json!(report),&number,&node]).await?;
        let result = describe(&load(&m.tx, s, id).await?)?;
        m.write_completion(
            &result,
            &[Event {
                kind: "github.pull.observed".into(),
                payload: json!({"pull":id.to_string(),"state":state}),
            }],
        )
        .await?;
        assigned(&m.tx, auth, c.epoch, &row).await?;
        m.tx.commit().await?;
        Ok(result)
    }
}
