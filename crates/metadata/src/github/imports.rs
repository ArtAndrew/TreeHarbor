use super::*;
use crate::{
    Mutation,
    catalog::{ObjectKey, ObjectLease},
    uploads::UploadLimits,
};
use serde::{Deserialize, Serialize};
use treeharbor_core::{ObjectId, RevisionRef};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportSpec {
    pub git_revision: String,
    pub max_objects: i64,
    pub max_bytes: i64,
    pub ttl_seconds: i32,
}
impl ImportSpec {
    pub fn validate(&self) -> Result<()> {
        let revision: RevisionRef = self
            .git_revision
            .parse()
            .map_err(|_| Error::Invalid("Git import revision"))?;
        if !matches!(revision, RevisionRef::Git(_)) || revision.to_string() != self.git_revision {
            return Err(Error::Invalid("Git import revision"));
        }
        UploadLimits {
            objects: self.max_objects,
            bytes: self.max_bytes,
            ttl_seconds: self.ttl_seconds,
        }
        .validate()
    }
}
async fn load(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt("SELECT *,expires_at>clock_timestamp() AS unexpired,(EXTRACT(EPOCH FROM expires_at)*1000)::bigint AS deadline FROM th_github_imports WHERE tenant=$1 AND repository=$2 AND import=$3",&[&s.tenant(),&s.repository(),&id]).await?.ok_or(Error::NotFound)
}
fn epoch(row: &Row, epoch: i64) -> Result<()> {
    if row.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    Ok(())
}
fn live(row: &Row) -> Result<()> {
    if !row.get::<_, bool>("unexpired")
        || !matches!(
            row.get::<_, String>("state").as_str(),
            "pending" | "receiving"
        )
    {
        return Err(Error::UploadState);
    }
    Ok(())
}
async fn deadline(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<()> {
    if !load(tx, s, id).await?.get::<_, bool>("unexpired") {
        return Err(Error::UploadState);
    }
    Ok(())
}
fn describe(row: &Row) -> Result<Value> {
    let commit = row
        .get::<_, Option<Vec<u8>>>("native_commit")
        .map(|b| {
            b.try_into()
                .map(ObjectId::from_bytes)
                .map_err(|_| Error::SchemaMismatch)
        })
        .transpose()?;
    Ok(
        json!({"version":1,"import":row.get::<_,Uuid>("import").to_string(),"binding":row.get::<_,Uuid>("binding").to_string(),"epoch":row.get::<_,i64>("epoch"),"state":row.get::<_,String>("state"),"bridge":row.get::<_,String>("bridge"),"git_revision":row.get::<_,String>("git_revision"),"max_objects":row.get::<_,i64>("max_objects"),"max_bytes":row.get::<_,i64>("max_bytes"),"expires_at_unix_ms":row.get::<_,i64>("deadline"),"unexpired":row.get::<_,bool>("unexpired"),"native_commit":commit.map(|c|c.to_string()),"publication_ready":false}),
    )
}
async fn bridge_context(
    tx: &Transaction<'_>,
    auth: &Authorization,
    id: Uuid,
) -> Result<(Row, BindingSpec)> {
    let row = load(tx, auth.scope(), id).await?;
    let (_, spec) = assigned(tx, auth, row.get("epoch"), row.get("binding")).await?;
    if row.get::<_, String>("bridge") != auth.principal() {
        return Err(Error::Forbidden);
    }
    Ok((row, spec))
}
pub(crate) async fn check_upload(
    tx: &Transaction<'_>,
    auth: &Authorization,
    id: Uuid,
) -> Result<()> {
    let (row, _) = bridge_context(tx, auth, id).await?;
    live(&row)?;
    if row.get::<_, String>("state") != "receiving" {
        return Err(Error::UploadState);
    }
    let s = auth.scope();
    let linked:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM th_source_uploads WHERE tenant=$1 AND repository=$2 AND upload=$3 AND github_import=$3 AND principal=$4)",&[&s.tenant(),&s.repository(),&id,&auth.principal()]).await?.get(0);
    if !linked {
        return Err(Error::UploadState);
    }
    Ok(())
}
async fn complete(
    mutation: Mutation<'_>,
    auth: &Authorization,
    id: Uuid,
    result: Value,
    events: &[Event],
) -> Result<Value> {
    mutation.write_completion(&result, events).await?;
    let row = load(&mutation.tx, auth.scope(), id).await?;
    context(&mutation.tx, auth, row.get("epoch"), Permission::HostRead).await?;
    assigned(&mutation.tx, auth, row.get("epoch"), row.get("binding")).await?;
    deadline(&mutation.tx, auth.scope(), id).await?;
    mutation.tx.commit().await?;
    Ok(result)
}
impl Metadata {
    pub async fn request_github_import(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        binding_id: Uuid,
        spec: &ImportSpec,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(binding_id)?;
        spec.validate()?;
        let request = request(
            auth,
            cmd,
            "github.import.request.v1",
            json!([1, id.to_string(), binding_id.to_string(), spec]),
        )?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&mutation.tx, auth, cmd.epoch, Permission::RepositoryWrite).await?;
        let row = binding(&mutation.tx, auth.scope(), binding_id).await?;
        if row.get::<_, i64>("epoch") != cmd.epoch || row.get::<_, String>("state") != "active" {
            return Err(Error::HostBindingState);
        }
        let s = auth.scope();
        let count:i64=mutation.tx.query_one("SELECT count(*) FROM th_github_imports WHERE tenant=$1 AND repository=$2 AND state IN ('pending','receiving') AND expires_at>clock_timestamp()",&[&s.tenant(),&s.repository()]).await?.get(0);
        if count >= 4 {
            return Err(Error::UploadLimit);
        }
        let changed=mutation.tx.execute("INSERT INTO th_github_imports(tenant,repository,import,binding,epoch,principal,bridge,git_revision,state,expires_at,max_objects,max_bytes) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'pending',clock_timestamp()+($9::integer*interval '1 second'),$10,$11) ON CONFLICT DO NOTHING",&[&s.tenant(),&s.repository(),&id,&binding_id,&cmd.epoch,&auth.principal(),&row.get::<_,String>("bridge"),&spec.git_revision,&spec.ttl_seconds,&spec.max_objects,&spec.max_bytes]).await?;
        if changed != 1 {
            return Err(Error::UploadState);
        }
        let result = describe(&load(&mutation.tx, s, id).await?)?;
        mutation
            .write_completion(
                &result,
                &[Event {
                    kind: "github.import.requested".into(),
                    payload: json!({"import":id.to_string(),"binding":binding_id.to_string()}),
                }],
            )
            .await?;
        deadline(&mutation.tx, s, id).await?;
        mutation.tx.commit().await?;
        Ok(result)
    }
    pub async fn github_import_status(
        &mut self,
        auth: &Authorization,
        current_epoch: i64,
        id: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        context(&tx, auth, current_epoch, Permission::Read).await?;
        let result = describe(&load(&tx, auth.scope(), id).await?)?;
        context(&tx, auth, current_epoch, Permission::Read).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn github_import_context(
        &mut self,
        auth: &Authorization,
        current_epoch: i64,
        id: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let (row, spec) = bridge_context(&tx, auth, id).await?;
        epoch(&row, current_epoch)?;
        if row.get::<_, String>("state") != "completed" {
            live(&row)?;
        }
        let mut result = describe(&row)?;
        result["specification"] = json!(spec);
        bridge_context(&tx, auth, id).await?;
        if row.get::<_, String>("state") != "completed" {
            deadline(&tx, auth.scope(), id).await?;
        }
        tx.commit().await?;
        Ok(result)
    }
    pub async fn cancel_github_import(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        let request = request(
            auth,
            cmd,
            "github.import.cancel.v1",
            json!([1, id.to_string()]),
        )?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&mutation.tx, auth, cmd.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        let row = load(&mutation.tx, s, id).await?;
        epoch(&row, cmd.epoch)?;
        if row.get::<_, String>("state") == "completed" {
            return Err(Error::UploadState);
        }
        mutation.tx.execute("UPDATE th_github_imports SET state='cancelled' WHERE tenant=$1 AND repository=$2 AND import=$3 AND state IN ('pending','receiving')",&[&s.tenant(),&s.repository(),&id]).await?;
        mutation.tx.execute("UPDATE th_source_uploads SET state='aborted' WHERE tenant=$1 AND repository=$2 AND upload=$3 AND github_import=$3 AND state='active'",&[&s.tenant(),&s.repository(),&id]).await?;
        mutation
            .complete(
                json!({"version":1,"import":id.to_string(),"outcome":"cancelled"}),
                vec![Event {
                    kind: "github.import.cancelled".into(),
                    payload: json!({"import":id.to_string()}),
                }],
            )
            .await
    }
    pub async fn begin_github_import_upload(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        backend: &str,
        commit: ObjectId,
    ) -> Result<Value> {
        uuid(id)?;
        crate::identifier(backend, 64)?;
        let request = request(
            auth,
            cmd,
            "github.import.begin.v1",
            json!([1, id.to_string(), backend, commit.to_string()]),
        )?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let (row, _) = bridge_context(&mutation.tx, auth, id).await?;
        epoch(&row, cmd.epoch)?;
        live(&row)?;
        if row.get::<_, String>("state") != "pending" {
            return Err(Error::UploadState);
        }
        let s = auth.scope();
        let count:i64=mutation.tx.query_one("SELECT count(*) FROM th_source_uploads WHERE tenant=$1 AND repository=$2 AND state='active' AND expires_at>clock_timestamp()",&[&s.tenant(),&s.repository()]).await?.get(0);
        if count >= 4 {
            return Err(Error::UploadLimit);
        }
        let changed=mutation.tx.execute("INSERT INTO th_source_uploads(tenant,repository,upload,principal,authority,epoch,backend,expires_at,max_objects,max_bytes,github_import) SELECT tenant,repository,import,bridge,'connected',epoch,$4,expires_at,max_objects,max_bytes,import FROM th_github_imports WHERE tenant=$1 AND repository=$2 AND import=$3 ON CONFLICT DO NOTHING",&[&s.tenant(),&s.repository(),&id,&backend]).await?;
        if changed != 1 {
            return Err(Error::UploadState);
        }
        mutation.tx.execute("UPDATE th_github_imports SET state='receiving',native_commit=$4 WHERE tenant=$1 AND repository=$2 AND import=$3",&[&s.tenant(),&s.repository(),&id,&commit.as_bytes().to_vec()]).await?;
        complete(mutation,auth,id,json!({"version":1,"import":id.to_string(),"upload":id.to_string(),"outcome":"receiving","native_commit":commit.to_string()}),&[]).await
    }
    pub async fn finish_github_import(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        let request = request(
            auth,
            cmd,
            "github.import.finish.v1",
            json!([1, id.to_string()]),
        )?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let (row, _) = bridge_context(&mutation.tx, auth, id).await?;
        epoch(&row, cmd.epoch)?;
        check_upload(&mutation.tx, auth, id).await?;
        let s = auth.scope();
        let digest: Vec<u8> = row.get("native_commit");
        let commit = ObjectId::from_bytes(
            digest
                .clone()
                .try_into()
                .map_err(|_| Error::SchemaMismatch)?,
        );
        let item=mutation.tx.query_opt("SELECT i.generation,i.hold_token,i.snapshot,i.git_origin,u.backend,u.expires_at>clock_timestamp() AND u.state='active' AS active FROM th_source_upload_items i JOIN th_source_uploads u USING(tenant,repository,upload) WHERE i.tenant=$1 AND i.repository=$2 AND i.upload=$3 AND i.digest=$4 AND i.kind=5 AND i.complete",&[&s.tenant(),&s.repository(),&id,&digest]).await?.ok_or(Error::UploadMembership)?;
        if !item.get::<_, bool>("active") {
            return Err(Error::UploadState);
        }
        if item.get::<_, Option<String>>("git_origin").as_deref()
            != Some(row.get::<_, String>("git_revision").as_str())
        {
            return Err(Error::UploadMembership);
        }
        let snapshot = ObjectId::from_bytes(
            item.get::<_, Vec<u8>>("snapshot")
                .try_into()
                .map_err(|_| Error::SchemaMismatch)?,
        );
        let lease = ObjectLease {
            key: ObjectKey {
                tenant: s.tenant().into(),
                backend: item.get("backend"),
                id: commit,
                generation: item.get("generation"),
            },
            token: item.get("hold_token"),
            upload: false,
        };
        mutation.register_base(commit, snapshot, &lease).await?;
        mutation.tx.execute("UPDATE th_source_uploads SET state='completed' WHERE tenant=$1 AND repository=$2 AND upload=$3",&[&s.tenant(),&s.repository(),&id]).await?;
        mutation.tx.execute("UPDATE th_github_imports SET state='completed',completed_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND import=$3",&[&s.tenant(),&s.repository(),&id]).await?;
        let result = json!({"version":1,"import":id.to_string(),"outcome":"completed","git_revision":row.get::<_,String>("git_revision"),"revision":RevisionRef::Native(commit).to_string(),"snapshot":snapshot.to_string(),"publication_ready":false});
        complete(mutation,auth,id,result,&[Event {kind:"github.import.completed".into(),payload:json!({"import":id.to_string(),"revision":RevisionRef::Native(commit).to_string()})}]).await
    }
}
