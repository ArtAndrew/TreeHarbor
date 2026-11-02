//! Paged, immutable discovery. Completeness means this capture, never all host history.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
#[path = "restore_inventory_tests.rs"]
mod tests;

const START: &str = "github.restore.inventory.start.v1";
const PAGE: &str = "github.restore.inventory.page.v1";
const SEAL: &str = "github.restore.inventory.seal.v1";
pub const RESTORE_INVENTORY_PAGE_ITEMS: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RestoreInventoryItem {
    Branch {
        node: String,
        name: String,
        git_revision: String,
    },
    Pull {
        node: String,
        number: u64,
        state: String,
        head_ref: String,
        base_ref: String,
        head_repository: Option<u64>,
        head_revision: String,
        base_revision: String,
        merge_revision: Option<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreInventoryPage {
    pub version: u8,
    pub index: usize,
    pub items: Vec<RestoreInventoryItem>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreInventorySpec {
    pub version: u8,
    pub binding: Uuid,
    pub manifest_sha256: String,
    pub context_sha256: String,
    pub app_id: u64,
    pub installation_id: u64,
    pub repository_id: u64,
    pub repository_node: String,
    pub default_branch: String,
    pub started_at_unix: u64,
    pub finished_at_unix: u64,
    pub head_count: usize,
    pub pull_count: usize,
    pub page_sha256: Vec<String>,
    pub token_revoked: bool,
    pub token_expires_at: String,
}
fn text(v: &str, max: usize) -> Result<()> {
    if v.is_empty() || v.len() > max || v.chars().any(char::is_control) {
        return Err(Error::Invalid("restore inventory text"));
    }
    Ok(())
}
fn git(v: &str) -> Result<()> {
    use treeharbor_core::{GitOid, RevisionRef};
    match v.parse::<RevisionRef>() {
        Ok(RevisionRef::Git(id @ GitOid::Sha1(bytes)))
            if bytes != [0; 20] && RevisionRef::Git(id).to_string() == v =>
        {
            Ok(())
        }
        _ => Err(Error::Invalid("restore inventory revision")),
    }
}
impl RestoreInventoryItem {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Branch {
                node,
                name,
                git_revision,
            } => {
                text(node, 256)?;
                text(name, 1024)?;
                git(git_revision)
            }
            Self::Pull {
                node,
                number,
                state,
                head_ref,
                base_ref,
                head_repository,
                head_revision,
                base_revision,
                merge_revision,
            } => {
                text(node, 256)?;
                text(head_ref, 1024)?;
                text(base_ref, 1024)?;
                if *number == 0
                    || *number > i32::MAX as u64
                    || !matches!(state.as_str(), "OPEN" | "CLOSED" | "MERGED")
                    || head_repository.is_some_and(|n| n == 0 || n > i64::MAX as u64)
                {
                    return Err(Error::Invalid("restore inventory pull"));
                }
                git(head_revision)?;
                git(base_revision)?;
                if let Some(v) = merge_revision {
                    git(v)?;
                }
                Ok(())
            }
        }
    }
}
impl RestoreInventorySpec {
    pub fn validate(&self) -> Result<()> {
        uuid(self.binding)?;
        restore::digest_valid(&self.manifest_sha256)?;
        restore::digest_valid(&self.context_sha256)?;
        text(&self.repository_node, 256)?;
        text(&self.default_branch, 1024)?;
        text(&self.token_expires_at, 64)?;
        if self.version != 1
            || self.head_count > 5000
            || self.pull_count > 5000
            || self.page_sha256.len()
                != (self.head_count + self.pull_count).div_ceil(RESTORE_INVENTORY_PAGE_ITEMS)
            || [self.app_id, self.installation_id, self.repository_id]
                .iter()
                .any(|n| *n == 0 || *n > i64::MAX as u64)
            || self.started_at_unix == 0
            || self.finished_at_unix < self.started_at_unix
            || self.finished_at_unix - self.started_at_unix > 120
            || self.finished_at_unix > i64::MAX as u64
        {
            return Err(Error::Invalid("restore inventory manifest"));
        }
        for hash in &self.page_sha256 {
            restore::digest_valid(hash)?;
        }
        Ok(())
    }
    pub fn validate_page(&self, p: &RestoreInventoryPage) -> Result<()> {
        let expected = self
            .page_sha256
            .get(p.index)
            .ok_or(Error::Invalid("restore inventory page index"))?;
        if p.version != 1
            || p.items.len()
                != (self.head_count + self.pull_count - p.index * RESTORE_INVENTORY_PAGE_ITEMS)
                    .min(RESTORE_INVENTORY_PAGE_ITEMS)
            || serde_json::to_vec(p)
                .map_err(|_| Error::SchemaMismatch)?
                .len()
                > 48 * 1024
            || restore_digest(p)? != *expected
        {
            return Err(Error::Invalid("restore inventory page"));
        }
        for i in &p.items {
            i.validate()?;
        }
        Ok(())
    }
}
fn page_key(key: &str, index: usize) -> Result<String> {
    Ok(format!(
        "restore-inventory-page:{}:{index}",
        restore_digest(&json!([1, key]))?
    ))
}
fn seal_key(key: &str) -> Result<String> {
    Ok(format!(
        "restore-inventory-seal:{}",
        restore_digest(&json!([1, key]))?
    ))
}
async fn inventory(
    tx: &Transaction<'_>,
    s: &Scope,
    key: &str,
    epoch: i64,
) -> Result<RestoreInventorySpec> {
    crate::identifier(key, 128)?;
    let receipt:Value=tx.query_opt("SELECT result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind=$4", &[&s.tenant(),&s.repository(),&key,&START]).await?.ok_or(Error::NotFound)?.get(0);
    if receipt["epoch"] != epoch {
        return Err(Error::Fenced);
    }
    serde_json::from_value(receipt["specification"].clone()).map_err(|_| Error::SchemaMismatch)
}
async fn current(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    spec: &RestoreInventorySpec,
    permission: Permission,
) -> Result<()> {
    let ctx = restore::retained_context(
        tx,
        auth,
        epoch,
        spec.binding,
        &spec.manifest_sha256,
        permission,
    )
    .await?;
    if restore_digest(&ctx)? != spec.context_sha256 {
        return Err(Error::RestoreContextChanged);
    }
    if spec.app_id != ctx.specification.app_id
        || spec.installation_id != ctx.specification.upstream.installation_id
        || spec.repository_id != ctx.specification.upstream.repository_id
    {
        return Err(Error::HostBindingState);
    }
    Ok(())
}
async fn page(
    tx: &Transaction<'_>,
    s: &Scope,
    key: &str,
    index: usize,
) -> Result<RestoreInventoryPage> {
    let k = page_key(key, index)?;
    let v:Value=tx.query_opt("SELECT result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind=$4", &[&s.tenant(),&s.repository(),&k,&PAGE]).await?.ok_or(Error::NotFound)?.get(0);
    serde_json::from_value(v["page"].clone()).map_err(|_| Error::SchemaMismatch)
}
impl Metadata {
    pub async fn start_github_restore_inventory(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        spec: &RestoreInventorySpec,
    ) -> Result<Value> {
        spec.validate()?;
        let m = match self
            .begin(request(auth, cmd, START, json!([1, spec]))?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        current(&m.tx, auth, cmd.epoch, spec, Permission::HostRead).await?;
        let now: i64 =
            m.tx.query_one("SELECT extract(epoch FROM clock_timestamp())::bigint", &[])
                .await?
                .get(0);
        let finished = spec.finished_at_unix as i64;
        if finished > now.saturating_add(5) || finished < now.saturating_sub(120) {
            return Err(Error::Invalid("restore inventory capture time"));
        }
        m.complete(json!({"version":1,"key":cmd.key,"epoch":cmd.epoch,"tenant":auth.scope().tenant(),"repository":auth.scope().repository(),
            "recorded_by":auth.principal(),"specification":spec,"inventory_complete":false,"reconciliation_complete":false,"publication_authorized":false}),vec![]).await
    }
    pub async fn record_github_restore_inventory_page(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        key: &str,
        p: &RestoreInventoryPage,
    ) -> Result<Value> {
        crate::identifier(key, 128)?;
        let cmd = Command {
            key: page_key(key, p.index)?,
            epoch,
        };
        let m = match self
            .begin(request(auth, &cmd, PAGE, json!([1, key, p]))?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let spec = inventory(&m.tx, auth.scope(), key, epoch).await?;
        current(&m.tx, auth, epoch, &spec, Permission::HostRead).await?;
        spec.validate_page(p)?;
        m.complete(json!({"version":1,"key":cmd.key,"inventory_key":key,"epoch":epoch,"tenant":auth.scope().tenant(),"repository":auth.scope().repository(),
            "page":p,"page_sha256":restore_digest(p)?,"publication_authorized":false}),vec![]).await
    }
    pub async fn seal_github_restore_inventory(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        key: &str,
    ) -> Result<Value> {
        crate::identifier(key, 128)?;
        let cmd = Command {
            key: seal_key(key)?,
            epoch,
        };
        let m = match self
            .begin(request(auth, &cmd, SEAL, json!([1, key]))?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let spec = inventory(&m.tx, auth.scope(), key, epoch).await?;
        current(&m.tx, auth, epoch, &spec, Permission::HostRead).await?;
        let (mut nodes, mut names, mut numbers) =
            (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        for index in 0..spec.page_sha256.len() {
            let p = page(&m.tx, auth.scope(), key, index).await?;
            spec.validate_page(&p)?;
            for item in p.items {
                let (node, unique) = match item {
                    RestoreInventoryItem::Branch { node, name, .. } => (node, names.insert(name)),
                    RestoreInventoryItem::Pull { node, number, .. } => {
                        (node, numbers.insert(number))
                    }
                };
                if !unique || !nodes.insert(node) {
                    return Err(Error::Invalid("duplicate restore inventory identity"));
                }
            }
        }
        if names.len() != spec.head_count || numbers.len() != spec.pull_count {
            return Err(Error::Invalid("restore inventory counts"));
        }
        m.complete(json!({"version":1,"key":cmd.key,"inventory_key":key,"epoch":epoch,"tenant":auth.scope().tenant(),"repository":auth.scope().repository(),
            "specification_sha256":restore_digest(&spec)?,"pages":spec.page_sha256.len(),"heads":spec.head_count,"pulls":spec.pull_count,
            "capture_started_at_unix":spec.started_at_unix,"capture_finished_at_unix":spec.finished_at_unix,
            "inventory_complete":true,"atomic_host_snapshot":false,"reconciliation_complete":false,"publication_authorized":false}),vec![]).await
    }
    /// Each response is a current transaction, not a transferable admission receipt.
    pub async fn github_restore_inventory_coverage(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        key: &str,
        index: usize,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        context(&tx, auth, epoch, Permission::RepositoryWrite).await?;
        let spec = inventory(&tx, s, key, epoch).await?;
        current(&tx, auth, epoch, &spec, Permission::RepositoryWrite).await?;
        let seal = seal_key(key)?;
        let sealed:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind=$4)",&[&s.tenant(),&s.repository(),&seal,&SEAL]).await?.get(0);
        if !sealed {
            return Err(Error::Invalid("restore inventory not sealed"));
        }
        let p = if spec.page_sha256.is_empty() && index == 0 {
            RestoreInventoryPage {
                version: 1,
                index,
                items: vec![],
            }
        } else {
            if index >= spec.page_sha256.len() {
                return Err(Error::Invalid("restore inventory page index"));
            }
            page(&tx, s, key, index).await?
        };
        let mut revisions = BTreeMap::<String, Vec<Value>>::new();
        for item in &p.items {
            match item {
                RestoreInventoryItem::Branch {name,node,git_revision}=>revisions.entry(git_revision.clone()).or_default().push(json!({"kind":"branch","name":name,"node":node,"repository_id":spec.repository_id})),
                RestoreInventoryItem::Pull {number,node,head_repository,head_revision,base_revision,merge_revision,..}=>{
                    revisions.entry(head_revision.clone()).or_default().push(json!({"kind":"pull_head","number":number,"node":node,"repository_id":head_repository}));
                    revisions.entry(base_revision.clone()).or_default().push(json!({"kind":"pull_base","number":number,"node":node,"repository_id":spec.repository_id}));
                    if let Some(v)=merge_revision { revisions.entry(v.clone()).or_default().push(json!({"kind":"pull_merge","number":number,"node":node,"repository_id":spec.repository_id})); }
                }
            }
        }
        let mut sources = Vec::new();
        for (git_revision, origins) in revisions {
            // Only server-produced projector receipts backed by the same restored
            // scope and a still-live permanent pin count. No caller-supplied flags.
            let row=tx.query_opt("SELECT m.key,m.result FROM th_operations m JOIN th_operations d ON d.tenant=m.tenant AND d.repository=m.repository AND d.key=m.result->>'decision_key' AND d.kind='github.restore.decision.v1' JOIN th_object_pins p ON p.tenant=m.tenant AND p.repository=m.repository AND p.holder=m.result->>'source_holder' AND p.generation=(m.result->>'generation')::bigint JOIN th_objects o ON o.tenant=p.tenant AND o.digest=p.digest AND o.generation=p.generation WHERE m.tenant=$1 AND m.repository=$2 AND m.kind='github.restore.mapping.v1' AND m.result->>'git_revision'=$3 AND (m.result->>'epoch')::bigint=$4 AND m.result->>'verifier'='rust-git-projection-v1' AND m.result->>'git_mapping_verified'='true' AND d.result->'specification'->>'manifest_sha256'=$5 AND d.result->'specification'->>'binding'=$6 AND 'native:sha256:'||encode(p.digest,'hex')=m.result->>'native_revision' AND o.kind=5 AND o.state='live' ORDER BY m.key LIMIT 1",
                &[&s.tenant(),&s.repository(),&git_revision,&epoch,&spec.manifest_sha256,&spec.binding.to_string()]).await?;
            let mapping =
                row.map(|r| json!({"key":r.get::<_,String>(0),"receipt":r.get::<_,Value>(1)}));
            sources.push(json!({"git_revision":git_revision,"origins":origins,"classification":if mapping.is_some(){"preserved"}else{"missing_verified_source"},"mapping":mapping}));
        }
        let missing = sources
            .iter()
            .filter(|v| v["classification"] == "missing_verified_source")
            .count();
        context(&tx, auth, epoch, Permission::RepositoryWrite).await?;
        tx.commit().await?;
        Ok(
            json!({"version":1,"tenant":s.tenant(),"repository":s.repository(),"epoch":epoch,"inventory_key":key,"page":index,
            "next_page":if index+1<spec.page_sha256.len(){Some(index+1)}else{None},"sources":sources,"missing_on_page":missing,
            "capture_finished_at_unix":spec.finished_at_unix,"inventory_complete":true,"atomic_host_snapshot":false,"reconciliation_complete":false,"publication_authorized":false}),
        )
    }
}
