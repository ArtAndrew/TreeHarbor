//! Durable owner intent and one-shot bridge dispatch. No network effects here.
use super::*;
use crate::Mutation;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use treeharbor_core::{GitOid, ObjectId, RevisionRef};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationSpec {
    pub projection: String,
    pub expected_head: Option<String>,
    pub ttl_seconds: i32,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationMapping {
    pub branch_id: String,
    pub native_revision: String,
    pub git_revision: String,
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicationOutcome {
    Observed,
    Unknown,
    NotSent,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationReport {
    pub attempt: String,
    pub branch_id: String,
    pub binding: String,
    pub native_revision: String,
    pub prepared_git: String,
    pub remote_revision: Option<String>,
    pub outcome: PublicationOutcome,
    pub observed_at_unix_ms: i64,
    pub token_revoked: bool,
}
fn git(value: &str) -> Result<()> {
    match value.parse::<RevisionRef>() {
        Ok(RevisionRef::Git(GitOid::Sha1(id)))
            if id != [0; 20] && RevisionRef::Git(GitOid::Sha1(id)).to_string() == value =>
        {
            Ok(())
        }
        _ => Err(Error::Invalid("publication Git SHA-1")),
    }
}
fn native(value: &str) -> Result<ObjectId> {
    match value.parse::<RevisionRef>() {
        Ok(RevisionRef::Native(id)) if RevisionRef::Native(id).to_string() == value => Ok(id),
        _ => Err(Error::Invalid("publication native revision")),
    }
}
fn canonical_uuid(value: &str) -> Result<Uuid> {
    let id: Uuid = value
        .parse()
        .map_err(|_| Error::Invalid("publication UUID"))?;
    uuid(id)?;
    if id.to_string() != value {
        return Err(Error::Invalid("canonical publication UUID"));
    }
    Ok(id)
}
fn branch(scope: &Scope, change: Uuid) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"treeharbor-github-publication-branch\0v1\0");
    hash.update((scope.tenant().len() as u64).to_be_bytes());
    hash.update(scope.tenant().as_bytes());
    hash.update(scope.repository().as_bytes());
    hash.update(change.as_bytes());
    let mut bytes: [u8; 16] = hash.finalize()[..16].try_into().expect("digest length");
    bytes[6] = (bytes[6] & 15) | 0x80;
    bytes[8] = (bytes[8] & 63) | 0x80;
    Uuid::from_bytes(bytes)
}
async fn load(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt("SELECT p.*,p.expires_at>clock_timestamp() AS unexpired,(extract(epoch from p.expires_at)*1000)::bigint AS deadline,(extract(epoch from p.started_at)*1000)::bigint AS started,(extract(epoch from clock_timestamp())*1000)::bigint AS now_ms FROM th_github_publications p WHERE tenant=$1 AND repository=$2 AND publication=$3",&[&s.tenant(),&s.repository(),&id]).await?.ok_or(Error::NotFound)
}
fn native_id(row: &Row) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        row.get::<_, Vec<u8>>("native_commit")
            .try_into()
            .map_err(|_| Error::SchemaMismatch)?,
    ))
}
fn describe(row: &Row) -> Result<Value> {
    let branch: Uuid = row.get("branch");
    Ok(
        json!({"version":1,"publication":row.get::<_,Uuid>("publication").to_string(),
        "projection":row.get::<_,Uuid>("projection").to_string(),"binding":row.get::<_,Uuid>("binding").to_string(),
        "epoch":row.get::<_,i64>("epoch"),"branch_id":branch.to_string(),"branch":format!("refs/heads/treeharbor/{}",branch.simple()),
        "native_revision":RevisionRef::Native(native_id(row)?).to_string(),"expected_head":row.get::<_,Option<String>>("expected_git"),
        "prepared_git":row.get::<_,Option<String>>("prepared_git"),"state":row.get::<_,String>("state"),
        "cancel_requested":row.get::<_,bool>("cancel_requested"),"unexpired":row.get::<_,bool>("unexpired"),"expires_at_unix_ms":row.get::<_,i64>("deadline"),
        "attempt":row.get::<_,Option<Uuid>>("attempt").map(|v|v.to_string()),"observation":row.get::<_,Option<Value>>("observation"),"merge_authorized":false}),
    )
}
async fn assigned_historical(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    row: &Row,
) -> Result<BindingSpec> {
    context(tx, auth, epoch, Permission::HostPublish).await?;
    if row.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    let b = binding(tx, auth.scope(), row.get("binding")).await?;
    let spec = specification(&b)?;
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
    let row = load(tx, auth.scope(), id).await?;
    if row.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    if !row.get::<_, bool>("unexpired") || row.get::<_, bool>("cancel_requested") {
        return Err(Error::HostPublicationState);
    }
    let p = projections::load(tx, auth.scope(), row.get("projection")).await?;
    projections::live(&p)?;
    if p.row.get::<_, String>("state") != "ready" || p.object.id() != native_id(&row)? {
        return Err(Error::HostProjectionState);
    }
    projections::active_binding(tx, auth.scope(), row.get("binding"), epoch).await?;
    if permission == Permission::HostPublish {
        assigned_historical(tx, auth, epoch, &row).await?;
    }
    Ok(row)
}
async fn complete_fresh(
    m: Mutation<'_>,
    auth: &Authorization,
    cmd: &Command,
    id: Uuid,
    permission: Permission,
    kind: &str,
) -> Result<Value> {
    let result = describe(&load(&m.tx, auth.scope(), id).await?)?;
    m.write_completion(
        &result,
        &[Event {
            kind: kind.into(),
            payload: json!({"publication":id.to_string()}),
        }],
    )
    .await?;
    fresh(&m.tx, auth, cmd.epoch, id, permission).await?;
    m.tx.commit().await?;
    Ok(result)
}
impl Metadata {
    /// Eligibility recheck for the original live dispatcher, never another grant.
    pub async fn github_publication_dispatch_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        attempt: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(attempt)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let row = fresh(&tx, auth, epoch, id, Permission::HostPublish).await?;
        if row.get::<_, String>("state") != "attempting"
            || row.get::<_, Option<Uuid>>("attempt") != Some(attempt)
        {
            return Err(Error::HostPublicationState);
        }
        let result = describe(&row)?;
        fresh(&tx, auth, epoch, id, Permission::HostPublish).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn request_github_publication(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        spec: &PublicationSpec,
    ) -> Result<Value> {
        uuid(id)?;
        let projection = canonical_uuid(&spec.projection)?;
        if !(1..=3600).contains(&spec.ttl_seconds) {
            return Err(Error::Invalid("publication deadline"));
        }
        if let Some(head) = &spec.expected_head {
            git(head)?;
        }
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.publication.request.v1",
                json!([1, id.to_string(), spec]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, cmd.epoch, Permission::RepositoryWrite).await?;
        let p = projections::load(&m.tx, auth.scope(), projection).await?;
        projections::live(&p)?;
        if p.row.get::<_, i64>("epoch") != cmd.epoch {
            return Err(Error::Fenced);
        }
        if p.row.get::<_, String>("state") != "ready" {
            return Err(Error::HostProjectionState);
        }
        let binding_id: Uuid = p.row.get("binding");
        let binding =
            projections::active_binding(&m.tx, auth.scope(), binding_id, cmd.epoch).await?;
        let s = auth.scope();
        let branch = branch(s, p.row.get("changeset"));
        let busy=m.tx.query_opt("SELECT publication FROM th_github_publications WHERE tenant=$1 AND repository=$2 AND (publication=$3 OR (branch=$4 AND state IN ('requested','prepared','attempting','uncertain'))) LIMIT 1",&[&s.tenant(),&s.repository(),&id,&branch]).await?;
        if busy.is_some() {
            return Err(Error::HostPublicationState);
        }
        let pin = format!("github-publication:{id}");
        m.copy_root_pin(
            &p.row
                .get::<_, Option<String>>("pin_holder")
                .ok_or(Error::SchemaMismatch)?,
            &pin,
        )
        .await?;
        m.tx.execute("INSERT INTO th_github_publications(tenant,repository,publication,projection,binding,epoch,principal,bridge,branch,native_commit,expected_git,state,expires_at,pin_holder) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'requested',LEAST(clock_timestamp()+($12::integer*interval '1 second'),(SELECT expires_at FROM th_github_projections WHERE tenant=$1 AND repository=$2 AND projection=$4)),$13)",&[&s.tenant(),&s.repository(),&id,&projection,&binding_id,&cmd.epoch,&auth.principal(),&binding.bridge,&branch,&p.object.id().as_bytes().to_vec(),&spec.expected_head,&spec.ttl_seconds,&pin]).await?;
        complete_fresh(
            m,
            auth,
            cmd,
            id,
            Permission::RepositoryWrite,
            "github.publication.requested",
        )
        .await
    }
    pub async fn github_publication_status(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        bridge: bool,
    ) -> Result<Value> {
        uuid(id)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let row = load(&tx, auth.scope(), id).await?;
        let mut result = describe(&row)?;
        if bridge {
            result["specification"] = json!(assigned_historical(&tx, auth, epoch, &row).await?);
        } else {
            context(&tx, auth, epoch, Permission::Read).await?;
            if row.get::<_, i64>("epoch") != epoch {
                return Err(Error::Fenced);
            }
        }
        if bridge {
            assigned_historical(&tx, auth, epoch, &row).await?;
        } else {
            context(&tx, auth, epoch, Permission::Read).await?;
        }
        tx.commit().await?;
        Ok(result)
    }
    pub async fn prepare_github_publication(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        mapping: &PublicationMapping,
    ) -> Result<Value> {
        uuid(id)?;
        let branch = canonical_uuid(&mapping.branch_id)?;
        let native = native(&mapping.native_revision)?;
        git(&mapping.git_revision)?;
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.publication.prepare.v1",
                json!([1, id.to_string(), mapping]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let row = fresh(&m.tx, auth, cmd.epoch, id, Permission::HostPublish).await?;
        if row.get::<_, String>("state") != "requested"
            || native_id(&row)? != native
            || row.get::<_, Uuid>("branch") != branch
        {
            return Err(Error::HostPublicationState);
        }
        let s = auth.scope();
        m.tx.execute("UPDATE th_github_publications SET prepared_git=$4,state='prepared' WHERE tenant=$1 AND repository=$2 AND publication=$3",&[&s.tenant(),&s.repository(),&id,&mapping.git_revision]).await?;
        complete_fresh(
            m,
            auth,
            cmd,
            id,
            Permission::HostPublish,
            "github.publication.prepared",
        )
        .await
    }
    pub async fn begin_github_publication(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        attempt: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(attempt)?;
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.publication.begin.v1",
                json!([1, id.to_string(), attempt.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(receipt) => {
                return Ok(json!({"receipt":receipt,"dispatch_granted":false}));
            }
            Begin::Fresh(m) => m,
        };
        let row = fresh(&m.tx, auth, cmd.epoch, id, Permission::HostPublish).await?;
        if row.get::<_, String>("state") != "prepared" {
            return Err(Error::HostPublicationState);
        }
        let s = auth.scope();
        m.tx.execute("UPDATE th_github_publications SET state='attempting',attempt=$4,started_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND publication=$3",&[&s.tenant(),&s.repository(),&id,&attempt]).await?;
        let receipt = complete_fresh(
            m,
            auth,
            cmd,
            id,
            Permission::HostPublish,
            "github.publication.attempting",
        )
        .await?;
        Ok(json!({"receipt":receipt,"dispatch_granted":true}))
    }
    pub async fn cancel_github_publication(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.publication.cancel.v1",
                json!([1, id.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, cmd.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        let row = load(&m.tx, s, id).await?;
        if row.get::<_, i64>("epoch") != cmd.epoch {
            return Err(Error::Fenced);
        }
        if matches!(
            row.get::<_, String>("state").as_str(),
            "requested" | "prepared" | "attempting" | "uncertain"
        ) {
            m.tx.execute("UPDATE th_github_publications SET cancel_requested=true,state=CASE WHEN attempt IS NULL THEN 'cancelled' ELSE state END WHERE tenant=$1 AND repository=$2 AND publication=$3",&[&s.tenant(),&s.repository(),&id]).await?;
        }
        let result = describe(&load(&m.tx, s, id).await?)?;
        m.write_completion(
            &result,
            &[Event {
                kind: "github.publication.cancel_requested".into(),
                payload: json!({"publication":id.to_string()}),
            }],
        )
        .await?;
        context(&m.tx, auth, cmd.epoch, Permission::RepositoryWrite).await?;
        m.tx.commit().await?;
        Ok(result)
    }
    pub async fn report_github_publication(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        report: &PublicationReport,
    ) -> Result<Value> {
        uuid(id)?;
        let attempt = canonical_uuid(&report.attempt)?;
        let branch = canonical_uuid(&report.branch_id)?;
        let binding = canonical_uuid(&report.binding)?;
        let native = native(&report.native_revision)?;
        git(&report.prepared_git)?;
        if let Some(head) = &report.remote_revision {
            git(head)?;
        }
        if report.observed_at_unix_ms <= 0
            || (report.outcome != PublicationOutcome::Observed && report.remote_revision.is_some())
        {
            return Err(Error::Invalid("publication observation"));
        }
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.publication.report.v1",
                json!([1, id.to_string(), report]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let s = auth.scope();
        let row = load(&m.tx, s, id).await?;
        assigned_historical(&m.tx, auth, cmd.epoch, &row).await?;
        let state: String = row.get("state");
        if !matches!(state.as_str(), "attempting" | "uncertain")
            || row.get::<_, Option<Uuid>>("attempt") != Some(attempt)
            || row.get::<_, Uuid>("branch") != branch
            || row.get::<_, Uuid>("binding") != binding
            || native_id(&row)? != native
            || row.get::<_, Option<String>>("prepared_git").as_deref() != Some(&report.prepared_git)
            || (state == "uncertain" && report.outcome == PublicationOutcome::NotSent)
        {
            return Err(Error::HostPublicationState);
        }
        let start: i64 = row
            .get::<_, Option<i64>>("started")
            .ok_or(Error::SchemaMismatch)?;
        let now: i64 = row.get("now_ms");
        let previous = row
            .get::<_, Option<Value>>("observation")
            .and_then(|v| v["observed_at_unix_ms"].as_i64())
            .unwrap_or(start.saturating_sub(15000));
        if report.observed_at_unix_ms < previous
            || report.observed_at_unix_ms > now.saturating_add(15000)
        {
            return Err(Error::Invalid("publication observation time"));
        }
        let next = match report.outcome {
            PublicationOutcome::NotSent => "not_sent",
            PublicationOutcome::Observed
                if report.remote_revision.as_deref() == Some(&report.prepared_git) =>
            {
                "observed"
            }
            _ => "uncertain",
        };
        m.tx.execute("UPDATE th_github_publications SET state=$4,observation=$5 WHERE tenant=$1 AND repository=$2 AND publication=$3",&[&s.tenant(),&s.repository(),&id,&next,&json!(report)]).await?;
        let result = describe(&load(&m.tx, s, id).await?)?;
        m.write_completion(
            &result,
            &[Event {
                kind: "github.publication.observed".into(),
                payload: json!({"publication":id.to_string(),"state":next}),
            }],
        )
        .await?;
        assigned_historical(&m.tx, auth, cmd.epoch, &row).await?;
        m.tx.commit().await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn branch_identity_matches_independent_vector_and_is_scoped() {
        let v: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/github-publication-v1.json"
        ))
        .unwrap();
        let s = Scope::new(
            v["tenant"].as_str().unwrap(),
            v["repository"].as_str().unwrap().parse().unwrap(),
        )
        .unwrap();
        let change: Uuid = v["changeset"].as_str().unwrap().parse().unwrap();
        let id = branch(&s, change);
        assert_eq!(id.to_string(), v["branch_id"]);
        assert_eq!(
            format!("refs/heads/treeharbor/{}", id.simple()),
            v["branch"]
        );
        assert_ne!(
            id,
            branch(&Scope::new("other", s.repository()).unwrap(), change)
        );
        assert_ne!(
            id,
            branch(
                &Scope::new(s.tenant(), Uuid::from_bytes([7; 16])).unwrap(),
                change
            )
        );
        let mut request = json!({"action":"publication_request","key":"request","epoch":1,"publication":change.to_string(),"specification":{"projection":change.to_string(),"expected_head":null,"ttl_seconds":60}});
        assert_eq!(
            serde_json::from_value::<WireCommand>(request.clone())
                .unwrap()
                .permission(),
            Permission::RepositoryWrite
        );
        request["specification"]["branch"] = json!("refs/heads/main");
        assert!(serde_json::from_value::<WireCommand>(request).is_err());
        for value in [
            "main",
            "git:sha1:0000000000000000000000000000000000000000",
            "git:sha256:abcd",
            "+HEAD:main",
        ] {
            assert!(git(value).is_err());
        }
    }
}
