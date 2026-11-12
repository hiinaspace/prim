//! Golden wire vectors (CONTRACTS.md). A mismatch is a wire break: regenerate with
//! `PRIM_UPDATE_VECTORS=1 cargo test -p room-core --test golden` only on purpose.

use std::collections::BTreeMap;

use room_core::frame::{caps, AckStatus, Datagram, Frame, Hello, Reason, Target};
use room_core::grants::GrantSet;
use room_core::record::{build, Kind, Record};
use room_core::staterec::{Class, Lifetime, Presence, Scope, StateRecord, Version};
use room_core::token::InviteToken;
use room_core::wire::hex;
use room_core::{Identity, RecordId};

const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/golden.json");

fn id(b: u8) -> Identity {
    Identity::from_secret([b; 32])
}

fn vectors() -> BTreeMap<String, String> {
    let (root, keyholder, member, eph, joiner) = (id(1), id(2), id(3), id(4), id(5));
    let mut v = BTreeMap::new();
    let mut put = |k: &str, bytes: &[u8]| {
        v.insert(k.to_string(), hex(bytes));
    };

    let genesis = build::genesis(&root, [0xa1; 16], 1_700_000_000_000, b"living room");
    let room = genesis.room;
    put("genesis", &genesis.bytes);
    put("genesis.room_id", &room.0);
    put("genesis.record_id", &genesis.id.0);
    put("identity.root.public", &root.key().0);

    let kh = build::grant(
        &root,
        room,
        Kind::Keyholder,
        keyholder.key(),
        [0xb1; 16],
        1_700_000_001_000,
        0,
        0,
        RecordId::ZERO,
    );
    put("grant.keyholder", &kh.bytes);
    put("grant.keyholder.record_id", &kh.id.0);
    let inv = build::grant(
        &root,
        room,
        Kind::Inviter,
        member.key(),
        [0xb2; 16],
        1_700_000_002_000,
        0,
        0,
        RecordId::ZERO,
    );
    put("grant.inviter", &inv.bytes);
    let mem = build::grant(
        &keyholder,
        room,
        Kind::Member,
        member.key(),
        [0xb3; 16],
        1_700_000_003_000,
        1_800_000_000_000,
        0,
        RecordId::ZERO,
    );
    put("grant.member", &mem.bytes);
    let open = build::open(&root, room, 1, [0xc1; 16], 1_700_000_004_000);
    put("open", &open.bytes);
    let close = build::close(&keyholder, room, 1, 1_700_000_005_000);
    put("close", &close.bytes);
    let claim = build::claim(&member, room, 1, 1, 1_700_000_003_500, 1_700_000_006_000);
    put("claim", &claim.bytes);
    let revoke = build::revoke(
        &keyholder,
        room,
        member.key(),
        [0xd1; 16],
        1_700_000_007_000,
        RecordId::ZERO,
    );
    put("revoke", &revoke.bytes);
    put("revoke.record_id", &revoke.id.0);
    let readmit = build::grant(
        &keyholder,
        room,
        Kind::Member,
        member.key(),
        [0xb4; 16],
        1_700_000_008_000,
        0,
        0,
        revoke.id,
    );
    put("grant.member.readmit", &readmit.bytes);
    let retire = build::retire(
        &root,
        room,
        keyholder.key(),
        [0xd2; 16],
        1_700_000_009_000,
        RecordId::ZERO,
        vec![mem.id],
    );
    put("retire", &retire.bytes);

    let token = InviteToken::mint(
        &member,
        room,
        1,
        eph.secret(),
        [0xe1; 16],
        1_700_000_010_000,
        1_700_086_400_000,
        vec![member.key(), root.key()],
        vec!["https://relay.example.net./".to_string()],
    );
    put("invite", &token.invite.bytes);
    let token_text = token.encode();
    let [_, redeemed] = token.redeem(joiner.key(), [0xe2; 16], 1_700_000_011_000);
    put("grant.member.redeemed", &redeemed.bytes);

    let presence = Presence {
        since_ms: 1_700_000_004_100,
        daemon_incarnation: 0x0102_0304_0506_0708,
    }
    .encode();
    let durable = StateRecord::build(
        &member,
        room,
        Class::Durable,
        Lifetime::PresenceBound,
        Scope::Member,
        Version {
            generation: 1,
            term: 7,
            seq: 42,
        },
        "presence",
        &presence,
    )
    .unwrap();
    put("state.presence", &durable.bytes);
    let room_state = StateRecord::build(
        &root,
        room,
        Class::Durable,
        Lifetime::Persistent,
        Scope::Room,
        Version {
            generation: 1,
            term: 0,
            seq: 3,
        },
        "activity",
        b"cowatch",
    )
    .unwrap();
    put("state.room.activity", &room_state.bytes);
    let live = StateRecord::build(
        &member,
        room,
        Class::Live,
        Lifetime::PresenceBound,
        Scope::Member,
        Version {
            generation: 1,
            term: 7,
            seq: 9000,
        },
        "pose",
        &[0x11; 24],
    )
    .unwrap();
    put("state.live.pose", &live.bytes);

    let hello = Frame::Hello(Hello {
        version: 1,
        caps: caps::R1,
        room,
        member: joiner.key(),
        daemon_incarnation: 0x1122_3344_5566_7788,
        open_generation: 1,
        proof: vec![token.invite.bytes.clone(), redeemed.bytes.clone()],
    });
    put("frame.hello", &hello.encode());
    put(
        "frame.reject",
        &Frame::Reject {
            reason: Reason::NotMember,
            detail: b"who?".to_vec(),
        }
        .encode(),
    );
    put(
        "frame.records",
        &Frame::Records(vec![kh.bytes.clone(), open.bytes.clone()]).encode(),
    );
    put("frame.records_end", &Frame::RecordsEnd.encode());
    put(
        "frame.snapshot_begin",
        &Frame::SnapshotBegin { count: 2 }.encode(),
    );
    put("frame.state", &Frame::State(durable.bytes.clone()).encode());
    put("frame.snapshot_end", &Frame::SnapshotEnd.encode());
    put(
        "frame.message",
        &Frame::Message {
            msg_id: 77,
            channel: 0x0501,
            flags: 1,
            target: Target::Coordinator,
            payload: b"play".to_vec(),
        }
        .encode(),
    );
    put(
        "frame.message_ack",
        &Frame::MessageAck {
            msg_id: 77,
            status: AckStatus::NoChannel,
        }
        .encode(),
    );
    put(
        "frame.goodbye",
        &Frame::Goodbye {
            reason: Reason::Leave,
        }
        .encode(),
    );
    put(
        "datagram.live",
        &Datagram::LiveState(live.bytes.clone()).encode(),
    );
    put(
        "datagram.app",
        &Datagram::App {
            channel: 0x0601,
            seq: 0xfffe,
            send_ms: 0xffff_fff0,
            payload: vec![1, 2, 3],
        }
        .encode(),
    );
    v.insert("token".into(), token_text);
    v
}

