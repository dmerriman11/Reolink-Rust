//! End-to-end check of the real device worker against a saved camera:
//! `cargo run --example probe -- "<device name>" [main|sub]`
//! Streams for a few seconds, records ~8 s, then reports decoded frame sizes and the MP4 ffprobe sees.

#[path = "../src/device.rs"]
mod device;
#[path = "../src/media.rs"]
mod media;

use device::{Cmd, Device, DeviceConfig};
use neolink_core::bc_protocol::StreamKind;
use std::time::{Duration, Instant};

/// Width/height from a baseline JPEG's SOF0 marker.
fn jpeg_size(j: &[u8]) -> Option<(u16, u16)> {
    let p = j.windows(2).position(|w| w == [0xFF, 0xC0])?;
    Some((u16::from_be_bytes([j[p + 7], j[p + 8]]), u16::from_be_bytes([j[p + 5], j[p + 6]])))
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut args = std::env::args().skip(1);
    let name = args.next().expect("device name");
    let kind = if args.next().as_deref() == Some("sub") { StreamKind::Sub } else { StreamKind::Main };
    let path = std::path::PathBuf::from(std::env::var("APPDATA")?).join("Reolink-rs/devices.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let cfg: DeviceConfig = serde_json::from_value(
        saved["devices"].as_array().unwrap().iter().find(|d| d["name"] == name.as_str()).expect("no such device").clone(),
    )?;

    let rt = tokio::runtime::Runtime::new()?;
    let dev = Device::spawn(rt.handle(), 1, cfg);
    let mut frames = dev.frames.clone();
    dev.send(Cmd::Live(Some(kind)));

    // Wait for the first decoded frame.
    let t = Instant::now();
    let first = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(60), frames.wait_for(|f| f.is_some())).await.map(|f| f.map(|f| f.clone()))
    });
    let first = match first {
        Ok(Ok(Some(f))) => f,
        _ => anyhow::bail!("no frame within 60s; status: {}", dev.state.lock().unwrap().status),
    };
    println!("{kind:?}: first frame after {:?}, JPEG {:?}, {} KB", t.elapsed(), jpeg_size(&first), first.len() / 1024);

    // Count frames for 5 s.
    let n = rt.block_on(async {
        let mut n = 0;
        let end = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::timeout_at(end, frames.changed()).await.is_ok() {
            n += 1;
        }
        n
    });
    println!("decoded {:.1} fps", n as f32 / 5.0);

    dev.send(Cmd::Record(true));
    std::thread::sleep(Duration::from_secs(8));
    let rec = dev.state.lock().unwrap().recording.clone();
    dev.send(Cmd::Record(false));
    std::thread::sleep(Duration::from_secs(3)); // ffmpeg finalizes the MP4 after stdin closes
    let msg = dev.state.lock().unwrap().message.take();
    let Some(rec) = rec else { anyhow::bail!("recording did not start: {msg:?}") };
    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_name,width,height,nb_frames:format=duration", "-of", "compact"])
        .arg(&rec)
        .output()?;
    println!("recorded {} ({} KB)", rec.display(), std::fs::metadata(&rec)?.len() / 1024);
    print!("{}", String::from_utf8_lossy(&probe.stdout));
    Ok(())
}
