//! Durable Connected configuration and assigned-bridge connection observations.
//! Neither configuration nor a successful probe grants publication or validation.
mod types;
mod wire;
use crate::{
    Authority, Begin, Error, Event, Metadata, Request, Result, Scope,
    access::{Authorization, Permission, repository_lock},
    catalog::settings,
};
use serde_json::{Value, json};
use tokio_postgres::{Row, Transaction};
pub use types::{BindingSpec, Observation, Upstream};
use uuid::Uuid;
pub use wire::WireCommand;

pub struct Command {
    pub key: String,
    pub epoch: i64,
}
fn request(auth: &Authorization, command: &Command, kind: &str, data: Value) -> Result<Request> {
    if command.epoch <= 0 {
        return Err(Error::Invalid("GitHub epoch"));
    }
    auth.context(command.key.clone(), Authority::Connected, command.epoch)
        .request(kind, data)
}
fn uuid(id: Uuid) -> Result<()> {
    if id.is_nil() {
        return Err(Error::Invalid("GitHub operation id"));
    }
    Ok(())
}
async fn context(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    permission: Permission,
) -> Result<()> {
    let s = auth.scope();
    auth.check_transaction(tx, s, auth.principal(), permission)
        .await?;
    let row = tx
        .query_one(
            "SELECT authority,epoch,state FROM th_repositories WHERE tenant=$1 AND repository=$2",
            &[&s.tenant(), &s.repository()],
        )
        .await?;
    if row.get::<_, String>(0) != "connected"
        || row.get::<_, i64>(1) != epoch
        || row.get::<_, String>(2) != "active"
    {
        return Err(Error::Fenced);
    }
    Ok(())
}
async fn binding(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt(
        "SELECT * FROM th_github_bindings WHERE tenant=$1 AND repository=$2 AND binding=$3",
        &[&s.tenant(), &s.repository(), &id],
    )
    .await?
    .ok_or(Error::NotFound)
}
fn specification(row: &Row) -> Result<BindingSpec> {
    let spec: BindingSpec =
        serde_json::from_value(row.get("specification")).map_err(|_| Error::SchemaMismatch)?;
    spec.validate().map_err(|_| Error::SchemaMismatch)?;
    if spec.bridge != row.get::<_, String>("bridge") {
        return Err(Error::SchemaMismatch);
    }
    Ok(spec)
}
async fn assigned(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    id: Uuid,
) -> Result<(Row, BindingSpec)> {
    context(tx, auth, epoch, Permission::HostRead).await?;
    let row = binding(tx, auth.scope(), id).await?;
    let spec = specification(&row)?;
    if row.get::<_, i64>("epoch") != epoch || row.get::<_, String>("state") != "active" {
        return Err(Error::HostBindingState);
    }
    if spec.bridge != auth.principal() {
        return Err(Error::Forbidden);
    }
    Ok((row, spec))
}
async fn probe(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt("SELECT *,expires_at>clock_timestamp() AS unexpired,EXTRACT(EPOCH FROM expires_at)::bigint AS deadline FROM th_github_probes WHERE tenant=$1 AND repository=$2 AND probe=$3",&[&s.tenant(),&s.repository(),&id]).await?.ok_or(Error::NotFound)
}
fn probe_context(row: &Row, binding: Uuid, epoch: i64, bridge: &str) -> Result<()> {
    if row.get::<_, Uuid>("binding") != binding
        || row.get::<_, i64>("epoch") != epoch
        || row.get::<_, String>("bridge") != bridge
    {
        return Err(Error::HostBindingState);
    }
    Ok(())
}
fn describe_binding(row: &Row) -> Result<Value> {
    Ok(
        json!({"version":1,"binding":row.get::<_,Uuid>("binding").to_string(),"epoch":row.get::<_,i64>("epoch"),"state":row.get::<_,String>("state"),"specification":specification(row)?,"publication_ready":false}),
    )
}
fn describe_probe(row: &Row, spec: &BindingSpec) -> Value {
    json!({"version":1,"probe":row.get::<_,Uuid>("probe").to_string(),"binding":row.get::<_,Uuid>("binding").to_string(),"epoch":row.get::<_,i64>("epoch"),"state":row.get::<_,String>("state"),"expires_at_unix":row.get::<_,i64>("deadline"),"unexpired":row.get::<_,bool>("unexpired"),"specification":spec,"observation":row.get::<_,Option<Value>>("observation"),"publication_ready":false})
}
impl Metadata {
    pub async fn bind_github(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        spec: &BindingSpec,
    ) -> Result<Value> {
        uuid(id)?;
        spec.validate()?;
        let request = request(
            auth,
            cmd,
            "github.bind.v1",
            json!([1, id.to_string(), spec]),
        )?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&mutation.tx, auth, cmd.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        let bridge=mutation.tx.query_opt("SELECT p.kind,p.enabled,a.permissions FROM th_principals p JOIN th_repository_access a USING(tenant,principal) WHERE p.tenant=$1 AND p.principal=$2 AND a.repository=$3 FOR SHARE OF p,a",&[&s.tenant(),&spec.bridge,&s.repository()]).await?.ok_or(Error::Forbidden)?;
        if bridge.get::<_, String>(0) != "bridge"
            || !bridge.get::<_, bool>(1)
            || !bridge
                .get::<_, Vec<String>>(2)
                .iter()
                .any(|p| p == "host.read")
        {
            return Err(Error::Forbidden);
        }
        let exists=mutation.tx.query_opt("SELECT binding FROM th_github_bindings WHERE tenant=$1 AND repository=$2 AND (binding=$3 OR state='active') LIMIT 1",&[&s.tenant(),&s.repository(),&id]).await?;
        if exists.is_some() {
            return Err(Error::HostBindingState);
        }
        mutation.tx.execute("INSERT INTO th_github_bindings(tenant,repository,binding,epoch,bridge,specification,state,principal) VALUES($1,$2,$3,$4,$5,$6,'active',$7)",&[&s.tenant(),&s.repository(),&id,&cmd.epoch,&spec.bridge,&json!(spec),&auth.principal()]).await?;
        let result =
            json!({"version":1,"binding":id.to_string(),"epoch":cmd.epoch,"outcome":"configured"});
        mutation
            .complete(
                result,
                vec![Event {
                    kind: "github.binding.configured".into(),
                    payload: json!({"binding":id.to_string(),"epoch":cmd.epoch}),
                }],
            )
            .await
    }

