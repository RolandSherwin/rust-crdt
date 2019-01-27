/// Observed-Remove Set With Out Tombstones (ORSWOT), ported directly from `riak_dt`.

use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;
use std::cmp::Ordering;
use std::mem;

use serde_derive::{Serialize, Deserialize};

use crate::traits::{CvRDT, CmRDT, Causal};
use crate::vclock::{VClock, Dot, Actor};
use crate::ctx::{ReadCtx, AddCtx, RmCtx};

/// Trait bound alias for members in a set
pub trait Member: Debug + Clone + Hash + Eq {}
impl<T: Debug + Clone + Hash + Eq> Member for T {}

/// `Orswot` is an add-biased or-set without tombstones ported from
/// the riak_dt CRDT library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Orswot<M: Member, A: Actor> {
    pub(crate) clock: VClock<A>,
    pub(crate) entries: HashMap<M, VClock<A>>,
    pub(crate) deferred: HashMap<VClock<A>, HashSet<M>>,
}

/// Op's define an edit to an Orswot, Op's must be replayed in the exact order
/// they were produced to guarantee convergence.
///
/// Op's are idempotent, that is, applying an Op twice will not have an effect
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op<M: Member, A: Actor> {
    /// Add a member to the set
    Add {
        /// witnessing dot
        dot: Dot<A>,
        /// Member to add
        member: M
    },
    /// Remove a member from the set
    Rm {
        /// witnessing clock
        clock: VClock<A>,
        /// Member to remove
        members: HashSet<M>
    }
}

impl<M: Member, A: Actor> Default for Orswot<M, A> {
    fn default() -> Self {
        Orswot::new()
    }
}

impl<M: Member, A: Actor> CmRDT for Orswot<M, A> {
    type Op = Op<M, A>;

    fn apply(&mut self, op: Self::Op) {
        match op {
            Op::Add { dot, member } => {
                if self.clock.get(&dot.actor) >= dot.counter {
                    // we've already seen this op
                    return;
                }

                let member_vclock = self.entries.entry(member)
                    .or_default();
                member_vclock.apply(dot.clone());

                self.clock.apply(dot);
                self.apply_deferred();
            },
            Op::Rm { clock, members } => {
                self.apply_rm(members, clock);
            }
        }
    }
}

