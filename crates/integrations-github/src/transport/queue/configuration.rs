//! Branch configuration before the first PR. No mutation or implicit scope upgrade.
use super::*;
use crate::PolicyReadOptions;
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
fn anchored_minutes(raw: u32, anchor: &Value) -> Result<u32> {
    let minutes = anchor
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(Error::Evidence)?;
    if raw != minutes && minutes.checked_mul(60) != Some(raw) {
        return Err(Error::Evidence);
    }
    Ok(minutes)
}

fn anchored_queue_times(
    queue: Option<QueueConfiguration>,
    rules: &[Value],
) -> Result<Option<QueueConfiguration>> {
    let Some(mut queue) = queue else {
        return Ok(None);
    };
    let mut applicable = rules.iter().filter(|r| r["type"] == "merge_queue");
    let rule = applicable.next().ok_or(Error::Evidence)?;
    if applicable.next().is_some() {
        return Err(Error::Evidence);
    }
    // The documented GraphQL unit is minutes, but provider responses can expose
    // seconds. Interpret neither by magnitude: require one exact applicable REST
    // rule as the independent minute-valued anchor. Never round or rewrite rules.
    let parameters = &rule["parameters"];
    queue.check_timeout_minutes = anchored_minutes(
        queue.check_timeout_minutes,
        &parameters["check_response_timeout_minutes"],
    )?;
    queue.minimum_entries_wait_minutes = anchored_minutes(
        queue.minimum_entries_wait_minutes,
        &parameters["min_entries_to_merge_wait_minutes"],
    )?;
    queue.validate().map_err(|_| Error::Evidence)?;
    Ok(Some(queue))
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

pub(super) struct PolicyAccess {
    pub(super) owner_type: OwnerType,
    pub(super) token: wire::Token,
    ruleset_token: Option<wire::Token>,
}

impl PolicyAccess {
    pub(super) fn ruleset_token(&self) -> &wire::Token {
        self.ruleset_token.as_ref().unwrap_or(&self.token)
    }
}

impl Connection {
    async fn mint_verified_policy_token(
        &self,
        b: &RepositoryBinding,
        capability: wire::Capability,
    ) -> Result<(OwnerType, wire::Token)> {
        let (installation, token) =
            tokio::time::timeout(PROBE_TIMEOUT, self.mint_for(b, capability))
                .await
                .map_err(|_| Error::Deadline)??;
        let verified = tokio::time::timeout(
            PROBE_TIMEOUT,
            self.observe_for(b, &installation, &token, capability),
        )
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        match verified {
            Ok(probe) => Ok((probe.owner_type, token)),
            Err(error) => {
                let _ = self.revoke(&token).await;
                Err(error)
            }
        }
    }

    pub(super) async fn mint_policy_access(
        &self,
        b: &RepositoryBinding,
        capability: wire::Capability,
        include_ruleset_bypass: bool,
    ) -> Result<PolicyAccess> {
        let (owner_type, token) = self.mint_verified_policy_token(b, capability).await?;
        let mut access = PolicyAccess {
            owner_type,
            token,
            ruleset_token: None,
        };
        if include_ruleset_bypass {
            match self
                .mint_verified_policy_token(b, wire::Capability::RulesetAudit)
                .await
            {
                Ok((_, token)) => access.ruleset_token = Some(token),
                Err(error) => {
                    let _ = self.revoke(&access.token).await;
                    return Err(error);
                }
            }
        }
        Ok(access)
    }

    pub(super) async fn revoke_policy_access(&self, access: &PolicyAccess) -> bool {
        let audit_revoked = if let Some(token) = &access.ruleset_token {
            self.revoke(token).await.is_ok()
        } else {
            true
        };
        self.revoke(&access.token).await.is_ok() && audit_revoked
    }

    pub(super) async fn configuration_round(
        &self,
        b: &RepositoryBinding,
        t: &hc::Target,
        access: &PolicyAccess,
        settings: bool,
    ) -> Result<hc::Configuration> {
        let token = &access.token;
        let v=self.json(Stage::CheckEvidence,Method::POST,"graphql",token.token.header()?,
            Some(json!({"query":CONFIGURATION_QUERY,"variables":{"owner":b.owner,"name":b.name,"branch":t.base_ref,"ref":format!("refs/heads/{}",t.base_ref),"settings":settings}})),StatusCode::OK).await?;
        let (visibility, queue) = decode(v, b, t, access.owner_type, settings)?;
        visibility.validate(t).map_err(|_| Error::Evidence)?;
        let base = t.base_ref.replace('/', "%2F");
        let protection = self
            .host_document(
                &format!("repos/{}/{}/branches/{base}/protection", b.owner, b.name),
                token,
            )
            .await?;
        let (rules, rulesets) = self
            .host_rules(b, &t.base_ref, token, access.ruleset_token())
            .await?;
        let queue = anchored_queue_times(queue, &rules)?;
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
    /// Observe policy before a PR exists. Additional token permissions require an
    /// explicit option and are limited to the selected repository.
    pub async fn inspect_host_configuration(
        &self,
        b: &RepositoryBinding,
        base: &str,
        options: impl Into<PolicyReadOptions>,
    ) -> Result<hc::Inspection> {
        let options = options.into();
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
        let cap = if options.include_queue_settings {
            wire::Capability::PolicySettings
        } else {
            wire::Capability::PolicyInspect
        };
        let access = self
            .mint_policy_access(b, cap, options.include_ruleset_bypass)
            .await?;
        let result = tokio::time::timeout(PROBE_TIMEOUT, async {
            let before = self
                .configuration_round(b, &t, &access, options.include_queue_settings)
                .await?;
            let after = self
                .configuration_round(b, &t, &access, options.include_queue_settings)
                .await?;
            Ok::<_, Error>((before, after))
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let token_revoked = self.revoke_policy_access(&access).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(timeout: u32, wait: u32) -> QueueConfiguration {
        QueueConfiguration {
            id: "MQ_units".into(),
            merge_method: "MERGE".into(),
            merging_strategy: "ALLGREEN".into(),
            maximum_entries_to_build: 2,
            maximum_entries_to_merge: 1,
            minimum_entries_to_merge: 1,
            minimum_entries_wait_minutes: wait,
            check_timeout_minutes: timeout,
        }
    }

    fn rule(timeout: Value, wait: Value) -> Value {
        json!({"type":"merge_queue","parameters":{
            "check_response_timeout_minutes":timeout,
            "min_entries_to_merge_wait_minutes":wait,
        }})
    }

    #[test]
    fn time_units_require_one_exact_rest_anchor_without_rounding() {
        for (raw, minutes) in [
            (queue(10, 3), queue(10, 3)),
            (queue(600, 180), queue(10, 3)),
            (queue(601, 180), queue(601, 180)),
            (queue(600, 180), queue(600, 3)),
            (queue(1, 180), queue(1, 180)),
            (queue(60, 0), queue(1, 0)),
            (queue(86400, 86400), queue(1440, 1440)),
        ] {
            let rules = vec![rule(
                json!(minutes.check_timeout_minutes),
                json!(minutes.minimum_entries_wait_minutes),
            )];
            let original = rules.clone();
            let normalized = anchored_queue_times(Some(raw), &rules).unwrap().unwrap();
            assert_eq!(normalized, minutes);
            assert_eq!(rules, original);
        }
        for raw in [0, 1, 599, 601, 1200, 86401, u32::MAX] {
            assert!(
                anchored_queue_times(Some(queue(raw, 180)), &[rule(json!(10), json!(3))]).is_err(),
                "{raw}"
            );
        }
        for raw in [0, 2, 179, 181, 240, u32::MAX] {
            assert!(
                anchored_queue_times(Some(queue(600, raw)), &[rule(json!(10), json!(3))]).is_err(),
                "{raw}"
            );
        }
    }

    #[test]
    fn missing_invalid_or_ambiguous_time_anchors_are_not_guessed() {
        let valid = rule(json!(10), json!(3));
        let mut missing = valid.clone();
        missing["parameters"]
            .as_object_mut()
            .unwrap()
            .remove("min_entries_to_merge_wait_minutes");
        for rules in [
            vec![],
            vec![missing],
            vec![rule(Value::Null, json!(3))],
            vec![rule(json!("10"), json!(3))],
            vec![rule(json!(-1), json!(3))],
            vec![rule(json!(10), Value::Null)],
            vec![rule(json!(10), json!("3"))],
            vec![rule(json!(10), json!(-1))],
            vec![rule(json!(10), json!(u64::MAX))],
            vec![valid.clone(), valid.clone()],
            vec![valid, rule(json!(600), json!(180))],
        ] {
            assert!(anchored_queue_times(Some(queue(600, 180)), &rules).is_err());
        }
        for (timeout, wait) in [(0, 3), (1441, 3), (10, 1441)] {
            assert!(
                anchored_queue_times(
                    Some(queue(timeout, wait)),
                    &[rule(json!(timeout), json!(wait))],
                )
                .is_err()
            );
        }
        assert!(anchored_queue_times(None, &[]).unwrap().is_none());
    }
}
