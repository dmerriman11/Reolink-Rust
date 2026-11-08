//! Localhost server for the live view, bound to 127.0.0.1 with a per-run random token so other local users/pages
//! can't watch the cameras:
//! - GET /live/<device id>?t=<token>: MJPEG for <img> tiles (decoded by ffmpeg).
//! - GET /raw/<device id>?t=<token>: the camera's own H.264/H.265 for tiles that decode with WebCodecs. Each frame is
//!   a 1-byte flag (bit 0 keyframe, bit 1 H.265, bit 2 cached from before this viewer joined), a 4-byte big-endian
//!   length, the 4-byte big-endian time on the camera's clock (microseconds, wrapping) and an Annex-B access unit.

use crate::device::Views;
use crate::media::Raw;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast::{self, error::RecvError};

pub type Feeds = Arc<Mutex<HashMap<u64, Arc<Views>>>>;

#[derive(Debug, PartialEq, Clone, Copy)]
enum Route {
    Mjpeg(u64),
    Raw(u64),
    Audio(u64),
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
        ("audio", id) => Route::Audio(id.parse().ok()?),
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
        let (Route::Mjpeg(id) | Route::Raw(id) | Route::Audio(id)) = r;
        feeds.lock().unwrap().get(&id).cloned().map(|v| (r, v))
    });
    let Some((route, views)) = found else {
        return sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    };
    // Hold the receivers only: the device owns the feed, so removing it ends the response.
    if let Route::Audio(_) = route {
        let (format, rx) = views.audio.subscribe();
        drop(views);
        return audio(sock, app_origin(&req), format, rx).await;
    }
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

async fn write_raw(sock: &mut (impl AsyncWrite + Unpin), f: &Raw, cached: bool) -> std::io::Result<()> {
    let mut head = [f.key as u8 | (f.hevc as u8) << 1 | (cached as u8) << 2, 0, 0, 0, 0, 0, 0, 0, 0];
    head[1..5].copy_from_slice(&(f.data.len() as u32).to_be_bytes());
    head[5..].copy_from_slice(&f.us.to_be_bytes());
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
        write_raw(&mut sock, f, true).await?;
    }
    let mut resync = false; // fell behind: skip to the next keyframe so the decoder never sees a gap
    loop {
        match rx.recv().await {
            Ok(f) => {
                resync &= !f.key;
                if !resync {
                    write_raw(&mut sock, &f, false).await?;
                }
            }
            Err(RecvError::Lagged(_)) => resync = true,
            Err(RecvError::Closed) => return Ok(()), // device removed
        }
    }
}

async fn write_audio(sock: &mut (impl AsyncWrite + Unpin), kind: u8, us: u64, data: &[u8]) -> std::io::Result<()> {
    let mut head = [0u8; 13];
    head[0] = kind;
    head[1..5].copy_from_slice(&(data.len() as u32).to_be_bytes());
    head[5..].copy_from_slice(&us.to_be_bytes());
    sock.write_all(&head).await?;
    sock.write_all(data).await
}