impl<M: Member, A: Actor> CvRDT for Orswot<M, A> {
    /// Merge combines another `Orswot` with this one.
    fn merge(&mut self, other: &Self) {
        for (entry, clock) in other.entries.iter() {
            if let Some(our_clock) = self.entries.get_mut(&entry) {
                // SUBTLE: this entry is present in both orswots, BUT that doesn't mean we
                // shouldn't drop it!
                let mut common = clock.intersection(&our_clock);
                let mut e_clock = clock.clone();
                let mut oe_clock = our_clock.clone();
                e_clock.forget(&self.clock);
                oe_clock.forget(&other.clock);

                // Perfectly possible that an item in both sets should be dropped
                common.merge(&e_clock);
                common.merge(&oe_clock);
                if common.is_empty() {
                    // both maps had seen each others entry and removed them
                    self.entries.remove(&entry).unwrap();
                } else {
                    // we should not drop, as there is information still tracked in
                    // the common clock.
                    *our_clock = common;
                }
            } else {
                // we don't have this entry, is it because we:
                //  1. have seen it and dropped it
                //  2. have not seen it
                if &self.clock >= clock {
                    // We've seen this entry and dropped it, we won't add it back
                } else {
                    // We have not seen this version of this entry, so we add it.
                    // but first, we have to remove the information on this entry
                    // that we have seen and deleted
                    let entry = entry.clone();
                    let mut clock = clock.clone();
                    clock.forget(&self.clock);
                    self.entries.insert(entry, clock);
                }
            }
        }

        self.entries = mem::replace(&mut self.entries, HashMap::new())
            .into_iter()
            .filter_map(|(entry, mut clock)| {
                if !other.entries.contains_key(&entry) {
                    // other doesn't contain this entry because it:
                    //  1. has seen it and dropped it
                    //  2. hasn't seen it
                    if other.clock >= clock {
                        // other has seen this entry and dropped it
                        None
                    } else {
                        // the other map has not seen this version of this
                        // entry, so add it. But first, we have to remove any
                        // information that may have been known at some point
                        // by the other map about this key and was removed.
                        clock.forget(&other.clock);
                        Some((entry, clock))
                    }
                } else {
                    Some((entry, clock))
                }
            })
            .collect();

        // merge deferred removals
        for (rm_clock, members) in other.deferred.iter() {
            self.apply_rm(members.clone(), rm_clock.clone());
        }

        self.clock.merge(&other.clock);

        self.apply_deferred();
//        
//        let mut other_remaining = other.entries.clone();
//        let mut keep = HashMap::new();
//        for (entry, mut clock) in self.entries.clone().into_iter() {
//            match other.entries.get(&entry).cloned() {
//                None => {
//                    // other doesn't contain this entry because it:
//                    //  1. has witnessed it and dropped it
//                    //  2. hasn't witnessed it
//                    if clock <= other.clock {
//                        // other has seen this entry and dropped it
//                    } else {
//                        // other has not seen this entry, so add it
//                        keep.insert(entry, clock);
//                    }
//                }
//                Some(mut other_entry_clock) => {
//                    // SUBTLE: this entry is present in both orswots, BUT that doesn't mean we
//                    // shouldn't drop it!
//
//                    let mut common = clock.intersection(&other_entry_clock);
//                    clock.forget(&common);
//                    other_entry_clock.forget(&common);
//                    clock.forget(&other.clock);
//                    other_entry_clock.forget(&self.clock);
//
//                    common.merge(&clock);
//                    common.merge(&other_entry_clock);
//
//                    // Perfectly possible that an item present in both sets
//                    // is dropped
//                    if common.is_empty() {
//                        // we should drop, there are no common dots
//                    } else {
//                        // we should not drop, as there are common clocks
//                        keep.insert(entry.clone(), common);
//                    }
//                    // don't want to consider this again below
//                    other_remaining.remove(&entry).unwrap();
//                }
//            }
//        }
//
//        for (entry, mut clock) in other_remaining.into_iter() {
//            clock.forget(&self.clock);
//            if !clock.is_empty() {
//                // other has witnessed a novel addition, so add it
//                keep.insert(entry, clock);
//            }
//        }
//
//        // merge deferred removals
//        for (clock, deferred) in other.deferred.iter() {
//            let mut our_deferred =
//                self.deferred.remove(&clock).unwrap_or_default();
//            for e in deferred.iter() {
//                our_deferred.insert(e.clone());
//            }
//            self.deferred.insert(clock.clone(), our_deferred);
//        }
//
//        self.entries = keep;
//
//        // merge vclocks
//        self.clock.merge(&other.clock);
//
//        self.apply_deferred();
    }
}

impl<M: Member, A: Actor> Causal<A> for Orswot<M, A> {
    fn forget(&mut self, clock: &VClock<A>) {
        self.clock.forget(&clock);

        self.entries = self.entries
            .clone()
            .into_iter()
            .filter_map(|(val, mut val_clock)| {
                val_clock.forget(&clock);
                if val_clock.is_empty() {
                    None
                } else {
                    Some((val, val_clock))
                }
            }).collect();

        self.deferred = self.deferred
            .clone()
            .into_iter()
            .filter_map(|(mut vclock, deferred)| {
                vclock.forget(&clock);
                if vclock.is_empty() {
                    None
                } else {
                    Some((vclock, deferred))
                }
            }).collect();
    }
}

impl<M: Member, A: Actor> Orswot<M, A> {
    /// Returns a new `Orswot` instance.
    pub fn new() -> Self {
        Orswot {
            clock: VClock::new(),
            entries: HashMap::new(),
            deferred: HashMap::new(),
        }
    }

    /// Add a single element.
    pub fn add(&self, member: impl Into<M>, ctx: AddCtx<A>) -> Op<M, A> {
        Op::Add { dot: ctx.dot, member: member.into() }
    }

    /// Remove a member with a witnessing ctx.
    pub fn rm(&self, member: impl Into<M>, ctx: RmCtx<A>) -> Op<M, A> {
        let mut members = HashSet::new();
        members.insert(member.into());
        Op::Rm { clock: ctx.clock, members }
    }

    /// Remove a member using a witnessing clock.
    fn apply_rm(&mut self, members: HashSet<M>, clock: VClock<A>) {
        for member in members.iter() {
            if let Some(member_clock) = self.entries.get_mut(&member) {
                member_clock.forget(&clock);
                if member_clock.is_empty() {
                    self.entries.remove(&member);
                }
            }
        }

        match clock.partial_cmp(&self.clock) {
            None | Some(Ordering::Greater) => {
                if let Some(existing_deferred) = self.deferred.get_mut(&clock) {
                    existing_deferred.extend(members);
                } else {
                    self.deferred.insert(clock, members);
                }
                // let deferred_drops = self.deferred.entry(clock)
                //     .or_default();
                // for member in members {
                //     deferred_drops.insert(member);
                // }
                // deferred_drops.extend(members);
            }
            _ => {/* we've already seen this remove */}
        }
    }

