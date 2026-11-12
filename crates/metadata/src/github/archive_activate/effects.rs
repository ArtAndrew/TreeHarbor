//! Build the exact native rows from the verified, pinned source graphs.
use super::*;
use serde::Serialize;
use std::collections::{BTreeMap, btree_map::Entry};
use treeharbor_core::RevisionRef;

#[derive(PartialEq, Eq)]
struct Pin {
    object: ObjectId,
    generation: i64,
    kind: i16,
}

#[derive(Serialize)]
struct Base {
    revision: String,
    snapshot: String,
    pin: String,
}

#[derive(Serialize)]
struct Ref {
    name: String,
    head: String,
    pin: String,
    old_head: Option<String>,
    generation: i64,
}

#[derive(Serialize)]
struct Proposal {
    changeset: Uuid,
    number: i64,
    origin: Value,
    base_revision: String,
    base_snapshot: String,
    operations: String,
    result_snapshot: String,
    base_pin: String,
    operations_pin: String,
    result_pin: String,
}

#[derive(Default)]
struct NativeRows {
    pins: BTreeMap<String, Pin>,
    bases: BTreeMap<ObjectId, Base>,
    refs: Vec<Ref>,
    proposals: Vec<Proposal>,
    unresolved: Vec<Value>,
}

impl NativeRows {
    fn pin(
        &mut self,
        holder: &str,
        object: ObjectId,
        kind: i16,
        generations: &BTreeMap<ObjectId, i64>,
    ) -> Result<()> {
        let generation = *generations.get(&object).ok_or(Error::UploadMembership)?;
        let pin = Pin {
            object,
            generation,
            kind,
        };
        match self.pins.entry(holder.to_owned()) {
            Entry::Vacant(slot) => {
                slot.insert(pin);
            }
            Entry::Occupied(slot) if slot.get() != &pin => return Err(Error::PinConflict),
            Entry::Occupied(_) => {}
        }
        Ok(())
    }

    fn base(
        &mut self,
        tip: &ArchiveBranchTip,
        generations: &BTreeMap<ObjectId, i64>,
    ) -> Result<(ObjectId, ObjectId)> {
        let RevisionRef::Native(revision) = tip.native_revision.parse()? else {
            return Err(Error::ArchiveArtifactMismatch);
        };
        let snapshot: ObjectId = tip.snapshot.parse()?;
        self.native_base(revision, snapshot, generations)?;
        Ok((revision, snapshot))
    }

    fn native_base(
        &mut self,
        revision: ObjectId,
        snapshot: ObjectId,
        generations: &BTreeMap<ObjectId, i64>,
    ) -> Result<()> {
        let pin = format!("base:{}", revision.hex());
        self.pin(&pin, revision, 5, generations)?;
        match self.bases.entry(revision) {
            Entry::Vacant(slot) => {
                slot.insert(Base {
                    revision: RevisionRef::Native(revision).to_string(),
                    snapshot: snapshot.hex(),
                    pin,
                });
            }
            Entry::Occupied(slot) if slot.get().snapshot != snapshot.hex() => {
                return Err(Error::ArchiveArtifactMismatch);
            }
            Entry::Occupied(_) => {}
        }
        Ok(())
    }
}

