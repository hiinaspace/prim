//! The grant set: a grow-only set of signed records whose validity is a pure
//! function of the set (CONTRACTS.md section 2, "Authority").

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::{Key, RecordId, RoomId};
use crate::record::{Body, Kind, Record};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertError {
    WrongRoom,
    SecondGenesis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitError {
    NotMember,
    Expired,
    Closed,
    WrongGeneration,
    InviteInvalid,
    InviteSpent,
    BadProof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// Never opened.
    Dead,
    Open(u32),
    Closed(u32),
}

/// The coordinator epoch: `(generation, term)` and its holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Epoch {
    pub generation: u32,
    pub term: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimInfo {
    pub holder: Key,
    pub rank: u64,
    pub id: RecordId,
}

/// Everything derived from the set. Recomputed on each insert (the set is small).
#[derive(Debug, Clone, Default)]
pub struct Authority {
    pub keyholders: BTreeSet<Key>,
    pub inviters: BTreeSet<Key>,
    /// Valid invite grants by ephemeral key.
    pub invites: BTreeMap<Key, Vec<RecordId>>,
    /// Invite keys that issued more than one member grant.
    pub equivocated: BTreeSet<Key>,
    /// Members (keyholders included) and the valid grants that make them members.
    /// Keyholders and root map to an empty list when they have no member grant.
    pub members: BTreeMap<Key, Vec<RecordId>>,
    /// Revocations and retirements that take effect.
    pub effective_revocations: BTreeSet<RecordId>,
    /// Every record that currently counts.
    pub valid: BTreeSet<RecordId>,
    pub lifecycle: Option<Lifecycle>,
    /// Lowest-id valid open per generation: the implicit term-0 claim.
    pub opens: BTreeMap<u32, ClaimInfo>,
    /// Best claim of each (generation, term), term ≥ 1.
    pub claims: BTreeMap<Epoch, ClaimInfo>,
}

#[derive(Debug, Clone)]
pub struct GrantSet {
    room: RoomId,
    root: Key,
    genesis: Record,
    records: BTreeMap<RecordId, Record>,
    auth: Authority,
}

impl GrantSet {
    pub fn new(genesis: Record) -> Self {
        assert_eq!(
            genesis.kind,
            Kind::Genesis,
            "GrantSet::new needs a genesis record"
        );
        let mut s = Self {
            room: genesis.room,
            root: genesis.issuer,
            genesis,
            records: BTreeMap::new(),
            auth: Authority::default(),
        };
        s.recompute();
        s
    }

    pub fn room(&self) -> RoomId {
        self.room
    }
    pub fn root(&self) -> Key {
        self.root
    }
    pub fn genesis(&self) -> &Record {
        &self.genesis
    }
    pub fn authority(&self) -> &Authority {
        &self.auth
    }
    pub fn get(&self, id: &RecordId) -> Option<&Record> {
        self.records.get(id)
    }
    pub fn contains(&self, id: &RecordId) -> bool {
        *id == self.genesis.id || self.records.contains_key(id)
    }
    /// All records (genesis first), for snapshots and persistence.
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        std::iter::once(&self.genesis).chain(self.records.values())
    }
    pub fn len(&self) -> usize {
        self.records.len() + 1
    }
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Add a record. `Ok(true)` if it was new.
    pub fn insert(&mut self, rec: Record) -> Result<bool, InsertError> {
        if rec.kind == Kind::Genesis {
            return if rec.id == self.genesis.id {
                Ok(false)
            } else {
                Err(InsertError::SecondGenesis)
            };
        }
        if rec.room != self.room {
            return Err(InsertError::WrongRoom);
        }
        if self.records.contains_key(&rec.id) {
            return Ok(false);
        }
        self.records.insert(rec.id, rec);
        self.recompute();
        Ok(true)
    }

    /// Add several records and recompute once. One result per input, in order.
    pub fn insert_all(
        &mut self,
        recs: impl IntoIterator<Item = Record>,
    ) -> Vec<Result<bool, InsertError>> {
        let mut changed = false;
        let mut out = Vec::new();
        for rec in recs {
            if rec.kind == Kind::Genesis {
                out.push(if rec.id == self.genesis.id {
                    Ok(false)
                } else {
                    Err(InsertError::SecondGenesis)
                });
            } else if rec.room != self.room {
                out.push(Err(InsertError::WrongRoom));
            } else if let std::collections::btree_map::Entry::Vacant(e) = self.records.entry(rec.id)
            {
                e.insert(rec);
                changed = true;
                out.push(Ok(true));
            } else {
                out.push(Ok(false));
            }
        }
        if changed {
            self.recompute();
        }
        out
    }

    pub fn is_valid(&self, id: &RecordId) -> bool {
        self.auth.valid.contains(id)
    }
    pub fn is_member(&self, k: &Key) -> bool {
        self.auth.members.contains_key(k)
    }
    pub fn is_keyholder(&self, k: &Key) -> bool {
        self.auth.keyholders.contains(k)
    }
    pub fn is_inviter(&self, k: &Key) -> bool {
        self.auth.inviters.contains(k)
    }
    pub fn lifecycle(&self) -> Lifecycle {
        self.auth.lifecycle.unwrap_or(Lifecycle::Dead)
    }

    /// Member at wall-clock `unix_ms`: set-valid and holding an unexpired grant
    /// (root and keyholders need none).
    pub fn is_member_at(&self, k: &Key, unix_ms: u64) -> bool {
        let Some(grants) = self.auth.members.get(k) else {
            return false;
        };
        if *k == self.root || self.auth.keyholders.contains(k) {
            return true;
        }
        grants.iter().any(|id| {
            let g = self.records[id].grant().unwrap();
            g.expiry_ms == 0 || unix_ms < g.expiry_ms
        })
    }

    /// Members at `unix_ms`, sorted.
    pub fn roster(&self, unix_ms: u64) -> Vec<Key> {
        self.auth
            .members
            .keys()
            .filter(|k| self.is_member_at(k, unix_ms))
            .copied()
            .collect()
    }

    /// Admission check for a connecting peer that is already a member.
    pub fn admit(&self, k: &Key, unix_ms: u64) -> Result<(), AdmitError> {
        match self.lifecycle() {
            Lifecycle::Open(_) => {}
            _ => return Err(AdmitError::Closed),
        }
        if !self.is_member(k) {
            return Err(AdmitError::NotMember);
        }
        if !self.is_member_at(k, unix_ms) {
            return Err(AdmitError::Expired);
        }
        Ok(())
    }

    /// Admission of an invite redemption: `[invite, member]` from a hello proof.
    /// On success returns the set with both records added.
    pub fn check_redemption(
        &self,
        invite: &Record,
        member: &Record,
        joiner: Key,
        unix_ms: u64,
    ) -> Result<GrantSet, AdmitError> {
        if invite.kind != Kind::Invite || member.kind != Kind::Member {
            return Err(AdmitError::BadProof);
        }
        let (ig, mg) = (invite.grant().unwrap(), member.grant().unwrap());
        if member.issuer != ig.subject
            || mg.subject != joiner
            || invite.room != self.room
            || member.room != self.room
        {
            return Err(AdmitError::BadProof);
        }
        match self.lifecycle() {
            Lifecycle::Open(g) if g == ig.generation => {}
            Lifecycle::Open(_) => return Err(AdmitError::WrongGeneration),
            _ => return Err(AdmitError::Closed),
        }
        if ig.expiry_ms != 0 && unix_ms >= ig.expiry_ms {
            return Err(AdmitError::Expired);
        }
        let spent = self
            .records
            .values()
            .any(|r| r.kind == Kind::Member && r.issuer == ig.subject && r.id != member.id);
        if spent {
            return Err(AdmitError::InviteSpent);
        }
        let mut next = self.clone();
        let _ = next.insert(invite.clone());
        let _ = next.insert(member.clone());
        if !next.auth.invites.contains_key(&ig.subject) {
            return Err(AdmitError::InviteInvalid);
        }
        if !next
            .auth
            .members
            .get(&joiner)
            .is_some_and(|g| g.contains(&member.id))
        {
            return Err(AdmitError::InviteInvalid);
        }
        Ok(next)
    }

    /// Current coordinator epoch and holder for the open generation, if open.
    pub fn coordinator(&self) -> Option<(Epoch, ClaimInfo)> {
        let Lifecycle::Open(g) = self.lifecycle() else {
            return None;
        };
        self.best_claim(g)
    }

    /// Best claim of the highest term in generation `g` (term 0 = the open).
    pub fn best_claim(&self, g: u32) -> Option<(Epoch, ClaimInfo)> {
        let explicit = self
            .auth
            .claims
            .range(
                Epoch {
                    generation: g,
                    term: 0,
                }..=Epoch {
                    generation: g,
                    term: u32::MAX,
                },
            )
            .next_back()
            .map(|(e, c)| (*e, *c));
        explicit.or_else(|| {
            self.auth.opens.get(&g).map(|c| {
                (
                    Epoch {
                        generation: g,
                        term: 0,
                    },
                    *c,
                )
            })
        })
    }

    /// Holder of the best claim of exactly `epoch`, if any. Room-scope state written
    /// at `epoch` is valid only from this key.
    pub fn holder_of(&self, epoch: Epoch) -> Option<Key> {
        if epoch.term == 0 {
            return self.auth.opens.get(&epoch.generation).map(|c| c.holder);
        }
        self.auth.claims.get(&epoch).map(|c| c.holder)
    }

    /// The latest revocation of `subject` known here, for a new revocation's or a
    /// re-admission grant's `cites_revoke`. Picks one covering all others when the
    /// chain is linear; otherwise the lowest id (a re-admitter must cite each
    /// concurrent branch by issuing after it sees them all).
    pub fn latest_revocation_of(&self, subject: &Key) -> RecordId {
        let revs: Vec<&Record> = self
            .records
            .values()
            .filter(|r| {
                matches!(r.kind, Kind::Revoke | Kind::Retire) && r.subject() == Some(*subject)
            })
            .collect();
        for cand in &revs {
            if revs.iter().all(|x| self.covers(cand.id, x.id)) {
                return cand.id;
            }
        }
        revs.first().map(|r| r.id).unwrap_or(RecordId::ZERO)
    }

    /// `covers(c, x)`: `c == x`, or `c` is a revocation in the set whose cited chain covers `x`.
    pub fn covers(&self, mut c: RecordId, x: RecordId) -> bool {
        // Ids are hashes, so a chain can't loop; the bound guards against a bug anyway.
        for _ in 0..=self.records.len() {
            if c.is_zero() {
                return false;
            }
            if c == x {
                return true;
            }
            match self.records.get(&c) {
                Some(r) if matches!(r.kind, Kind::Revoke | Kind::Retire) => c = r.cites_revoke(),
                _ => return false,
            }
        }
        false
    }

    /// Does effective revocation `x` mask record `g`?
    fn masks(&self, x: &Record, g: &Record) -> bool {
        if x.id == g.id {
            return false;
        }
        let rv = x.revocation().unwrap();
        let k = rv.subject;
        let in_scope = match x.kind {
            Kind::Revoke => (g.kind.is_grant() && g.subject() == Some(k)) || g.issuer == k,
            Kind::Retire => {
                (matches!(g.kind, Kind::Keyholder | Kind::Inviter) && g.subject() == Some(k))
                    || (g.issuer == k && rv.kept.binary_search(&g.id).is_err())
            }
            _ => unreachable!(),
        };
        in_scope && !self.covers(g.cites_revoke(), x.id)
    }

    fn masked_by<'a>(&self, set: impl IntoIterator<Item = &'a Record>, g: &Record) -> bool {
        set.into_iter().any(|x| self.masks(x, g))
    }

    fn recompute(&mut self) {
        let root = self.root;
        let recs: Vec<&Record> = self.records.values().collect();
        let revs: Vec<&Record> = recs
            .iter()
            .copied()
            .filter(|r| matches!(r.kind, Kind::Revoke | Kind::Retire))
            .collect();

        // 1. Root and self revocations; never against root.
        let stage1: Vec<&Record> = revs
            .iter()
            .copied()
            .filter(|x| {
                let s = x.subject().unwrap();
                s != root && (x.issuer == root || x.issuer == s)
            })
            .collect();

        // 2. Keyholders: root-issued keyholder grants not masked by stage 1.
        let mut keyholders: BTreeSet<Key> = BTreeSet::from([root]);
        for g in recs
            .iter()
            .filter(|r| r.kind == Kind::Keyholder && r.issuer == root)
        {
            if !self.masked_by(stage1.iter().copied(), g) {
                keyholders.insert(g.subject().unwrap());
            }
        }

        // 3. Keyholder revocations: issuer a keyholder, subject neither root nor a keyholder.
        let mut effective: Vec<&Record> = stage1.clone();
        for x in revs.iter().copied() {
            if stage1.iter().any(|s| s.id == x.id) {
                continue;
            }
            let s = x.subject().unwrap();
            if keyholders.contains(&x.issuer)
                && x.issuer != root
                && !keyholders.contains(&s)
                && !self.masked_by(stage1.iter().copied(), x)
            {
                effective.push(x);
            }
        }
        // Stage-1 records masked by another stage-1 record don't need removal: a masked
        // self-revocation only ever concerns a subject that is revoked anyway.
        let masked = |g: &Record| self.masked_by(effective.iter().copied(), g);
        // A retire lists records its subject issued that stay valid even though the
        // subject no longer holds the authority that issued them.
        // Only authority the subject really held is preserved: a root-issued keyholder
        // grant (or, for invites, a keyholder-issued inviter grant) that no *revoke*
        // masks. Without this, any member could sign grants and self-retire with them
        // in `kept`.
        let revoked = |g: &Record| {
            effective
                .iter()
                .any(|x| x.kind == Kind::Revoke && self.masks(x, g))
        };
        let held = |k: Key, kind: Kind, issuer_ok: &dyn Fn(Key) -> bool| {
            recs.iter().any(|r| {
                r.kind == kind && r.subject() == Some(k) && issuer_ok(r.issuer) && !revoked(r)
            })
        };
        let kept_ok = |g: &Record| {
            let k = g.issuer;
            let listed = effective.iter().any(|x| {
                x.kind == Kind::Retire
                    && x.subject() == Some(k)
                    && x.revocation().unwrap().kept.binary_search(&g.id).is_ok()
            });
            listed
                && (held(k, Kind::Keyholder, &|i| i == root)
                    || (g.kind == Kind::Invite
                        && held(k, Kind::Inviter, &|i| keyholders.contains(&i))))
        };

        let mut valid: BTreeSet<RecordId> = BTreeSet::from([self.genesis.id]);
        valid.extend(effective.iter().map(|x| x.id));
        for g in recs
            .iter()
            .filter(|r| r.kind == Kind::Keyholder && r.issuer == root)
        {
            if keyholders.contains(&g.subject().unwrap()) && !masked(g) {
                valid.insert(g.id);
            }
        }

        // 4. Inviters (issued by keyholders only: non-transitive).
        let mut inviters = BTreeSet::new();
        for g in recs.iter().filter(|r| r.kind == Kind::Inviter) {
            if (keyholders.contains(&g.issuer) || kept_ok(g)) && !masked(g) {
                inviters.insert(g.subject().unwrap());
                valid.insert(g.id);
            }
        }

        // 5. Invites.
        let mut invites: BTreeMap<Key, Vec<RecordId>> = BTreeMap::new();
        for g in recs.iter().filter(|r| r.kind == Kind::Invite) {
            if (keyholders.contains(&g.issuer) || inviters.contains(&g.issuer) || kept_ok(g))
                && !masked(g)
            {
                invites.entry(g.subject().unwrap()).or_default().push(g.id);
                valid.insert(g.id);
            }
        }

        // 6. Members. An invite key may issue one member grant ever.
        let mut by_invite_key: BTreeMap<Key, usize> = BTreeMap::new();
        for g in recs.iter().filter(|r| r.kind == Kind::Member) {
            *by_invite_key.entry(g.issuer).or_default() += 1;
        }
        let equivocated: BTreeSet<Key> = by_invite_key
            .iter()
            .filter(|(k, n)| **n > 1 && invites.contains_key(k))
            .map(|(k, _)| *k)
            .collect();
        let mut members: BTreeMap<Key, Vec<RecordId>> =
            keyholders.iter().map(|k| (*k, Vec::new())).collect();
        for g in recs.iter().filter(|r| r.kind == Kind::Member) {
            let issuer_ok = keyholders.contains(&g.issuer)
                || kept_ok(g)
                || (invites.contains_key(&g.issuer) && !equivocated.contains(&g.issuer));
            if issuer_ok && !masked(g) {
                members.entry(g.subject().unwrap()).or_default().push(g.id);
                valid.insert(g.id);
            }
        }

        // 7. Lifecycle and claims.
        let mut opens: BTreeMap<u32, ClaimInfo> = BTreeMap::new();
        let mut max_close = 0u32;
        for r in recs.iter().copied() {
            if !matches!(r.kind, Kind::Open | Kind::Close)
                || !(keyholders.contains(&r.issuer) || kept_ok(r))
                || masked(r)
            {
                continue;
            }
            valid.insert(r.id);
            match r.body {
                Body::Open { generation, .. } => {
                    let e = opens.entry(generation).or_insert(ClaimInfo {
                        holder: r.issuer,
                        rank: 0,
                        id: r.id,
                    });
                    if r.id < e.id {
                        *e = ClaimInfo {
                            holder: r.issuer,
                            rank: 0,
                            id: r.id,
                        };
                    }
                }
                Body::Close { generation, .. } => max_close = max_close.max(generation),
                _ => unreachable!(),
            }
        }
        let max_open = opens.keys().next_back().copied().unwrap_or(0);
        let lifecycle = if max_open == 0 && max_close == 0 {
            Lifecycle::Dead
        } else if max_close >= max_open {
            Lifecycle::Closed(max_close)
        } else {
            Lifecycle::Open(max_open)
        };

        let mut claims: BTreeMap<Epoch, ClaimInfo> = BTreeMap::new();
        for r in recs.iter().filter(|r| r.kind == Kind::Claim) {
            let Body::Claim {
                generation,
                term,
                rank,
                ..
            } = r.body
            else {
                unreachable!()
            };
            if !members.contains_key(&r.issuer) || masked(r) || !opens.contains_key(&generation) {
                continue;
            }
            valid.insert(r.id);
            let info = ClaimInfo {
                holder: r.issuer,
                rank,
                id: r.id,
            };
            let e = claims.entry(Epoch { generation, term }).or_insert(info);
            if (rank, r.issuer) < (e.rank, e.holder) {
                *e = info;
            }
        }

        self.auth = Authority {
            keyholders,
            inviters,
            invites,
            equivocated,
            members,
            effective_revocations: effective.iter().map(|x| x.id).collect(),
            valid,
            lifecycle: Some(lifecycle),
            opens,
            claims,
        };
    }
}
