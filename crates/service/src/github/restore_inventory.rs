use super::publish::journal::Journal;
use super::*;
use serde::Serialize;
use treeharbor_metadata::github::{
    RESTORE_INVENTORY_PAGE_ITEMS, RestoreContext, RestoreInventoryItem, RestoreInventoryPage,
    RestoreInventorySpec, restore_digest,
};

#[derive(clap::Args)]
pub struct Options {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    credential_file: PathBuf,
    #[arg(long)]
    binding: Uuid,
    #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
    epoch: i64,
    #[arg(long)]
    manifest_sha256: String,
    /// Existing private directory; retain it for interrupted capture uploads.
    #[arg(long)]
    state: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    version: u8,
    key: String,
    specification: RestoreInventorySpec,
    pages: Vec<RestoreInventoryPage>,
}
async fn context(o: &Options, c: &Credential) -> Result<RestoreContext> {
    let value=command(&o.socket,c,json!({"action":"restore_context","epoch":o.epoch,"binding":o.binding,"manifest_sha256":o.manifest_sha256})).await?;
    let ctx: RestoreContext =
        serde_json::from_value(value).map_err(|_| Error::RestoreAssessment)?;
    if ctx.version != 2
        || ctx.tenant != c.scope.tenant()
        || ctx.repository != c.scope.repository()
        || ctx.epoch != o.epoch
        || ctx.binding != o.binding
        || ctx.manifest_sha256 != o.manifest_sha256
        || ctx.reconciliation_complete
    {
        return Err(Error::RestoreAssessment);
    }
    Ok(ctx)
}
fn capture(ctx: &RestoreContext, inv: &treeharbor_github::RestoreInventory) -> Result<Capture> {
    // Reuse the existing provider/scope assessment before normalization.
    super::restore::assess(ctx, ctx, inv)?;
    let revision = |oid: &str| format!("git:sha1:{oid}");
    let mut items = Vec::with_capacity(inv.heads.len() + inv.pulls.len());
    for h in &inv.heads {
        items.push(RestoreInventoryItem::Branch {
            node: h.id.clone(),
            name: h.name.clone(),
            git_revision: revision(&h.target.oid),
        });
    }
    for p in &inv.pulls {
        items.push(RestoreInventoryItem::Pull {
            node: p.id.clone(),
            number: p.number,
            state: p.state.clone(),
            head_ref: p.head_ref_name.clone(),
            base_ref: p.base_ref_name.clone(),
            head_repository: p.head_repository.as_ref().map(|r| r.database_id),
            head_revision: revision(&p.head_ref_oid),
            base_revision: revision(&p.base_ref_oid),
            merge_revision: p.merge_commit.as_ref().map(|c| revision(&c.oid)),
        });
    }
    let pages: Vec<_> = items
        .chunks(RESTORE_INVENTORY_PAGE_ITEMS)
        .enumerate()
        .map(|(index, items)| RestoreInventoryPage {
            version: 1,
            index,
            items: items.to_vec(),
        })
        .collect();
    let specification = RestoreInventorySpec {
        version: 1,
        binding: ctx.binding,
        manifest_sha256: ctx.manifest_sha256.clone(),
        context_sha256: restore_digest(ctx).map_err(|_| Error::RestoreAssessment)?,
        app_id: inv.app_id,
        installation_id: inv.binding.installation_id,
        repository_id: inv.binding.repository_id,
        repository_node: inv.repository_node_id.clone(),
        default_branch: inv.default_branch.clone(),
        started_at_unix: inv.started_at_unix,
        finished_at_unix: inv.finished_at_unix,
        head_count: inv.heads.len(),
        pull_count: inv.pulls.len(),
        page_sha256: pages
            .iter()
            .map(restore_digest)
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| Error::RestoreAssessment)?,
        token_revoked: inv.token_revoked,
        token_expires_at: inv.token_expires_at.clone(),
    };
    specification
        .validate()
        .map_err(|_| Error::RestoreAssessment)?;
    Ok(Capture {
        version: 1,
        key: format!("restore-inventory:{}", Uuid::new_v4()),
        specification,
        pages,
    })
}
fn identity(value: &Value, c: &Credential, epoch: i64) -> Result<()> {
    if value["version"] != 1
        || value["tenant"] != c.scope.tenant()
        || value["repository"] != json!(c.scope.repository())
        || value["epoch"] != epoch
        || value["publication_authorized"] != false
    {
        return Err(Error::RestoreAssessment);
    }
    Ok(())
}
pub(super) async fn run(o: Options) -> Result<bool> {
    let c = Credential::load(&o.credential_file)?;
    let intent = json!({"version":1,"kind":"restore_inventory","tenant":c.scope.tenant(),"repository":c.scope.repository(),
        "epoch":o.epoch,"binding":o.binding,"manifest_sha256":o.manifest_sha256});
    let journal = Journal::open_inventory(&o.state, &intent)?;
    // The whole capture is durable before the first metadata write. Never call
    // GitHub on retry of an uncertain write, even if the App file disappeared.
    let saved: Capture = match journal.read_optional("inventory.json")? {
        Some(v) => v,
        None => {
            let before = context(&o, &c).await?;
            let (app, config) = load(&o.config)?;
            matches_config(&config, &before.specification)?;
            let inv = Connection::new(app)?
                .observe_restore_inventory(&config.repository)
                .await?;
            if context(&o, &c).await? != before {
                return Err(Error::RestoreAssessment);
            }
            let capture = capture(&before, &inv)?;
            journal.bind("inventory.json", &json!(capture))?;
            capture
        }
    };
    let s = &saved.specification;
    s.validate().map_err(|_| Error::RestoreAssessment)?;
    if saved.version != 1
        || s.binding != o.binding
        || s.manifest_sha256 != o.manifest_sha256
        || saved.pages.len() != s.page_sha256.len()
    {
        return Err(Error::RestoreAssessment);
    }
    for (index, p) in saved.pages.iter().enumerate() {
        s.validate_page(p).map_err(|_| Error::RestoreAssessment)?;
        if p.index != index
            || restore_digest(p).map_err(|_| Error::RestoreAssessment)? != s.page_sha256[index]
        {
            return Err(Error::RestoreAssessment);
        }
    }
    let started=command(&o.socket,&c,json!({"action":"restore_inventory_start","key":saved.key,"epoch":o.epoch,"specification":s})).await?;
    identity(&started, &c, o.epoch)?;
    if started["key"] != saved.key || started["specification"] != json!(s) {
        return Err(Error::RestoreAssessment);
    }
    journal.bind("started.json", &started)?;
    for p in &saved.pages {
        let value=command(&o.socket,&c,json!({"action":"restore_inventory_page","epoch":o.epoch,"inventory_key":saved.key,"page":p})).await?;
        identity(&value, &c, o.epoch)?;
        if value["inventory_key"] != saved.key
            || value["page"] != json!(p)
            || value["page_sha256"] != s.page_sha256[p.index]
        {
            return Err(Error::RestoreAssessment);
        }
        // Server page keys are deterministic. Local acknowledgments are optional;
        // replay the same page on restart to recheck access and recover its receipt.
    }
    let sealed = command(
        &o.socket,
        &c,
        json!({"action":"restore_inventory_seal","epoch":o.epoch,"inventory_key":saved.key}),
    )
    .await?;
    identity(&sealed, &c, o.epoch)?;
    if sealed["inventory_key"] != saved.key
        || sealed["specification_sha256"]
            != restore_digest(s).map_err(|_| Error::RestoreAssessment)?
        || sealed["inventory_complete"] != true
        || sealed["reconciliation_complete"] != false
    {
        return Err(Error::RestoreAssessment);
    }
    journal.bind("sealed.json", &sealed)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&sealed).map_err(|_| Error::Http)?
    );
    Ok(s.token_revoked)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_keeps_non_treeharbor_refs_and_closed_fork_heads_across_pages() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../metadata/tests/fixtures/restore-context-v2.json"
        ))
        .unwrap();
        let ctx: RestoreContext = serde_json::from_value(fixture["context"].clone()).unwrap();
        let mut inventory = treeharbor_github::RestoreInventory {
            version: 1,
            binding: serde_json::from_value(json!(ctx.specification.upstream)).unwrap(),
            app_id: ctx.specification.app_id,
            repository_node_id: "R_fixture".into(),
            default_branch: "main".into(),
            started_at_unix: 10,
            finished_at_unix: 11,
            heads: vec![],
            pulls: vec![],
            token_revoked: true,
            token_expires_at: "later".into(),
            reconciliation_complete: false,
        };
        for i in 0..17 {
            inventory.heads.push(serde_json::from_value(json!({"id":format!("REF_{i}"),"name":format!("ordinary/{i}"),"prefix":"refs/heads/","target":{"__typename":"Commit","oid":"11".repeat(20)}})).unwrap());
        }
        for (index, origin) in [Some(11), Some(99), None].iter().enumerate() {
            inventory.pulls.push(serde_json::from_value(json!({"id":format!("PR_{index}"),"number":index+1,"state":"CLOSED","merged":false,"isDraft":false,
                "headRefName":"deleted-branch","headRefOid":"22".repeat(20),"baseRefName":"main","baseRefOid":"11".repeat(20),
                "headRepository":origin.map(|id|json!({"databaseId":id})),"baseRepository":{"databaseId":11},"mergeCommit":null})).unwrap());
        }
        let c = capture(&ctx, &inventory).unwrap();
        assert_eq!(c.pages.len(), 2);
        assert_eq!(c.pages[0].items.len(), 16);
        assert_eq!(c.pages[1].items.len(), 4);
        assert_eq!(c.specification.head_count, 17);
        assert_eq!(c.specification.pull_count, 3);
        for p in &c.pages {
            c.specification.validate_page(p).unwrap();
        }
        for (index, expected) in [Some(11), Some(99), None].iter().enumerate() {
            let RestoreInventoryItem::Pull {
                head_repository,
                head_revision,
                ..
            } = &c.pages[1].items[index + 1]
            else {
                panic!()
            };
            assert_eq!(head_repository, expected);
            assert_eq!(head_revision, &format!("git:sha1:{}", "22".repeat(20)));
        }
        inventory.app_id += 1;
        assert!(capture(&ctx, &inventory).is_err());
    }
}