fn native_rows(
    spec: &ArchiveActivateSpec,
    proof: &VerifiedArchiveActivation,
    epoch: i64,
    new_commits: &BTreeMap<ObjectId, i64>,
) -> Result<NativeRows> {
    let mut rows = NativeRows::default();
    let branches = &proof.branches;
    let previous = proof.reentry.as_ref().map(|entry| {
        entry
            .history
            .freeze()
            .snapshot
            .refs
            .iter()
            .map(|tip| (tip.name.as_str(), tip))
            .collect::<BTreeMap<_, _>>()
    });
    for (reference, tip) in &branches.receipt.specification.branches {
        let name = reference
            .strip_prefix("refs/heads/")
            .ok_or(Error::ArchiveArtifactMismatch)?;
        crate::integration::ref_name(name)?;
        let (head, generations) = if let Some(reentry) = &proof.reentry {
            let tip = reentry
                .graph
                .tips
                .get(reference)
                .ok_or(Error::ArchiveArtifactMismatch)?;
            let generations = [
                &branches.generations,
                reentry.history.generations(),
                new_commits,
            ]
            .into_iter()
            .find(|objects| objects.contains_key(&tip.head))
            .ok_or(Error::UploadMembership)?;
            rows.native_base(tip.head, tip.snapshot, generations)?;
            (tip.head, generations)
        } else {
            (
                rows.base(tip, &branches.generations)?.0,
                &branches.generations,
            )
        };
        let prior = previous.as_ref().and_then(|entries| entries.get(name));
        let hash = ObjectId::hash_frame(name.as_bytes()).hex();
        let (pin, old_head, generation) = match prior {
            Some(prior) => {
                let RevisionRef::Native(old) = prior.head.parse()? else {
                    return Err(Error::ArchiveArtifactMismatch);
                };
                let generation = prior.generation.checked_add(1).ok_or(Error::Fenced)?;
                (
                    format!("native-archive:{epoch}:{hash}"),
                    Some(old.hex()),
                    generation,
                )
            }
            None => (format!("native-ref:{hash}"), None, 1),
        };
        rows.pin(&pin, head, 5, generations)?;
        rows.refs.push(Ref {
            name: name.into(),
            head: head.hex(),
            pin,
            old_head,
            generation,
        });
    }

    let objects = &proof.proposals.stage.objects.generations;
    for item in &proof.proposals.proposals.proposals {
        rows.base(&item.base_tip, objects)?;
        let (_, head_snapshot) = rows.base(&item.head, objects)?;
        let number =
            i64::try_from(item.source.number).map_err(|_| Error::ArchiveArtifactMismatch)?;
        let (merge_base, operations) = match &item.patch {
            ArchiveProposalPatch::Ready {
                merge_base,
                operations,
            } => (merge_base, operations),
            ArchiveProposalPatch::NeedsResolution {
                reason,
                merge_bases,
            } => {
                for base in merge_bases {
                    rows.base(base, objects)?;
                }
                rows.unresolved
                    .push(json!({"number":number,"reason":reason}));
                continue;
            }
        };
        let (base, base_snapshot) = rows.base(merge_base, objects)?;
        if !item.target_present {
            rows.unresolved
                .push(json!({"number":number,"reason":"missing_target"}));
            continue;
        }
        let operations: ObjectId = operations.parse()?;
        let changeset = Uuid::new_v4();
        let base_pin = format!("change:{changeset}:1:base");
        let operations_pin = format!("change:{changeset}:1:operations");
        let result_pin = format!("change:{changeset}:1:result");
        rows.pin(&base_pin, base, 5, objects)?;
        rows.pin(&operations_pin, operations, 4, objects)?;
        rows.pin(&result_pin, head_snapshot, 3, objects)?;
        rows.proposals.push(Proposal {
            changeset, number,
            origin: json!({"version":1,"stage_key":spec.proposal_stage,"discussion":item.discussion,
                "source":item.source,"base_tip":item.base_tip,"head":item.head,"merge_base":merge_base}),
            base_revision: merge_base.native_revision.clone(), base_snapshot: base_snapshot.hex(),
            operations: operations.hex(), result_snapshot: head_snapshot.hex(), base_pin, operations_pin, result_pin,
        });
    }
    if rows.proposals.len() != proof.proposals.proposals.ready || rows.refs.is_empty() {
        return Err(Error::ArchiveArtifactMismatch);
    }
    Ok(rows)
}

