use super::*;
#[test]
fn media_identity_and_consent_are_scoped() {
    let descriptor = Descriptor {
        id: random_id(),
        owner: random_id(),
        name: "movie.mkv".into(),
        size: 100,
    };
    assert!(descriptor.valid());
    let wire = serde_json::to_value(&descriptor).unwrap();
    assert!(
        wire["size"].is_string(),
        "Godot JSON must preserve integer sizes"
    );
    assert_eq!(
        serde_json::from_value::<Descriptor>(wire).unwrap(),
        descriptor
    );
    for path in [0, 1, 2] {
        assert_eq!(gate(path, "pending"), if path == 1 { OK } else { WAIT });
        assert_eq!(gate(path, "allowed"), if path == 0 { WAIT } else { OK });
    }
    assert_eq!(gate(2, "denied"), DENIED);
    let media = Media::new();
    media.view.lock().unwrap().publication = Some(descriptor.clone());
    media.view.lock().unwrap().viewers.insert(
        "viewer".into(),
        Viewer {
            consent: "pending".into(),
            ..Viewer::default()
        },
    );
    media.consent("stale", "viewer", true);
    assert_eq!(
        media.view.lock().unwrap().viewers["viewer"].consent,
        "pending"
    );
    media.consent(&descriptor.id, "viewer", true);
    assert_eq!(
        media.view.lock().unwrap().viewers["viewer"].consent,
        "allowed"
    );
    media.stop();
    assert!(media.view.lock().unwrap().viewers.is_empty());
}
#[test]
fn cache_memory_is_bounded_and_deduplicated() {
    let mut cache = Cache::default();
    for i in 0..40 {
        cache.insert(i * BLOCK, Bytes::from(vec![i as u8; BLOCK as usize]));
    }
    assert_eq!(cache.bytes, CACHE_BYTES);
    assert!(!cache.blocks.contains_key(&0));
    assert_eq!(cache.blocks[&(39 * BLOCK)][0], 39);
    cache.insert(39 * BLOCK, Bytes::from(vec![0; BLOCK as usize]));
    assert_eq!(cache.bytes, CACHE_BYTES);
    assert_eq!(cache.blocks[&(39 * BLOCK)][0], 39);
}

