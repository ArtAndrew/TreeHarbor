mod common;
use common::*;
use treeharbor_core::*;
use treeharbor_merge::*;
#[test]
fn candidate_preserves_proposal_parent_and_verifiable_delta_against_current_head() {
    let store = Store::default();
    let base = snapshot(&store, &plain(&[("file", "a\nb\nc\nd\ne\n")]));
    let base_revision = commit(&store, base, vec![], "base");
    let old = Object::Blob(b"a\nb\nc\nd\ne\n".to_vec()).id().unwrap();
    let replacement = store
        .put(&Object::Blob(b"a\nb\nc\nd\nE\n".to_vec()))
        .unwrap();
    let operations = Changes::new(vec![ChangeOperation {
        path: RepoPath::new(b"file".to_vec()).unwrap(),
        expected: Some(EntryRef {
            mode: EntryMode::File,
            target: old,
        }),
        replacement: Some(EntryRef {
            mode: EntryMode::File,
            target: replacement,
        }),
    }])
    .unwrap();
    let applied = prepare_change(&store, base, &operations).unwrap();
    let current = snapshot(&store, &plain(&[("file", "A\nb\nc\nd\ne\n")]));
    let current_revision = commit(&store, current, vec![base_revision], "current");
    let input = CandidateInput {
        base_revision,
        base_snapshot: base,
        original_operations: applied.operations(),
        proposed_snapshot: applied.result_snapshot(),
        current_revision,
        current_snapshot: current,
        timestamp_seconds: 123,
        label: "entry/version/1".into(),
    };
    let CandidateOutcome::Ready(c) = prepare_candidate(&store, &input, Limits::default()).unwrap()
    else {
        panic!()
    };
    assert_eq!(
        contents(&store, c.commit().snapshot),
        plain(&[("file", "A\nb\nc\nd\nE\n")])
    );
    let Object::NativeCommit(proposal) = store.get(c.proposal_revision()).unwrap() else {
        panic!()
    };
    assert_eq!(proposal.parents, vec![base_revision]);
    assert_eq!(proposal.snapshot, applied.result_snapshot());
    assert_eq!(proposal.changes, applied.operations());
    assert_eq!(
        c.commit().parents,
        vec![current_revision, c.proposal_revision()]
    );
    assert!(c.commit().message.contains(PROFILE));
    let Object::ChangeOperations(delta) = store.get(c.commit().changes).unwrap() else {
        panic!()
    };
    assert_eq!(
        prepare_change(&store, current, &delta)
            .unwrap()
            .result_snapshot(),
        c.commit().snapshot
    );
    let CandidateOutcome::Ready(retry) =
        prepare_candidate(&store, &input, Limits::default()).unwrap()
    else {
        panic!()
    };
    assert_eq!(retry.revision(), c.revision());
    let mut bad = input.clone();
    bad.proposed_snapshot = current;
    assert!(matches!(
        prepare_candidate(&store, &bad, Limits::default()),
        Err(treeharbor_merge::Error::Core(
            treeharbor_core::Error::PredicateMismatch
        ))
    ));
    bad = input.clone();
    bad.current_snapshot = base;
    assert!(matches!(
        prepare_candidate(&store, &bad, Limits::default()),
        Err(treeharbor_merge::Error::Core(
            treeharbor_core::Error::PredicateMismatch
        ))
    ));
    assert!(
        prepare_candidate(
            &store,
            &input,
            Limits {
                max_read_bytes: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
}
