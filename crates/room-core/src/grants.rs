//! The grant set: a grow-only set of signed records whose validity is a pure
//! function of the set (CONTRACTS.md section 2, "Authority").

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::{Key, RecordId, RoomId};
use crate::record::{Body, Kind, Record};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertError {
    WrongRoom,
    SecondGenesis,
    /// The set already holds [`MAX_RECORDS`] records.
    Full,
}

/// Cap on records in one room's grant set (CONTRACTS.md section 2). A room for a
/// handful of friends needs tens; the cap bounds a hostile member's flood.
pub const MAX_RECORDS: usize = 4096;

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
        if self.records.len() >= MAX_RECORDS {
            return Err(InsertError::Full);
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
            } else if self.records.contains_key(&rec.id) {
                out.push(Ok(false));
            } else if self.records.len() >= MAX_RECORDS {
                out.push(Err(InsertError::Full));
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

    /// When `k`'s membership lapses (wall-clock ms): the latest expiry among its
    /// grants, if every grant expires. `None` for root, keyholders, non-members and
    /// members holding a grant that never expires.
    pub fn member_expiry_ms(&self, k: &Key) -> Option<u64> {
        let grants = self.auth.members.get(k)?;
        if *k == self.root || self.auth.keyholders.contains(k) || grants.is_empty() {
            return None;
        }
        let mut latest = 0;
        for id in grants {
            let e = self.records[id].grant()?.expiry_ms;
            if e == 0 {
                return None;
            }
            latest = latest.max(e);
        }
        Some(latest)
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
    /// re-admission grant's `cites_revoke`, or a record's `issuer_cites`. Picks one
    /// covering all others when the chain is linear; otherwise the lowest id. A cite
    /// is a single chain, so two concurrent revocations that neither cites can't both
    /// be covered: the subject stays out until a keyholder revokes again citing one
    /// branch (that new revocation masks too, so in practice concurrent revocations
    /// are final, which fails safe).
    pub fn latest_revocation_of(&self, subject: &Key) -> RecordId {
        let revs: Vec<&Record> = self
            .records
            .values()
            .filter(|r| {
                matches!(r.kind, Kind::Revoke | Kind::Retire)
                    && r.subject() == Some(*subject)
                    && self.auth.effective_revocations.contains(&r.id)
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
    ///
    /// Two sides, each with its own waiver:
    /// - subject side (`g` grants to the revoked key): waived when `g`'s body
    ///   `cites_revoke` covers `x`, except for member grants signed by an invite key
    ///   (the joiner holds that key, so the cite would be theirs to choose);
    /// - issuer side (`g` was issued by the revoked key, the cascade): waived when
    ///   `g.issuer_cites` covers `x`, or for a retire when `g` is in its `kept`.
    ///   The issuer must still hold, on its own, the authority `g` needs; the waiver
    ///   only stops the cascade from killing records a re-admitted key signs later.
    fn masks(&self, x: &Record, g: &Record, invite_keys: &BTreeSet<Key>) -> bool {
        if x.id == g.id {
            return false;
        }
        let rv = x.revocation().unwrap();
        let k = rv.subject;
        let subject_side = match x.kind {
            Kind::Revoke => g.kind.is_grant() && g.subject() == Some(k),
            Kind::Retire => {
                matches!(g.kind, Kind::Keyholder | Kind::Inviter) && g.subject() == Some(k)
            }
            _ => unreachable!(),
        };
        let subject_waived = !(g.kind == Kind::Member && invite_keys.contains(&g.issuer))
            && self.covers(g.cites_revoke(), x.id);
        let issuer_side = g.issuer == k;
        let issuer_waived = self.covers(g.issuer_cites, x.id)
            || (x.kind == Kind::Retire && rv.kept.binary_search(&g.id).is_ok());
        (subject_side && !subject_waived) || (issuer_side && !issuer_waived)
    }

    fn masked_by<'a>(
        &self,
        set: impl IntoIterator<Item = &'a Record>,
        g: &Record,
        invite_keys: &BTreeSet<Key>,
    ) -> bool {
        set.into_iter().any(|x| self.masks(x, g, invite_keys))
    }

    /// `g` survives its issuer's retirement: at least one retire of the issuer in
    /// `revs` exists, every one of them lists `g` in `kept` (no `issuer_cites`
    /// shortcut: a record signed after a retire can't be in that retire's list), and
    /// the issuer really held the authority `g` needs: a root-issued keyholder grant
    /// (or, for invites, a keyholder-issued inviter grant) that no revoke in `revs`
    /// masks. Without the authority check any member could sign grants and
    /// self-retire with them kept.
    fn kept_by(
        &self,
        revs: &[&Record],
        g: &Record,
        keyholders: &BTreeSet<Key>,
        invite_keys: &BTreeSet<Key>,
    ) -> bool {
        let k = g.issuer;
        let mut retires = revs
            .iter()
            .filter(|x| x.kind == Kind::Retire && x.subject() == Some(k))
            .peekable();
        if retires.peek().is_none() {
            return false;
        }
        if !retires.all(|x| x.revocation().unwrap().kept.binary_search(&g.id).is_ok()) {
            return false;
        }
        let revoked = |r: &Record| {
            revs.iter()
                .any(|x| x.kind == Kind::Revoke && self.masks(x, r, invite_keys))
        };
        let held = |kind: Kind, issuer_ok: &dyn Fn(Key) -> bool| {
            self.records.values().any(|r| {
                r.kind == kind && r.subject() == Some(k) && issuer_ok(r.issuer) && !revoked(r)
            })
        };
        held(Kind::Keyholder, &|i| i == self.root)
            || (g.kind == Kind::Invite && held(Kind::Inviter, &|i| keyholders.contains(&i)))
    }

    fn recompute(&mut self) {
        let root = self.root;
        let recs: Vec<&Record> = self.records.values().collect();
        let revs: Vec<&Record> = recs
            .iter()
            .copied()
            .filter(|r| matches!(r.kind, Kind::Revoke | Kind::Retire))
            .collect();
        let invite_keys: BTreeSet<Key> = recs
            .iter()
            .filter(|r| r.kind == Kind::Invite)
            .filter_map(|r| r.subject())
            .collect();
        let ik = &invite_keys;

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
            if !self.masked_by(stage1.iter().copied(), g, ik) {
                keyholders.insert(g.subject().unwrap());
            }
        }

        // 3. Keyholder revocations: issuer a keyholder (or a retired keyholder that
        // kept it), subject neither root nor a keyholder.
        let mut effective: Vec<&Record> = stage1.clone();
        for x in revs.iter().copied() {
            if stage1.iter().any(|s| s.id == x.id) {
                continue;
            }
            let s = x.subject().unwrap();
            let issuer_ok = (keyholders.contains(&x.issuer) && x.issuer != root)
                || self.kept_by(&stage1, x, &keyholders, ik);
            if issuer_ok
                && !keyholders.contains(&s)
                && !self.masked_by(stage1.iter().copied(), x, ik)
            {
                effective.push(x);
            }
        }
        // Stage-1 records masked by another stage-1 record don't need removal: a masked
        // self-revocation only ever concerns a subject that is revoked anyway.
        let masked = |g: &Record| self.masked_by(effective.iter().copied(), g, ik);
        let kept_ok = |g: &Record| self.kept_by(&effective, g, &keyholders, ik);

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

        // 7. Lifecycle and claims. Generations step by one: open(1); close(g) needs a
        // valid open(g); open(g+1) needs a valid close(g). Terms step by one too:
        // claim(g, t) needs a valid claim at (g, t-1), with the open as term 0. Records
        // past the chain don't count, so nobody can jump a counter to its maximum.
        let lc_ok = |r: &Record| (keyholders.contains(&r.issuer) || kept_ok(r)) && !masked(r);
        let mut open_cands: BTreeMap<u32, Vec<&Record>> = BTreeMap::new();
        let mut close_cands: BTreeMap<u32, Vec<&Record>> = BTreeMap::new();
        for r in recs.iter().copied() {
            match r.body {
                Body::Open { generation, .. } if lc_ok(r) => {
                    open_cands.entry(generation).or_default().push(r)
                }
                Body::Close { generation, .. } if lc_ok(r) => {
                    close_cands.entry(generation).or_default().push(r)
                }
                _ => {}
            }
        }
        let mut opens: BTreeMap<u32, ClaimInfo> = BTreeMap::new();
        let mut lifecycle = Lifecycle::Dead;
        let mut g = 1u32;
        while let Some(os) = open_cands.get(&g) {
            let first = os.iter().min_by_key(|r| r.id).unwrap();
            opens.insert(
                g,
                ClaimInfo {
                    holder: first.issuer,
                    rank: 0,
                    id: first.id,
                },
            );
            valid.extend(os.iter().map(|r| r.id));
            lifecycle = Lifecycle::Open(g);
            let Some(cs) = close_cands.get(&g) else { break };
            valid.extend(cs.iter().map(|r| r.id));
            lifecycle = Lifecycle::Closed(g);
            match g.checked_add(1) {
                Some(n) => g = n,
                None => break,
            }
        }

        let mut claim_cands: BTreeMap<Epoch, Vec<&Record>> = BTreeMap::new();
        for r in recs.iter().copied() {
            let Body::Claim {
                generation, term, ..
            } = r.body
            else {
                continue;
            };
            if members.contains_key(&r.issuer) && !masked(r) && opens.contains_key(&generation) {
                claim_cands
                    .entry(Epoch { generation, term })
                    .or_default()
                    .push(r);
            }
        }
        let mut claims: BTreeMap<Epoch, ClaimInfo> = BTreeMap::new();
        for &generation in opens.keys() {
            let mut term = 1u32;
            while let Some(cs) = claim_cands.get(&Epoch { generation, term }) {
                let best = cs
                    .iter()
                    .min_by_key(|r| {
                        let Body::Claim { rank, .. } = r.body else {
                            unreachable!()
                        };
                        (rank, r.issuer)
                    })
                    .unwrap();
                let Body::Claim { rank, .. } = best.body else {
                    unreachable!()
                };
                claims.insert(
                    Epoch { generation, term },
                    ClaimInfo {
                        holder: best.issuer,
                        rank,
                        id: best.id,
                    },
                );
                valid.extend(cs.iter().map(|r| r.id));
                match term.checked_add(1) {
                    Some(n) => term = n,
                    None => break,
                }
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