use crate::network::{Address, Event, Handle};
use std::io::{Read, Write};
fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !predicate() {
        assert!(std::time::Instant::now() < deadline, "condition timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn request(url: &str, method: &str, range: &str) -> Vec<u8> {
    let (host, path) = url
        .strip_prefix("http://")
        .unwrap()
        .split_once('/')
        .unwrap();
    let mut socket = std::net::TcpStream::connect(host).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    write!(
        socket,
        "{method} /{path} HTTP/1.1\r\nHost: {host}\r\n{range}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut data = Vec::new();
    socket.read_to_end(&mut data).unwrap();
    data
}
fn body(response: &[u8]) -> &[u8] {
    let start = response.windows(4).position(|x| x == b"\r\n\r\n").unwrap() + 4;
    &response[start..]
}
fn pair() -> (Handle, Handle) {
    let host = Handle::start([41; 32], "Host".into(), None, true).unwrap();
    let address = loop {
        if let Event::Ready(info) = host.events.recv_timeout(Duration::from_secs(10)).unwrap() {
            break serde_json::from_str::<Address>(&info).unwrap();
        }
    };
    wait_until(|| host.shared.is_host());
    let client = Handle::start([41; 32], "Viewer".into(), Some(address), true).unwrap();
    wait_until(|| {
        !client.shared.host.read().unwrap().is_empty()
            && !client.shared.local.read().unwrap().is_empty()
    });
    let peer = client.shared.local.read().unwrap().clone();
    wait_until(|| host.shared.media_peer(&peer).is_some());
    (host, client)
}
#[test]
fn real_iroh_ranges_seek_cache_revoke_and_relay_gate() {
    // The transport is real direct Iroh; path overrides exercise policy only.
    // They deliberately do not claim a real packet-relay migration benchmark.
    let path = std::env::temp_dir().join(format!("prim-media-{}.bin", random_id()));
    let fixture: Vec<u8> = (0..(3 * BLOCK + 113)).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &fixture).unwrap();
    let (host, client) = pair();
    assert!(host.shared.media.share(path.to_string_lossy().into()));
    wait_until(|| host.shared.media.view.lock().unwrap().offered.is_some());
    let descriptor = host
        .shared
        .media
        .view
        .lock()
        .unwrap()
        .offered
        .clone()
        .unwrap();
    assert!(host.shared.media.publish(&descriptor.id));
    assert!(client.shared.media.receive(descriptor.clone()));
    wait_until(|| !client.shared.media.view.lock().unwrap().url.is_empty());
    let url = client.shared.media.view.lock().unwrap().url.clone();
    let response = request(&url, "GET", "Range: bytes=1048500-1048700\r\n");
    assert!(response.starts_with(b"HTTP/1.1 206"));
    assert_eq!(body(&response), &fixture[1048500..1048701]);
    let received = client.shared.media.view.lock().unwrap().bytes_received;
    assert!(
        received < fixture.len() as u64,
        "startup did not fetch whole file"
    );
    assert_eq!(
        body(&request(&url, "GET", "Range: bytes=1048500-1048700\r\n")),
        &fixture[1048500..1048701]
    );
    assert_eq!(
        client.shared.media.view.lock().unwrap().bytes_received,
        received,
        "cached repeat fetched again"
    );
    assert_eq!(
        body(&request(&url, "GET", "Range: bytes=-7\r\n")),
        &fixture[fixture.len() - 7..]
    );
    assert!(body(&request(&url, "HEAD", "")).is_empty());
    assert!(request(&url, "GET", "Range: bytes=99999999-\r\n").starts_with(b"HTTP/1.1 416"));
    assert!(request(&(url.clone() + "other"), "GET", "").starts_with(b"HTTP/1.1 404"));
    let peer = client.shared.local.read().unwrap().clone();
    host.shared.media.test_path.store(2, Ordering::Release);
    let before = host.shared.media.view.lock().unwrap().viewers[&peer].bytes_sent;
    let u = url.clone();
    let read = std::thread::spawn(move || request(&u, "GET", "Range: bytes=2097152-2097252\r\n"));
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        host.shared.media.view.lock().unwrap().viewers[&peer].bytes_sent,
        before,
        "unapproved relay payload"
    );
    host.shared.media.consent("old-share", &peer, true);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        host.shared.media.view.lock().unwrap().viewers[&peer].bytes_sent,
        before
    );
    host.shared.media.consent(&descriptor.id, &peer, true);
    assert_eq!(body(&read.join().unwrap()), &fixture[2097152..2097253]);
    // Replacing a share revokes old capabilities and consent, even for the same path.
    assert!(host.shared.media.share(path.to_string_lossy().into()));
    wait_until(|| host.shared.media.view.lock().unwrap().offered.is_some());
    let replacement = host
        .shared
        .media
        .view
        .lock()
        .unwrap()
        .offered
        .clone()
        .unwrap();
    assert_ne!(replacement.id, descriptor.id);
    assert!(host.shared.media.publish(&replacement.id));
    host.shared.media.test_path.store(1, Ordering::Release);
    assert!(client.shared.media.receive(replacement));
    wait_until(|| !client.shared.media.view.lock().unwrap().url.is_empty());
    assert!(request(&url, "GET", "").starts_with(b"HTTP/1.1 404"));
    let new_url = client.shared.media.view.lock().unwrap().url.clone();
    assert_eq!(
        body(&request(&new_url, "GET", "Range: bytes=0-10\r\n")),
        &fixture[..11]
    );
    assert_eq!(
        host.shared.media.view.lock().unwrap().viewers[&peer].consent,
        "pending"
    );
    // A changed source must fail uncached reads instead of mixing file versions.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&[0])
        .unwrap();
    assert!(body(&request(
        &new_url,
        "GET",
        "Range: bytes=2097152-2097162\r\n"
    ))
    .is_empty());
    assert!(!host.shared.media.publication_active());
    host.shared.media.stop();
    client.shared.media.stop();
    assert!(request(&url, "GET", "").starts_with(b"HTTP/1.1 404"));
    assert!(host.shared.media.view.lock().unwrap().viewers.is_empty());
    drop(client);
    drop(host);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn nonhost_provider_preparation_preserves_receiving_and_old_publication() {
    let path = std::env::temp_dir().join(format!("prim-provider-{}.bin", random_id()));
    std::fs::write(&path, vec![17u8; (2 * BLOCK) as usize]).unwrap();
    let (host, provider) = pair();
    assert!(!provider.shared.is_host());
    let prepare = |media: &Media| {
        assert!(media.share(path.to_string_lossy().into()));
        wait_until(|| media.view.lock().unwrap().offered.is_some());
        media.view.lock().unwrap().offered.clone().unwrap()
    };
    let old = prepare(&host.shared.media);
    assert!(host.shared.media.publish(&old.id));
    assert!(provider.shared.media.receive(old));
    wait_until(|| !provider.shared.media.view.lock().unwrap().url.is_empty());
    let receiving = provider.shared.media.view.lock().unwrap().url.clone();
    assert_eq!(
        body(&request(&receiving, "GET", "Range: bytes=0-10\r\n")),
        &[17; 11]
    );
    let offered = prepare(&provider.shared.media);
    assert_eq!(provider.shared.media.view.lock().unwrap().url, receiving);
    assert!(provider.shared.media.publish(&offered.id));
    assert_eq!(provider.shared.media.view.lock().unwrap().url, receiving);
    assert!(host.shared.media.receive(offered.clone()));
    wait_until(|| !host.shared.media.view.lock().unwrap().url.is_empty());
    let url = host.shared.media.view.lock().unwrap().url.clone();
    assert_eq!(
        body(&request(&url, "GET", "Range: bytes=0-10\r\n")),
        &[17; 11]
    );
    let pending = prepare(&provider.shared.media);
    provider.shared.media.cancel_offer();
    assert!(!provider.shared.media.publish(&pending.id));
    assert!(provider.shared.media.publication_active());
    provider.shared.media.stop_publishing(&pending.id);
    assert!(provider.shared.media.publication_active());
    assert!(provider.shared.media.share("/no/such/prim-file".into()));
    wait_until(|| {
        provider
            .shared
            .media
            .view
            .lock()
            .unwrap()
            .offer_status
            .contains("Cannot")
    });
    assert!(provider.shared.media.publication_active());
    // A non-host provider owns relay permission, including for the room host.
    provider.shared.media.test_path.store(2, Ordering::Release);
    let host_id = host.shared.local.read().unwrap().clone();
    let u = url.clone();
    let pending_read =
        std::thread::spawn(move || request(&u, "GET", "Range: bytes=1048576-1048586\r\n"));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        provider.shared.media.view.lock().unwrap().viewers[&host_id].consent,
        "pending"
    );
    provider.shared.media.consent(&offered.id, &host_id, true);
    assert_eq!(body(&pending_read.join().unwrap()), &[17; 11]);
    provider.shared.media.stop_publishing(&offered.id);
    assert!(!provider.shared.media.publication_active());
    host.shared.stop();
    provider.shared.stop();
    std::fs::remove_file(path).unwrap();
}
