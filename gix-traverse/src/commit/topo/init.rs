use gix_error::Result;
use std::collections::HashSet;

use gix_error::{OptionExt, ResultExt};
use gix_hash::{ObjectId, oid};
use gix_revwalk::{PriorityQueue, graph::IdMap};

use crate::commit::{
    Info, Parents, Topo, find,
    topo::{Sorting, WalkFlags, iter::gen_and_commit_time},
};

/// Builder for [`Topo`].
pub struct Builder<Find, Predicate> {
    commit_graph: Option<gix_commitgraph::Graph>,
    find: Find,
    predicate: Predicate,
    sorting: Sorting,
    parents: Parents,
    tips: Vec<ObjectId>,
    tips_seen: HashSet<ObjectId>,
    ends: Vec<ObjectId>,
    admission: Option<std::sync::Arc<super::admission::State>>,
}

impl<Find> Builder<Find, fn(&oid) -> bool>
where
    Find: gix_object::Find,
{
    /// Create a new `Builder` for a [`Topo`] that reads commits from a repository with `find`.
    /// starting at the `tips` and ending at the `ends`. Like `git rev-list
    /// --topo-order ^ends tips`.
    pub fn from_iters(
        find: Find,
        tips: impl IntoIterator<Item = impl Into<ObjectId>>,
        ends: Option<impl IntoIterator<Item = impl Into<ObjectId>>>,
    ) -> Self {
        Self::new(find).with_tips(tips).with_ends(ends.into_iter().flatten())
    }

    /// Create a new `Builder` for a [`Topo`] that reads commits from a
    /// repository with `find`.
    pub fn new(find: Find) -> Self {
        Self {
            commit_graph: Default::default(),
            find,
            sorting: Default::default(),
            parents: Default::default(),
            tips: Default::default(),
            tips_seen: Default::default(),
            ends: Default::default(),
            predicate: |_| true,
            admission: None,
        }
    }

    /// Set a `predicate` to filter out revisions from the walk. Can be used to
    /// implement e.g. filtering on paths or time. This does *not* exclude the
    /// parent(s) of a revision that is excluded. Specify a revision as an 'end'
    /// if you want that behavior.
    pub fn with_predicate<Predicate>(self, predicate: Predicate) -> Builder<Find, Predicate>
    where
        Predicate: FnMut(&oid) -> bool,
    {
        Builder {
            commit_graph: self.commit_graph,
            find: self.find,
            sorting: self.sorting,
            parents: self.parents,
            tips: self.tips,
            tips_seen: self.tips_seen,
            ends: self.ends,
            predicate,
            admission: self.admission,
        }
    }
}

