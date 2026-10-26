//! Branch configuration before the first PR. No mutation or implicit scope upgrade.
use super::*;
use treeharbor_github_contract::{host_configuration as hc, host_policy_evaluation::Profile};

const CONFIGURATION_QUERY: &str = r"query TreeHarborHostConfiguration($owner:String!,$name:String!,$branch:String!,$ref:String!,$settings:Boolean!){
 repository(owner:$owner,name:$name,followRenames:false){databaseId nameWithOwner owner{__typename}
 ref(qualifiedName:$ref){name prefix repository{databaseId} branchProtectionRule{
 id repository{databaseId} allowsDeletions allowsForcePushes blocksCreations dismissesStaleReviews
 isAdminEnforced lockAllowsFetchAndMerge lockBranch requireLastPushApproval requiresApprovingReviews
 requiresCodeOwnerReviews requiresCommitSignatures requiresConversationResolution requiresDeployments
 requiresLinearHistory requiresStatusChecks requiresStrictStatusChecks restrictsPushes restrictsReviewDismissals
 requiredApprovingReviewCount requiredStatusCheckContexts requiredDeploymentEnvironments
 bypassPullRequestAllowances(first:100){totalCount pageInfo{hasNextPage hasPreviousPage} nodes{id}}
 bypassForcePushAllowances(first:100){totalCount pageInfo{hasNextPage hasPreviousPage} nodes{id}}
 }}
 mergeQueue(branch:$branch){id repository{databaseId} configuration @include(if:$settings){
 mergeMethod mergingStrategy maximumEntriesToBuild maximumEntriesToMerge minimumEntriesToMerge
 minimumEntriesToMergeWaitTime checkResponseTimeout}}
 }}";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AllowancePage {
    total_count: u32,
    page_info: AllowancePageInfo,
    nodes: Vec<Allowance>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AllowancePageInfo {
    has_next_page: bool,
    has_previous_page: bool,
}
#[derive(Deserialize)]
struct Allowance {
    id: String,
}
fn allowances(value: &Value) -> Result<hc::Allowances> {
    let p: AllowancePage = serde_json::from_value(value.clone()).map_err(|_| Error::Evidence)?;
    if p.page_info.has_previous_page {
        return Err(Error::Evidence);
    }
    Ok(hc::Allowances {
        total_count: p.total_count,
        nodes: p.nodes.into_iter().map(|n| n.id).collect(),
        has_next_page: p.page_info.has_next_page,
    })
}
fn nullable<T: serde::de::DeserializeOwned>(value: &Value, key: &str) -> Result<Option<T>> {
    serde_json::from_value(value.get(key).ok_or(Error::Evidence)?.clone())
        .map_err(|_| Error::Evidence)
}
fn decode(
    v: Value,
    b: &RepositoryBinding,
    t: &hc::Target,
    kind: OwnerType,
    settings: bool,
) -> Result<(hc::Visibility, Option<QueueConfiguration>)> {
    if v.get("errors")
        .is_some_and(|e| e.as_array().is_none_or(|e| !e.is_empty()))
    {
        return Err(Error::Evidence);
    }
    let r = &v["data"]["repository"];
    let f = &r["ref"];
    let owner_kind = match kind {
        OwnerType::User => "User",
        OwnerType::Organization => "Organization",
    };
    if r["databaseId"].as_u64() != Some(b.repository_id)
        || r["nameWithOwner"]
            .as_str()
            .is_none_or(|n| !n.eq_ignore_ascii_case(&t.repository_name))
        || r["owner"]["__typename"] != owner_kind
        || f["name"] != t.base_ref
        || f["prefix"] != "refs/heads/"
        || f["repository"]["databaseId"].as_u64() != Some(b.repository_id)
    {
        return Err(Error::Identity);
    }
    let legacy = match f.get("branchProtectionRule").ok_or(Error::Evidence)? {
        Value::Null => None,
        l => {
            let flags = hc::FLAGS
                .iter()
                .map(|k| {
                    l[*k]
                        .as_bool()
                        .map(|v| ((*k).to_owned(), v))
                        .ok_or(Error::Evidence)
                })
                .collect::<Result<_>>()?;
            Some(hc::Legacy {
                id: l["id"].as_str().ok_or(Error::Evidence)?.to_owned(),
                repository_id: l["repository"]["databaseId"]
                    .as_u64()
                    .ok_or(Error::Evidence)?,
                flags,
                approving_reviews: nullable(l, "requiredApprovingReviewCount")?,
                required_contexts: nullable(l, "requiredStatusCheckContexts")?,
                deployment_environments: nullable(l, "requiredDeploymentEnvironments")?,
                bypass_pull_requests: allowances(&l["bypassPullRequestAllowances"])?,
                bypass_force_pushes: allowances(&l["bypassForcePushAllowances"])?,
            })
        }
    };
    let (queue_id, queue) = match r.get("mergeQueue").ok_or(Error::Evidence)? {
        Value::Null => (None, None),
        q => {
            let id = q["id"].as_str().ok_or(Error::Evidence)?.to_owned();
            if q["repository"]["databaseId"].as_u64() != Some(b.repository_id) {
                return Err(Error::Identity);
            }
            let configuration = if settings {
                let c: Option<Configuration> = nullable(q, "configuration")?;
                c.map(|c| QueueConfiguration {
                    id: id.clone(),
                    merge_method: c.merge_method,
                    merging_strategy: c.merging_strategy,
                    maximum_entries_to_build: c.maximum_entries_to_build,
                    maximum_entries_to_merge: c.maximum_entries_to_merge,
                    minimum_entries_to_merge: c.minimum_entries_to_merge,
                    minimum_entries_wait_minutes: c.minimum_entries_to_merge_wait_time,
                    check_timeout_minutes: c.check_response_timeout,
                })
            } else {
                if q.get("configuration").is_some() {
                    return Err(Error::Evidence);
                }
                None
            };
            (Some(id), configuration)
        }
    };
    Ok((
        hc::Visibility {
            target: t.clone(),
            owner_kind: owner_kind.into(),
            queue_id,
            legacy,
        },
        queue,
    ))
}

