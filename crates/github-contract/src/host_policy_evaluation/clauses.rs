use super::*;

pub(super) fn conditions(v: &Value, path: &str, a: &mut Analysis) {
    fields(v, &["ref_name"], path, a);
    fields(
        &v["ref_name"],
        &["include", "exclude"],
        &format!("{path}/ref_name"),
        a,
    );
    for key in ["include", "exclude"] {
        match v["ref_name"][key].as_array() {
            Some(patterns)
                if patterns.len() <= 100
                    && patterns
                        .iter()
                        .all(|p| p.as_str().is_some_and(|s| text(s, 1024).is_ok())) => {}
            _ => a.reason(&format!("{path}/ref_name/{key}/patterns_unproven")),
        }
    }
}

pub(super) fn fields(v: &Value, known: &[&str], path: &str, a: &mut Analysis) {
    match v.as_object() {
        Some(m) => {
            for key in m.keys().filter(|k| !known.contains(&k.as_str())) {
                a.reason(&format!("{path}/{key}/field_unproven"));
            }
        }
        None => a.reason(&format!("{path}/object_unproven")),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Flag {
    enabled: bool,
    #[serde(rename = "url")]
    _url: Option<String>,
}
fn flag(v: &Value, name: &str, a: &mut Analysis) -> Option<bool> {
    match serde_json::from_value::<Flag>(v[name].clone()) {
        Ok(f) => Some(f.enabled),
        Err(_) => {
            a.reason(&format!("legacy/{name}/flag_unproven"));
            None
        }
    }
}
fn required_check(v: &Value, issuer: &str, path: &str, a: &mut Analysis) -> Option<RequiredCheck> {
    fields(v, &["context", issuer], path, a);
    let check = RequiredCheck {
        name: v["context"].as_str().unwrap_or("").to_owned(),
        app_id: v[issuer].as_u64().unwrap_or(0),
    };
    if check.validate().is_err() {
        a.reason(&format!("{path}/explicit_check_issuer_unproven"));
        None
    } else {
        Some(check)
    }
}
fn checks(v: &Value, issuer: &str, path: &str, a: &mut Analysis) -> BTreeSet<RequiredCheck> {
    let mut result = BTreeSet::new();
    match v.as_array() {
        Some(rows) if rows.len() <= 100 => {
            for row in rows {
                if let Some(c) = required_check(row, issuer, path, a)
                    && !result.insert(c)
                {
                    a.reason(&format!("{path}/duplicate_check"));
                }
            }
        }
        _ => a.reason(&format!("{path}/check_list_unproven")),
    }
    result
}
fn bool_field(v: &Value, name: &str, path: &str, a: &mut Analysis) -> Option<bool> {
    let b = v[name].as_bool();
    if b.is_none() {
        a.reason(&format!("{path}/{name}/boolean_unproven"));
    }
    b
}

pub(super) fn status_rule(v: &Value, path: &str, a: &mut Analysis) {
    fields(
        v,
        &[
            "required_status_checks",
            "strict_required_status_checks_policy",
            "do_not_enforce_on_create",
        ],
        path,
        a,
    );
    let required = checks(&v["required_status_checks"], "integration_id", path, a);
    a.required.extend(required);
    // Both values are interpreted: exact candidate checks remain mandatory in
    // TreeHarbor even when GitHub does not require an up-to-date original PR.
    bool_field(v, "strict_required_status_checks_policy", path, a);
    if let Some(create) = v.get("do_not_enforce_on_create")
        && !create.is_boolean()
    {
        a.reason(&format!("{path}/do_not_enforce_on_create/boolean_unproven"));
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QueueParameters {
    check_response_timeout_minutes: u32,
    grouping_strategy: String,
    max_entries_to_build: u32,
    max_entries_to_merge: u32,
    merge_method: String,
    min_entries_to_merge: u32,
    min_entries_to_merge_wait_minutes: u32,
}
impl QueueParameters {
    pub(super) fn matches(&self, q: &QueueConfiguration) -> bool {
        self.check_response_timeout_minutes == q.check_timeout_minutes
            && self.grouping_strategy == q.merging_strategy
            && self.max_entries_to_build == q.maximum_entries_to_build
            && self.max_entries_to_merge == q.maximum_entries_to_merge
            && self.merge_method == q.merge_method
            && self.min_entries_to_merge == q.minimum_entries_to_merge
            && self.min_entries_to_merge_wait_minutes == q.minimum_entries_wait_minutes
    }
}

fn review(v: &Value, path: &str, threads: bool, teams: bool, a: &mut Analysis) {
    let count = v["required_approving_review_count"].as_u64();
    if count.is_none_or(|n| n > 100) {
        a.reason(&format!("{path}/approval_count_unproven"));
    }
    let code_owners = bool_field(v, "require_code_owner_review", path, a);
    let last_push = bool_field(v, "require_last_push_approval", path, a);
    let stale = bool_field(v, "dismiss_stale_reviews_on_push", path, a);
    a.reviews.push(ReviewRequirement {
        source: path.to_owned(),
        approvals: count.unwrap_or(0).min(100) as u32,
        code_owners: code_owners.unwrap_or(false),
        last_push: last_push.unwrap_or(false),
        dismiss_stale: stale.unwrap_or(false),
        resolve_threads: threads,
        required_teams: teams,
    });
}
pub(super) fn review_rule(v: &Value, path: &str, a: &mut Analysis) {
    fields(
        v,
        &[
            "allowed_merge_methods",
            "dismiss_stale_reviews_on_push",
            "dismissal_restriction",
            "require_code_owner_review",
            "require_last_push_approval",
            "required_approving_review_count",
            "required_review_thread_resolution",
            "required_reviewers",
        ],
        path,
        a,
    );
    let threads = bool_field(v, "required_review_thread_resolution", path, a).unwrap_or(false);
    // Extra actor/team rules remain visible and unproven until their full
    // identity/approval evidence is collected; an APPROVED enum cannot satisfy them.
    let teams = match v.get("required_reviewers") {
        None => false,
        Some(Value::Array(t)) if t.is_empty() => false,
        Some(_) => {
            a.reason(&format!("{path}/required_reviewers_unproven"));
            true
        }
    };
    if let Some(d) = v.get("dismissal_restriction") {
        fields(
            d,
            &["enabled", "allowed_actors"],
            &format!("{path}/dismissal_restriction"),
            a,
        );
        if d["enabled"] != false || d["allowed_actors"].as_array().is_none_or(|t| !t.is_empty()) {
            a.reason(&format!("{path}/dismissal_restriction_unproven"));
        }
    }
    if let Some(m) = v.get("allowed_merge_methods") {
        match m.as_array() {
            Some(methods)
                if !methods.is_empty()
                    && methods
                        .iter()
                        .all(|m| matches!(m.as_str(), Some("merge" | "squash" | "rebase"))) =>
            {
                // Exact-MERGE publication is the current proof path.
                if !methods.contains(&Value::String("merge".into())) {
                    a.candidate_requirements
                        .insert("non_merge_publication".into());
                }
            }
            _ => a.reason(&format!("{path}/allowed_merge_methods_unproven")),
        }
    }
    review(v, path, threads, teams, a);
}

fn empty_actors(v: &Value, path: &str, a: &mut Analysis) {
    fields(
        v,
        &[
            "users",
            "teams",
            "apps",
            "url",
            "users_url",
            "teams_url",
            "apps_url",
        ],
        path,
        a,
    );
    for kind in ["users", "teams", "apps"] {
        if v[kind].as_array().is_none_or(|a| !a.is_empty()) {
            a.reason(&format!("{path}/{kind}/actors_unproven"));
        }
    }
}
pub(super) fn legacy(v: &Value, a: &mut Analysis) {
    fields(
        v,
        &[
            "url",
            "required_status_checks",
            "enforce_admins",
            "required_pull_request_reviews",
            "restrictions",
            "required_linear_history",
            "allow_force_pushes",
            "allow_deletions",
            "block_creations",
            "required_conversation_resolution",
            "lock_branch",
            "allow_fork_syncing",
            "required_signatures",
        ],
        "legacy",
        a,
    );
    if flag(v, "enforce_admins", a) == Some(false) {
        a.reason("legacy/administrator_bypass");
    }
    for name in ["allow_force_pushes", "allow_deletions", "lock_branch"] {
        if flag(v, name, a) == Some(true) {
            a.reason(&format!("legacy/{name}/enabled"));
        }
    }
    for (name, requirement) in [
        ("required_linear_history", "required_linear_history"),
        ("required_signatures", "required_signatures"),
    ] {
        if flag(v, name, a) == Some(true) {
            a.candidate_requirements.insert(requirement.to_owned());
        }
    }
    // Creation and fork synchronization do not bypass update protection on this
    // existing base branch. Still require valid flags and include them in the pin.
    flag(v, "block_creations", a);
    flag(v, "allow_fork_syncing", a);
    let threads = flag(v, "required_conversation_resolution", a) == Some(true);
    match v.get("restrictions") {
        Some(Value::Null) => (),
        Some(r) => empty_actors(r, "legacy/restrictions", a),
        None => a.reason("legacy/restrictions_unavailable"),
    }
    match v.get("required_status_checks") {
        Some(Value::Null) => (),
        Some(s) => {
            fields(
                s,
                &["url", "contexts_url", "strict", "contexts", "checks"],
                "legacy/checks",
                a,
            );
            bool_field(s, "strict", "legacy/checks", a);
            let required = checks(&s["checks"], "app_id", "legacy/checks", a);
            let names: BTreeSet<_> = required.iter().map(|c| c.name.as_str()).collect();
            match s["contexts"].as_array() {
                Some(contexts) if contexts.len() <= 100 => {
                    let seen: BTreeSet<_> = contexts.iter().filter_map(Value::as_str).collect();
                    if contexts.len() != seen.len() || seen != names {
                        a.reason("legacy/checks/contexts_disagree");
                    }
                }
                _ => a.reason("legacy/checks/contexts_unavailable"),
            }
            a.required.extend(required);
        }
        None => a.reason("legacy/checks/unavailable"),
    }
    match v.get("required_pull_request_reviews") {
        Some(Value::Null) => {
            a.reviews.push(ReviewRequirement {
                source: "legacy/reviews".into(),
                approvals: 0,
                code_owners: false,
                last_push: false,
                dismiss_stale: false,
                resolve_threads: threads,
                required_teams: false,
            });
        }
        Some(r) => {
            fields(
                r,
                &[
                    "url",
                    "dismissal_restrictions",
                    "dismiss_stale_reviews",
                    "require_code_owner_reviews",
                    "required_approving_review_count",
                    "require_last_push_approval",
                    "bypass_pull_request_allowances",
                ],
                "legacy/reviews",
                a,
            );
            empty_actors(
                &r["bypass_pull_request_allowances"],
                "legacy/reviews/bypass",
                a,
            );
            if let Some(d) = r.get("dismissal_restrictions") {
                empty_actors(d, "legacy/reviews/dismissal", a);
            }
            let mut normalized = r.clone();
            if let Some(m) = normalized.as_object_mut() {
                m.insert(
                    "require_code_owner_review".into(),
                    r["require_code_owner_reviews"].clone(),
                );
                m.insert(
                    "dismiss_stale_reviews_on_push".into(),
                    r["dismiss_stale_reviews"].clone(),
                );
            }
            review(&normalized, "legacy/reviews", threads, false, a);
        }
        None => a.reason("legacy/reviews/unavailable"),
    }
}
