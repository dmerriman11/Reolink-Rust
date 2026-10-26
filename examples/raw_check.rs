#![allow(dead_code)] // shares the app's modules but uses only part of them
//! Serve one saved camera's live stream the way the app does, for trying the WebCodecs player in a browser.
//! Prints the /raw and /live URLs (token "check") and runs for `secs` seconds, switching main/sub halfway.
//! `cargo run --example raw_check -- "<device name>" [main|sub] [secs]`

#[path = "../src/device.rs"]
mod device;
#[path = "../src/secrets.rs"]
mod secrets;
#[path = "../src/prefs.rs"]
mod prefs;
#[path = "../src/media.rs"]
mod media;
#[path = "../src/settings.rs"]
mod settings;
#[path = "../src/stream.rs"]
mod stream;
#[path = "../src/xmledit.rs"]
mod xmledit;

use device::{Cmd, Device, DeviceConfig};
use neolink_core::bc_protocol::StreamKind;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut args = std::env::args().skip(1);
    let want = args.next().expect("camera name");
    let kind = if args.next().as_deref() == Some("sub") { StreamKind::Sub } else { StreamKind::Main };
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(120);
    let path = dirs::config_dir().unwrap().join("Reolink-rs/devices.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let d = saved["devices"].as_array().unwrap().iter().find(|d| d["name"] == want.as_str()).expect("no such camera");
    let mut cfg: DeviceConfig = serde_json::from_value(d.clone())?;
    secrets::load(&mut cfg); // the password is in the OS credential store

    let rt = tokio::runtime::Runtime::new()?;
    let feeds = stream::Feeds::default();
    let port = rt.block_on(stream::serve(feeds.clone(), "check".into()))?;
    let dev = Device::spawn(rt.handle(), 1, cfg);
    feeds.lock().unwrap().insert(1, dev.views.clone());
    dev.send(Cmd::Live(Some(kind)));
    println!("raw:  http://127.0.0.1:{port}/raw/1?t=check");
    println!("mjpeg: http://127.0.0.1:{port}/live/1?t=check");
    // Halfway, switch streams: the player must reconfigure for the other resolution (and codec, if it differs).
    std::thread::sleep(Duration::from_secs(secs / 2));
    let other = if kind == StreamKind::Main { StreamKind::Sub } else { StreamKind::Main };
    dev.send(Cmd::Live(Some(other)));
    std::thread::sleep(Duration::from_secs(secs - secs / 2));
    println!("status: {}", dev.state.lock().unwrap().status);
    Ok(())
}