    pub async fn revoke_github(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        let request = request(auth, cmd, "github.revoke.v1", json!([1, id.to_string()]))?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&mutation.tx, auth, cmd.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        let row = binding(&mutation.tx, s, id).await?;
        if row.get::<_, i64>("epoch") != cmd.epoch {
            return Err(Error::HostBindingState);
        }
        mutation.tx.execute("UPDATE th_github_bindings SET state='revoked',revoked_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND binding=$3 AND state='active'",&[&s.tenant(),&s.repository(),&id]).await?;
        mutation
            .complete(
                json!({"version":1,"binding":id.to_string(),"epoch":cmd.epoch,"outcome":"revoked"}),
                vec![Event {
                    kind: "github.binding.revoked".into(),
                    payload: json!({"binding":id.to_string(),"epoch":cmd.epoch}),
                }],
            )
            .await
    }

    pub async fn github_binding_status(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        context(&tx, auth, epoch, Permission::Read).await?;
        let result = describe_binding(&binding(&tx, auth.scope(), id).await?)?;
        context(&tx, auth, epoch, Permission::Read).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn begin_github_probe(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        probe_id: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(probe_id)?;
        let request = request(
            auth,
            cmd,
            "github.probe.begin.v1",
            json!([1, id.to_string(), probe_id.to_string()]),
        )?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        assigned(&mutation.tx, auth, cmd.epoch, id).await?;
        let s = auth.scope();
        let count:i64=mutation.tx.query_one("SELECT count(*) FROM th_github_probes WHERE tenant=$1 AND repository=$2 AND binding=$3 AND state='pending' AND expires_at>clock_timestamp()",&[&s.tenant(),&s.repository(),&id]).await?.get(0);
        if count >= 4 {
            return Err(Error::HostBindingState);
        }
        let inserted=mutation.tx.execute("INSERT INTO th_github_probes(tenant,repository,binding,probe,epoch,bridge,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,'pending',clock_timestamp()+interval '120 seconds') ON CONFLICT DO NOTHING",&[&s.tenant(),&s.repository(),&id,&probe_id,&cmd.epoch,&auth.principal()]).await?;
        if inserted != 1 {
            return Err(Error::HostBindingState);
        }
        let result = json!({"version":1,"binding":id.to_string(),"probe":probe_id.to_string(),"epoch":cmd.epoch,"outcome":"requested"});
        mutation.write_completion(&result, &[]).await?;
        if !probe(&mutation.tx, s, probe_id)
            .await?
            .get::<_, bool>("unexpired")
        {
            return Err(Error::HostBindingState);
        }
        mutation.tx.commit().await?;
        Ok(result)
    }

    /// Assigned bridge read for resuming a pending attempt or a completed receipt.
    /// A completed observation is historical, never a grant for a new operation.
    pub async fn github_probe_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        probe_id: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(probe_id)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let (_, spec) = assigned(&tx, auth, epoch, id).await?;
        let row = probe(&tx, auth.scope(), probe_id).await?;
        probe_context(&row, id, epoch, auth.principal())?;
        if row.get::<_, String>("state") == "pending" && !row.get::<_, bool>("unexpired") {
            return Err(Error::HostBindingState);
        }
        let result = describe_probe(&row, &spec);
        assigned(&tx, auth, epoch, id).await?;
        if row.get::<_, String>("state") == "pending"
            && !probe(&tx, auth.scope(), probe_id)
                .await?
                .get::<_, bool>("unexpired")
        {
            return Err(Error::HostBindingState);
        }
        tx.commit().await?;
        Ok(result)
    }