    /// Check if the set contains a member
    pub fn contains(&self, member: &M) -> ReadCtx<bool, A> {
        let member_clock_opt = self.entries.get(&member);
        let exists = member_clock_opt.is_some();
        ReadCtx {
            add_clock: self.clock.clone(),
            rm_clock: member_clock_opt
                .cloned()
                .unwrap_or_default(),
            val: exists
        }
    }

    /// Retrieve the current members.
    pub fn read(&self) -> ReadCtx<HashSet<M>, A> {
        ReadCtx {
            add_clock: self.clock.clone(),
            rm_clock: self.clock.clone(),
            val: self.entries.keys().cloned().collect()
        }
    }

    fn apply_deferred(&mut self) {
        let deferred = mem::replace(&mut self.deferred, HashMap::new());
        for (clock, entries) in deferred.into_iter() {
            self.apply_rm(entries, clock)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate rand;

    #[test]
    // a bug found with rust quickcheck where deferred operations
    // are not carried over after a merge.
    // symptoms:
    //  if nothing is added, it works
    //  if removed elem is added first, it only misses one
    //  if non-related elem is added, it misses both
    fn ensure_deferred_merges() {
        let mut a = Orswot::<String, u8>::new();
        let mut b = Orswot::<String, u8>::new();

        b.apply(b.add("element 1", b.read().derive_add_ctx(5)));

        // remove with a future context
        b.apply(b.rm("element 1", RmCtx { clock: Dot::new(5, 4).into() }));

        a.apply(a.add("element 4", a.read().derive_add_ctx(6)));
        
        // remove with a future context
        b.apply(b.rm("element 9", RmCtx { clock: Dot::new(4, 4).into() }));

        let mut merged = Orswot::new();
        merged.merge(&a);
        merged.merge(&b);
        merged.merge(&Orswot::new());
        assert_eq!(merged.deferred.len(), 2);
    }

    // a bug found with rust quickcheck where deferred removals
    // were not properly preserved across merges.
    #[test]
    fn preserve_deferred_across_merges() {
        let mut a = Orswot::<u8, u8>::new();
        let mut b = a.clone();
        let mut c = a.clone();

        // add element 5 from witness 1
        a.apply(a.add(5, a.read().derive_add_ctx(1)));

        // on another clock, remove 5 with an advanced clock for witnesses 1 and 4
        let mut vc = VClock::new();
        vc.apply_dot(Dot::new(1, 3));
        vc.apply_dot(Dot::new(4, 8));

        // remove from b (has not yet seen add for 5) with advanced ctx
        b.apply(b.rm(5, RmCtx { clock: vc }));
        assert_eq!(b.deferred.len(), 1);

        // ensure that the deferred elements survive across a merge
        c.merge(&b);
        assert_eq!(c.deferred.len(), 1);

        // after merging the set with deferred elements with the set that contains
        // an inferior member, ensure that the member is no longer visible and
        // the deferred set still contains this info
        a.merge(&c);
        assert!(a.read().val.is_empty());
    }

    // port from riak_dt
    // Bug found by EQC, not dropping dots in merge when an element is
    // present in both Sets leads to removed items remaining after merge.
    #[test]
    fn test_present_but_removed() {
        let mut a = Orswot::<u8, String>::new();
        let mut b = Orswot::<u8, String>::new();

        a.apply(a.add(0, a.read().derive_add_ctx("A".to_string())));

        // Replicate it to C so A has 0->{a, 1}
        let c = a.clone();

        a.apply(a.rm(0, a.contains(&0).derive_rm_ctx()));
        assert_eq!(a.deferred.len(), 0);

        b.apply(b.add(0, b.read().derive_add_ctx("B".to_string())));

        // Replicate B to A, so now A has a 0
        // the one with a Dot of {b,1} and clock
        // of [{a, 1}, {b, 1}]
        a.merge(&b);

        b.apply(b.rm(0, b.contains(&0).derive_rm_ctx()));

        // Both C and A have a '0', but when they merge, there should be
        // no '0' as C's has been removed by A and A's has been removed by
        // C.
        a.merge(&b);
        a.merge(&c);
        assert!(a.read().val.is_empty());
    }
}
