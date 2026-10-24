//! Localhost MJPEG server feeding the WebView's <img> tags: GET /live/<device id>?t=<token>.
//! Bound to 127.0.0.1 with a per-run random token so other local users/pages can't watch the cameras.

use crate::device::Frame;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

pub type Feeds = Arc<Mutex<HashMap<u64, watch::Receiver<Frame>>>>;

pub fn token() -> String {
    use std::hash::{BuildHasher, Hasher};
    // RandomState is seeded from the OS RNG; two draws give 128 bits.
    let a = std::collections::hash_map::RandomState::new().build_hasher().finish();
    let b = std::collections::hash_map::RandomState::new().build_hasher().finish();
    format!("{a:016x}{b:016x}")
}

/// Starts the server and returns its port.
pub async fn serve(feeds: Feeds, token: String) -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let (feeds, token) = (feeds.clone(), token.clone());
            tokio::spawn(async move {
                if let Err(e) = handle(sock, feeds, &token).await {
                    log::debug!("mjpeg client: {e}");
                }
            });
        }
    });
    Ok(port)
}

/// Parses "GET /live/<id>?t=<token> HTTP/1.1" and returns the id if the token matches.
fn route(request: &str, token: &str) -> Option<u64> {
    let path = request.lines().next()?.strip_prefix("GET ")?.split(' ').next()?;
    let (path, query) = path.split_once('?')?;
    let id = path.strip_prefix("/live/")?.parse().ok()?;
    query.split('&').any(|kv| kv == format!("t={token}")).then_some(id)
}

async fn handle(mut sock: TcpStream, feeds: Feeds, token: &str) -> std::io::Result<()> {
    let mut buf = vec![0u8; 2048];
    let mut n = 0;
    while !buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
        if n == buf.len() {
            return Ok(()); // oversized request
        }
        let r = sock.read(&mut buf[n..]).await?;
        if r == 0 {
            return Ok(());
        }
        n += r;
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let rx = route(&req, token).and_then(|id| feeds.lock().unwrap().get(&id).cloned());
    let Some(mut rx) = rx else {
        return sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    };
    sock.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\n\
          Cache-Control: no-store\r\nConnection: close\r\n\r\n",
    )
    .await?;
    loop {
        let frame = rx.borrow_and_update().clone();
        if let Some(jpeg) = frame {
            let head = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len());
            sock.write_all(head.as_bytes()).await?;
            sock.write_all(&jpeg).await?;
            sock.write_all(b"\r\n").await?;
        }
        if rx.changed().await.is_err() {
            return Ok(()); // device removed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::route;

    #[test]
    fn routes_only_with_token() {
        assert_eq!(route("GET /live/7?t=abc HTTP/1.1\r\n", "abc"), Some(7));
        assert_eq!(route("GET /live/7?x=1&t=abc HTTP/1.1\r\n", "abc"), Some(7));
        assert_eq!(route("GET /live/7?t=abcd HTTP/1.1\r\n", "abc"), None);
        assert_eq!(route("GET /live/7 HTTP/1.1\r\n", "abc"), None);
        assert_eq!(route("POST /live/7?t=abc HTTP/1.1\r\n", "abc"), None);
    }
}