impl<Find, Predicate> Builder<Find, Predicate>
where
    Find: gix_object::Find,
    Predicate: FnMut(&oid) -> bool,
{
    /// Add commits to start reading from.
    ///
    /// The behavior is similar to specifying additional `ends` in `git rev-list --topo-order ^ends tips`.
    pub fn with_tips(mut self, tips: impl IntoIterator<Item = impl Into<ObjectId>>) -> Self {
        for tip in tips.into_iter().map(Into::into) {
            if self.tips_seen.insert(tip) {
                self.tips.push(tip);
            }
        }
        self
    }

    /// Add commits ending the traversal.
    ///
    /// These commits themselves will not be read, i.e. the behavior is similar to specifying additional
    /// `ends` in `git rev-list --topo-order ^ends tips`.
    pub fn with_ends(mut self, ends: impl IntoIterator<Item = impl Into<ObjectId>>) -> Self {
        self.ends.extend(ends.into_iter().map(Into::into));
        self
    }

    /// Set the `sorting` to use for the topological walk.
    pub fn sorting(mut self, sorting: Sorting) -> Self {
        self.sorting = sorting;
        self
    }

    /// Specify how to handle commit `parents` during traversal.
    pub fn parents(mut self, parents: Parents) -> Self {
        self.parents = parents;
        self
    }

    /// Set or unset the `commit_graph` to use for the iteration.
    pub fn with_commit_graph(mut self, commit_graph: Option<gix_commitgraph::Graph>) -> Self {
        self.commit_graph = commit_graph;
        self
    }

    /// Pre-admit a complete physical traversal before allocating its maps, queues
    /// or Find buffer. The controlled profile excludes commit-graph shortcuts and
    /// permits at most 1,024 tips/ends so initial sorting is a bounded operation.
    /// `Find` must honor `limits.buffer_bytes` without growing the supplied Vec.
    pub fn with_admission(mut self, budget: std::sync::Arc<dyn super::Budget>, limits: super::Limits) -> Result<Self> {
        self.admission = Some(std::sync::Arc::new(super::admission::State::new(budget, limits)?));
        Ok(self)
    }

    /// Build a new [`Topo`] instance.
    ///
    /// Note that merely building an instance is currently expensive.
    pub fn build(self) -> Result<Topo<Find, Predicate>> {
        if self.admission.is_some() {
            return Err(gix_error::validation("use build_admitted to retain parent-list admission").into());
        }
        self.build_inner()
    }

    /// Build an iterator whose returned values retain admission for owned parent
    /// lists even after the iterator itself has been destroyed.
    pub fn build_admitted(self) -> Result<super::AdmittedTopo<Find, Predicate>> {
        let admission = self.admission.clone().ok_or_raise(|| gix_error::validation("topo admission is required"))?;
        let inner = self.build_inner()?;
        Ok(super::AdmittedTopo { inner, admission })
    }

    fn build_inner(self) -> Result<Topo<Find, Predicate>> {
        if self.admission.is_some() && (self.commit_graph.is_some() || self.tips.len() + self.ends.len() > 1024) {
            return Err(gix_error::validation("controlled topo requires physical reads and bounded tips").into());
        }
        let mut w = Topo {
            commit_graph: self.commit_graph,
            find: self.find,
            predicate: self.predicate,
            indegrees: IdMap::default(),
            states: IdMap::default(),
            explore_queue: PriorityQueue::new(),
            indegree_queue: PriorityQueue::new(),
            topo_queue: super::iter::Queue::new(self.sorting),
            parents: self.parents,
            min_gen: gix_commitgraph::GENERATION_NUMBER_INFINITY,
            buf: vec![],
            admission: self.admission,
        };

        if let Some(admission) = &w.admission {
            admission.check()?;
            let limits = admission.limits;
            w.indegrees.try_reserve(limits.commits)
                .or_raise(|| gix_error::message("could not allocate admitted indegrees"))?;
            w.states.try_reserve(limits.commits)
                .or_raise(|| gix_error::message("could not allocate admitted walk states"))?;
            w.explore_queue.try_reserve_exact(limits.commits)
                .or_raise(|| gix_error::message("could not allocate admitted exploration queue"))?;
            w.indegree_queue.try_reserve_exact(limits.commits)
                .or_raise(|| gix_error::message("could not allocate admitted indegree queue"))?;
            w.topo_queue.try_reserve_exact(limits.commits)?;
            w.buf.try_reserve_exact(limits.buffer_bytes)
                .or_raise(|| gix_error::message("could not allocate admitted commit buffer"))?;
            admission.check()?;
        }

        // Initial flags for the states of the tips and ends. All of them are
        // seen and added to the explore and indegree queues. The ends are by
        // definition (?) uninteresting and bottom.
        let tip_flags = WalkFlags::Seen | WalkFlags::Explored | WalkFlags::InDegree;
        let end_flags = tip_flags | WalkFlags::Uninteresting | WalkFlags::Bottom;

        for (id, flags) in self
            .tips
            .iter()
            .map(|id| (id, tip_flags))
            .chain(self.ends.iter().map(|id| (id, end_flags)))
        {
            if let Some(state) = w.states.get_mut(id) {
                // Repeated ends and a tip that is also an end are one physical
                // commit. They must not grow the queues past the admitted count.
                *state |= flags;
                continue;
            }
            super::admission::room(w.admission.as_deref(), &w.indegrees, id)?;
            *w.indegrees.entry(*id).or_default() = 1;
            let commit = find(w.commit_graph.as_ref(), &w.find, id, &mut w.buf)?;
            let (generation, time) = gen_and_commit_time(commit, w.admission.as_deref())?;

            if generation < w.min_gen {
                w.min_gen = generation;
            }

            super::admission::room(w.admission.as_deref(), &w.states, id)?;
            w.states.insert(*id, flags);
            w.explore_queue.insert((generation, time), *id);
            w.indegree_queue.insert((generation, time), *id);
        }

        // NOTE: Parents of the ends must also be marked uninteresting for some
        // reason. See handle_commit()
        for id in &self.ends {
            let parents = w.collect_all_parents(id)?;
            for parent in parents {
                super::admission::room(w.admission.as_deref(), &w.states, &parent.commit_id)?;
                w.states
                    .entry(parent.commit_id)
                    .and_modify(|s| *s |= WalkFlags::Uninteresting)
                    .or_insert(WalkFlags::Uninteresting | WalkFlags::Seen);
            }
        }

        w.compute_indegrees_to_depth(w.min_gen)?;

        // NOTE: in Git the ends are also added to the topo_queue in addition to
        // the tips, but then in simplify_commit() Git is told to ignore it. For
        // now the tests pass.
        for id in self.tips.iter() {
            let i = w
                .indegrees
                .get(id)
                .ok_or_raise(|| gix_error::corruption("Indegree information is missing"))?;

            if *i != 1 || w.states.get(id).is_some_and(|state| state.contains(WalkFlags::Uninteresting)) {
                continue;
            }

            let commit = find(w.commit_graph.as_ref(), &w.find, id, &mut w.buf)?;
            let (generation, time) = gen_and_commit_time(commit, w.admission.as_deref())?;
            let parent_ids = w
                .collect_all_parents(id)?
                .into_iter()
                .map(|parent| parent.commit_id)
                .collect();

            w.topo_queue.push(
                time,
                Info {
                    id: *id,
                    parent_ids,
                    generation: (generation != gix_commitgraph::GENERATION_NUMBER_INFINITY).then_some(generation),
                    commit_time: Some(time),
                },
            );
        }

        w.topo_queue.initial_sort();
        if let Some(admission) = &w.admission { admission.check()?; }
        Ok(w)
    }
}
