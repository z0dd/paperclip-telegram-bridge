//! Minimal test proxies.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Accepts TCP connections, counts them and closes them immediately.
pub struct CountingListener {
    addr: SocketAddr,
    count: Arc<AtomicUsize>,
}

impl CountingListener {
    /// Runs on a plain thread so it works with or without a tokio runtime.
    pub fn start_blocking() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                c.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        Self { addr, count }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn connections(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

/// HTTP forward proxy for plain-HTTP targets (absolute-form requests).
/// Counts every proxied request and rewrites it to origin-form.
pub struct ForwardProxy {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
}

impl ForwardProxy {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let r = requests.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let r = r.clone();
                tokio::spawn(async move {
                    let _ = handle(client, r).await;
                });
            }
        });
        Self { addr, requests }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

async fn read_head(stream: &mut TcpStream, buf: &mut Vec<u8>) -> std::io::Result<Option<usize>> {
    loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            return Ok(Some(pos + 4));
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn handle(mut client: TcpStream, requests: Arc<AtomicUsize>) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut upstream: Option<TcpStream> = None;
    loop {
        let Some(head_len) = read_head(&mut client, &mut buf).await? else {
            return Ok(());
        };
        let head = String::from_utf8_lossy(&buf[..head_len]).into_owned();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default().to_string();
        let mut parts = request_line.split(' ');
        let (method, target, version) = (
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
        );
        let content_length: usize = lines
            .clone()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while buf.len() < head_len + content_length {
            let mut chunk = [0u8; 4096];
            let n = client.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let rest = target.strip_prefix("http://").unwrap_or(target);
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let path = if path.is_empty() { "/" } else { path };
        requests.fetch_add(1, Ordering::SeqCst);

        if upstream.is_none() {
            let up = TcpStream::connect(authority).await?;
            upstream = Some(up);
        }
        let up = upstream.as_mut().unwrap();
        let mut out = format!("{method} {path} {version}\r\n").into_bytes();
        for l in lines {
            if l.is_empty() || l.to_ascii_lowercase().starts_with("proxy-") {
                continue;
            }
            out.extend_from_slice(l.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&buf[head_len..head_len + content_length]);
        buf.drain(..head_len + content_length);
        up.write_all(&out).await?;

        // Relay one response: head + Content-Length body.
        let mut rbuf = Vec::new();
        let Some(rhead) = read_head(up, &mut rbuf).await? else {
            return Ok(());
        };
        let rh = String::from_utf8_lossy(&rbuf[..rhead]).into_owned();
        let rlen: usize = rh
            .split("\r\n")
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while rbuf.len() < rhead + rlen {
            let mut chunk = [0u8; 4096];
            let n = up.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            rbuf.extend_from_slice(&chunk[..n]);
        }
        client.write_all(&rbuf).await?;
    }
}
