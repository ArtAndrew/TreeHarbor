use super::*;

#[test]
fn inventory_wire_bounds_pages_and_never_accepts_client_coverage() {
    let v: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/restore-inventory-v1.json"
    ))
    .unwrap();
    let wire: WireCommand = serde_json::from_value(v["request"].clone()).unwrap();
    assert_eq!(wire.permission(), Permission::HostRead);
    let WireCommand::RestoreInventoryStart {
        specification: s, ..
    } = wire
    else {
        panic!()
    };
    s.validate().unwrap();
    assert_eq!(restore_digest(&s).unwrap(), v["specification_sha256"]);
    for (i, p) in v["pages"].as_array().unwrap().iter().enumerate() {
        let p: RestoreInventoryPage = serde_json::from_value(p.clone()).unwrap();
        s.validate_page(&p).unwrap();
        assert_eq!(page_key("inventory-001", i).unwrap(), v["page_keys"][i]);
        let mut invalid = p.clone();
        invalid.index += 1;
        assert!(s.validate_page(&invalid).is_err());
        let mut invalid = p.clone();
        invalid.items.clear();
        assert!(s.validate_page(&invalid).is_err());
        let mut invalid = p;
        invalid.version = 2;
        assert!(s.validate_page(&invalid).is_err());
    }
    assert_eq!(seal_key("inventory-001").unwrap(), v["seal_key"]);
    for field in [
        "inventory_complete",
        "publication_authorized",
        "source_mapping_verified",
    ] {
        let mut bad = v["request"].clone();
        bad["specification"][field] = json!(true);
        assert!(serde_json::from_value::<WireCommand>(bad).is_err());
    }
    let mut invalid = (*s).clone();
    invalid.head_count = 5001;
    assert!(invalid.validate().is_err());
    let mut invalid = (*s).clone();
    invalid.page_sha256.pop();
    assert!(invalid.validate().is_err());
    let mut invalid = (*s).clone();
    invalid.finished_at_unix += 121;
    assert!(invalid.validate().is_err());
    // Full supported manifest stays below the existing request bound; source
    // pages remain separate instead of silently truncating a large traversal.
    let mut maximum = (*s).clone();
    maximum.head_count = 5000;
    maximum.pull_count = 5000;
    maximum.page_sha256 = vec!["aa".repeat(32); 625];
    maximum.validate().unwrap();
    assert!(serde_json::to_vec(&json!({"action":"restore_inventory_start","key":"maximum","epoch":1,"specification":maximum})).unwrap().len()<crate::commands::MAX_REQUEST_BYTES);
    for revision in [
        "main",
        "git:sha1:0000000000000000000000000000000000000000",
        "git:sha256:1111",
    ] {
        assert!(git(revision).is_err());
    }
}
