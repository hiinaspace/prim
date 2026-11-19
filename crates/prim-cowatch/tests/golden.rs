//! Golden vectors for the wire formats. Each vector is assembled by hand from the layout
//! documented in `wire.rs`, not by calling `encode`, so a change to the layout fails here.
use prim_cowatch::wire::*;

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}
const HOLDER: [u8; 32] = [0xab; 32];
// generation 1, term 2 (u32 LE each), then the 32-byte holder key.
fn key_bytes() -> Vec<u8> {
    cat(&[&[1, 0, 0, 0], &[2, 0, 0, 0], &HOLDER])
}
fn key() -> AuthKey {
    AuthKey {
        generation: 1,
        term: 2,
        holder: HOLDER,
    }
}

#[test]
fn channel_numbers_are_fixed() {
    assert_eq!((CH_CONTROL, CH_PROBE), (101, 102));
    assert_eq!((NAME_PLAYBACK, NAME_CAP), ("playback", "cowatch"));
}

#[test]
fn intent_v1() {
    let i = Intent {
        rev: 3,
        paused: true,
        live: false,
        rate_milli: 1000,
        position_ms: 90_000,
        ref_us: 5_000_000,
        duration_ms: 3_600_000,
        kind: SourceKind::Url,
        source: b"ab".to_vec(),
    };
    let want = cat(&[
        &[1],                               // version
        &[3, 0, 0, 0],                      // rev
        &[0b01],                            // flags: paused
        &[0xe8, 0x03],                      // rate 1000
        &[0x90, 0x5f, 1, 0, 0, 0, 0, 0],    // 90_000 = 0x015f90
        &[0x40, 0x4b, 0x4c, 0, 0, 0, 0, 0], // 5_000_000 = 0x4c4b40
        &[0x80, 0xee, 0x36, 0, 0, 0, 0, 0], // 3_600_000 = 0x36ee80
        &[1],                               // kind url
        &[2, 0, b'a', b'b'],                // source, u16 length
    ]);
    assert_eq!(i.encode(), want);
    assert_eq!(Intent::decode(&want).unwrap(), i);
}

#[test]
fn cap_v1() {
    let want = cat(&[&[1], &[0x39, 0x30, 0, 0, 0, 0, 0, 0], &[0]]); // 12345
    assert_eq!(Cap { since_ms: 12345 }.encode(), want);
    assert_eq!(Cap::decode(&want).unwrap(), Cap { since_ms: 12345 });
}

#[test]
fn control_request_v1() {
    // seek to 2 s, pause, and a source: every field present.
    let c = Control::Request {
        req_id: 0x0102_0304,
        key: key(),
        body: Body {
            source: Some((SourceKind::File, b"x".to_vec())),
            paused: Some(true),
            seek_ms: Some(2000),
        },
    };
    let want = cat(&[
        &[0x11],       // version 1, kind 1
        &[4, 3, 2, 1], // req_id
        &key_bytes(),
        &[0b1111], // flags: source | has-paused | paused | has-seek
        &[0xd0, 0x07, 0, 0, 0, 0, 0, 0],
        &[2, 1, 0, b'x'], // kind file, u16 length, bytes
    ]);
    assert_eq!(c.encode(), want);
    assert_eq!(Control::decode(&want).unwrap(), c);
    // Only "resume": has-paused set, paused clear.
    let c = Control::Request {
        req_id: 1,
        key: key(),
        body: Body {
            paused: Some(false),
            ..Default::default()
        },
    };
    let want = cat(&[&[0x11], &[1, 0, 0, 0], &key_bytes(), &[0b10]]);
    assert_eq!(c.encode(), want);
    assert_eq!(Control::decode(&want).unwrap(), c);
}

#[test]
fn control_reply_v1() {
    let c = Control::Reply {
        req_id: 9,
        status: ReplyStatus::Stale,
        rev: 0x0a0b,
    };
    let want = cat(&[&[0x12], &[9, 0, 0, 0], &[3], &[0x0b, 0x0a, 0, 0]]);
    assert_eq!(c.encode(), want);
    assert_eq!(Control::decode(&want).unwrap(), c);
}

#[test]
fn probe_v1() {
    let ping = Probe::Ping {
        id: 7,
        key: key(),
        t1_us: 0x0102,
    };
    let want = cat(&[
        &[0x11],
        &[7, 0, 0, 0],
        &key_bytes(),
        &[2, 1, 0, 0, 0, 0, 0, 0],
    ]);
    assert_eq!(ping.encode(), want);
    assert_eq!(Probe::decode(&want).unwrap(), ping);
    let pong = Probe::Pong {
        id: 7,
        key: key(),
        t1_us: 1,
        t2_us: 2,
        t3_us: 3,
    };
    let want = cat(&[
        &[0x12],
        &[7, 0, 0, 0],
        &key_bytes(),
        &[1, 0, 0, 0, 0, 0, 0, 0],
        &[2, 0, 0, 0, 0, 0, 0, 0],
        &[3, 0, 0, 0, 0, 0, 0, 0],
    ]);
    assert_eq!(pong.encode(), want);
    assert_eq!(Probe::decode(&want).unwrap(), pong);
}

#[test]
fn decoders_are_strict() {
    let good = Control::Reply {
        req_id: 1,
        status: ReplyStatus::Applied,
        rev: 1,
    }
    .encode();
    // Every truncation fails; trailing bytes fail; a future version and unknown kinds fail.
    for n in 0..good.len() {
        assert!(Control::decode(&good[..n]).is_err(), "prefix {n}");
    }
    let mut t = good.clone();
    t.push(0);
    assert_eq!(Control::decode(&t), Err(DecodeError::Trailing));
    let mut v = good.clone();
    v[0] = 0x22;
    assert_eq!(Control::decode(&v), Err(DecodeError::Version));
    let mut k = good.clone();
    k[0] = 0x1f;
    assert_eq!(Control::decode(&k), Err(DecodeError::Kind));
    let mut s = good.clone();
    s[5] = 9;
    assert!(Control::decode(&s).is_err());
    // Intent: bad version, reserved flag bits, zero rate, kind/source mismatch.
    let i = Intent {
        rev: 1,
        paused: false,
        live: false,
        rate_milli: 1000,
        position_ms: 0,
        ref_us: 0,
        duration_ms: 0,
        kind: SourceKind::None,
        source: vec![],
    }
    .encode();
    assert!(Intent::decode(&i).is_ok());
    let mut b = i.clone();
    b[0] = 2;
    assert_eq!(Intent::decode(&b), Err(DecodeError::Version));
    let mut b = i.clone();
    b[5] = 4;
    assert!(Intent::decode(&b).is_err());
    let mut b = i.clone();
    b[6] = 0;
    b[7] = 0;
    assert!(Intent::decode(&b).is_err(), "zero rate");
    let mut b = i.clone();
    b[26 + 8] = 1; // kind url with an empty source
    assert!(Intent::decode(&b).is_err());
    // A paused flag without "has paused" is not a request.
    let r = cat(&[&[0x11], &[1, 0, 0, 0], &key_bytes(), &[0b100]]);
    assert!(Control::decode(&r).is_err());
}
