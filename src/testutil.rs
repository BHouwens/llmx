//! Test-only helpers: a minimal HTTP server and a dummy child process.
//!
//! The mock server is ~60 lines of `tokio::net` rather than a dependency,
//! because llmx only ever issues one request shape (`GET <base>/models`) and
//! the tests need to control exactly what comes back — including malformed
//! responses a real library would not produce.

use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// A canned HTTP response served on every connection.
pub struct MockServer {
    addr: SocketAddr,
    handle: JoinHandle<()>,
    requests: Arc<AtomicUsize>,
}

impl MockServer {
    /// Serve `body` as `200 application/json` for every request.
    pub async fn json(body: &'static str) -> Self {
        Self::raw(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        ))
        .await
    }

    /// Serve an arbitrary status with a plain-text body.
    pub async fn status(code: u16, body: &str) -> Self {
        Self::raw(format!(
            "HTTP/1.1 {code} X\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        ))
        .await
    }

    async fn raw(response: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();

        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let response = response.clone();
                tokio::spawn(async move {
                    // Read the request head so the client does not see a reset
                    // before it finishes writing.
                    let mut buf = [0u8; 2048];
                    let _ = socket.read(&mut buf).await;
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        Self {
            addr,
            handle,
            requests,
        }
    }

    /// Base URL in the shape a profile would configure, ending in `/v1`.
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// A loopback address with nothing listening, for connection-refused paths.
pub async fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{addr}/v1")
}
