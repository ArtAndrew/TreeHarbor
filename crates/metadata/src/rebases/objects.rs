use super::*;
use crate::{
    catalog::{lease_pin_in, reserve_in},
    reads::{ReadBatch, ReadNode, ReadSession},
};
use sha2::{Digest, Sha256};
use treeharbor_storage::{ManagedStore, PreparedObject};

pub struct RebaseRead {
    auth: Authorization,
    rebase: Uuid,
    epoch: i64,
    input: CandidateInput,
    sessions: Vec<ReadSession>,
}
impl RebaseRead {
    pub fn input(&self) -> &CandidateInput {
        &self.input
    }
    pub fn roots(&self) -> Vec<ReadNode> {
        self.sessions.iter().map(ReadSession::root).collect()
    }
}
fn object_token(s: &Scope, rebase: Uuid, id: ObjectId) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"treeharbor-rebase-object\0v1\0");
    hash.update((s.tenant.len() as u64).to_be_bytes());
    hash.update(s.tenant.as_bytes());
    hash.update(s.repository.as_bytes());
    hash.update(rebase.as_bytes());
    hash.update(id.as_bytes());
    Uuid::from_bytes(hash.finalize()[..16].try_into().expect("digest length"))
}
impl Metadata {
    pub async fn open_change_rebase(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
    ) -> Result<RebaseRead> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let r = preparing(&tx, auth, id, epoch).await?;
        let s = auth.scope();
        tx.query("SELECT o.digest FROM th_objects o JOIN th_object_pins p ON p.tenant=o.tenant AND p.digest=o.digest WHERE p.tenant=$1 AND p.repository=$2 AND p.holder=ANY($3::text[]) ORDER BY o.digest FOR UPDATE OF o",&[&s.tenant,&s.repository,&r.source_pins]).await?;
        let mut sessions = Vec::new();
        for pin in &r.source_pins {
            sessions.push(ReadSession::from_lease(
                lease_pin_in(&tx, s, pin, Uuid::new_v4(), 300).await?,
            ));
        }
        preparing(&tx, auth, id, epoch).await?;
        tx.commit().await?;
        Ok(RebaseRead {
            auth: auth.clone(),
            rebase: id,
            epoch,
            input: r.input,
            sessions,
        })
    }
    pub async fn read_change_rebase(
        &mut self,
        read: &RebaseRead,
        source: usize,
        store: &ManagedStore,
        nodes: &[ReadNode],
        budget: u64,
    ) -> Result<ReadBatch> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let r = preparing(&tx, &read.auth, read.rebase, read.epoch).await?;
        if r.input != read.input {
            return Err(Error::ReceiptMismatch);
        }
        tx.commit().await?;
        self.open_read_batch(
            read.sessions.get(source).ok_or(Error::ReadScope)?,
            store,
            nodes,
            budget,
        )
        .await
    }
    pub async fn close_change_rebase_read(&mut self, read: RebaseRead) -> Result<()> {
        let mut error = None;
        for session in read.sessions {
            if let Err(e) = self.close_read_session(session).await {
                error.get_or_insert(e);
            }
        }
        error.map_or(Ok(()), Err)
    }
    pub async fn reserve_change_rebase_object(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        rebase: Uuid,
        store: &ManagedStore,
        object: &PreparedObject,
    ) -> Result<ObjectLease> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        preparing(&tx, auth, rebase, epoch).await?;
        let s = auth.scope();
        if store.tenant() != s.tenant {
            return Err(Error::Forbidden);
        }
        let digest = object.id();
        let token = object_token(s, rebase, digest);
        let old=tx.query_opt("SELECT generation,hold_token,length,backend FROM th_change_rebase_objects WHERE tenant=$1 AND repository=$2 AND rebase=$3 AND digest=$4",&[&s.tenant,&s.repository,&rebase,&bytes(digest)]).await?;
        if old.is_none() {
            let size=tx.query_one("SELECT count(*)::bigint,coalesce(sum(length),0)::bigint FROM th_change_rebase_objects WHERE tenant=$1 AND repository=$2 AND rebase=$3",&[&s.tenant,&s.repository,&rebase]).await?;
            if size.get::<_, i64>(0) >= 4096
                || size.get::<_, i64>(1).saturating_add(object.length() as i64) > 128 * 1024 * 1024
            {
                return Err(Error::UploadLimit);
            }
        }
        let lease = reserve_in(&tx, store, object, token, 300).await?;
        if let Some(old) = old {
            if old.get::<_, i64>(0) != lease.key().generation()
                || old.get::<_, Uuid>(1) != token
                || old.get::<_, i64>(2) != object.length() as i64
                || old.get::<_, String>(3) != store.backend()
            {
                return Err(Error::ReceiptMismatch);
            }
        } else {
            tx.execute("INSERT INTO th_change_rebase_objects(tenant,repository,rebase,digest,generation,hold_token,length,backend) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",&[&s.tenant,&s.repository,&rebase,&bytes(digest),&lease.key().generation(),&token,&(object.length() as i64),&store.backend()]).await?;
        }
        tx.execute("UPDATE th_object_holds h SET expires_at=r.expires_at FROM th_change_rebases r WHERE r.tenant=$1 AND r.repository=$2 AND r.rebase=$3 AND h.tenant=r.tenant AND h.token=$4",&[&s.tenant,&s.repository,&rebase,&token]).await?;
        preparing(&tx, auth, rebase, epoch).await?;
        tx.commit().await?;
        Ok(lease)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservation_matches_independent_vector_and_scope() {
        let v: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/change-rebase-v1.json"))
                .unwrap();
        let scope = Scope::new(
            v["tenant"].as_str().unwrap(),
            v["repository"].as_str().unwrap().parse().unwrap(),
        )
        .unwrap();
        let rebase = v["rebase"].as_str().unwrap().parse().unwrap();
        let object = v["object"].as_str().unwrap().parse().unwrap();
        let token = object_token(&scope, rebase, object);
        assert_eq!(token.to_string(), v["hold_token"]);
        assert_ne!(
            token,
            object_token(
                &Scope::new("other", scope.repository()).unwrap(),
                rebase,
                object
            )
        );
        assert_ne!(
            token,
            object_token(
                &Scope::new(scope.tenant(), Uuid::nil()).unwrap(),
                rebase,
                object
            )
        );
        assert_ne!(token, object_token(&scope, Uuid::nil(), object));
        assert_ne!(
            token,
            object_token(&scope, rebase, ObjectId::from_bytes([0; 32]))
        );
        assert_eq!(REBASE_PROFILE, v["profile"]);
    }
}