async fn audio(
    mut sock: TcpStream,
    origin: Option<&str>,
    initial: Option<crate::audio::Format>,
    mut rx: broadcast::Receiver<Arc<crate::audio::Frame>>,
) -> std::io::Result<()> {
    sock.set_nodelay(true)?;
    let cors = origin.map(|o| format!("Access-Control-Allow-Origin: {o}\r\nVary: Origin\r\n")).unwrap_or_default();
    sock.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n{cors}Cache-Control: no-store\r\nConnection: close\r\n\r\n").as_bytes()).await?;
    let mut current = None;
    if let Some(format) = initial {
        write_audio(&mut sock, 255, 0, &serde_json::to_vec(&format)?).await?;
        current = Some(format);
    }
    loop {
        match rx.recv().await {
            Ok(f) => {
                if current != Some(f.format) {
                    write_audio(&mut sock, 255, 0, &serde_json::to_vec(&f.format)?).await?;
                    current = Some(f.format);
                }
                if f.format.codec == "pcm" {
                    if let Some(samples) = crate::talk::decode(&f.data) {
                        let pcm: Vec<u8> = samples.into_iter().flat_map(i16::to_le_bytes).collect();
                        write_audio(&mut sock, 0, f.us, &pcm).await?;
                    }
                } else {
                    write_audio(&mut sock, 1, f.us, &f.data).await?;
                }
            }
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Route, app_origin, route, write_raw, write_audio};

    #[tokio::test]
    async fn audio_http_sends_formats_pcm_aac_and_closes_on_removal() {
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use neolink_core::bcmedia::model::{BcMedia, BcMediaAdpcm, BcMediaAac};
        async fn packet(sock: &mut tokio::net::TcpStream) -> (u8, u64, Vec<u8>) {
            let mut head = [0; 13];
            sock.read_exact(&mut head).await.unwrap();
            let mut data = vec![0; u32::from_be_bytes(head[1..5].try_into().unwrap()) as usize];
            sock.read_exact(&mut data).await.unwrap();
            (head[0], u64::from_be_bytes(head[5..].try_into().unwrap()), data)
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let views = Arc::new(crate::device::Views::default());
            let pcm = || BcMedia::Adpcm(BcMediaAdpcm { data: vec![0, 0, 0, 0, 0x71] });
            views.audio.push(pcm(), 1);
            let feeds: super::Feeds = Arc::new(Mutex::new([(7, views.clone())].into()));
            let port = super::serve(feeds.clone(), "abc".into()).await.unwrap();
            let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            sock.write_all(b"GET /audio/7?t=abc HTTP/1.1\r\nOrigin: http://tauri.localhost\r\n\r\n").await.unwrap();
            let mut head = vec![];
            while !head.ends_with(b"\r\n\r\n") { head.push(sock.read_u8().await.unwrap()); }
            let head = String::from_utf8(head).unwrap();
            assert!(head.contains("Content-Type: application/octet-stream"));
            assert!(head.contains("Access-Control-Allow-Origin: http://tauri.localhost"));
            assert!(head.contains("Cache-Control: no-store"));
            assert!(head.contains("Connection: close"));
            let (kind, us, data) = packet(&mut sock).await;
            assert_eq!((kind, us), (255, 0));
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&data).unwrap(), serde_json::json!({"codec":"pcm","rate":8000,"channels":1}));
            views.audio.push(pcm(), 42);
            // Each chunk continues the last (250 µs here) while that stays near the video time it was given.
            assert_eq!(packet(&mut sock).await, (0, 251, vec![0, 0, 1, 0, 12, 0]));
            let aac = vec![255, 241, 0x60, 0x40, 0, 0xff, 0xfc];
            views.audio.push(BcMedia::Aac(BcMediaAac { data: aac.clone() }), 43);
            let (kind, _, data) = packet(&mut sock).await;
            assert_eq!(kind, 255);
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&data).unwrap(), serde_json::json!({"codec":"aac","rate":16000,"channels":1}));
            assert_eq!(packet(&mut sock).await, (1, 501, aac));
            feeds.lock().unwrap().remove(&7);
            drop(views);
            assert_eq!(sock.read(&mut [0]).await.unwrap(), 0);
        }).await.unwrap();
    }

    #[tokio::test]
    async fn audio_packet_has_kind_length_and_64_bit_time() {
        let mut out = vec![];
        write_audio(&mut out, 1, 0x0102030405060708, &[9, 10]).await.unwrap();
        assert_eq!(out, [1, 0, 0, 0, 2, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        out.clear();
        write_audio(&mut out, 255, 0, b"{}").await.unwrap();
        assert_eq!(&out[..5], &[255, 0, 0, 0, 2]);
        assert_eq!(&out[13..], b"{}");
    }
    use crate::media::Raw;

    #[tokio::test]
    async fn raw_frames_carry_flags_length_and_camera_time() {
        let mut out = vec![];
        write_raw(&mut out, &Raw { hevc: true, key: true, us: 0x0102_0304, data: vec![7, 8] }, false).await.unwrap();
        write_raw(&mut out, &Raw { hevc: false, key: false, us: u32::MAX, data: vec![9] }, true).await.unwrap();
        #[rustfmt::skip]
        assert_eq!(out, [
            0b011, 0, 0, 0, 2, 1, 2, 3, 4, 7, 8,
            0b100, 0, 0, 0, 1, 255, 255, 255, 255, 9,
        ]);
    }

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
        assert_eq!(route("GET /audio/7?t=abc HTTP/1.1\r\n", "abc"), Some(Route::Audio(7)));
        assert_eq!(route("GET /audio/7?t=nope HTTP/1.1\r\n", "abc"), None);
        assert_eq!(route("GET /audio/7 HTTP/1.1\r\n", "abc"), None);
        assert_eq!(route("GET /audio/no?t=abc HTTP/1.1\r\n", "abc"), None);
        assert_eq!(route("POST /audio/7?t=abc HTTP/1.1\r\n", "abc"), None);
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
