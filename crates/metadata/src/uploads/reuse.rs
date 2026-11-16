//! Enrollment from a retained repository base. An arbitrary object digest
//! never authorizes enrollment, even if its bytes are available in the tenant.
use super::{Purpose, active_for, lock_authorized};
use crate::{Error, Metadata, Result, catalog::settings};
use treeharbor_core::{ObjectId, RevisionRef};
use uuid::Uuid;

const MAX_REUSE_OBJECTS: i64 = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReusedSourceBase {
    pub objects: i64,
    pub bytes: i64,
}

impl Metadata {
    /// Enroll the exact live generations reachable from an accepted base in
    /// this repository. The caller may omit the corresponding source frames.
    /// Repeating this operation preserves the session budget and deadline.
    pub async fn reuse_source_base(
        &mut self,
        auth: &crate::access::Authorization,
        upload: Uuid,
        base: ObjectId,
    ) -> Result<ReusedSourceBase> {
        let purpose = Purpose::Human;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        lock_authorized(&tx, auth, purpose).await?;
        let session = active_for(&tx, auth, upload, purpose).await?;
        let scope = auth.scope();
        auth.check_additional_read(&tx).await?;
        let tenant = scope.tenant();
        let repository = scope.repository();
        let backend: &str = session.get(0);
        let object_limit: i64 = session.get(3);
        let byte_limit: i64 = session.get(4);
        let reserved_objects: i64 = session.get(5);
        let reserved_bytes: i64 = session.get(6);
        let base_revision = RevisionRef::Native(base).to_string();
        let base_digest = base.as_bytes().as_slice();
        // The repository lock protects the base pin; its live catalogue edges
        // protect every reached physical generation against retirement.
        let bound = object_limit.min(MAX_REUSE_OBJECTS);
        let rows = tx
            .query(
                r#"
            WITH RECURSIVE reached(digest,generation) AS (
                SELECT p.digest,p.generation FROM th_bases b
                JOIN th_object_pins p
                  ON (p.tenant,p.repository,p.holder)=(b.tenant,b.repository,b.pin_holder)
                WHERE b.tenant=$1 AND b.repository=$2 AND b.revision=$3 AND p.digest=$4
                UNION
                SELECT e.child,e.child_generation FROM reached r
                JOIN th_object_edges e ON e.tenant=$1
                  AND (e.parent,e.parent_generation)=(r.digest,r.generation)
            )
            SELECT o.digest,o.generation,o.kind,o.length,o.backend,o.state,
              CASE WHEN o.kind=5 THEN (
                SELECT s.digest FROM th_object_edges e JOIN th_objects s
                  ON (s.tenant,s.digest,s.generation)=(e.tenant,e.child,e.child_generation)
                WHERE (e.tenant,e.parent,e.parent_generation)=(o.tenant,o.digest,o.generation)
                  AND s.kind=3
              ) END AS snapshot,
              i.generation AS enrolled_generation,i.complete AS enrolled_complete
            FROM reached r JOIN th_objects o
              ON (o.tenant,o.digest,o.generation)=($1,r.digest,r.generation)
            LEFT JOIN th_source_upload_items i
              ON (i.tenant,i.repository,i.upload,i.digest)=($1,$2,$5,r.digest)
            LIMIT $6
            "#,
                &[
                    &tenant,
                    &repository,
                    &base_revision,
                    &base_digest,
                    &upload,
                    &(bound + 1),
                ],
            )
            .await?;
        let objects = rows.len() as i64;
        if objects == 0 {
            return Err(Error::NotFound);
        }
        if objects > bound {
            return Err(Error::UploadLimit);
        }
        let mut bytes = 0_i64;
        let mut added_bytes = 0_i64;
        let mut digests = Vec::<Vec<u8>>::new();
        let mut generations = Vec::<i64>::new();
        let mut kinds = Vec::<i16>::new();
        let mut lengths = Vec::<i64>::new();
        let mut snapshots = Vec::<Option<Vec<u8>>>::new();
        for row in rows {
            if row.get::<_, &str>("state") != "live" || row.get::<_, &str>("backend") != backend {
                return Err(Error::ObjectNotLive);
            }
            let generation = row.get("generation");
            let kind = row.get("kind");
            let length: i64 = row.get("length");
            let snapshot: Option<Vec<u8>> = row.get("snapshot");
            if kind == 5 && snapshot.is_none() {
                return Err(Error::ReceiptMismatch);
            }
            bytes += length;
            if let Some(enrolled) = row.get::<_, Option<i64>>("enrolled_generation") {
                if enrolled != generation
                    || row.get::<_, Option<bool>>("enrolled_complete") != Some(true)
                {
                    return Err(Error::UploadMembership);
                }
                continue;
            }
            added_bytes += length;
            digests.push(row.get("digest"));
            generations.push(generation);
            kinds.push(kind);
            lengths.push(length);
            snapshots.push(snapshot);
        }
        let added_objects = digests.len() as i64;
        if added_objects > object_limit - reserved_objects
            || added_bytes > byte_limit - reserved_bytes
        {
            return Err(Error::UploadLimit);
        }
        if added_objects != 0 {
            let enrolled = tx.execute(r#"
                WITH new_items AS MATERIALIZED (
                    SELECT *,gen_random_uuid() AS token
                    FROM unnest($4::bytea[],$5::bigint[],$6::smallint[],$7::bigint[],$8::bytea[])
                        AS items(digest,generation,kind,length,snapshot)
                ), holds AS (
                    INSERT INTO th_object_holds(tenant,token,digest,generation,expires_at)
                    SELECT $1,n.token,n.digest,n.generation,u.expires_at FROM new_items n
                    JOIN th_source_uploads u ON (u.tenant,u.repository,u.upload)=($1,$2,$3)
                    RETURNING token
                )
                INSERT INTO th_source_upload_items
                  (tenant,repository,upload,digest,generation,hold_token,kind,length,snapshot,complete)
                SELECT $1,$2,$3,n.digest,n.generation,n.token,n.kind,n.length,n.snapshot,true
                  FROM new_items n JOIN holds h USING(token)
                "#, &[&tenant,&repository,&upload,&digests,&generations,&kinds,&lengths,&snapshots]).await?;
            if enrolled != added_objects as u64 {
                return Err(Error::ReceiptMismatch);
            }
            tx.execute(r#"
                UPDATE th_source_uploads SET reserved_objects=reserved_objects+$4,reserved_bytes=reserved_bytes+$5
                WHERE (tenant,repository,upload)=($1,$2,$3)
                "#, &[&tenant,&repository,&upload,&added_objects,&added_bytes]).await?;
        }
        active_for(&tx, auth, upload, purpose).await?;
        auth.check_transaction(&tx, scope, auth.principal(), purpose.permission())
            .await?;
        tx.commit().await?;
        Ok(ReusedSourceBase { objects, bytes })
    }
}
