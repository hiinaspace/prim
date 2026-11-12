//! Adversarial authority tests, written from CONTRACTS.md section 2.

use room_core::grants::{AdmitError, Epoch, GrantSet, Lifecycle};
use room_core::ids::{Identity, Key, RecordId};
use room_core::record::{build, Kind, Record};

fn ident(n: u8) -> Identity {
    Identity::from_secret([n; 32])
}
fn nonce(n: u8) -> [u8; 16] {
    [n; 16]
}

struct World {
    gs: GrantSet,
    root: Identity,
    n: u8,
}

impl World {
    fn new() -> World {
        let root = ident(1);
        let g = build::genesis(&root, nonce(1), 1000, b"");
        World {
            gs: GrantSet::new(g),
            root,
            n: 100,
        }
    }
    fn nn(&mut self) -> [u8; 16] {
        self.n += 1;
        nonce(self.n)
    }
    fn add(&mut self, r: Record) -> Record {
        self.gs.insert(r.clone()).unwrap();
        r
    }
    fn grant(&mut self, by: &Identity, kind: Kind, subj: Key, cites: RecordId) -> Record {
        let n = self.nn();
        let r = build::grant(by, self.gs.room(), kind, subj, n, 2000, 0, 0, cites);
        self.add(r)
    }
    fn revoke(&mut self, by: &Identity, subj: Key) -> Record {
        let cites = self.gs.latest_revocation_of(&subj);
        let n = self.nn();
        let r = build::revoke(by, self.gs.room(), subj, n, 3000, cites);
        self.add(r)
    }
    fn open(&mut self, by: &Identity, g: u32) -> Record {
        let n = self.nn();
        let r = build::open(by, self.gs.room(), g, n, 2500);
        self.add(r)
    }
    fn invite(&mut self, by: &Identity, eph: &Identity, gen: u32, expiry: u64) -> Record {
        let n = self.nn();
        let r = build::grant(
            by,
            self.gs.room(),
            Kind::Invite,
            eph.key(),
            n,
            2000,
            expiry,
            gen,
            RecordId::ZERO,
        );
        self.add(r)
    }
    fn keyholder(&mut self, k: &Identity) {
        self.grant(&self.root.clone(), Kind::Keyholder, k.key(), RecordId::ZERO);
    }
}

#[test]
fn revoke_cascades_through_invite_to_member() {
    let mut w = World::new();
    let (a, e, m) = (ident(2), ident(3), ident(4));
    w.keyholder(&a);
    w.invite(&a, &e, 1, 0);
    w.grant(&e, Kind::Member, m.key(), RecordId::ZERO);
    assert!(w.gs.is_member(&m.key()));
    w.revoke(&w.root.clone(), a.key());
    assert!(!w.gs.is_keyholder(&a.key()));
    assert!(
        !w.gs.is_member(&m.key()),
        "member admitted by revoked keyholder's invite"
    );
}

#[test]
fn retire_keeps_kept_and_kills_the_rest() {
    let mut w = World::new();
    let (a, m1, m2) = (ident(2), ident(4), ident(5));
    w.keyholder(&a);
    let g1 = w.grant(&a, Kind::Member, m1.key(), RecordId::ZERO);
    let _g2 = w.grant(&a, Kind::Member, m2.key(), RecordId::ZERO);
    assert!(w.gs.is_member(&m1.key()) && w.gs.is_member(&m2.key()));
    let n = w.nn();
    let r = build::retire(
        &w.root.clone(),
        w.gs.room(),
        a.key(),
        n,
        3000,
        RecordId::ZERO,
        vec![g1.id],
    );
    w.add(r);
    assert!(!w.gs.is_keyholder(&a.key()));
    assert!(
        w.gs.is_member(&m1.key()),
        "kept member must survive the retire"
    );
    assert!(!w.gs.is_member(&m2.key()), "unlisted member must go");
}