async fn pin(
    tx: &Transaction<'_>,
    scope: &Scope,
    backend: &str,
    pins: &BTreeMap<String, Pin>,
) -> Result<()> {
    let holders: Vec<_> = pins.keys().collect();
    let digests: Vec<_> = pins
        .values()
        .map(|pin| pin.object.as_bytes().to_vec())
        .collect();
    let generations: Vec<_> = pins.values().map(|pin| pin.generation).collect();
    let kinds: Vec<_> = pins.values().map(|pin| pin.kind).collect();
    let locked = tx.query(r#"
        SELECT o.digest FROM unnest($2::text[],$3::bytea[],$4::bigint[],$5::smallint[]) AS r(holder,digest,generation,kind)
        JOIN th_objects o ON o.tenant=$1 AND (o.digest,o.generation,o.kind)=(r.digest,r.generation,r.kind)
        WHERE o.backend=$6 AND o.state='live'
        ORDER BY o.digest,r.holder FOR UPDATE OF o
        "#, &[&scope.tenant(), &holders, &digests, &generations, &kinds, &backend]).await?;
    if locked.len() != pins.len() {
        return Err(Error::UploadMembership);
    }
    let params: &[&(dyn tokio_postgres::types::ToSql + Sync)] = &[
        &scope.tenant(),
        &scope.repository(),
        &holders,
        &digests,
        &generations,
    ];
    tx.execute(
        r#"
        INSERT INTO th_object_pins(tenant,repository,holder,digest,generation)
        SELECT $1,$2,holder,digest,generation
        FROM unnest($3::text[],$4::bytea[],$5::bigint[]) AS r(holder,digest,generation)
        ON CONFLICT DO NOTHING
        "#,
        params,
    )
    .await?;
    let count: i64 = tx.query_one(r#"
        SELECT count(*) FROM unnest($3::text[],$4::bytea[],$5::bigint[]) AS r(holder,digest,generation)
        JOIN th_object_pins p ON p.tenant=$1 AND p.repository=$2
            AND (p.holder,p.digest,p.generation)=(r.holder,r.digest,r.generation)
        "#, params).await?.get(0);
    if count != pins.len() as i64 {
        return Err(Error::PinConflict);
    }
    tx.execute(
        "UPDATE th_objects SET orphaned_at=NULL WHERE tenant=$1 AND digest=ANY($2::bytea[])",
        &[&scope.tenant(), &digests],
    )
    .await?;
    Ok(())
}

async fn insert_bases(
    tx: &Transaction<'_>,
    scope: &Scope,
    bases: &BTreeMap<ObjectId, Base>,
) -> Result<()> {
    let rows = json!(bases.values().collect::<Vec<_>>());
    let args: &[&(dyn tokio_postgres::types::ToSql + Sync)] =
        &[&scope.tenant(), &scope.repository(), &rows];
    tx.execute(
        r#"
        INSERT INTO th_bases(tenant,repository,revision,snapshot,pin_holder)
        SELECT $1,$2,revision,decode(snapshot,'hex'),pin
        FROM jsonb_to_recordset($3::jsonb) AS b(revision text,snapshot text,pin text)
        ON CONFLICT DO NOTHING
        "#,
        args,
    )
    .await?;
    let count: i64 = tx.query_one(r#"
        SELECT count(*) FROM jsonb_to_recordset($3::jsonb) AS b(revision text,snapshot text,pin text)
        JOIN th_bases x ON x.tenant=$1 AND x.repository=$2 AND x.revision=b.revision
            AND x.snapshot=decode(b.snapshot,'hex') AND x.pin_holder=b.pin
        "#, args).await?.get(0);
    if count != bases.len() as i64 {
        return Err(Error::ArchiveArtifactMismatch);
    }
    Ok(())
}

async fn insert_proposals(
    tx: &Transaction<'_>,
    auth: &Authorization,
    command: &Command,
    stage: &str,
    proposals: &[Proposal],
) -> Result<()> {
    let scope = auth.scope();
    let rows = json!(proposals);
    tx.execute(
        r#"
        INSERT INTO th_changesets(tenant,repository,changeset,principal,latest_version)
        SELECT $1,$2,changeset,$4,1 FROM jsonb_to_recordset($3::jsonb) AS p(changeset uuid)
        "#,
        &[
            &scope.tenant(),
            &scope.repository(),
            &rows,
            &auth.principal(),
        ],
    )
    .await?;
    tx.execute(r#"
        INSERT INTO th_archive_pull_origins(tenant,repository,changeset,stage_key,pull_number,origin)
        SELECT $1,$2,changeset,$4,number,origin
        FROM jsonb_to_recordset($3::jsonb) AS p(changeset uuid,number bigint,origin jsonb)
        "#, &[&scope.tenant(), &scope.repository(), &rows, &stage]).await?;
    tx.execute(r#"
        INSERT INTO th_change_versions(tenant,repository,changeset,version,archived_pull,base_revision,
            base_snapshot,operations,result_snapshot,base_pin,operations_pin,result_pin,operation_key)
        SELECT $1,$2,changeset,1,changeset,base_revision,decode(base_snapshot,'hex'),
            decode(operations,'hex'),decode(result_snapshot,'hex'),base_pin,operations_pin,result_pin,$4
        FROM jsonb_to_recordset($3::jsonb) AS p(changeset uuid,base_revision text,base_snapshot text,
            operations text,result_snapshot text,base_pin text,operations_pin text,result_pin text)
        "#, &[&scope.tenant(), &scope.repository(), &rows, &command.key]).await?;
    Ok(())
}

pub(super) async fn install(
    tx: &Transaction<'_>,
    auth: &Authorization,
    command: &Command,
    spec: &ArchiveActivateSpec,
    proof: &VerifiedArchiveActivation,
    new_commits: &BTreeMap<ObjectId, i64>,
) -> Result<Value> {
    let scope = auth.scope();
    let epoch = command.epoch.checked_add(1).ok_or(Error::Fenced)?;
    let rows = native_rows(spec, proof, epoch, new_commits)?;
    let policy = ObjectId::from_bytes(spec.policy.digest()?);
    let definition = spec.policy.canonical()?;
    pin(
        tx,
        scope,
        &proof.proposals.stage.objects.backend,
        &rows.pins,
    )
    .await?;
    insert_bases(tx, scope, &rows.bases).await?;
    let policy_bytes = policy.as_bytes().as_slice();
    tx.execute(
        r#"
        INSERT INTO th_native_policies(tenant,repository,digest,definition,principal)
        VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING
        "#,
        &[
            &scope.tenant(),
            &scope.repository(),
            &policy_bytes,
            &definition,
            &auth.principal(),
        ],
    )
    .await?;
    let registered: Vec<u8> = tx
        .query_one(
            r#"
        SELECT definition FROM th_native_policies WHERE tenant=$1 AND repository=$2 AND digest=$3
        "#,
            &[&scope.tenant(), &scope.repository(), &policy_bytes],
        )
        .await?
        .get(0);
    if registered != definition {
        return Err(Error::ArchiveArtifactMismatch);
    }
    let refs = json!(rows.refs);
    let installed = tx
        .execute(
            r#"
        INSERT INTO th_native_refs(tenant,repository,name,head,generation,epoch,policy,pin_holder)
        SELECT $1,$2,name,decode(head,'hex'),generation,$4,$5,pin
        FROM jsonb_to_recordset($3::jsonb) AS r(name text,head text,pin text,generation bigint)
        ON CONFLICT (tenant,repository,name) DO UPDATE
        SET head=EXCLUDED.head,generation=EXCLUDED.generation,epoch=EXCLUDED.epoch,
            policy=EXCLUDED.policy,pin_holder=EXCLUDED.pin_holder
        WHERE th_native_refs.generation=EXCLUDED.generation-1
        "#,
            &[
                &scope.tenant(),
                &scope.repository(),
                &refs,
                &epoch,
                &policy_bytes,
            ],
        )
        .await?;
    if installed != rows.refs.len() as u64 {
        return Err(Error::NativeHandoffMismatch);
    }
    tx.execute(r#"
        INSERT INTO th_native_ref_updates(tenant,repository,target,generation,old_head,new_head,policy,epoch,principal,operation_key,kind)
        SELECT r.tenant,r.repository,r.name,r.generation,decode(a.old_head,'hex'),r.head,r.policy,r.epoch,$5,$6,
            CASE WHEN a.old_head IS NULL THEN 'create' ELSE 'archive' END
        FROM jsonb_to_recordset($4::jsonb) AS a(name text,old_head text)
        JOIN th_native_refs r ON r.tenant=$1 AND r.repository=$2 AND r.epoch=$3 AND r.name=a.name
        "#, &[&scope.tenant(), &scope.repository(), &epoch, &refs, &auth.principal(), &command.key]).await?;
    insert_proposals(tx, auth, command, &spec.proposal_stage, &rows.proposals).await?;
    Ok(json!({
        "version":1,"state":"activated","authority":"standalone","previous_epoch":command.epoch,"epoch":epoch,
        "key":command.key,"tenant":scope.tenant(),"repository":scope.repository(),
        "binding":proof.fence.request.binding,"proposal_stage":spec.proposal_stage,"observation_key":spec.observation_key,
        "archive_manifest_sha256":proof.proposals.stage.archive.fingerprint(),
        "default_branch":proof.fence.observation.default_branch,"branches":rows.refs.len(),"policy":policy.to_string(),
        "proposals":rows.proposals.iter().map(|p| json!({"number":p.number,"changeset":p.changeset,"version":1})).collect::<Vec<_>>(),
        "unresolved":rows.unresolved,
    }))
}
