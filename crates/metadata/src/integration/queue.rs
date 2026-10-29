use super::*;

async fn queue_status(tx: &Transaction<'_>, s: &Scope, entry: Uuid) -> Result<Value> {
    let row=tx.query_opt("SELECT target,epoch,changeset,version,state,candidate,reason FROM th_native_queue WHERE tenant=$1 AND repository=$2 AND entry=$3", &[&s.tenant,&s.repository,&entry]).await?.ok_or(Error::NotFound)?;
    Ok(
        json!({"entry":entry.to_string(),"target":row.get::<_,String>(0),"epoch":row.get::<_,i64>(1),"changeset":row.get::<_,Uuid>(2).to_string(),"version":row.get::<_,i64>(3),"state":row.get::<_,String>(4),"candidate":row.get::<_,Option<Uuid>>(5).map(|v|v.to_string()),"reason":row.get::<_,Option<String>>(6)}),
    )
}

impl Metadata {
    /// Live pending view, not an immutable log. Restart from zero when polling.
    pub async fn native_queue_pending(
        &mut self,
        auth: &Authorization,
        name: &str,
        after: i64,
        limit: i64,
    ) -> Result<Value> {
        ref_name(name)?;
        if after < 0 || !(1..=100).contains(&limit) {
            return Err(Error::Invalid("native queue page"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        read_authorized(&tx, auth).await?;
        let s = auth.scope();
        // Confirm the target exists in this scope; never leak another repository.
        target(&tx, s, name).await?;
        let rows = tx.query(
            "SELECT entry,position,epoch,changeset,version,state,candidate FROM th_native_queue \
             WHERE tenant=$1 AND repository=$2 AND target=$3 AND position>$4 \
             AND state IN ('queued','preparing','checking') ORDER BY position LIMIT $5",
            &[&s.tenant, &s.repository, &name, &after, &(limit + 1)],
        ).await?;
        let more = rows.len() > limit as usize;
        let entries: Vec<_> = rows
            .iter()
            .take(limit as usize)
            .map(|row| {
                json!({
                    "entry": row.get::<_,Uuid>(0).to_string(), "position": row.get::<_,i64>(1),
                    "epoch": row.get::<_,i64>(2), "changeset": row.get::<_,Uuid>(3).to_string(),
                    "version": row.get::<_,i64>(4), "state": row.get::<_,String>(5),
                    "candidate": row.get::<_,Option<Uuid>>(6).map(|v| v.to_string())
                })
            })
            .collect();
        let next = if more {
            entries.last().and_then(|v| v["position"].as_i64())
        } else {
            None
        };
        read_authorized(&tx, auth).await?;
        tx.commit().await?;
        Ok(json!({"target":name,"entries":entries,"next_after":next}))
    }

    pub async fn enqueue_native_change(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        entry: Uuid,
        name: &str,
        changeset: Uuid,
        version: i64,
    ) -> Result<Value> {
        ref_name(name)?;
        if version <= 0 {
            return Err(Error::Invalid("change version"));
        }
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.queue.enqueue.v1",
                json!([1, entry.to_string(), name, changeset.to_string(), version]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let current = target(&tx.tx, s, name).await?;
        if current.epoch != cmd.epoch {
            return Err(Error::Fenced);
        }
        let owner=tx.tx.query_opt("SELECT c.principal FROM th_changesets c JOIN th_change_versions v USING(tenant,repository,changeset) WHERE c.tenant=$1 AND c.repository=$2 AND c.changeset=$3 AND v.version=$4", &[&s.tenant,&s.repository,&changeset,&version]).await?.ok_or(Error::NotFound)?;
        if owner.get::<_, String>(0) != auth.principal() {
            return Err(Error::Forbidden);
        }
        let existing=tx.tx.query_opt("SELECT entry FROM th_native_queue WHERE tenant=$1 AND repository=$2 AND target=$3 AND changeset=$4 AND version=$5 AND state IN ('queued','preparing','checking','landed')", &[&s.tenant,&s.repository,&name,&changeset,&version]).await?;
        if let Some(existing) = existing {
            let value = queue_status(&tx.tx, s, existing.get(0)).await?;
            return tx.complete(value, vec![]).await;
        }
        tx.tx.execute("INSERT INTO th_native_queue(tenant,repository,entry,target,epoch,principal,changeset,version,state) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'queued')", &[&s.tenant,&s.repository,&entry,&name,&cmd.epoch,&auth.principal(),&changeset,&version]).await?;
        let result = queue_status(&tx.tx, s, entry).await?;
        tx.complete(
            result,
            vec![Event {
                kind: "native.queue.requested".into(),
                payload: json!({"entry":entry.to_string(),"epoch":cmd.epoch}),
            }],
        )
        .await
    }

    pub async fn native_queue_status(
        &mut self,
        auth: &Authorization,
        entry: Uuid,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        read_authorized(&tx, auth).await?;
        let result = queue_status(&tx, auth.scope(), entry).await?;
        read_authorized(&tx, auth).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn cancel_native_entry(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        entry: Uuid,
    ) -> Result<Value> {
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.queue.cancel.v1",
                json!([1, entry.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let row=tx.tx.query_opt("SELECT principal,state,candidate FROM th_native_queue WHERE tenant=$1 AND repository=$2 AND entry=$3", &[&s.tenant,&s.repository,&entry]).await?.ok_or(Error::NotFound)?;
        if row.get::<_, String>(0) != auth.principal() {
            return Err(Error::Forbidden);
        }
        let state: String = row.get(1);
        if state == "landed" {
            return Err(Error::IntegrationState);
        }
        if let Some(candidate) = row.get::<_, Option<Uuid>>(2) {
            tx.tx.execute("UPDATE th_native_candidates SET state='cancelled' WHERE tenant=$1 AND repository=$2 AND candidate=$3", &[&s.tenant,&s.repository,&candidate]).await?;
        }
        tx.tx.execute("UPDATE th_native_queue SET state='cancelled',reason='owner_cancelled' WHERE tenant=$1 AND repository=$2 AND entry=$3", &[&s.tenant,&s.repository,&entry]).await?;
        let result = queue_status(&tx.tx, s, entry).await?;
        tx.complete(result, vec![]).await
    }

    pub async fn start_native_candidate(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        entry: Uuid,
        candidate: Uuid,
    ) -> Result<Value> {
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.candidate.start.v1",
                json!([
                    1,
                    entry.to_string(),
                    candidate.to_string(),
                    treeharbor_merge::PROFILE
                ]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let row=tx.tx.query_opt("SELECT target,state FROM th_native_queue WHERE tenant=$1 AND repository=$2 AND entry=$3 AND epoch=$4", &[&s.tenant,&s.repository,&entry,&cmd.epoch]).await?.ok_or(Error::NotFound)?;
        let name: String = row.get(0);
        if row.get::<_, String>(1) != "queued" {
            return Err(Error::IntegrationState);
        }
        let oldest=tx.tx.query_opt("SELECT entry FROM th_native_queue WHERE tenant=$1 AND repository=$2 AND target=$3 AND state IN ('queued','preparing','checking') ORDER BY position LIMIT 1", &[&s.tenant,&s.repository,&name]).await?.ok_or(Error::IntegrationState)?;
        if oldest.get::<_, Uuid>(0) != entry {
            return Err(Error::IntegrationState);
        }
        let current = target(&tx.tx, s, &name).await?;
        if current.epoch != cmd.epoch {
            return Err(Error::Fenced);
        }
        tx.tx.execute("INSERT INTO th_native_candidates(tenant,repository,candidate,entry,expected_head,expected_snapshot,expected_generation,epoch,policy,merge_profile,timestamp_seconds,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,floor(extract(epoch from clock_timestamp()))::bigint,'preparing',clock_timestamp()+interval '1 hour')", &[&s.tenant,&s.repository,&candidate,&entry,&bytes(current.head),&bytes(current.snapshot),&current.generation,&cmd.epoch,&bytes(current.policy),&treeharbor_merge::PROFILE]).await?;
        tx.tx.execute("UPDATE th_native_queue SET state='preparing',candidate=$4 WHERE tenant=$1 AND repository=$2 AND entry=$3", &[&s.tenant,&s.repository,&entry,&candidate]).await?;
        let result = queue_status(&tx.tx, s, entry).await?;
        tx.complete(
            result,
            vec![Event {
                kind: "native.candidate.prepare".into(),
                payload: json!({"candidate":candidate.to_string(),"epoch":cmd.epoch}),
            }],
        )
        .await
    }
}
