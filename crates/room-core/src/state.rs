//! The state store (CONTRACTS.md section 3, ROOM.md "State store").
//!
//! Records are keyed by [`Slot`] = `(scope, writer, name, class)` (room scope uses a
//! zero writer). Member-scope slots keep only the greatest version. Room-scope slots
//! keep the best record of **each epoch** that has one, so that when a claim changes
//! the holder of an epoch the store can drop that epoch's records and fall back to
//! the next-best stored version.
//!
//! The store never deletes a superseded presence-bound record just because its
//! writer went quiet: it becomes invisible (see [`Store::visible`]). Deleting it
//! would let a snapshot from a peer that still holds it resurrect it, and the flood
//! would then ping-pong. Records of a stale generation are the exception (they are
//! also rejected on arrival, so they can't come back).

use std::collections::BTreeMap;

use crate::grants::{Epoch, GrantSet, Lifecycle};
use crate::ids::Key;
use crate::staterec::{path, Class, Lifetime, Scope, StateRecord, Version, PRESENCE};

/// Per-member and room-scope caps (CONTRACTS.md section 3).
pub const MAX_KEYS: usize = 128;
pub const MAX_BYTES: usize = 64 * 1024;
/// Entries per writer including tombstones (not in the contract: bounds memory).
pub const MAX_ENTRIES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Slot {
    pub scope: Scope,
    pub writer: Key,
    pub name: String,
    pub class: Class,
}