#[test]
fn retire_keeps_invite_and_its_member() {
    let mut w = World::new();
    let (a, e1, e2, m1, m2) = (ident(2), ident(3), ident(6), ident(4), ident(5));
    w.keyholder(&a);
    let i1 = w.invite(&a, &e1, 1, 0);
    w.invite(&a, &e2, 1, 0);
    w.grant(&e1, Kind::Member, m1.key(), RecordId::ZERO);
    w.grant(&e2, Kind::Member, m2.key(), RecordId::ZERO);
    let n = w.nn();
    let r = build::retire(
        &w.root.clone(),
        w.gs.room(),
        a.key(),
        n,
        3000,
        RecordId::ZERO,
        vec![i1.id],
    );
    w.add(r);
    assert!(w.gs.is_member(&m1.key()));
    assert!(!w.gs.is_member(&m2.key()));
}

#[test]
fn readmission_must_cite_the_revocation() {
    let mut w = World::new();
    let (a, m) = (ident(2), ident(4));
    w.keyholder(&a);
    w.grant(&a, Kind::Member, m.key(), RecordId::ZERO);
    let rv = w.revoke(&a, m.key());
    assert!(!w.gs.is_member(&m.key()));
    // Re-grant that does not cite the revocation is masked.
    w.grant(&a, Kind::Member, m.key(), RecordId::ZERO);
    assert!(!w.gs.is_member(&m.key()));
    // One citing it counts.
    w.grant(&a, Kind::Member, m.key(), rv.id);
    assert!(w.gs.is_member(&m.key()));
}

#[test]
fn concurrent_unseen_revocation_still_wins() {
    let mut w = World::new();
    let (a, b, m) = (ident(2), ident(3), ident(4));
    w.keyholder(&a);
    w.keyholder(&b);
    w.grant(&a, Kind::Member, m.key(), RecordId::ZERO);
    // a revokes m; b, not having seen it, re-admits citing nothing.
    w.revoke(&a, m.key());
    w.grant(&b, Kind::Member, m.key(), RecordId::ZERO);
    assert!(
        !w.gs.is_member(&m.key()),
        "tombstone beats the unaware re-admission"
    );
}

#[test]
fn keyholder_cannot_revoke_keyholder_or_root() {
    let mut w = World::new();
    let (a, b) = (ident(2), ident(3));
    w.keyholder(&a);
    w.keyholder(&b);
    w.revoke(&a, b.key());
    assert!(w.gs.is_keyholder(&b.key()));
    let root = w.root.key();
    w.revoke(&a, root);
    assert!(w.gs.is_keyholder(&root));
    // Even root-issued revocation of root is ignored.
    w.revoke(&w.root.clone(), root);
    assert!(w.gs.is_keyholder(&root));
}

#[test]
fn only_root_grants_keyholder() {
    let mut w = World::new();
    let (a, c) = (ident(2), ident(3));
    w.keyholder(&a);
    w.grant(&a, Kind::Keyholder, c.key(), RecordId::ZERO);
    assert!(!w.gs.is_keyholder(&c.key()));
}

#[test]
fn inviter_cannot_mint_inviters_but_can_invite() {
    let mut w = World::new();
    let (a, i, j, e, m) = (ident(2), ident(3), ident(4), ident(5), ident(6));
    w.keyholder(&a);
    w.grant(&a, Kind::Inviter, i.key(), RecordId::ZERO);
    assert!(w.gs.is_inviter(&i.key()));
    w.grant(&i, Kind::Inviter, j.key(), RecordId::ZERO);
    assert!(!w.gs.is_inviter(&j.key()));
    w.invite(&i, &e, 1, 0);
    w.grant(&e, Kind::Member, m.key(), RecordId::ZERO);
    assert!(w.gs.is_member(&m.key()));
    // A non-inviter's invite is void.
    let (e2, m2) = (ident(7), ident(8));
    w.invite(&j, &e2, 1, 0);
    w.grant(&e2, Kind::Member, m2.key(), RecordId::ZERO);
    assert!(!w.gs.is_member(&m2.key()));
}

