//! `iroh-hello listen` prints its endpoint id and serves one-message round trips.
//! `iroh-hello connect <endpoint-id> [ip:port ...]` dials it (n0 lookup and relay, plus any
//! given direct addresses), sends a hello, prints the reply, the RTT and the selected path.
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use iroh::{endpoint::presets, Endpoint, EndpointAddr, EndpointId};

const ALPN: &[u8] = b"prim/hello/0";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("listen") => listen().await,
        Some("connect") if args.len() >= 2 => connect(&args[1], &args[2..]).await,
        _ => {
            eprintln!("usage: iroh-hello listen | iroh-hello connect <endpoint-id> [ip:port ...]");
            std::process::exit(2);
        }
    }
}

async fn bind() -> Result<Endpoint> {
    let endpoint = Endpoint::builder(presets::N0).alpns(vec![ALPN.to_vec()]).bind().await?;
    let _ = tokio::time::timeout(Duration::from_secs(10), endpoint.online()).await;
    Ok(endpoint)
}

fn arch() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

async fn listen() -> Result<()> {
    let endpoint = bind().await?;
    let addr = endpoint.addr();
    println!("listening id={} arch={}", addr.id, arch());
    for transport in &addr.addrs {
        println!("addr {transport:?}");
    }
    while let Some(incoming) = endpoint.accept().await {
        tokio::spawn(async move {
            let result: Result<()> = async {
                let connection = incoming.await?;
                let (mut send, mut recv) = connection.accept_bi().await?;
                let message = recv.read_to_end(1024).await?;
                println!("from {}: {}", connection.remote_id(), String::from_utf8_lossy(&message));
                send.write_all(format!("hello back from {}", arch()).as_bytes()).await?;
                send.finish()?;
                connection.closed().await;
                Ok(())
            }
            .await;
            if let Err(error) = result {
                eprintln!("serve: {error:#}");
            }
        });
    }
    Ok(())
}

async fn connect(id: &str, direct: &[String]) -> Result<()> {
    let mut addr = EndpointAddr::new(id.parse::<EndpointId>().context("endpoint id")?);
    for ip in direct {
        addr = addr.with_ip_addr(ip.parse().context("direct address")?);
    }
    let endpoint = bind().await?;
    let started = Instant::now();
    let connection = tokio::time::timeout(Duration::from_secs(20), endpoint.connect(addr, ALPN))
        .await
        .context("connect timed out")??;
    let (mut send, mut recv) = connection.open_bi().await?;
    send.write_all(format!("hello from {}", arch()).as_bytes()).await?;
    send.finish()?;
    let reply = recv.read_to_end(1024).await?;
    let elapsed = started.elapsed();
    if reply.is_empty() {
        bail!("empty reply");
    }
    println!("reply: {}", String::from_utf8_lossy(&reply));
    println!("round trip incl. handshake: {elapsed:?}");
    for path in connection.paths().iter() {
        println!(
            "path {:?} selected={} relay={} rtt={:?}",
            path.remote_addr(),
            path.is_selected(),
            path.is_relay(),
            path.rtt()
        );
    }
    connection.close(0u32.into(), b"done");
    endpoint.close().await;
    Ok(())
}
