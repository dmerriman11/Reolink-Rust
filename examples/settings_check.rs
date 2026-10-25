#![allow(dead_code)] // shares the app's modules but uses only part of them
//! Validate camera settings end to end on saved cameras, through the same worker and engine the app uses:
//! `cargo run --example settings_check -- [--write] [--only <camera name>]`
//! Reads every settings page. With --write: (1) writes every writable field back with its current value, which
//! proves each write id and format is accepted without changing anything; (2) makes small reversible changes,
//! verifies the camera applied them, restores the originals and verifies again.

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
use std::time::{Duration, Instant};

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
    async fn set(&self, msg: Msg, xml: String) -> Result<(), String> {
        self.request(msg, Some(xml)).await.map(|_| ())
    }
}

fn refused(e: &str) -> bool {
    e.contains("doesn't allow changing")
}

fn one(key: &str, v: String) -> BTreeMap<String, String> {
    [(key.to_string(), v)].into_iter().collect()
}

fn flip(s: &str, i: usize) -> String {
    let c = if &s[i..i + 1] == "1" { "0" } else { "1" };
    format!("{}{c}{}", &s[..i], &s[i + 1..])
}

/// A reversible edit: page, field, and how to derive the test value from the current one.
fn tweak(key: &str, cur: &str) -> Option<String> {
    Some(match key {
        "md_sens" => (if cur == "50" { 49 } else { cur.parse::<i64>().ok()? + 1 }).to_string(),
        "dn_level" | "spk_volume" | "stitch_x" => (if cur == "100" { 99 } else { cur.parse::<i64>().ok()? + 1 }).to_string(),
        "push_interval" => (if cur == "20" { 30 } else { 20 }).to_string(),
        "ai_denoise" | "rich_push" | "pre_alarm" => (if cur == "1" { "0" } else { "1" }).to_string(),
        "ps_sched" => {
            let mut m: BTreeMap<String, String> = serde_json::from_str(cur).ok()?;
            let k = m.keys().next()?.clone();
            let t = flip(&m[&k], 3);
            m.insert(k, t);
            serde_json::to_string(&m).ok()?
        }
        "ir_bright" => (if cur == "100" { 99 } else { cur.parse::<i64>().ok()? + 1 }).to_string(),
        "tl_overwrite" => (if cur == "1" { "0" } else { "1" }).to_string(),
        "ftp_port" => (if cur == "2121" { "2122" } else { "2121" }).to_string(),
        "smtp_nick" => if cur == "Reolink-rs test" { "Reolink-rs check".into() } else { "Reolink-rs test".into() },
        "push_sched" => {
            let mut m: BTreeMap<String, String> = serde_json::from_str(cur).ok()?;
            let k = m.keys().next()?.clone();
            let t = flip(&m[&k], 3); // Sunday 03:00
            m.insert(k, t);
            serde_json::to_string(&m).ok()?
        }
        "md_zone" => {
            let mut z: serde_json::Value = serde_json::from_str(cur).ok()?;
            let cells = z["cells"].as_str()?.to_string();
            z["cells"] = flip(&cells, cells.len() - 1).into(); // bottom-right cell
            z.to_string()
        }
        "masks" => {
            let mut v: serde_json::Value = serde_json::from_str(cur).ok()?;
            if v["rects"].as_array()?.len() >= v["max"].as_u64()? as usize {
                return None;
            }
            v["rects"].as_array_mut()?.push(serde_json::json!([20, 20, 120, 80]));
            v["enable"] = "1".into();
            v.to_string()
        }
        _ => return None,
    })
}

const REVERSIBLE: &[(&str, &str)] = &[
    ("motion", "md_sens"), ("motion", "md_zone"), ("schedules", "push_sched"), ("privacy", "masks"),
    ("smart", "tl_overwrite"), ("ftp", "ftp_port"), ("email", "smtp_nick"), ("lights", "ir_bright"),
    ("display", "dn_level"), ("display", "ai_denoise"), ("display", "stitch_x"), ("detection", "push_interval"),
    ("detection", "rich_push"), ("audio", "spk_volume"), ("audio", "pre_alarm"), ("power", "ps_sched"),
];

