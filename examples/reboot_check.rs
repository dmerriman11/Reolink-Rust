#![allow(dead_code)] // shares the app's modules but uses only part of them
//! Reboot one saved camera through the app's worker and time how long it takes to come back:
//! `cargo run --example reboot_check -- <camera name>`

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
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let want = std::env::args().nth(1).expect("camera name");
    let path = std::path::PathBuf::from(std::env::var("APPDATA")?).join("Reolink-rs/devices.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let d = saved["devices"].as_array().unwrap().iter().find(|d| d["name"] == want.as_str()).expect("no such camera");
    let cfg: DeviceConfig = serde_json::from_value(d.clone())?;
    let rt = tokio::runtime::Runtime::new()?;
    let dev = Device::spawn(rt.handle(), 0, cfg);
    let online = || dev.state.lock().unwrap().online;
    let wait = |want: bool, limit: u64| {
        let t = Instant::now();
        while online() != want && t.elapsed() < Duration::from_secs(limit) {
            std::thread::sleep(Duration::from_millis(250));
        }
        (online() == want).then(|| t.elapsed())
    };
    wait(true, 60).ok_or_else(|| anyhow::anyhow!("camera never came online"))?;
    println!("online; sending reboot");
    let (reply, rx) = tokio::sync::oneshot::channel();
    dev.send(Cmd::Reboot(reply));
    rt.block_on(rx)?.map_err(|e| anyhow::anyhow!(e))?;
    let t0 = Instant::now();
    println!("reboot accepted");
    match wait(false, 60) {
        Some(_) => println!("went offline after {:.1}s: {}", t0.elapsed().as_secs_f32(), dev.state.lock().unwrap().status),
        None => println!("never reported offline within 60s"),
    }
    match wait(true, 300) {
        Some(_) => println!("back online {:.1}s after the reboot command", t0.elapsed().as_secs_f32()),
        None => anyhow::bail!("not back online within 5 minutes: {}", dev.state.lock().unwrap().status),
    }
    Ok(())
}