    pub async fn finish_github_probe(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        probe_id: Uuid,
        observation: &Observation,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(probe_id)?;
        let request = request(
            auth,
            cmd,
            "github.probe.finish.v1",
            json!([1, id.to_string(), probe_id.to_string(), observation]),
        )?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let (_, spec) = assigned(&mutation.tx, auth, cmd.epoch, id).await?;
        observation.validate(&spec)?;
        let s = auth.scope();
        let row = probe(&mutation.tx, s, probe_id).await?;
        probe_context(&row, id, cmd.epoch, auth.principal())?;
        if row.get::<_, String>("state") != "pending" || !row.get::<_, bool>("unexpired") {
            return Err(Error::HostBindingState);
        }
        mutation.tx.execute("UPDATE th_github_probes SET state='observed',observation=$4,completed_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND probe=$3",&[&s.tenant(),&s.repository(),&probe_id,&json!(observation)]).await?;
        let result = json!({"version":1,"binding":id.to_string(),"probe":probe_id.to_string(),"epoch":cmd.epoch,"outcome":"observed","observation":observation,"publication_ready":false});
        mutation.write_completion(&result,&[Event {kind:"github.connection.observed".into(),payload:json!({"binding":id.to_string(),"probe":probe_id.to_string(),"epoch":cmd.epoch})}]).await?;
        assigned(&mutation.tx, auth, cmd.epoch, id).await?;
        if !probe(&mutation.tx, s, probe_id)
            .await?
            .get::<_, bool>("unexpired")
        {
            return Err(Error::HostBindingState);
        }
        mutation.tx.commit().await?;
        Ok(result)
    }
}