impl Connection {
    async fn configuration_round(
        &self,
        b: &RepositoryBinding,
        t: &hc::Target,
        kind: OwnerType,
        token: &wire::Token,
        settings: bool,
    ) -> Result<hc::Configuration> {
        let v=self.json(Stage::CheckEvidence,Method::POST,"graphql",token.token.header()?,
            Some(json!({"query":CONFIGURATION_QUERY,"variables":{"owner":b.owner,"name":b.name,"branch":t.base_ref,"ref":format!("refs/heads/{}",t.base_ref),"settings":settings}})),StatusCode::OK).await?;
        let (visibility, queue) = decode(v, b, t, kind, settings)?;
        visibility.validate(t).map_err(|_| Error::Evidence)?;
        let base = t.base_ref.replace('/', "%2F");
        let protection = self
            .host_document(
                &format!("repos/{}/{}/branches/{base}/protection", b.owner, b.name),
                token,
            )
            .await?;
        let (rules, rulesets) = self.host_rules(b, &t.base_ref, token).await?;
        let c = hc::Configuration {
            version: 1,
            visibility,
            profile: Profile {
                version: 1,
                repository_id: b.repository_id,
                repository_name: t.repository_name.clone(),
                base_ref: t.base_ref.clone(),
                protection,
                rules,
                rulesets,
                queue,
            },
        };
        c.validate(t).map_err(|_| Error::Evidence)?;
        Ok(c)
    }
    /// No PR, service mutation or approval prerequisite. Settings reads explicitly
    /// request Contents-write; false never falls back to broader credentials.
    pub async fn inspect_host_configuration(
        &self,
        b: &RepositoryBinding,
        base: &str,
        include_queue_settings: bool,
    ) -> Result<hc::Inspection> {
        b.validate()?;
        let t = hc::Target {
            repository_id: b.repository_id,
            repository_name: format!("{}/{}", b.owner, b.name),
            base_ref: base.into(),
        };
        t.validate().map_err(|_| Error::Evidence)?;
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let started = now()? as i64 * 1000;
        let cap = if include_queue_settings {
            wire::Capability::PolicySettings
        } else {
            wire::Capability::PolicyInspect
        };
        let (installation, token) = tokio::time::timeout(PROBE_TIMEOUT, self.mint_for(b, cap))
            .await
            .map_err(|_| Error::Deadline)??;
        let result = tokio::time::timeout(PROBE_TIMEOUT, async {
            let observed = self.observe_for(b, &installation, &token, cap).await?;
            let before = self
                .configuration_round(b, &t, observed.owner_type, &token, include_queue_settings)
                .await?;
            let after = self
                .configuration_round(b, &t, observed.owner_type, &token, include_queue_settings)
                .await?;
            Ok::<_, Error>((before, after))
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let token_revoked = self.revoke(&token).await.is_ok();
        let (before, after) = result?;
        let report = hc::Inspection {
            version: 1,
            started_at_unix_ms: started,
            finished_at_unix_ms: now()? as i64 * 1000,
            before: Box::new(before),
            after: Box::new(after),
            token_revoked,
        };
        report.validate(&t).map_err(|_| Error::Evidence)?;
        Ok(report)
    }
}
