use super::*;
pub(super) const PROFILE: &str = "treeharbor-review-diff-v1/diffy-0.5.2/context-3";
pub(super) const TEXT_BYTES: usize = 64 * 1024;
pub(super) const MAX_RESPONSE: usize = 1024 * 1024;
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) fn entry(value: Option<EntryRef>) -> Value {
    value
        .map(|v| json!({"mode":v.mode as u32,"object":v.target.to_string()}))
        .unwrap_or(Value::Null)
}
pub(super) fn change_kind(before: Option<EntryRef>, after: Option<EntryRef>) -> &'static str {
    match (before, after) {
        (None, _) => "add",
        (_, None) => "delete",
        (Some(a), Some(b)) if a.mode != b.mode => {
            let file = |m| matches!(m, EntryMode::File | EntryMode::Executable);
            if file(a.mode) && file(b.mode) {
                "mode"
            } else {
                "type"
            }
        }
        _ => "modify",
    }
}
#[derive(Clone)]
pub(super) enum Content {
    Bytes(Vec<u8>),
    Omitted(&'static str),
}
pub(super) fn text(before: Content, after: Content, work: &mut usize) -> Value {
    let (before, after) = match (before, after) {
        (Content::Bytes(a), Content::Bytes(b)) => (a, b),
        (Content::Omitted(reason), _) | (_, Content::Omitted(reason)) => {
            return json!({"status":"omitted","reason":reason});
        }
    };
    if before.len() > TEXT_BYTES || after.len() > TEXT_BYTES {
        return json!({"status":"omitted","reason":"size_limit"});
    }
    let (a, b) = match (std::str::from_utf8(&before), std::str::from_utf8(&after)) {
        (Ok(a), Ok(b)) if !a.contains('\0') && !b.contains('\0') => (a, b),
        _ => return json!({"status":"omitted","reason":"binary"}),
    };
    let lines_a = a.lines().count();
    let lines_b = b.lines().count();
    if lines_a > 4000 || lines_b > 4000 || lines_a.saturating_mul(lines_b) > *work {
        return json!({"status":"omitted","reason":"work_limit"});
    }
    *work -= lines_a.saturating_mul(lines_b);
    if a == b {
        return json!({"status":"unchanged"});
    }
    let mut options = diffy::DiffOptions::new();
    options
        .set_context_len(3)
        .set_original_filename("before")
        .set_modified_filename("after");
    json!({"status":"patch","patch":options.create_patch(a,b).to_string()})
}
struct Writer(Vec<u8>);
impl std::io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_RESPONSE {
            return Err(std::io::Error::other("review response limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn encode(value: &Value) -> Result<Vec<u8>> {
    let mut writer = Writer(Vec::new());
    serde_json::to_writer(&mut writer, value).map_err(|_| limit())?;
    Ok(writer.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unified_patch_and_metadata_match_independent_wire_vector() {
        let v: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/review-diff-v1.json")).unwrap();
        let request =
            parse(&serde_json::to_vec(&v["request"]).unwrap()).unwrap_or_else(|_| panic!("wire"));
        assert_eq!(request.fence.epoch, 3);
        assert_eq!(PROFILE, v["profile"]);
        let a = v["before"].as_str().unwrap();
        let b = v["after"].as_str().unwrap();
        let rendered = text(
            Content::Bytes(a.as_bytes().to_vec()),
            Content::Bytes(b.as_bytes().to_vec()),
            &mut 16_000_000,
        );
        assert_eq!(rendered, v["entry"]["text"]);
        let e = |s: &str| {
            Some(EntryRef {
                mode: EntryMode::File,
                target: Object::Blob(s.as_bytes().to_vec()).id().unwrap(),
            })
        };
        assert_eq!(entry(e(a)), v["entry"]["before"]);
        assert_eq!(entry(e(b)), v["entry"]["after"]);
        let patch = diffy::Patch::from_str(rendered["patch"].as_str().unwrap()).unwrap();
        assert_eq!(diffy::apply(a, &patch).unwrap(), b);
    }
    #[test]
    fn patch_limits_are_explicit_and_json_budget_counts_escaping() {
        let c = |v: &[u8]| Content::Bytes(v.to_vec());
        for value in [b"nul\0".as_slice(), &[255, 254]] {
            assert_eq!(text(c(value), c(b"x"), &mut 16_000_000)["reason"], "binary");
        }
        assert_eq!(
            text(c(&vec![b'a'; TEXT_BYTES + 1]), c(b"x"), &mut 16_000_000)["reason"],
            "size_limit"
        );
        let mut budget = 1;
        assert_eq!(text(c(b"a\n"), c(b"b\n"), &mut budget)["status"], "patch");
        assert_eq!(budget, 0);
        assert_eq!(
            text(c(b"a\n"), c(b"b\n"), &mut budget)["reason"],
            "work_limit"
        );
        assert_eq!(
            text(Content::Omitted("directory"), c(b""), &mut 1)["reason"],
            "directory"
        );
        assert!(encode(&json!({"patch":"\0".repeat(MAX_RESPONSE/6+1)})).is_err());
        for (a, b) in [
            ("", "added\n"),
            ("gone\n", ""),
            ("no newline", "also no newline"),
            ("one\r\ntwo\r\n", "ONE\r\ntwo\r\n"),
        ] {
            let result = text(c(a.as_bytes()), c(b.as_bytes()), &mut 16_000_000);
            let patch = diffy::Patch::from_str(result["patch"].as_str().unwrap()).unwrap();
            assert_eq!(diffy::apply(a, &patch).unwrap(), b);
        }
    }
    #[test]
    fn wire_rejects_ambiguous_identity_and_unbounded_queries() {
        let v: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/review-diff-v1.json")).unwrap();
        for (key, value) in [
            ("snapshot", json!("sha256:untrusted")),
            ("epoch", json!(0)),
            ("limit", json!(101)),
            ("after", json!("2e2e2f78")),
            ("authority", json!("automatic")),
        ] {
            let mut r = v["request"].clone();
            r[key] = value;
            assert!(parse(&serde_json::to_vec(&r).unwrap()).is_err(), "{key}");
        }
        let duplicate =
            serde_json::to_string(&v["request"])
                .unwrap()
                .replacen('{', "{\"version\":1,", 1);
        assert!(parse(duplicate.as_bytes()).is_err());
    }
    #[test]
    fn candidate_selector_is_explicit_and_cannot_override_stored_context() {
        let vector: Value = serde_json::from_str(include_str!(
            "../../../metadata/tests/fixtures/review-candidate-v1.json"
        ))
        .unwrap();
        let request = parse(&serde_json::to_vec(&vector["request"]).unwrap())
            .unwrap_or_else(|_| panic!("candidate wire"));
        assert_eq!(request.fence.epoch, 4);
        let ReviewTarget::Candidate { candidate } = request.fence.target else {
            panic!("selector")
        };
        assert_eq!(candidate.to_string(), vector["identity"]["candidate"]);
        for (field, value) in [
            ("revision", vector["identity"]["revision"].clone()),
            ("expected_head", vector["identity"]["expected_head"].clone()),
            ("policy", vector["identity"]["policy"].clone()),
            ("candidate_epoch", json!(4)),
            ("candidate", json!("not-a-uuid")),
        ] {
            let mut body = vector["request"].clone();
            body["query"][field] = value;
            assert!(
                parse(&serde_json::to_vec(&body).unwrap()).is_err(),
                "{field}"
            );
        }
        let failure = metadata(treeharbor_metadata::Error::IntegrationState);
        assert_eq!((failure.status, failure.code), (409, "review_unavailable"));
    }
}
