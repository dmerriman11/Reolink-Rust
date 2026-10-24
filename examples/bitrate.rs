//! Measurement: per-camera, per-stream bitrate split into video and audio.
use neolink_core::bc_protocol::{BcCamera, BcCameraOpt, ConnectionProtocol, Credentials, DiscoveryMethods, StreamKind};
use neolink_core::bcmedia::model::BcMedia;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::path::PathBuf::from(std::env::var("APPDATA")?).join("Reolink-rs/devices.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    for d in saved["devices"].as_array().unwrap() {
        let s = |k: &str| d[k].as_str().unwrap_or("").to_string();
        let cam = BcCamera::new(&BcCameraOpt {
            name: s("name"), channel_id: 0, addrs: s("host").parse().ok().into_iter().collect(),
            uid: Some(s("uid")).filter(|u| !u.is_empty()), port: None, protocol: ConnectionProtocol::TcpUdp,
            discovery: DiscoveryMethods::Local, max_discovery_retries: 5,
            credentials: Credentials { username: s("username"), password: Some(s("password")) }, debug: false,
        }).await?;
        cam.login().await?;
        for kind in [StreamKind::Sub, StreamKind::Main] {
            let mut st = cam.start_video(kind, 0, false).await?;
            let (mut v, mut a, mut frames, mut keys, mut codec, mut acodec) = (0usize, 0usize, 0, 0, String::new(), "");
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(15) {
                match st.get_data().await?? {
                    BcMedia::Iframe(f) => { v += f.data.len(); frames += 1; keys += 1; codec = format!("{:?}", f.video_type); }
                    BcMedia::Pframe(f) => { v += f.data.len(); frames += 1; }
                    BcMedia::Aac(x) => { a += x.data.len(); acodec = "AAC"; }
                    BcMedia::Adpcm(x) => { a += x.data.len(); acodec = "ADPCM"; }
                    _ => {}
                }
            }
            let secs = t.elapsed().as_secs_f64();
            println!("{:10} {:?}: {codec} video {:.0} kbit/s @ {:.1} fps (keyframe every {:.1}s), {acodec} audio {:.0} kbit/s",
                s("name"), kind, v as f64 * 8.0 / 1000.0 / secs, frames as f64 / secs, secs / keys.max(1) as f64, a as f64 * 8.0 / 1000.0 / secs);
            drop(st);
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    Ok(())
}
