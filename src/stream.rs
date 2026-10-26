//! Localhost server for the live view, bound to 127.0.0.1 with a per-run random token so other local users/pages
//! can't watch the cameras:
//! - GET /live/<device id>?t=<token>: MJPEG for <img> tiles (decoded by ffmpeg).
//! - GET /raw/<device id>?t=<token>: the camera's own H.264/H.265 for tiles that decode with WebCodecs. Each frame is
//!   a 1-byte flag (bit 0 keyframe, bit 1 H.265), a 4-byte big-endian length and an Annex-B access unit.

use crate::device::Views;
use crate::media::Raw;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast::{self, error::RecvError};

pub type Feeds = Arc<Mutex<HashMap<u64, Arc<Views>>>>;

#[derive(Debug, PartialEq, Clone, Copy)]
enum Route {
    Mjpeg(u64),
    Raw(u64),
}

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

/// Parses "GET /live/<id>?t=<token> HTTP/1.1" (or /raw/) and returns the route if the token matches.
fn route(request: &str, token: &str) -> Option<Route> {
    let path = request.lines().next()?.strip_prefix("GET ")?.split(' ').next()?;
    let (path, query) = path.split_once('?')?;
    let route = match path.strip_prefix('/')?.split_once('/')? {
        ("live", id) => Route::Mjpeg(id.parse().ok()?),
        ("raw", id) => Route::Raw(id.parse().ok()?),
        _ => return None,
    };
    query.split('&').any(|kv| kv == format!("t={token}")).then_some(route)
}

/// The app's own page origins (Windows serves it from http://tauri.localhost, macOS/Linux from tauri://localhost).
const APP_ORIGINS: [&str; 3] = ["http://tauri.localhost", "https://tauri.localhost", "tauri://localhost"];

/// The request's Origin header when it is the app's page; no other site may read /raw.
fn app_origin(request: &str) -> Option<&'static str> {
    let origin = request.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("origin")))?.1.trim();
    APP_ORIGINS.into_iter().find(|o| *o == origin)
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
    let found = route(&req, token).and_then(|r| {
        let (Route::Mjpeg(id) | Route::Raw(id)) = r;
        feeds.lock().unwrap().get(&id).cloned().map(|v| (r, v))
    });
    let Some((route, views)) = found else {
        return sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    };
    // Hold the receivers only: the device owns the feed, so removing it ends the response.
    if let Route::Raw(_) = route {
        let (gop, rx) = views.raw.subscribe();
        drop(views);
        return raw(sock, app_origin(&req), gop, rx).await;
    }
    let mut rx = views.jpeg.subscribe();
    drop(views);
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

async fn write_raw(sock: &mut TcpStream, f: &Raw) -> std::io::Result<()> {
    let mut head = [f.key as u8 | (f.hevc as u8) << 1, 0, 0, 0, 0];
    head[1..].copy_from_slice(&(f.data.len() as u32).to_be_bytes());
    sock.write_all(&head).await?;
    sock.write_all(&f.data).await
}

async fn raw(
    mut sock: TcpStream,
    origin: Option<&str>,
    gop: Vec<Arc<Raw>>,
    mut rx: broadcast::Receiver<Arc<Raw>>,
) -> std::io::Result<()> {
    sock.set_nodelay(true)?;
    // Fetched cross-origin from the app's page (a plain GET, so no preflight). Only that page may read it; the
    // token guards it as well.
    let cors = origin.map(|o| format!("Access-Control-Allow-Origin: {o}\r\nVary: Origin\r\n")).unwrap_or_default();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n{cors}Cache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    sock.write_all(head.as_bytes()).await?;
    for f in &gop {
        write_raw(&mut sock, f).await?;
    }
    let mut resync = false; // fell behind: skip to the next keyframe so the decoder never sees a gap
    loop {
        match rx.recv().await {
            Ok(f) => {
                resync &= !f.key;
                if !resync {
                    write_raw(&mut sock, &f).await?;
                }
            }
            Err(RecvError::Lagged(_)) => resync = true,
            Err(RecvError::Closed) => return Ok(()), // device removed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Route, app_origin, route};

    #[test]
    fn only_the_app_page_may_read_raw() {
        let req = |origin: &str| format!("GET /raw/1?t=x HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {origin}\r\n\r\n");
        assert_eq!(app_origin(&req("http://tauri.localhost")), Some("http://tauri.localhost"));
        assert_eq!(app_origin(&req("tauri://localhost")), Some("tauri://localhost"));
        assert_eq!(app_origin(&req("https://evil.example")), None);
        assert_eq!(app_origin(&req("http://tauri.localhost.evil.example")), None);
        assert_eq!(app_origin("GET /raw/1?t=x HTTP/1.1\r\n\r\n"), None);
    }

    #[test]
    fn routes_only_with_token() {
        assert_eq!(route("GET /live/7?t=abc HTTP/1.1\r\n", "abc"), Some(Route::Mjpeg(7)));
        assert_eq!(route("GET /live/7?x=1&t=abc HTTP/1.1\r\n", "abc"), Some(Route::Mjpeg(7)));
        assert_eq!(route("GET /raw/7?t=abc HTTP/1.1\r\n", "abc"), Some(Route::Raw(7)));
        assert_eq!(route("GET /raw/7?t=nope HTTP/1.1\r\n", "abc"), None);
        assert_eq!(route("GET /other/7?t=abc HTTP/1.1\r\n", "abc"), None);
        assert_eq!(route("GET /live/7?t=abcd HTTP/1.1\r\n", "abc"), None);
        assert_eq!(route("GET /live/7 HTTP/1.1\r\n", "abc"), None);
        assert_eq!(route("POST /live/7?t=abc HTTP/1.1\r\n", "abc"), None);
    }
}