#[test]
fn revoking_a_hostile_keyholder_restores_its_victims() {
    let mut w = World::new();
    let (a, m) = (ident(2), ident(4));
    w.keyholder(&a);
    w.grant(&w.root.clone(), Kind::Member, m.key(), RecordId::ZERO);
    w.revoke(&a, m.key());
    assert!(!w.gs.is_member(&m.key()));
    w.revoke(&w.root.clone(), a.key());
    assert!(
        w.gs.is_member(&m.key()),
        "the hostile keyholder's revocation is void"
    );
}

#[test]
fn invite_equivocation_voids_both() {
    let mut w = World::new();
    let (a, e, m1, m2) = (ident(2), ident(3), ident(4), ident(5));
    w.keyholder(&a);
    w.invite(&a, &e, 1, 0);
    w.grant(&e, Kind::Member, m1.key(), RecordId::ZERO);
    assert!(w.gs.is_member(&m1.key()));
    w.grant(&e, Kind::Member, m2.key(), RecordId::ZERO);
    assert!(!w.gs.is_member(&m1.key()));
    assert!(!w.gs.is_member(&m2.key()));
}

#[test]
fn close_beats_replayed_open_and_reopen_needs_higher_generation() {
    let mut w = World::new();
    let root = w.root.clone();
    assert_eq!(w.gs.lifecycle(), Lifecycle::Dead);
    let o1 = w.open(&root, 1);
    assert_eq!(w.gs.lifecycle(), Lifecycle::Open(1));
    let n = w.nn();
    let c = build::close(&root, w.gs.room(), 1, 4000);
    let _ = n;
    w.add(c);
    assert_eq!(w.gs.lifecycle(), Lifecycle::Closed(1));
    // Replay and a fresh open at the same generation.
    assert_eq!(w.gs.insert(o1.clone()), Ok(false));
    w.open(&root, 1);
    assert_eq!(w.gs.lifecycle(), Lifecycle::Closed(1));
    w.open(&root, 2);
    assert_eq!(w.gs.lifecycle(), Lifecycle::Open(2));
}

#[test]
fn old_token_rejected_after_reopen_and_expired_invite_rejected() {
    let mut w = World::new();
    let root = w.root.clone();
    let (e, j) = (ident(30), ident(31));
    w.open(&root, 1);
    let tok = room_core::token::InviteToken::mint(
        &root,
        w.gs.room(),
        1,
        e.secret(),
        nonce(50),
        2000,
        9000,
        vec![],
        vec![],
    );
    let proof = tok.redeem(j.key(), nonce(51), 2100);
    assert!(w
        .gs
        .check_redemption(&proof[0], &proof[1], j.key(), 3000)
        .is_ok());
    assert_eq!(
        w.gs.check_redemption(&proof[0], &proof[1], j.key(), 9000)
            .err(),
        Some(AdmitError::Expired)
    );
    let c = build::close(&root, w.gs.room(), 1, 4000);
    w.add(c);
    assert_eq!(
        w.gs.check_redemption(&proof[0], &proof[1], j.key(), 5000)
            .err(),
        Some(AdmitError::Closed)
    );
    w.open(&root, 2);
    assert_eq!(
        w.gs.check_redemption(&proof[0], &proof[1], j.key(), 5000)
            .err(),
        Some(AdmitError::WrongGeneration)
    );
}

#[test]
fn record_for_another_room_rejected() {
    let mut w = World::new();
    let other = build::genesis(&w.root, nonce(99), 1000, b"");
    let other_room = GrantSet::new(other).room();
    let r = build::grant(
        &w.root,
        other_room,
        Kind::Keyholder,
        ident(9).key(),
        nonce(7),
        1,
        0,
        0,
        RecordId::ZERO,
    );
    assert!(w.gs.insert(r).is_err());
    let g2 = build::genesis(&w.root, nonce(98), 1000, b"");
    assert!(w.gs.insert(g2).is_err());
    let _ = w.nn();
}

