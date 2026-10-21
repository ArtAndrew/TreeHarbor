use treeharbor_core::{MAX_PROFILE_BYTES, Object, RepoPath, SourceProfile};
fn path(raw: &[u8]) -> RepoPath {
    RepoPath::new(raw.to_vec()).unwrap()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn profiles_match_independent_canonical_frames_and_hashes() {
    // Independent Python struct/hashlib vectors, anchored to profile v1.
    for (profile, frame, digest) in [
        (
            SourceProfile::full(),
            "74726565686172626f722d70726f66696c650000010000000000",
            "f6d053964fd79fd002aae9f952200e9c60cb51dfc2a14eca69b931d3f94d7dae",
        ),
        (
            SourceProfile::sparse(vec![path(b"src"), path(b"\xff.rs")]).unwrap(),
            "74726565686172626f722d70726f66696c6500000101000000020000000373726300000004ff2e7273",
            "18ad403f8e4063898c0c049530b1da7501992e13dba015793984f4dd893e06d7",
        ),
    ] {
        assert_eq!(hex(&profile.encode()), frame);
        assert_eq!(profile.id().to_string(), format!("sha256:{digest}"));
        assert_eq!(SourceProfile::decode(&profile.encode()).unwrap(), profile);
        assert!(Object::decode(&profile.encode()).is_err());
        assert_eq!(
            profile
                .id()
                .to_string()
                .parse::<treeharbor_core::ProfileId>()
                .unwrap(),
            profile.id()
        );
    }
}

#[test]
fn sparse_normalization_is_component_based_and_not_an_empty_full_fallback() {
    let profile = SourceProfile::sparse(vec![
        path(b"src/a"),
        path(b"src-other"),
        path(b"src"),
        path(b"src"),
        path(b"z/\xff"),
    ])
    .unwrap();
    assert_eq!(
        profile.paths().unwrap(),
        &[path(b"src"), path(b"src-other"), path(b"z/\xff")]
    );
    let equivalent =
        SourceProfile::sparse(vec![path(b"z/\xff"), path(b"src-other"), path(b"src")]).unwrap();
    assert_eq!(profile.id(), equivalent.id());
    assert_ne!(profile.id(), SourceProfile::full().id());
    assert!(SourceProfile::sparse(vec![]).is_err());
    assert!(SourceProfile::sparse(vec![path(b"src"); 1025]).is_err());
}

#[test]
fn profile_decoder_rejects_truncation_noncanonical_paths_and_resource_overflow() {
    let sparse = SourceProfile::sparse(vec![path(b"src"), path(b"z")])
        .unwrap()
        .encode();
    for end in 0..sparse.len() {
        assert!(SourceProfile::decode(&sparse[..end]).is_err());
    }
    let mut trailing = sparse.clone();
    trailing.push(0);
    assert!(SourceProfile::decode(&trailing).is_err());
    let mut version = sparse.clone();
    version[20] = 2;
    assert!(SourceProfile::decode(&version).is_err());
    let mut count = sparse.clone();
    count[22..26].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(SourceProfile::decode(&count).is_err());
    let mut zero = SourceProfile::full().encode();
    zero[21] = 1;
    assert!(SourceProfile::decode(&zero).is_err());
    let mut redundant = SourceProfile::full().encode();
    redundant[21] = 1;
    redundant[22..26].copy_from_slice(&2u32.to_be_bytes());
    for raw in [b"src".as_slice(), b"src/a"] {
        redundant.extend_from_slice(&(raw.len() as u32).to_be_bytes());
        redundant.extend_from_slice(raw);
    }
    assert!(SourceProfile::decode(&redundant).is_err());
    assert!(SourceProfile::decode(&vec![0; MAX_PROFILE_BYTES + 1]).is_err());
    let large = path(&[b'a'; 200]);
    assert!(SourceProfile::sparse(vec![large; 400]).is_err());
}
