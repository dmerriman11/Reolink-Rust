#![allow(dead_code)] // shares the app's modules but uses only part of them
//! Check on one saved camera that a recording always takes the main stream: view the sub stream, record (must
//! switch to main and keep showing frames), take a snapshot mid-stream (the stream must keep flowing), stop (must go
//! back to sub). The recording must be a fragmented MP4. Files go to a temp folder.
//! `cargo run --example record_stream_check -- "<device name>"`

#[path = "../src/device.rs"]
mod device;
#[path = "../src/prefs.rs"]
mod prefs;
#[path = "../src/media.rs"]
mod media;
#[path = "../src/settings.rs"]
mod settings;
#[path = "../src/xmledit.rs"]
mod xmledit;

use device::{Cmd, Device, DeviceConfig};
use neolink_core::bc_protocol::StreamKind;
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let want = std::env::args().nth(1).expect("camera name");
    let path = dirs::config_dir().unwrap().join("Reolink-rs/devices.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let d = saved["devices"].as_array().unwrap().iter().find(|d| d["name"] == want.as_str()).expect("no such camera");
    let cfg: DeviceConfig = serde_json::from_value(d.clone())?;
    let dir = std::env::temp_dir().join("reolink-rs-record-stream-check");
    let _ = std::fs::remove_dir_all(&dir);
    prefs::set(prefs::Prefs { record_dir: dir.clone(), snapshot_dir: dir.clone(), segment_minutes: 0, ..Default::default() });

    let rt = tokio::runtime::Runtime::new()?;
    let dev = Device::spawn(rt.handle(), 0, cfg);
    let wait = |what: &str, limit: u64, f: &dyn Fn(&device::State) -> bool| -> anyhow::Result<f32> {
        let t = Instant::now();
        while !f(&dev.state.lock().unwrap()) {
            anyhow::ensure!(t.elapsed() < Duration::from_secs(limit), "{what}: not within {limit}s ({})", dev.state.lock().unwrap().status);
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(t.elapsed().as_secs_f32())
    };
    // Frames shown in the next `secs` seconds.
    let frames = |secs: u64| {
        let mut rx = dev.frames.clone();
        let (t, mut n) = (Instant::now(), 0);
        while t.elapsed() < Duration::from_secs(secs) {
            if rx.has_changed().unwrap_or(false) && rx.borrow_and_update().is_some() {
                n += 1;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        n
    };

    wait("online", 60, &|s| s.online)?;
    dev.send(Cmd::Live(Some(StreamKind::Sub)));
    wait("sub stream", 20, &|s| s.live == Some(StreamKind::Sub))?;
    println!("viewing sub: {} frames in 5s", frames(5));

    dev.send(Cmd::Record(true));
    let t = wait("recording on main", 30, &|s| s.recording.is_some() && s.live == Some(StreamKind::Main))?;
    println!("record: on the main stream after {t:.1}s, {} frames in 5s", frames(5));

    dev.send(Cmd::Snapshot);
    let n = frames(6);
    let msg = dev.state.lock().unwrap().message.take();
    println!("snapshot: {msg:?}; {n} frames in the 6s around it");
    anyhow::ensure!(n > 0, "stream stalled during the snapshot");

    dev.send(Cmd::Record(false));
    let t = wait("back to sub", 30, &|s| s.recording.is_none() && s.live == Some(StreamKind::Sub))?;
    println!("stop: back on sub after {t:.1}s, {} frames in 3s", frames(3));
    dev.send(Cmd::Live(None));
    std::thread::sleep(Duration::from_secs(3)); // ffmpeg finishes the file

    let files: Vec<_> = std::fs::read_dir(&dir)?.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "mp4")).collect();
    anyhow::ensure!(files.len() == 1, "expected one recording, found {}", files.len());
    let bytes = std::fs::read(files[0].path())?;
    anyhow::ensure!(bytes.windows(4).any(|w| w == b"moof"), "recording is not fragmented");
    println!("file {} {} KB, fragmented", files[0].file_name().to_string_lossy(), bytes.len() / 1024);
    Ok(())
}
