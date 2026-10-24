#![allow(dead_code)] // shares the app's modules but uses only part of them
//! Check the local recording schedule on one saved camera with nothing on screen: continuous mode must start a
//! recording-only stream and write a file, and switching it off must stop it. Motion events are reported as they
//! arrive. Files go to a temp folder. `cargo run --example auto_record_check -- "<device name>"`

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

use device::{Device, DeviceConfig};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let want = std::env::args().nth(1).expect("camera name");
    let path = std::path::PathBuf::from(std::env::var("APPDATA")?).join("Reolink-rs/devices.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let d = saved["devices"].as_array().unwrap().iter().find(|d| d["name"] == want.as_str()).expect("no such camera");
    let cfg: DeviceConfig = serde_json::from_value(d.clone())?;
    let dir = std::env::temp_dir().join("reolink-rs-auto-record-check");
    let _ = std::fs::remove_dir_all(&dir);
    let base = prefs::Prefs { record_dir: dir.clone(), snapshot_dir: dir.clone(), segment_minutes: 0, ..Default::default() };
    prefs::set(prefs::Prefs { local_record: "continuous".into(), ..base.clone() });

    let rt = tokio::runtime::Runtime::new()?;
    let dev = Device::spawn(rt.handle(), 0, cfg);
    let t = Instant::now();
    let wait = |what: &str, limit: u64, f: &dyn Fn(&device::State) -> bool| -> anyhow::Result<f32> {
        let t = Instant::now();
        while !f(&dev.state.lock().unwrap()) {
            anyhow::ensure!(t.elapsed() < Duration::from_secs(limit), "{what}: not within {limit}s ({})", dev.state.lock().unwrap().status);
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(t.elapsed().as_secs_f32())
    };
    wait("online", 60, &|s| s.online)?;
    let rec = wait("scheduled recording", 30, &|s| s.recording.is_some())?;
    println!("continuous schedule, nothing on screen: recording after {rec:.1}s -> {}", dev.state.lock().unwrap().recording.as_ref().unwrap().display());
    std::thread::sleep(Duration::from_secs(10));
    prefs::set(prefs::Prefs { local_record: "off".into(), ..base });
    let stop = wait("recording stop", 15, &|s| s.recording.is_none())?;
    println!("schedule off: recording stopped after {stop:.1}s");
    std::thread::sleep(Duration::from_secs(3)); // ffmpeg finalises the file
    let files: Vec<_> = std::fs::read_dir(&dir)?.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "mp4")).collect();
    anyhow::ensure!(!files.is_empty(), "no recording written");
    for f in &files {
        println!("file {} {} KB", f.file_name().to_string_lossy(), f.metadata()?.len() / 1024);
    }
    let s = dev.state.lock().unwrap();
    println!("motion events seen: {} (now {}), total {:.0}s", s.motion_events, if s.motion { "moving" } else { "still" }, t.elapsed().as_secs_f32());
    Ok(())
}
