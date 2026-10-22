//! Native DAG queries. All commit parents participate in ancestry; timestamps
//! and object ordering do not imply ancestry. No Git process or host adapter.

use std::collections::{BTreeMap, BTreeSet};
use treeharbor_core::{Commit, Error, Object, ObjectId, ObjectStore, Result};

pub struct History<'a, S> {
    store: &'a S,
    max_commits: usize,
}

impl<'a, S: ObjectStore> History<'a, S> {
    pub fn new(store: &'a S, max_commits: usize) -> Self {
        Self { store, max_commits }
    }

    pub fn commit(&self, id: ObjectId) -> Result<Commit> {
        match self.store.get(id)? {
            Object::NativeCommit(commit) => Ok(commit),
            _ => Err(Error::WrongKind),
        }
    }

    pub fn root(&self, id: ObjectId) -> Result<ObjectId> {
        let commit = self.commit(id)?;
        match self.store.get(commit.snapshot)? {
            Object::Snapshot { root } => match self.store.get(root)? {
                Object::Directory(_) => Ok(root),
                _ => Err(Error::WrongKind),
            },
            _ => Err(Error::WrongKind),
        }
    }

    fn collect(&self, roots: &[ObjectId]) -> Result<BTreeMap<ObjectId, Commit>> {
        let mut graph = BTreeMap::new();
        let mut pending: BTreeSet<_> = roots.iter().copied().collect();
        while let Some(id) = pending.pop_first() {
            if graph.contains_key(&id) {
                continue;
            }
            if graph.len() >= self.max_commits {
                return Err(Error::TooLarge);
            }
            let commit = self.commit(id)?;
            pending.extend(
                commit
                    .parents
                    .iter()
                    .filter(|id| !graph.contains_key(*id))
                    .copied(),
            );
            if pending.len().saturating_add(graph.len()) > self.max_commits {
                return Err(Error::TooLarge);
            }
            graph.insert(id, commit);
        }
        Ok(graph)
    }

    pub fn ancestors(&self, head: ObjectId) -> Result<BTreeSet<ObjectId>> {
        Ok(self.collect(&[head])?.into_keys().collect())
    }

    /// Complete reverse-topological history: a child precedes every parent.
    /// Equal-ready nodes use ascending raw IDs, never timestamps. Exhausting the
    /// traversal budget is an error, not a silently truncated history.
    pub fn topological_log(&self, head: ObjectId) -> Result<Vec<(ObjectId, Commit)>> {
        let mut graph = self.collect(&[head])?;
        let total = graph.len();
        let mut children: BTreeMap<_, usize> = graph.keys().map(|id| (*id, 0)).collect();
        for commit in graph.values() {
            for parent in &commit.parents {
                *children
                    .get_mut(parent)
                    .ok_or(Error::MissingObject(*parent))? += 1;
            }
        }
        let mut ready: BTreeSet<_> = children
            .iter()
            .filter_map(|(id, n)| (*n == 0).then_some(*id))
            .collect();
        let mut result = Vec::with_capacity(total);
        while let Some(id) = ready.pop_first() {
            let commit = graph.remove(&id).ok_or(Error::MissingObject(id))?;
            for parent in &commit.parents {
                let n = children
                    .get_mut(parent)
                    .ok_or(Error::MissingObject(*parent))?;
                *n -= 1;
                if *n == 0 {
                    ready.insert(*parent);
                }
            }
            result.push((id, commit));
        }
        if result.len() != total {
            return Err(Error::InvalidObject("cyclic commit graph"));
        }
        Ok(result)
    }

    pub fn is_ancestor(&self, ancestor: ObjectId, head: ObjectId) -> Result<bool> {
        // Missing IDs must never turn into an authoritative negative answer.
        self.commit(ancestor)?;
        Ok(self.ancestors(head)?.contains(&ancestor))
    }

    /// Returns every best common ancestor, including multiple bases in
    /// criss-cross history. A caller must synthesize a valid virtual base or
    /// report unsupported history; it must not arbitrarily pick one result.
    pub fn merge_bases(&self, left: ObjectId, right: ObjectId) -> Result<Vec<ObjectId>> {
        let graph = self.collect(&[left, right])?;
        fn walk(
            graph: &BTreeMap<ObjectId, Commit>,
            roots: impl IntoIterator<Item = ObjectId>,
        ) -> BTreeSet<ObjectId> {
            let mut seen = BTreeSet::new();
            let mut pending: BTreeSet<_> = roots.into_iter().collect();
            while let Some(id) = pending.pop_first() {
                if seen.insert(id) {
                    pending.extend(
                        graph[&id]
                            .parents
                            .iter()
                            .copied()
                            .filter(|id| !seen.contains(id)),
                    );
                }
            }
            seen
        }
        let a = walk(&graph, [left]);
        let b = walk(&graph, [right]);
        let common: BTreeSet<_> = a.intersection(&b).copied().collect();
        let dominated = walk(
            &graph,
            common
                .iter()
                .flat_map(|id| graph[id].parents.iter().copied()),
        );
        Ok(common.difference(&dominated).copied().collect())
    }

    /// A first-parent view is explicit; it never replaces full-DAG traversal
    /// for reachability, backup, GC, or merge-base computation.
    pub fn first_parent_log(
        &self,
        head: ObjectId,
        limit: usize,
    ) -> Result<Vec<(ObjectId, Commit)>> {
        if limit > self.max_commits {
            return Err(Error::TooLarge);
        }
        let mut result = Vec::new();
        let mut next = Some(head);
        while let Some(id) = next {
            if result.len() == limit {
                break;
            }
            let commit = self.commit(id)?;
            next = commit.parents.first().copied();
            result.push((id, commit));
        }
        Ok(result)
    }
}
