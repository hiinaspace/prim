use super::*;
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
};

/// Inclusive range; None means an empty ordinary response. Multi-range is ignored
/// and returns a full 200, as allowed by RFC 9110, never a false partial response.
fn range(value: Option<&str>, size: u64) -> Result<(u64, u64, bool)> {
    let full = (0, size, false);
    let Some(value) = value else {
        return Ok(full);
    };
    if value.contains(',') {
        return Ok(full);
    }
    let Some(spec) = value.strip_prefix("bytes=") else {
        return Ok(full);
    };
    let (a, b) = spec.split_once('-').context("range")?;
    if a.is_empty() {
        let suffix: u64 = b.parse()?;
        ensure!(suffix > 0 && size > 0, "empty range");
        return Ok((size.saturating_sub(suffix), size, true));
    }
    let start: u64 = a.parse()?;
    ensure!(start < size, "unsatisfiable");
    let end = if b.is_empty() {
        size
    } else {
        let end: u64 = b.parse()?;
        ensure!(end >= start, "reversed");
        end.min(size - 1) + 1
    };
    Ok((start, end, true))
}
pub(super) async fn serve(listener: TcpListener, media: Arc<Media>, room: Arc<Shared>) {
    let permits = Arc::new(Semaphore::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((socket,_)) = accepted else { break; };
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                let m = media.clone(); let r = room.clone();
                tasks.spawn(async move { let _permit = permit; let _ = response(socket,m,r).await; });
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => { if room.is_stopped() { break; } }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    tasks.abort_all();
}
async fn response(mut socket: TcpStream, media: Arc<Media>, room: Arc<Shared>) -> Result<()> {
    let port = socket.local_addr()?.port();
    let request = tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        loop {
            let b = socket.read_u8().await?;
            bytes.push(b);
            if bytes.ends_with(b"\r\n\r\n") {
                break;
            }
            ensure!(bytes.len() < 8192, "header too large");
        }
        anyhow::Ok(String::from_utf8(bytes)?)
    })
    .await??;
    let mut lines = request.split("\r\n");
    let first: Vec<_> = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    ensure!(first.len() == 3 && first[2] == "HTTP/1.1", "request");
    let mut headers = HashMap::new();
    for line in lines.filter(|s| !s.is_empty()) {
        let (k, v) = line.split_once(':').context("header")?;
        ensure!(
            headers.insert(k.to_ascii_lowercase(), v.trim()).is_none(),
            "duplicate header"
        );
    }
    let remote = media.remote.lock().unwrap().clone();
    let remote = remote.filter(|r| r.path == first[1] && r.active.load(Ordering::Acquire));
    if headers.get("host").copied() != Some(format!("127.0.0.1:{port}").as_str())
        || remote.is_none()
    {
        socket
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await?;
        return Ok(());
    }
    if !matches!(first[0], "HEAD" | "GET") {
        socket.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nAllow: GET, HEAD\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
        return Ok(());
    }
    let remote = remote.unwrap();
    let size = remote.descriptor.size;
    let parsed = if first[0] == "HEAD" {
        Ok((0, size, false))
    } else {
        range(headers.get("range").copied(), size)
    };
    let (start, end, partial) = match parsed {
        Ok(r) => r,
        Err(_) => {
            socket.write_all(format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await?;
            return Ok(());
        }
    };
    let header = format!("HTTP/1.1 {}\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n{}\r\n", if partial {"206 Partial Content"} else {"200 OK"},end-start,if partial { format!("Content-Range: bytes {start}-{}/{size}\r\n",end-1) } else { String::new() });
    socket.write_all(header.as_bytes()).await?;
    if first[0] == "HEAD" {
        return Ok(());
    }
    let mut offset = start;
    while offset < end {
        // Detect player cancellation even while waiting for relay consent or I/O.
        let mut probe = [0];
        let block = tokio::select! {
            result = remote.block(offset/BLOCK*BLOCK,&media,&room) => result?,
            _ = socket.read(&mut probe) => { return Ok(()); }
        };
        ensure!(
            remote.active.load(Ordering::Acquire) && !room.is_stopped(),
            "cancelled"
        );
        let from = (offset % BLOCK) as usize;
        let count = (end - offset).min((block.len() - from) as u64) as usize;
        tokio::time::timeout(
            Duration::from_secs(10),
            socket.write_all(&block[from..from + count]),
        )
        .await??;
        offset += count as u64;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn http_ranges_cover_boundaries() {
        assert_eq!(range(None, 0).unwrap(), (0, 0, false));
        assert_eq!(range(Some("bytes=0-0"), 20).unwrap(), (0, 1, true));
        assert_eq!(range(Some("bytes=19-"), 20).unwrap(), (19, 20, true));
        assert_eq!(range(Some("bytes=-100"), 20).unwrap(), (0, 20, true));
        assert_eq!(range(Some("bytes=2-999"), 20).unwrap(), (2, 20, true));
        assert_eq!(range(Some("bytes=0-1,4-5"), 20).unwrap(), (0, 20, false));
        for value in [
            "bytes=20-",
            "bytes=-0",
            "bytes=5-2",
            "bytes=18446744073709551616-",
            "bytes=x-y",
        ] {
            assert!(range(Some(value), 20).is_err(), "{value}");
        }
        assert!(range(Some("bytes=0-"), 0).is_err());
    }
}