impl Slot {
    pub fn of(r: &StateRecord) -> Slot {
        Slot {
            scope: r.scope,
            writer: if r.scope == Scope::Room {
                Key::ZERO
            } else {
                r.writer
            },
            name: r.name.clone(),
            class: r.class,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Insert {
    /// Stored: strictly greater version (or a key not held before).
    Improved,
    /// Not greater than what is held.
    Stale,
    /// Can't be validated yet (writer not a member, or epoch holder unknown).
    Orphan,
    Rejected(&'static str),
}

/// A record as subscribers see it.
#[derive(Clone, Debug, PartialEq)]
pub struct Vis {
    pub writer: Key,
    pub version: Version,
    pub class: Class,
    pub lifetime: Lifetime,
    pub value: Vec<u8>,
}

/// What the store needs to know about a writer's presence.
#[derive(Clone, Copy, Debug)]
pub struct PresView {
    pub epoch: Epoch,
    pub online: bool,
    pub since_ms: u64,
    /// Fresh or within rejoin grace (presence-bound keys of the writer are visible).
    pub live: bool,
    /// Fresh: arrived within `presence_expiry` and not marked unresponsive.
    pub fresh: bool,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub keys: usize,
    pub bytes: usize,
    pub entries: usize,
}

#[derive(Default, Clone)]
pub struct Store {
    slots: BTreeMap<Slot, BTreeMap<Epoch, StateRecord>>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.slots.values().map(|m| m.len()).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Best stored record of a slot (greatest epoch).
    pub fn best(&self, slot: &Slot) -> Option<&StateRecord> {
        self.slots.get(slot).and_then(|m| m.values().next_back())
    }

    pub fn member_best(&self, writer: Key, name: &str, class: Class) -> Option<&StateRecord> {
        self.best(&Slot {
            scope: Scope::Member,
            writer,
            name: name.to_string(),
            class,
        })
    }

    pub fn insert(&mut self, rec: StateRecord, grants: &GrantSet) -> Insert {
        if rec.lifetime == Lifetime::PresenceBound {
            if let Lifecycle::Open(g) = grants.lifecycle() {
                if rec.version.generation < g {
                    return Insert::Rejected("stale generation");
                }
            }
        }
        // A generation past the last open one can't be real yet: hold it back (it may
        // be valid once our grant set catches up) instead of storing it.
        let known = match grants.lifecycle() {
            Lifecycle::Open(g) | Lifecycle::Closed(g) => g,
            Lifecycle::Dead => 0,
        };
        if rec.version.generation > known {
            return Insert::Orphan;
        }
        match rec.scope {
            Scope::Member => {
                if !grants.is_member(&rec.writer) {
                    return Insert::Orphan;
                }
            }
            Scope::Room => match grants.holder_of(rec.version.epoch()) {
                None => return Insert::Orphan,
                Some(h) if h != rec.writer => return Insert::Rejected("not the epoch holder"),
                Some(_) => {}
            },
        }
        let slot = Slot::of(&rec);
        let epoch = rec.version.epoch();
        let old = self.slots.get(&slot).cloned();
        if let Some(m) = &old {
            let stale = match rec.scope {
                Scope::Member => m
                    .values()
                    .next_back()
                    .is_some_and(|c| rec.version <= c.version),
                Scope::Room => m.get(&epoch).is_some_and(|c| rec.version <= c.version),
            };
            if stale {
                return Insert::Stale;
            }
        }
        let (scope, writer) = (rec.scope, slot.writer);
        let m = self.slots.entry(slot.clone()).or_default();
        if scope == Scope::Member {
            m.clear();
        }
        m.insert(epoch, rec);
        let u = self.usage(scope, writer);
        if u.keys > MAX_KEYS || u.bytes > MAX_BYTES || u.entries > MAX_ENTRIES {
            match old {
                Some(o) => {
                    self.slots.insert(slot, o);
                }
                None => {
                    self.slots.remove(&slot);
                }
            }
            return Insert::Rejected("over budget");
        }
        Insert::Improved
    }

    /// Greatest seq of `writer`'s room-scope records at `epoch` (0 if none).
    pub fn max_room_seq(&self, epoch: Epoch, writer: Key) -> u64 {
        self.slots
            .iter()
            .filter(|(s, _)| s.scope == Scope::Room)
            .filter_map(|(_, m)| m.get(&epoch))
            .filter(|r| r.writer == writer)
            .map(|r| r.version.seq)
            .max()
            .unwrap_or(0)
    }

    /// Usage of one writer (member scope) or of the room scope. Only the current
    /// record of each slot counts (the best epoch's), so a writer that outlived an
    /// epoch isn't charged for the old one. Live names count toward `keys` and
    /// `entries` (their values are capped per record); only durable values count
    /// toward `bytes`.
    pub fn usage(&self, scope: Scope, writer: Key) -> Usage {
        let writer = if scope == Scope::Room {
            Key::ZERO
        } else {
            writer
        };
        let lo = Slot {
            scope,
            writer,
            name: String::new(),
            class: Class::Durable,
        };
        let mut u = Usage::default();
        for (slot, m) in self.slots.range(lo..) {
            if slot.scope != scope || slot.writer != writer {
                break;
            }
            let Some(best) = m.values().next_back() else {
                continue;
            };
            u.entries += 1;
            if !best.value.is_empty() {
                u.keys += 1;
            }
            if slot.class == Class::Durable {
                u.bytes += best.value.len();
            }
        }
        u
    }

    /// Drop every member-scope record of `writer` (it was revoked).
    pub fn purge_writer(&mut self, writer: Key) -> usize {
        let before = self.len();
        self.slots
            .retain(|s, _| !(s.scope == Scope::Member && s.writer == writer));
        before - self.len()
    }

    /// Drop room-scope records whose writer is no longer the holder of their epoch.
    pub fn revalidate_room(&mut self, grants: &GrantSet) -> usize {
        let before = self.len();
        for (s, m) in self.slots.iter_mut() {
            if s.scope == Scope::Room {
                m.retain(|e, r| grants.holder_of(*e) == Some(r.writer));
            }
        }
        self.slots.retain(|_, m| !m.is_empty());
        before - self.len()
    }

    /// Drop presence-bound records of generations older than the open one.
    pub fn prune_stale_generations(&mut self, open: u32) {
        for m in self.slots.values_mut() {
            m.retain(|e, r| !(r.lifetime == Lifetime::PresenceBound && e.generation < open));
        }
        self.slots.retain(|_, m| !m.is_empty());
    }

    /// Every durable record held (all writers, all epochs, tombstones included), in
    /// slot order. This is the snapshot.
    pub fn durable_records(&self) -> Vec<&StateRecord> {
        self.slots
            .iter()
            .filter(|(s, _)| s.class == Class::Durable)
            .flat_map(|(_, m)| m.values())
            .collect()
    }

    /// Durable persistent records (what `persist` saves).
    pub fn persistent_records(&self) -> Vec<&StateRecord> {
        self.durable_records()
            .into_iter()
            .filter(|r| r.lifetime == Lifetime::Persistent)
            .collect()
    }

    /// The currently visible records, by path.
    pub fn visible(
        &self,
        grants: &GrantSet,
        pres: &BTreeMap<Key, PresView>,
    ) -> BTreeMap<String, Vis> {
        let life = grants.lifecycle();
        let mut out = BTreeMap::new();
        for (slot, m) in &self.slots {
            let Some(rec) = m.values().next_back() else {
                continue;
            };
            if rec.value.is_empty() {
                continue;
            }
            let ok = match slot.scope {
                Scope::Member => {
                    grants.is_member(&rec.writer)
                        && match rec.lifetime {
                            Lifetime::Persistent => true,
                            Lifetime::PresenceBound => match (life, pres.get(&rec.writer)) {
                                (Lifecycle::Open(g), Some(pv)) => {
                                    pv.online
                                        && pv.live
                                        && pv.epoch == rec.version.epoch()
                                        && pv.epoch.generation == g
                                }
                                _ => false,
                            },
                        }
                }
                Scope::Room => match rec.lifetime {
                    Lifetime::Persistent => true,
                    Lifetime::PresenceBound => life == Lifecycle::Open(rec.version.generation),
                },
            };
            if ok {
                out.insert(
                    path(slot.scope, &rec.writer, &rec.name),
                    Vis {
                        writer: rec.writer,
                        version: rec.version,
                        class: rec.class,
                        lifetime: rec.lifetime,
                        value: rec.value.clone(),
                    },
                );
            }
        }
        out
    }
}

/// Is this the presence slot of a member?
pub fn is_presence(r: &StateRecord) -> bool {
    r.scope == Scope::Member && r.class == Class::Durable && r.name == PRESENCE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::Identity;
    use crate::record::build;

    fn id(b: u8) -> Identity {
        Identity::from_secret([b; 32])
    }

    /// A grant set where `root` is the only member and generation 1 is open, with
    /// claims up to term `terms` (all by root).
    fn open_set(root: &Identity, terms: u32) -> GrantSet {
        let genesis = build::genesis(root, [7; 16], 1_700_000_000_000, b"t");
        let room = genesis.room;
        let mut g = GrantSet::new(genesis);
        g.insert(build::open(root, room, 1, [1; 16], 1_700_000_000_001))
            .unwrap();
        for t in 1..=terms {
            g.insert(build::claim(root, room, 1, t, 0, 1_700_000_000_002))
                .unwrap();
        }
        g
    }

    fn rec(
        who: &Identity,
        g: &GrantSet,
        class: Class,
        scope: Scope,
        version: Version,
        name: &str,
        len: usize,
    ) -> StateRecord {
        let life = if class == Class::Live {
            Lifetime::PresenceBound
        } else {
            Lifetime::Persistent
        };
        StateRecord::build(
            who,
            g.room(),
            class,
            life,
            scope,
            version,
            name,
            &vec![1; len],
        )
        .unwrap()
    }

    fn ver(generation: u32, term: u32, seq: u64) -> Version {
        Version {
            generation,
            term,
            seq,
        }
    }

    #[test]
    fn future_generation_is_not_stored() {
        let root = id(1);
        let g = open_set(&root, 0);
        let mut s = Store::new();
        for generation in [2, 9, u32::MAX] {
            for class in [Class::Durable, Class::Live] {
                let r = rec(
                    &root,
                    &g,
                    class,
                    Scope::Member,
                    ver(generation, 1, 1),
                    "kv/a",
                    3,
                );
                assert_eq!(s.insert(r, &g), Insert::Orphan, "generation {generation}");
            }
        }
        assert!(s.is_empty());
        // The open generation and older ones still store.
        let r = rec(
            &root,
            &g,
            Class::Durable,
            Scope::Member,
            ver(1, 1, 1),
            "kv/a",
            3,
        );
        assert_eq!(s.insert(r, &g), Insert::Improved);
    }

    #[test]
    fn closed_and_dead_rooms_bound_generation_too() {
        let root = id(1);
        let mut g = open_set(&root, 0);
        g.insert(build::close(&root, g.room(), 1, 1_700_000_000_003))
            .unwrap();
        assert_eq!(g.lifecycle(), Lifecycle::Closed(1));
        let mut s = Store::new();
        let r = rec(
            &root,
            &g,
            Class::Durable,
            Scope::Member,
            ver(2, 1, 1),
            "kv/a",
            3,
        );
        assert_eq!(s.insert(r, &g), Insert::Orphan);
        let r = rec(
            &root,
            &g,
            Class::Durable,
            Scope::Member,
            ver(1, 1, 1),
            "kv/a",
            3,
        );
        assert_eq!(s.insert(r, &g), Insert::Improved);

        let dead = GrantSet::new(build::genesis(&root, [8; 16], 1, b"t"));
        let r = rec(
            &root,
            &dead,
            Class::Durable,
            Scope::Member,
            ver(1, 1, 1),
            "kv/a",
            3,
        );
        assert_eq!(Store::new().insert(r, &dead), Insert::Orphan);
    }

    #[test]
    fn budget_counts_only_the_current_epoch() {
        let root = id(1);
        let g = open_set(&root, 1);
        let mut s = Store::new();
        // 14 room keys of 4 KiB at epoch (1,0), then the same keys at (1,1): 112 KiB
        // summed over epochs, 56 KiB of current records.
        for term in [0, 1] {
            for i in 0..14 {
                let r = rec(
                    &root,
                    &g,
                    Class::Durable,
                    Scope::Room,
                    ver(1, term, 1 + i as u64),
                    &format!("k{i}"),
                    4096,
                );
                assert_eq!(s.insert(r, &g), Insert::Improved, "term {term} key {i}");
            }
        }
        let u = s.usage(Scope::Room, Key::ZERO);
        assert_eq!((u.keys, u.bytes), (14, 14 * 4096));
    }

    #[test]
    fn live_names_count_toward_the_key_cap() {
        let root = id(1);
        let g = open_set(&root, 0);
        let mut s = Store::new();
        for i in 0..MAX_KEYS {
            let r = rec(
                &root,
                &g,
                Class::Live,
                Scope::Member,
                ver(1, 1, 1),
                &format!("l{i}"),
                8,
            );
            assert_eq!(s.insert(r, &g), Insert::Improved, "live {i}");
        }
        let r = rec(
            &root,
            &g,
            Class::Live,
            Scope::Member,
            ver(1, 1, 1),
            "one-too-many",
            8,
        );
        assert_eq!(s.insert(r, &g), Insert::Rejected("over budget"));
        // Updating an existing live name is still fine, and live values don't add bytes.
        let r = rec(&root, &g, Class::Live, Scope::Member, ver(1, 1, 2), "l0", 8);
        assert_eq!(s.insert(r, &g), Insert::Improved);
        assert_eq!(s.usage(Scope::Member, root.key()).bytes, 0);
        // Durable keys share the same cap.
        let r = rec(
            &root,
            &g,
            Class::Durable,
            Scope::Member,
            ver(1, 1, 1),
            "d",
            8,
        );
        assert_eq!(s.insert(r, &g), Insert::Rejected("over budget"));
    }
}
