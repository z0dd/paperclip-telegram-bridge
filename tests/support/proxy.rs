//! Minimal test proxies.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
