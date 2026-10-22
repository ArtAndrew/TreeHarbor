mod common;
use common::*;
use treeharbor_core::*;
use treeharbor_merge::*;

fn input(s: &Store, base: ObjectId, onto: ObjectId, result: ObjectId) -> CandidateInput {
    let base_revision = commit(s, base, vec![], "base");
    let current_revision = commit(s, onto, vec![base_revision], "onto");
    let delta = diff_snapshots(s, base, result, TreeDiffLimits::default()).unwrap();
    let applied = prepare_change(s, base, &delta).unwrap();
    CandidateInput {
        base_revision,
        base_snapshot: base,
        original_operations: applied.operations(),
        proposed_snapshot: result,
        current_revision,
        current_snapshot: onto,
        timestamp_seconds: 123,
        label: "rebase/change/1/run".into(),
    }
}
#[test]
fn rebase_is_single_parent_exact_new_delta_deterministic_and_retains_source() {
    let s = Store::default();
    let base = snapshot(&s, &plain(&[("file", "a\nb\nc\nd\ne\n")]));
    let onto = snapshot(&s, &plain(&[("file", "A\nb\nc\nd\ne\n")]));
    let result = snapshot(&s, &plain(&[("file", "a\nb\nc\nd\nE\n")]));
    let i = input(&s, base, onto, result);
    let original = s.get(i.original_operations).unwrap().encode().unwrap();
    let RebaseOutcome::Ready(r) = prepare_rebase(&s, &i, Limits::default()).unwrap() else {
        panic!()
    };
    assert_eq!(r.commit().parents, vec![i.current_revision]);
    assert!(r.commit().git_origin.is_none());
    assert_eq!(r.commit().author_name, "TreeHarbor");
    assert!(r.commit().message.contains(&i.label));
    assert!(r.profile().ends_with(PROFILE));
    assert_eq!(
        contents(&s, r.commit().snapshot),
        plain(&[("file", "A\nb\nc\nd\nE\n")])
    );
    assert_eq!(r.applied().base_snapshot(), onto);
    let Object::ChangeOperations(delta) = s.get(r.commit().changes).unwrap() else {
        panic!()
    };
    assert_eq!(
        prepare_change(&s, onto, &delta).unwrap().result_snapshot(),
        r.commit().snapshot
    );
    let RebaseOutcome::Ready(retry) = prepare_rebase(&s, &i, Limits::default()).unwrap() else {
        panic!()
    };
    assert_eq!(r.revision(), retry.revision());
    assert_eq!(
        s.get(i.original_operations).unwrap().encode().unwrap(),
        original
    );
    assert_eq!(contents(&s, result), plain(&[("file", "a\nb\nc\nd\nE\n")]));
    for which in 0..4 {
        let mut bad = i.clone();
        match which {
            0 => bad.proposed_snapshot = onto,
            1 => bad.current_snapshot = base,
            2 => bad.base_snapshot = onto,
            _ => bad.label = "untrusted\nlabel".into(),
        }
        assert!(prepare_rebase(&s, &bad, Limits::default()).is_err());
    }
    assert!(
        prepare_rebase(
            &s,
            &i,
            Limits {
                max_write_bytes: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert!(
        prepare_rebase(
            &s,
            &i,
            Limits {
                max_read_bytes: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
}
#[test]
fn conflicting_rebase_never_synthesizes_a_successful_revision() {
    let s = Store::default();
    let base = snapshot(&s, &plain(&[("file", "base\n")]));
    let onto = snapshot(&s, &plain(&[("file", "onto\n")]));
    let result = snapshot(&s, &plain(&[("file", "agent\n")]));
    let i = input(&s, base, onto, result);
    let RebaseOutcome::Conflicted(c) = prepare_rebase(&s, &i, Limits::default()).unwrap() else {
        panic!()
    };
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].kind, ConflictKind::Content);
}
#[test]
fn rebase_handles_subtrees_and_explicit_noop_with_exact_predicates() {
    let s = Store::default();
    let left = snapshot(&s, &plain(&[("a", "file")]));
    let right = snapshot(&s, &plain(&[("a/b", "child")]));
    for (base, result) in [(left, right), (right, left), (left, left)] {
        let i = input(&s, base, base, result);
        let RebaseOutcome::Ready(r) = prepare_rebase(&s, &i, Limits::default()).unwrap() else {
            panic!()
        };
        assert_eq!(r.applied().result_snapshot(), result);
        assert_eq!(r.commit().parents, vec![i.current_revision]);
        let Object::ChangeOperations(delta) = s.get(r.applied().operations()).unwrap() else {
            panic!()
        };
        assert_eq!(
            prepare_change(&s, base, &delta).unwrap().result_snapshot(),
            result
        );
    }
}

#[test]
fn rebase_budget_covers_all_phases_and_never_copies_git_origin() {
    let s = Store::default();
    let base = snapshot(&s, &plain(&[("file", "a\nb\nc\nd\ne\n")]));
    let onto = snapshot(&s, &plain(&[("file", "A\nb\nc\nd\ne\n")]));
    let result = snapshot(&s, &plain(&[("file", "a\nb\nc\nd\nE\n")]));
    let mut i = input(&s, base, onto, result);
    let Object::NativeCommit(mut parent) = s.get(i.current_revision).unwrap() else {
        panic!()
    };
    parent.git_origin = Some(Box::new(GitOrigin {
        oid: GitOid::Sha1([7; 20]),
        raw: s.put(&Object::Blob(b"original metadata".to_vec())).unwrap(),
    }));
    i.current_revision = s.put(&Object::NativeCommit(parent)).unwrap();
    s.reads.borrow_mut().clear();
    s.writes.borrow_mut().clear();
    let RebaseOutcome::Ready(r) = prepare_rebase(&s, &i, Limits::default()).unwrap() else {
        panic!()
    };
    assert!(r.commit().git_origin.is_none());
    let objects = s.objects.borrow();
    let reads: usize = s
        .reads
        .borrow()
        .iter()
        .map(|id| objects[id].encode().unwrap().len())
        .sum();
    let writes: usize = s
        .writes
        .borrow()
        .iter()
        .map(|id| objects[id].encode().unwrap().len())
        .sum();
    drop(objects);
    assert!(
        prepare_rebase(
            &s,
            &i,
            Limits {
                max_read_bytes: reads,
                max_write_bytes: writes,
                ..Limits::default()
            }
        )
        .is_ok()
    );
    for (read, write) in [(reads - 1, writes), (reads, writes - 1)] {
        assert!(matches!(
            prepare_rebase(
                &s,
                &i,
                Limits {
                    max_read_bytes: read,
                    max_write_bytes: write,
                    ..Limits::default()
                }
            ),
            Err(treeharbor_merge::Error::Core(
                treeharbor_core::Error::TooLarge
            ))
        ));
    }
}