/// The page once the camera answers it with `key` (motion writes make Argus cameras reconnect for up to ~2 min).
async fn read_with(cam: &Cam, page: &settings::Page, key: &str) -> settings::PageView {
    let t = Instant::now();
    loop {
        let v = settings::read_page(cam, page).await;
        if v.fields.iter().any(|f| f.key == key) || t.elapsed() > Duration::from_secs(180) {
            return v;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
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
    let mut total_fields = 0;
    for page in settings::PAGES {
        let t = Instant::now();
        let view = settings::read_page(cam, page).await;
        total_fields += view.fields.len();
        let shown: Vec<String> = view
            .fields
            .iter()
            .map(|f| {
                let v = if ["schedule", "zone", "masks"].contains(&f.kind) { format!("<{}>", f.kind) } else { f.display.clone() };
                format!("{}={}", f.key, v)
            })
            .collect();
        println!("{name:10} {:12} {:2} fields {:>5} ms: {}", page.id, view.fields.len(), t.elapsed().as_millis(), shown.join(", "));
    }
    println!("{name:10} total {total_fields} fields");
    if !write {
        return Ok(all_ok);
    }

    // CHECK_KEYS=a,b limits the run to those reversible changes (no no-op pass).
    let keys: Option<Vec<String>> = std::env::var("CHECK_KEYS").ok().map(|k| k.split(',').map(str::to_string).collect());
    // (1) No-op writes: every writable field back with its current value.
    for page in settings::PAGES.iter().filter(|_| keys.is_none()) {
        let view = settings::read_page(cam, page).await;
        let same: BTreeMap<String, String> = view
            .fields
            .iter()
            .filter(|f| f.kind != "info" && f.kind != "password")
            .map(|f| (f.key.to_string(), f.value.clone()))
            .collect();
        if same.is_empty() {
            continue;
        }
        let r = settings::write_page(cam, page, &same).await;
        if r.iter().all(|r| r.error.as_deref().is_some_and(refused)) {
            println!("{name:10} no-op write {:12} => READ-ONLY (camera refuses writes; the app shows these fields read-only)", page.id);
            continue;
        }
        let bad: Vec<_> = r.iter().filter(|r| !r.ok).map(|r| format!("{}: {}", r.key, r.error.clone().unwrap_or_default())).collect();
        all_ok &= bad.is_empty();
        println!("{name:10} no-op write {:12} {} fields  => {}", page.id, r.len(), if bad.is_empty() { "PASS".into() } else { format!("FAIL {bad:?}") });
    }

    // (2) Reversible changes.
    for (page_id, key) in REVERSIBLE.iter().filter(|(_, k)| keys.as_ref().is_none_or(|ks| ks.iter().any(|x| x == k))) {
        let page = settings::page(page_id).unwrap();
        let before = read_with(cam, page, key).await;
        let Some(field) = before.fields.iter().find(|f| f.key == *key) else {
            println!("{name:10} skip {key}: not offered by this camera");
            continue;
        };
        let original = field.value.clone();
        let Some(target) = tweak(key, &original) else {
            println!("{name:10} skip {key}: no safe test value");
            continue;
        };
        let r1 = settings::write_page(cam, page, &one(key, target)).await;
        if r1[0].error.as_deref().is_some_and(refused) {
            println!("{name:10} change {page_id}/{key}: READ-ONLY (camera refuses writes)");
            continue;
        }
        let r2 = settings::write_page(cam, page, &one(key, original.clone())).await;
        let after = read_with(cam, page, key).await;
        let restored = after.fields.iter().find(|f| f.key == *key).map(|f| f.value.clone());
        let ok = r1[0].ok && r2[0].ok && restored.as_deref() == Some(original.as_str());
        all_ok &= ok;
        println!(
            "{name:10} change {page_id}/{key}: applied {} restored {}  => {}",
            if r1[0].ok { "verified".into() } else { r1[0].error.clone().unwrap_or_default() },
            if r2[0].ok { "verified".into() } else { r2[0].error.clone().unwrap_or_default() },
            if ok { "PASS" } else { "FAIL" }
        );
    }
    Ok(all_ok)
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().collect();
    let write = args.iter().any(|a| a == "--write");
    let only = args.iter().position(|a| a == "--only").and_then(|i| args.get(i + 1)).cloned();
    let path = std::path::PathBuf::from(std::env::var("APPDATA")?).join("Reolink-rs/devices.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let rt = tokio::runtime::Runtime::new()?;
    let mut all_ok = true;
    for (i, d) in saved["devices"].as_array().unwrap().iter().enumerate() {
        let cfg: DeviceConfig = serde_json::from_value(d.clone())?;
        let name = cfg.name.clone();
        if only.as_ref().is_some_and(|o| *o != name) {
            continue;
        }
        let dev = Device::spawn(rt.handle(), i as u64, cfg);
        all_ok &= rt.block_on(check(&Cam(dev.sender()), &name, write))?;
    }
    println!("{}", if all_ok { "ALL PASS" } else { "SOME CHECKS FAILED" });
    Ok(())
}
