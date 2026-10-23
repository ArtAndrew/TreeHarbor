//! Exact retained ChangeSet versions projected to native commits for Git interop.
//! Preparation is not host publication, merge approval or a new authoritative base.
use super::*;
use crate::{
    Mutation,
    catalog::{ObjectLease, publish_in, reserve_in},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use treeharbor_core::{Commit, Object, ObjectId, RevisionRef};
use treeharbor_storage::{DurableObject, ManagedStore, PreparedObject};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionSpec {
    pub changeset: String,
    pub version: i64,
    pub title: String,
    pub ttl_seconds: i32,
}
impl ProjectionSpec {
    pub fn validate(&self) -> Result<Uuid> {
        let id: Uuid = self
            .changeset
            .parse()
            .map_err(|_| Error::Invalid("projection changeset"))?;
        uuid(id)?;
        if id.to_string() != self.changeset
            || self.version <= 0
            || self.title.is_empty()
            || self.title.len() > 256
            || self.title.trim() != self.title
            || self.title.chars().any(char::is_control)
            || !(1..=3600).contains(&self.ttl_seconds)
        {
            return Err(Error::Invalid("projection specification"));
        }
        Ok(id)
    }
}
fn id(raw: Vec<u8>) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        raw.try_into().map_err(|_| Error::SchemaMismatch)?,
    ))
}
fn commit(row: &Row, title: &str, changeset: Uuid, version: i64) -> Result<Object> {
    let RevisionRef::Native(base) = row.get::<_, String>("base_revision").parse()? else {
        return Err(Error::Invalid("native projection base"));
    };
    Ok(Object::NativeCommit(Commit {
        parents: vec![base],
        snapshot: id(row.get("result_snapshot"))?,
        changes: id(row.get("operations"))?,
        git_origin: None,
        author_name: "TreeHarbor".into(),
        author_email: "projection@treeharbor.invalid".into(),
        timestamp_seconds: row.get("timestamp_seconds"),
        timezone_minutes: 0,
        message: format!("TreeHarbor ChangeSet {changeset} version {version}\n\n{title}\n"),
    }))
}
struct Record {
    row: Row,
    object: PreparedObject,
}
impl Record {
    fn describe(&self) -> Value {
        json!({"version":1,"projection":self.row.get::<_,Uuid>("projection").to_string(),
            "binding":self.row.get::<_,Uuid>("binding").to_string(),"epoch":self.row.get::<_,i64>("epoch"),
            "state":self.row.get::<_,String>("state"),"changeset":self.row.get::<_,Uuid>("changeset").to_string(),
            "change_version":self.row.get::<_,i64>("version"),"run":self.row.get::<_,Uuid>("run").to_string(),
            "is_current_version":self.row.get::<_,i64>("version")==self.row.get::<_,i64>("latest_version"),
            "unexpired":self.row.get::<_,bool>("unexpired"),"expires_at_unix_ms":self.row.get::<_,i64>("deadline"),
            "revision":RevisionRef::Native(self.object.id()).to_string(),"snapshot":self.object.commit_snapshot().map(|v|v.to_string()),
            "title":self.row.get::<_,String>("title"),"publication_ready":false})
    }
}
async fn load(tx: &Transaction<'_>, s: &Scope, projection: Uuid) -> Result<Record> {
    let row=tx.query_opt(r"SELECT p.*,v.run,v.base_revision,v.operations,v.result_snapshot,
        floor(extract(epoch from v.accepted_at))::bigint AS timestamp_seconds,c.latest_version,
        p.expires_at>clock_timestamp() AS unexpired,(extract(epoch from p.expires_at)*1000)::bigint AS deadline
        FROM th_github_projections p JOIN th_change_versions v USING(tenant,repository,changeset,version)
        JOIN th_changesets c USING(tenant,repository,changeset)
        WHERE p.tenant=$1 AND p.repository=$2 AND p.projection=$3",&[&s.tenant(),&s.repository(),&projection]).await?.ok_or(Error::NotFound)?;
    let object = PreparedObject::new(&commit(
        &row,
        &row.get::<_, String>("title"),
        row.get("changeset"),
        row.get("version"),
    )?)?;
    if object.id() != id(row.get("native_commit"))? {
        return Err(Error::SchemaMismatch);
    }
    Ok(Record { row, object })
}
async fn active_binding(
    tx: &Transaction<'_>,
    s: &Scope,
    binding_id: Uuid,
    epoch: i64,
) -> Result<BindingSpec> {
    let b = binding(tx, s, binding_id).await?;
    if b.get::<_, String>("state") != "active" || b.get::<_, i64>("epoch") != epoch {
        return Err(Error::HostBindingState);
    }
    specification(&b)
}
fn live(record: &Record) -> Result<()> {
    if !record.row.get::<_, bool>("unexpired")
        || record.row.get::<_, String>("state") == "cancelled"
        || record.row.get::<_, i64>("version") != record.row.get::<_, i64>("latest_version")
    {
        return Err(Error::HostProjectionState);
    }
    Ok(())
}
async fn fresh(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    projection: Uuid,
) -> Result<Record> {
    context(tx, auth, epoch, Permission::RepositoryWrite).await?;
    let record = load(tx, auth.scope(), projection).await?;
    if record.row.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    active_binding(tx, auth.scope(), record.row.get("binding"), epoch).await?;
    live(&record)?;
    Ok(record)
}
fn token(s: &Scope, projection: Uuid, object: ObjectId) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"treeharbor-github-projection-object\0v1\0");
    hash.update((s.tenant().len() as u64).to_be_bytes());
    hash.update(s.tenant().as_bytes());
    hash.update(s.repository().as_bytes());
    hash.update(projection.as_bytes());
    hash.update(object.as_bytes());
    Uuid::from_bytes(hash.finalize()[..16].try_into().expect("digest length"))
}
fn prepare_request(auth: &Authorization, cmd: &Command, projection: Uuid) -> Result<Request> {
    request(
        auth,
        cmd,
        "github.projection.prepare.v1",
        json!([1, projection.to_string()]),
    )
}
async fn complete(
    m: Mutation<'_>,
    auth: &Authorization,
    cmd: &Command,
    projection: Uuid,
    result: Value,
    events: &[Event],
) -> Result<Value> {
    m.write_completion(&result, events).await?;
    fresh(&m.tx, auth, cmd.epoch, projection).await?;
    m.tx.commit().await?;
    Ok(result)
}
impl Metadata {
    pub async fn github_projection_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        projection: Uuid,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        export_context(&tx, auth, epoch, projection).await?;
        let r = load(&tx, auth.scope(), projection).await?;
        let mut result = r.describe();
        result["specification"] =
            json!(active_binding(&tx, auth.scope(), r.row.get("binding"), epoch).await?);
        export_context(&tx, auth, epoch, projection).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn request_github_projection(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        projection: Uuid,
        binding_id: Uuid,
        spec: &ProjectionSpec,
    ) -> Result<Value> {
        uuid(projection)?;
        uuid(binding_id)?;
        let changeset = spec.validate()?;
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.projection.request.v1",
                json!([1, projection.to_string(), binding_id.to_string(), spec]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, cmd.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        active_binding(&m.tx, s, binding_id, cmd.epoch).await?;
        let row=m.tx.query_opt(r"SELECT v.*,floor(extract(epoch from v.accepted_at))::bigint AS timestamp_seconds,c.latest_version
            FROM th_change_versions v JOIN th_changesets c USING(tenant,repository,changeset)
            WHERE v.tenant=$1 AND v.repository=$2 AND v.changeset=$3 AND v.version=$4",&[&s.tenant(),&s.repository(),&changeset,&spec.version]).await?.ok_or(Error::NotFound)?;
        if row.get::<_, i64>("latest_version") != spec.version {
            return Err(Error::VersionConflict);
        }
        let object = PreparedObject::new(&commit(&row, &spec.title, changeset, spec.version)?)?;
        let count:i64=m.tx.query_one("SELECT count(*) FROM th_github_projections WHERE tenant=$1 AND repository=$2 AND state='pending' AND expires_at>clock_timestamp()",&[&s.tenant(),&s.repository()]).await?.get(0);
        if count >= 4 {
            return Err(Error::UploadLimit);
        }
        let n=m.tx.execute("INSERT INTO th_github_projections(tenant,repository,projection,binding,epoch,principal,changeset,version,title,native_commit,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'pending',clock_timestamp()+($11::integer*interval '1 second')) ON CONFLICT DO NOTHING",&[&s.tenant(),&s.repository(),&projection,&binding_id,&cmd.epoch,&auth.principal(),&changeset,&spec.version,&spec.title,&object.id().as_bytes().to_vec(),&spec.ttl_seconds]).await?;
        if n != 1 {
            return Err(Error::HostProjectionState);
        }
        let result = load(&m.tx, s, projection).await?.describe();
        complete(
            m,
            auth,
            cmd,
            projection,
            result,
            &[Event {
                kind: "github.projection.requested".into(),
                payload: json!({"projection":projection.to_string()}),
            }],
        )
        .await
    }
    pub async fn github_projection_status(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        projection: Uuid,
    ) -> Result<Value> {
        uuid(projection)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        context(&tx, auth, epoch, Permission::Read).await?;
        let result = load(&tx, auth.scope(), projection).await?.describe();
        context(&tx, auth, epoch, Permission::Read).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn cancel_github_projection(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        projection: Uuid,
    ) -> Result<Value> {
        uuid(projection)?;
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.projection.cancel.v1",
                json!([1, projection.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, cmd.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        let row = load(&m.tx, s, projection).await?;
        if row.row.get::<_, i64>("epoch") != cmd.epoch {
            return Err(Error::Fenced);
        }
        m.tx.execute("UPDATE th_github_projections SET state='cancelled' WHERE tenant=$1 AND repository=$2 AND projection=$3 AND state<>'cancelled'",&[&s.tenant(),&s.repository(),&projection]).await?;
        m.complete(
            json!({"version":1,"projection":projection.to_string(),"outcome":"cancelled"}),
            vec![],
        )
        .await
    }
    pub async fn github_projection_preparation_replay(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        projection: Uuid,
    ) -> Result<Option<Value>> {
        uuid(projection)?;
        match self.begin(prepare_request(auth, cmd, projection)?).await? {
            Begin::Replay(v) => Ok(Some(v)),
            Begin::Fresh(m) => {
                let r = fresh(&m.tx, auth, cmd.epoch, projection).await?;
                if r.row.get::<_, String>("state") != "pending" {
                    return Err(Error::HostProjectionState);
                }
                m.tx.rollback().await?;
                Ok(None)
            }
        }
    }
    pub async fn reserve_github_projection(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        projection: Uuid,
        store: &ManagedStore,
    ) -> Result<(PreparedObject, ObjectLease)> {
        uuid(projection)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        let r = fresh(&tx, auth, epoch, projection).await?;
        if r.row.get::<_, String>("state") != "pending" {
            return Err(Error::HostProjectionState);
        }
        if store.tenant() != s.tenant() {
            return Err(Error::Forbidden);
        }
        let token = token(s, projection, r.object.id());
        let lease = reserve_in(&tx, store, &r.object, token, 300).await?;
        if let Some(backend) = r.row.get::<_, Option<String>>("backend") {
            if backend != store.backend()
                || r.row.get::<_, Option<i64>>("generation") != Some(lease.key().generation())
                || r.row.get::<_, Option<Uuid>>("hold_token") != Some(token)
            {
                return Err(Error::ReceiptMismatch);
            }
        } else {
            tx.execute("UPDATE th_github_projections SET backend=$4,generation=$5,hold_token=$6 WHERE tenant=$1 AND repository=$2 AND projection=$3",&[&s.tenant(),&s.repository(),&projection,&store.backend(),&lease.key().generation(),&token]).await?;
        }
        tx.execute("UPDATE th_object_holds h SET expires_at=p.expires_at FROM th_github_projections p WHERE p.tenant=$1 AND p.repository=$2 AND p.projection=$3 AND h.tenant=p.tenant AND h.token=$4",&[&s.tenant(),&s.repository(),&projection,&token]).await?;
        fresh(&tx, auth, epoch, projection).await?;
        tx.commit().await?;
        Ok((r.object, lease))
    }
    pub async fn finish_github_projection(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        projection: Uuid,
        lease: &ObjectLease,
        receipt: &DurableObject,
    ) -> Result<Value> {
        uuid(projection)?;
        let m = match self.begin(prepare_request(auth, cmd, projection)?).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let r = fresh(&m.tx, auth, cmd.epoch, projection).await?;
        let s = auth.scope();
        if r.row.get::<_, String>("state") != "pending" {
            return Err(Error::HostProjectionState);
        }
        if lease.key().tenant() != s.tenant()
            || lease.key().id() != r.object.id()
            || r.row.get::<_, Option<String>>("backend").as_deref() != Some(lease.key().backend())
            || r.row.get::<_, Option<i64>>("generation") != Some(lease.key().generation())
            || r.row.get::<_, Option<Uuid>>("hold_token") != Some(lease.token())
        {
            return Err(Error::ReceiptMismatch);
        }
        publish_in(&m.tx, lease, receipt).await?;
        let pin = format!("github-projection:{projection}");
        m.pin_kind(&pin, lease, 5).await?;
        m.tx.execute("UPDATE th_github_projections SET state='ready',pin_holder=$4 WHERE tenant=$1 AND repository=$2 AND projection=$3",&[&s.tenant(),&s.repository(),&projection,&pin]).await?;
        let result = load(&m.tx, s, projection).await?.describe();
        complete(m,auth,cmd,projection,result,&[Event{kind:"github.projection.ready".into(),payload:json!({"projection":projection.to_string(),"revision":RevisionRef::Native(r.object.id()).to_string()})}]).await
    }
}
/// Purpose-specific bridge export. A ready artifact never grants upstream mutation.
pub(crate) async fn export_context(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    projection: Uuid,
) -> Result<(ObjectId, ObjectId, String)> {
    uuid(projection)?;
    let r = load(tx, auth.scope(), projection).await?;
    if r.row.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    assigned_for(
        tx,
        auth,
        epoch,
        r.row.get("binding"),
        Permission::HostPublish,
    )
    .await?;
    live(&r)?;
    if r.row.get::<_, String>("state") != "ready" {
        return Err(Error::HostProjectionState);
    }
    Ok((
        r.object.id(),
        r.object.commit_snapshot().ok_or(Error::SchemaMismatch)?,
        r.row
            .get::<_, Option<String>>("pin_holder")
            .ok_or(Error::SchemaMismatch)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_projection_vector_and_strict_owner_input() {
        let vector: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/github-projection-v1.json"
        ))
        .unwrap();
        let scope = Scope::new(
            vector["tenant"].as_str().unwrap(),
            vector["repository"].as_str().unwrap().parse().unwrap(),
        )
        .unwrap();
        let request = vector["request"].clone();
        let command: WireCommand = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(command.permission(), Permission::RepositoryWrite);
        let WireCommand::ProjectionRequest {
            projection,
            specification,
            ..
        } = command
        else {
            panic!()
        };
        specification.validate().unwrap();
        assert_eq!(
            token(
                &scope,
                projection.parse().unwrap(),
                vector["object"].as_str().unwrap().parse().unwrap()
            )
            .to_string(),
            vector["hold_token"]
        );
        for field in [
            "revision",
            "token",
            "backend",
            "author",
            "timestamp",
            "force",
        ] {
            let mut invalid = request.clone();
            invalid["specification"][field] = json!("caller-selected");
            assert!(serde_json::from_value::<WireCommand>(invalid).is_err());
        }
        for title in ["", " leading", "trailing ", "line\nbreak"] {
            let mut spec = specification.clone();
            spec.title = title.into();
            assert!(spec.validate().is_err());
        }
        for value in [0, 3601] {
            let mut spec = specification.clone();
            spec.ttl_seconds = value;
            assert!(spec.validate().is_err());
        }
        for value in [0, -1] {
            let mut spec = specification.clone();
            spec.version = value;
            assert!(spec.validate().is_err());
        }
        for action in [
            "projection_status",
            "projection_context",
            "projection_cancel",
        ] {
            let mut value = json!({"action":action,"epoch":1,"projection":projection});
            if action == "projection_cancel" {
                value["key"] = json!("cancel");
            }
            let command: WireCommand = serde_json::from_value(value).unwrap();
            assert_eq!(
                command.permission(),
                match action {
                    "projection_status" => Permission::Read,
                    "projection_context" => Permission::HostPublish,
                    _ => Permission::RepositoryWrite,
                }
            );
        }
    }
}
