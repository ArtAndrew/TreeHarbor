use treeharbor_core::*;

fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn independent_v1_v2_vectors_preserve_old_identity_and_type_raw_metadata() {
    let vectors: Vec<_> = include_str!("fixtures/git-origin-v2.txt")
        .lines()
        .map(|l| {
            let (id, frame) = l.split_once(' ').unwrap();
            (
                format!("sha256:{id}").parse::<ObjectId>().unwrap(),
                bytes(frame),
            )
        })
        .collect();
    for (id, raw) in &vectors {
        let object = Object::decode_verified(*id, raw).unwrap();
        assert_eq!(object.encode().unwrap(), *raw);
        assert_eq!(object.id().unwrap(), *id);
    }
    let legacy = Commit {
        git_origin: None,
        parents: vec![],
        snapshot: vectors[1].0,
        changes: vectors[2].0,
        author_name: "Alice".into(),
        author_email: "a@example.invalid".into(),
        timestamp_seconds: 1234567890,
        timezone_minutes: -330,
        message: "message\n".into(),
    };
    assert_eq!(
        Object::NativeCommit(legacy.clone()).encode().unwrap(),
        vectors[3].1
    );
    for (blob, commit) in [(4, 5), (6, 7)] {
        let Object::NativeCommit(c) = Object::decode(&vectors[commit].1).unwrap() else {
            panic!()
        };
        let mut without = c.clone();
        without.git_origin = None;
        assert_eq!(without, legacy);
        assert_eq!(c.git_origin.as_ref().unwrap().raw, vectors[blob].0);
        assert_eq!(
            Object::NativeCommit(c).references(),
            vec![(vectors[1].0, 3), (vectors[2].0, 4), (vectors[blob].0, 1)]
        );
        let mut old = vectors[commit].1.clone();
        old[12] = 1;
        assert!(Object::decode(&old).is_err());
        let mut unknown = vectors[commit].1.clone();
        unknown[vectors[3].1.len()] = 3;
        assert!(Object::decode(&unknown).is_err());
    }
    let mut alias = vectors[3].1.clone();
    alias[12] = 2;
    assert!(Object::decode(&alias).is_err());
    for end in 0..vectors[5].1.len() {
        assert!(Object::decode(&vectors[5].1[..end]).is_err());
    }
}
