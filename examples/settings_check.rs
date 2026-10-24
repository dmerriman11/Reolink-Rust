#![allow(dead_code)] // shares the app's modules but uses only part of them
//! Validate camera settings end to end on saved cameras, through the same worker and engine the app uses:
//! `cargo run --example settings_check -- [--write]`
//! Reads every settings page. With --write, nudges a few harmless values by one step, verifies the camera
//! applied them, then restores the originals and verifies again.

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
use settings::{Camera, Msg};
use std::collections::BTreeMap;
use std::time::Duration;

struct Cam(tokio::sync::mpsc::UnboundedSender<Cmd>);

impl Cam {
    async fn request(&self, msg: Msg, set: Option<String>) -> Result<String, String> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.0.send(Cmd::Xml { msg, set, reply }).map_err(|e| e.to_string())?;
        tokio::time::timeout(Duration::from_secs(20), rx).await.map_err(|_| "timeout".to_string())?.map_err(|e| e.to_string())?
    }
}

impl Camera for Cam {
    fn get(&self, msg: Msg) -> impl std::future::Future<Output = Result<String, String>> + Send {
        self.request(msg, None)
    }
    fn set(&self, msg: Msg, xml: String) -> impl std::future::Future<Output = Result<(), String>> + Send {
        async move { self.request(msg, Some(xml)).await.map(|_| ()) }
    }
}

/// A one-step change that stays in range, so restoring is always possible.
fn nudge(v: &settings::FieldView) -> String {
    let cur: i64 = v.value.parse().unwrap();
    if cur < v.max.unwrap() { cur + 1 } else { cur - 1 }.to_string()
}

async fn check(cam: &Cam, name: &str, write: bool) -> anyhow::Result<bool> {
    // The worker connects in the background; wait until a settings read succeeds.
    for _ in 0..30 {
        if cam.get(Msg { get: 80, set: None, channel: false }).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let mut all_ok = true;
    for page in settings::PAGES {
        let view = settings::read_page(cam, page).await;
        let shown: Vec<String> = view.fields.iter().map(|f| format!("{}={}", f.key, f.display)).collect();
        println!("{name:10} {:12} {:2} fields: {}", page.id, view.fields.len(), shown.join(", "));
        if view.fields.is_empty() {
            all_ok = false;
        }
    }
    if !write {
        return Ok(all_ok);
    }
    for (page_id, key, untouched) in [("lights", "ir_bright", "status_led"), ("display", "sharpen", "bright")] {
        let page = settings::page(page_id).unwrap();
        let before = settings::read_page(cam, page).await;
        let field = before.fields.iter().find(|f| f.key == key).expect("field present");
        let other = before.fields.iter().find(|f| f.key == untouched).map(|f| f.value.clone());
        let original = field.value.clone();
        let target = nudge(field);
        let set = |v: String| -> BTreeMap<String, String> { [(key.to_string(), v)].into_iter().collect() };
        let r1 = settings::write_page(cam, page, &set(target.clone())).await;
        let r2 = settings::write_page(cam, page, &set(original.clone())).await;
        let after = settings::read_page(cam, page).await;
        let restored = after.fields.iter().find(|f| f.key == key).map(|f| f.value.clone());
        let other_after = after.fields.iter().find(|f| f.key == untouched).map(|f| f.value.clone());
        let ok = r1[0].ok && r2[0].ok && restored.as_deref() == Some(original.as_str()) && other == other_after;
        all_ok &= ok;
        println!(
            "{name:10} write {key}: {original} -> {target} ({}) -> {original} ({}); {untouched} unchanged: {}  => {}",
            if r1[0].ok { "verified" } else { r1[0].error.as_deref().unwrap_or("failed") },
            if r2[0].ok { "verified" } else { r2[0].error.as_deref().unwrap_or("failed") },
            other == other_after,
            if ok { "PASS" } else { "FAIL" }
        );
    }
    Ok(all_ok)
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let write = std::env::args().any(|a| a == "--write");
    let path = std::path::PathBuf::from(std::env::var("APPDATA")?).join("Reolink-rs/devices.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let rt = tokio::runtime::Runtime::new()?;
    let mut all_ok = true;
    for (i, d) in saved["devices"].as_array().unwrap().iter().enumerate() {
        let cfg: DeviceConfig = serde_json::from_value(d.clone())?;
        let name = cfg.name.clone();
        let dev = Device::spawn(rt.handle(), i as u64, cfg);
        all_ok &= rt.block_on(check(&Cam(dev.sender()), &name, write))?;
    }
    println!("{}", if all_ok { "ALL PASS" } else { "SOME CHECKS FAILED" });
    Ok(())
}