#[test]
fn golden_vectors() {
    let got = vectors();
    if std::env::var_os("PRIM_UPDATE_VECTORS").is_some() {
        std::fs::write(PATH, serde_json::to_string_pretty(&got).unwrap() + "\n").unwrap();
        return;
    }
    let want: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(PATH).unwrap()).unwrap();
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        want.keys().collect::<Vec<_>>(),
        "vector names changed"
    );
    for (k, w) in &want {
        assert_eq!(&got[k], w, "wire break in vector {k}");
    }
}

fn unhex(s: &str) -> Vec<u8> {
    data_encoding::HEXLOWER.decode(s.as_bytes()).unwrap()
}

/// Decoding the stored vectors (not our encoder's output) round-trips byte-exact.
#[test]
fn golden_vectors_decode() {
    let want: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(PATH).unwrap()).unwrap();
    let genesis = Record::decode(&unhex(&want["genesis"])).unwrap();
    assert_eq!(hex(&genesis.room.0), want["genesis.room_id"]);
    let room = genesis.room;
    for k in [
        "grant.keyholder",
        "grant.inviter",
        "grant.member",
        "open",
        "close",
        "claim",
        "revoke",
        "retire",
        "invite",
    ] {
        let b = unhex(&want[k]);
        let r = Record::decode(&b).unwrap_or_else(|e| panic!("{k}: {e}"));
        assert_eq!(r.bytes, b);
        assert_eq!(r.room, room);
    }
    for k in ["state.presence", "state.room.activity", "state.live.pose"] {
        let b = unhex(&want[k]);
        assert_eq!(StateRecord::decode(room, &b).unwrap().bytes, b, "{k}");
    }
    for (k, v) in want.iter().filter(|(k, _)| k.starts_with("frame.")) {
        let b = unhex(v);
        assert_eq!(Frame::decode(&b).unwrap().encode(), b, "{k}");
    }
    for k in ["datagram.live", "datagram.app"] {
        let b = unhex(&want[k]);
        assert_eq!(Datagram::decode(&b).unwrap().encode(), b, "{k}");
    }
    let t = InviteToken::decode(&want["token"]).unwrap();
    assert_eq!(t.encode(), want["token"]);
    // A durable state record signed for this room doesn't verify in another room.
    let other = room_core::RoomId([9; 32]);
    assert!(StateRecord::decode(other, &unhex(&want["state.presence"])).is_err());
    // The whole set evaluates as the contract says.
    let mut set = GrantSet::new(genesis);
    for k in ["grant.keyholder", "grant.member", "open"] {
        set.insert(Record::decode(&unhex(&want[k])).unwrap())
            .unwrap();
    }
    assert!(set.is_member(&id(3).key()));
}

/// Strictness: a flipped byte anywhere breaks decoding or the signature.
#[test]
fn tamper_rejected() {
    let want: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(PATH).unwrap()).unwrap();
    for k in ["grant.member", "revoke", "retire", "genesis"] {
        let b = unhex(&want[k]);
        for i in 0..b.len() {
            let mut t = b.clone();
            t[i] ^= 0x01;
            assert!(Record::decode(&t).is_err(), "{k}: flip at {i} accepted");
        }
        let mut t = b.clone();
        t.push(0);
        assert!(Record::decode(&t).is_err(), "{k}: trailing byte accepted");
    }
    let mut tok = want["token"].clone();
    let last = tok.pop().unwrap();
    tok.push(if last == 'a' { 'b' } else { 'a' });
    assert!(InviteToken::decode(&tok).is_err());
}