#[test]
fn claim_tie_break_by_rank_then_key() {
    let mut w = World::new();
    let root = w.root.clone();
    let (a, b) = (ident(2), ident(3));
    w.keyholder(&a);
    w.keyholder(&b);
    w.open(&root, 1);
    let room = w.gs.room();
    // Equal rank: lower key wins.
    let ca = build::claim(&a, room, 1, 1, 10, 5000);
    let cb = build::claim(&b, room, 1, 1, 10, 5000);
    w.add(ca);
    w.add(cb);
    let low = a.key().min(b.key());
    let (e, info) = w.gs.coordinator().unwrap();
    assert_eq!(
        e,
        Epoch {
            generation: 1,
            term: 1
        }
    );
    assert_eq!(info.holder, low);
    // Lower rank wins regardless of key.
    let high = a.key().max(b.key());
    let (hid, lid) = if high == a.key() { (&a, &b) } else { (&b, &a) };
    w.add(build::claim(hid, room, 1, 2, 3, 6000));
    w.add(build::claim(lid, room, 1, 2, 9, 6000));
    let (e, info) = w.gs.coordinator().unwrap();
    assert_eq!(e.term, 2);
    assert_eq!(info.holder, high);
    assert_eq!(
        w.gs.holder_of(Epoch {
            generation: 1,
            term: 2
        }),
        Some(high)
    );
}

#[test]
fn claim_from_non_member_ignored() {
    let mut w = World::new();
    let root = w.root.clone();
    let s = ident(40);
    w.open(&root, 1);
    w.add(build::claim(&s, w.gs.room(), 1, 1, 0, 5000));
    let (e, info) = w.gs.coordinator().unwrap();
    assert_eq!(e.term, 0);
    assert_eq!(info.holder, root.key());
}

#[test]
fn self_retire_cannot_launder_grants_without_authority() {
    let mut w = World::new();
    let m = ident(30);
    let x = ident(31);
    w.grant(&w.root.clone(), Kind::Member, m.key(), RecordId::ZERO);
    // A plain member forges a member grant and an invite, then self-retires keeping them.
    let forged = w.grant(&m, Kind::Member, x.key(), RecordId::ZERO);
    let eph = ident(32);
    let forged_inv = w.invite(&m, &eph, 1, 0);
    let mut kept = vec![forged.id, forged_inv.id];
    kept.sort();
    let n = w.nn();
    let r = build::retire(&m, w.gs.room(), m.key(), n, 4000, RecordId::ZERO, kept);
    w.add(r);
    assert!(
        !w.gs.is_member(&x.key()),
        "forged member laundered through self-retire"
    );
    assert!(
        !w.gs.is_valid(&forged_inv.id),
        "forged invite laundered through self-retire"
    );
    assert!(w.gs.is_member(&m.key()));
}

#[test]
fn kept_records_die_when_the_retiree_is_later_revoked() {
    let mut w = World::new();
    let kh = ident(40);
    let x = ident(41);
    w.keyholder(&kh);
    let g = w.grant(&kh, Kind::Member, x.key(), RecordId::ZERO);
    let n = w.nn();
    let r = build::retire(
        &w.root.clone(),
        w.gs.room(),
        kh.key(),
        n,
        4000,
        RecordId::ZERO,
        vec![g.id],
    );
    let r = w.add(r);
    assert!(w.gs.is_member(&x.key()));
    let n = w.nn();
    let rv = build::revoke(&w.root.clone(), w.gs.room(), kh.key(), n, 5000, r.id);
    w.add(rv);
    assert!(
        !w.gs.is_member(&x.key()),
        "revoke must cascade past an earlier retire's kept list"
    );
}
