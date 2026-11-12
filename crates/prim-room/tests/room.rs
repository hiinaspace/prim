//! Three daemons end to end through their local sockets: found, invite, join twice,
//! state visible across nodes, messages, revoke. The same scenario runs on the
//! in-memory network and on real iroh over loopback.
//!
//! These need a working `room_core::room::Room`.

#[macro_use]
mod common;

use std::time::Duration;

use common::*;
use prim_room::daemon::Start;
use prim_room::local::{Event, ReplyJson, Request, UpdateJson};

async fn members_connected(node: &mut Node, want: usize) -> bool {
    let s = node.status().await;
    s.members.len() >= want
        && s.members
            .iter()
            .filter(|m| m.key != s.me)
            .all(|m| m.connected)
}

async fn scenario(fleet: Fleet, expect_path: &str) {
    // found
    let mut a = fleet
        .start(
            "a",
            Start::Found {
                meta: b"test room".to_vec(),
            },
        )
        .await;
    let s = a.status().await;
    assert_eq!(s.lifecycle, "open");
    assert_eq!(s.me, a.key().hex());
    assert_eq!(s.room, Some(a.daemon.room.hex()));

    // invite -> join, twice
    let token = a.invite().await;
    let mut b = fleet.start("b", Start::Join(token)).await;
    assert_eq!(b.daemon.room, a.daemon.room);
    eventually!(15, "b joined", members_connected(&mut b, 2).await);
    let token = a.invite().await;
    let mut c = fleet.start("c", Start::Join(token)).await;
    eventually!(15, "c joined", members_connected(&mut c, 3).await);
    eventually!(15, "a sees both", members_connected(&mut a, 3).await);

    // a member state write is visible through another node's subscription
    c.ok(|req| Request::Subscribe {
        req,
        sub: 1,
        pattern: "member/**".into(),
    })
    .await;
    b.ok(|req| Request::Write {
        req,
        scope: "member".into(),
        name: "note".into(),
        live: false,
        persistent: true,
        value: payload("hello from b"),
    })
    .await;
    let want_path = format!("member/{}/note", b.key().hex());
    let ev = c
        .wait_event(15, |e| {
            matches!(e, Event::Updates { updates, .. } if updates.iter().any(|u| matches!(u, UpdateJson::Value { path, .. } if *path == want_path)))
        })
        .await;
    let Event::Updates { updates, .. } = ev else {
        unreachable!()
    };
    assert!(updates
        .iter()
        .any(|u| text_of(u).as_deref() == Some("hello from b")));

    // messages: registered channel delivers, unregistered answers no_channel
    c.ok(|req| Request::RegisterChannel { req, channel: 7 })
        .await;
    let r = b
        .client
        .call(|req| Request::Send {
            req,
            to: c.key().hex(),
            channel: 7,
            reliable: true,
            payload: payload("ping"),
        })
        .await
        .unwrap();
    let ReplyJson::Sending { msg_id, targets } = r else {
        panic!("{r:?}")
    };
    assert_eq!(targets, vec![c.key().hex()]);
    let ev = b
        .wait_event(
            10,
            |e| matches!(e, Event::MessageResult { msg_id: m, .. } if *m == msg_id),
        )
        .await;
    assert!(matches!(ev, Event::MessageResult { status, .. } if status == "delivered"));
    let ev = c
        .wait_event(10, |e| matches!(e, Event::Message { channel: 7, .. }))
        .await;
    assert!(matches!(ev, Event::Message { from, .. } if from == b.key().hex()));

    let r = b
        .client
        .call(|req| Request::Send {
            req,
            to: c.key().hex(),
            channel: 9,
            reliable: true,
            payload: payload("anyone?"),
        })
        .await
        .unwrap();
    let ReplyJson::Sending { msg_id, .. } = r else {
        panic!("{r:?}")
    };
    let ev = b
        .wait_event(
            10,
            |e| matches!(e, Event::MessageResult { msg_id: m, .. } if *m == msg_id),
        )
        .await;
    assert!(matches!(ev, Event::MessageResult { status, .. } if status == "no_channel"));

    // datagrams reach a registered channel, and status reports the route per peer
    let ck = c.key().hex();
    let mut got = false;
    for _ in 0..20 {
        b.client
            .send(&Request::SendDatagram {
                to: ck.clone(),
                channel: 7,
                payload: payload("dgram"),
                max_age_ms: 2_000,
            })
            .await
            .unwrap();
        if let Ok(Some(Ok(_))) = tokio::time::timeout(Duration::from_millis(300), async {
            loop {
                match c.client.next_event().await {
                    Ok(Some(Event::Datagram {
                        channel: 7, from, ..
                    })) => return Some(Ok(from)),
                    Ok(Some(_)) => {}
                    other => return Some(Err(format!("{other:?}"))),
                }
            }
        })
        .await
        {
            got = true;
            break;
        }
    }
    assert!(got, "datagram never arrived");
    let sb = b.status().await;
    let r = sb.routes.iter().find(|r| r.peer == ck).expect("route to c");
    assert_eq!(r.path, expect_path);
    assert!(r.max_datagram_size.is_some_and(|n| n >= 1000), "{r:?}");
    assert!(r.datagrams_sent >= 1);

    // revoke: b is told, disconnected and dropped from everyone's roster
    let bk = b.key().hex();
    a.ok(|req| Request::Revoke {
        req,
        subject: bk.clone(),
    })
    .await;
    b.wait_event(15, |e| matches!(e, Event::Removed)).await;
    for node in [&mut a, &mut c] {
        eventually!(15, "b removed from roster", {
            let s = node.status().await;
            !s.members.iter().any(|m| m.key == bk) && !s.routes.iter().any(|r| r.peer == bk)
        });
    }
    // The survivors still see each other.
    eventually!(15, "a and c connected", members_connected(&mut a, 2).await);

    c.daemon.shutdown().await;
    b.daemon.shutdown().await;
    a.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_daemons_on_mem_transport() {
    scenario(Fleet::mem(), "mem").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_daemons_on_real_iroh_loopback() {
    scenario(Fleet::iroh_loopback(), "direct").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_keeps_room_and_bumps_incarnation() {
    let fleet = Fleet::mem();
    let mut a = fleet.start("a", Start::Found { meta: vec![] }).await;
    let room = a.daemon.room;
    let key = a.key();
    assert_eq!(a.daemon.incarnation, 1);
    let token = a.invite().await;
    let mut b = fleet.start("b", Start::Join(token)).await;
    eventually!(15, "joined", members_connected(&mut b, 2).await);
    let state_dir = a.state_dir.clone();
    let socket = a.socket.clone();
    a.daemon.shutdown().await;
    drop(a.client);
    assert!(state_dir.join("room.bin").exists());
    assert!(state_dir.join("identity.key").exists());

    // Resume from the same dir: same identity and room, next incarnation.
    let net_fleet_node = fleet.start("a", Start::Resume).await;
    assert_eq!(net_fleet_node.daemon.incarnation, 2);
    assert_eq!(net_fleet_node.daemon.key, key);
    assert_eq!(net_fleet_node.daemon.room, room);
    assert_eq!(net_fleet_node.state_dir, state_dir);
    assert_eq!(net_fleet_node.socket, socket);
    let mut a2 = net_fleet_node;
    eventually!(
        15,
        "reconnected after resume",
        members_connected(&mut a2, 2).await
    );
    a2.daemon.shutdown().await;
    b.daemon.shutdown().await;
    let _ = Duration::from_secs(0);
}
