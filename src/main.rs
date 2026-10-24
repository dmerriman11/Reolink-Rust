#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod device;
mod media;
mod scan;
mod stream;

use device::{Cmd, Device, DeviceConfig};
use neolink_core::bc_protocol::{Direction, StreamKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Manager, State};

const LAYOUTS: [usize; 5] = [1, 2, 4, 9, 16];
const MAX_TILES: usize = 16;

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    devices: Vec<DeviceConfig>,
    layout: usize,
    /// Device index shown in each tile.
    tiles: Vec<Option<usize>>,
    /// Device indexes set to the Clear (main) stream.
    #[serde(default)]
    clear: Vec<usize>,
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    base.join("Reolink-rs").join("devices.json")
}

struct Inner {
    devices: Vec<Device>,
    tiles: Vec<Option<u64>>,
    layout: usize,
    clear: Vec<u64>,
    /// Last live request sent to each device, so syncing only sends changes.
    sent: HashMap<u64, Option<StreamKind>>,
    /// Tile shown alone (double-click); the other tiles are hidden and shouldn't stream.
    solo: Option<usize>,
    /// Window minimized: stop streams nobody can see (recordings keep theirs).
    hidden: bool,
    /// Last JPEG box sent to each device.
    boxes: HashMap<u64, (u32, u32)>,
    next_id: u64,
}

struct App {
    rt: tokio::runtime::Runtime,
    inner: Mutex<Inner>,
    feeds: stream::Feeds,
    port: u16,
    token: String,
}

impl App {
    fn spawn(&self, inner: &mut Inner, cfg: DeviceConfig) -> u64 {
        inner.next_id += 1;
        let dev = Device::spawn(self.rt.handle(), inner.next_id, cfg);
        self.feeds.lock().unwrap().insert(dev.id, dev.frames.clone());
        inner.devices.push(dev);
        inner.next_id
    }
}

impl Inner {
    fn index(&self, id: u64) -> Result<usize, String> {
        self.devices.iter().position(|d| d.id == id).ok_or_else(|| "unknown device".to_string())
    }

    /// Stream only what's on screen. Clear (main) is honored only where a tile is big enough to show it
    /// (single view or 1/2 layouts); smaller tiles get the sub stream, ~5x less camera Wi-Fi airtime.
    fn sync_live(&mut self) {
        let visible: Vec<u64> = match (self.hidden, self.solo) {
            (true, _) => vec![],
            (false, Some(t)) => self.tiles.get(t).copied().flatten().into_iter().collect(),
            (false, None) => self.tiles[..self.layout].iter().flatten().copied().collect(),
        };
        let big = self.solo.is_some() || self.layout <= 2;
        for d in &self.devices {
            let want = if visible.contains(&d.id) {
                Some(if big && self.clear.contains(&d.id) { StreamKind::Main } else { StreamKind::Sub })
            } else if d.state.lock().unwrap().recording.is_some() {
                // Off screen but recording: keep the stream as it is.
                self.sent.get(&d.id).copied().flatten()
            } else {
                None
            };
            if self.sent.get(&d.id) != Some(&want) {
                d.send(Cmd::Live(want));
                self.sent.insert(d.id, want);
            }
        }
    }

    fn save(&self) {
        let idx = |id: u64| self.devices.iter().position(|d| d.id == id);
        let saved = Saved {
            devices: self.devices.iter().map(|d| d.config.clone()).collect(),
            layout: self.layout,
            tiles: self.tiles.iter().map(|t| t.and_then(idx)).collect(),
            clear: self.clear.iter().filter_map(|&id| idx(id)).collect(),
        };
        let path = config_path();
        let res = std::fs::create_dir_all(path.parent().unwrap())
            .and_then(|_| std::fs::write(&path, serde_json::to_vec_pretty(&saved).unwrap()));
        if let Err(e) = res {
            log::error!("save {}: {e}", path.display());
        }
    }

    fn changed(&mut self) {
        self.sync_live();
        self.save();
    }
}

#[derive(Serialize)]
struct DeviceDto {
    id: u64,
    name: String,
    online: bool,
    status: String,
    battery: Option<u32>,
    charging: bool,
    wifi_dbm: Option<i32>,
    wifi_level: Option<u8>,
    live: Option<&'static str>,
    clear: bool,
    recording: bool,
    message: Option<String>,
}

#[derive(Serialize)]
struct StateDto {
    devices: Vec<DeviceDto>,
    layout: usize,
    tiles: Vec<Option<u64>>,
    stream_base: String,
    token: String,
}

#[tauri::command]
fn get_state(app: State<App>) -> StateDto {
    let mut inner = app.inner.lock().unwrap();
    inner.sync_live(); // picks up recordings that ended off screen, so their streams stop
    let devices = inner
        .devices
        .iter()
        .map(|d| {
            let mut s = d.state.lock().unwrap();
            DeviceDto {
                id: d.id,
                name: d.config.name.clone(),
                online: s.online,
                status: s.status.clone(),
                battery: s.battery,
                charging: s.charging,
                wifi_dbm: s.wifi_dbm,
                wifi_level: s.wifi_dbm.map(device::wifi_level),
                live: s.live.map(|k| if k == StreamKind::Main { "main" } else { "sub" }),
                clear: inner.clear.contains(&d.id),
                recording: s.recording.is_some(),
                message: s.message.take(),
            }
        })
        .collect();
    StateDto {
        devices,
        layout: inner.layout,
        tiles: inner.tiles.clone(),
        stream_base: format!("http://127.0.0.1:{}", app.port),
        token: app.token.clone(),
    }
}

/// Config for the edit form; the password is never sent back to the UI.
#[tauri::command]
fn device_config(app: State<App>, id: u64) -> Result<DeviceConfig, String> {
    let inner = app.inner.lock().unwrap();
    let mut cfg = inner.devices[inner.index(id)?].config.clone();
    cfg.password.clear();
    Ok(cfg)
}

#[tauri::command]
fn save_device(app: State<App>, id: Option<u64>, mut config: DeviceConfig) -> Result<(), String> {
    config.name = config.name.trim().to_string();
    config.host = config.host.trim().to_string();
    config.uid = config.uid.trim().to_string();
    if config.name.is_empty() {
        return Err("Name is required".into());
    }
    if config.host.is_empty() && config.uid.is_empty() {
        return Err("Enter an IP/host or a UID".into());
    }
    let mut inner = app.inner.lock().unwrap();
    match id {
        None => {
            app.spawn(&mut inner, config);
        }
        Some(id) => {
            let i = inner.index(id)?;
            if config.password.is_empty() {
                config.password = inner.devices[i].config.password.clone(); // blank = keep current
            }
            let old = inner.devices.remove(i);
            app.feeds.lock().unwrap().remove(&old.id);
            inner.sent.remove(&old.id);
            let new_id = app.spawn(&mut inner, config);
            let dev = inner.devices.pop().expect("just spawned");
            inner.devices.insert(i, dev); // keep list order
            for t in inner.tiles.iter_mut().filter(|t| **t == Some(id)) {
                *t = Some(new_id);
            }
            for c in inner.clear.iter_mut().filter(|c| **c == id) {
                *c = new_id;
            }
        }
    }
    inner.changed();
    Ok(())
}

#[tauri::command]
fn remove_device(app: State<App>, id: u64) -> Result<(), String> {
    let mut inner = app.inner.lock().unwrap();
    let i = inner.index(id)?;
    inner.devices.remove(i); // Drop aborts its worker
    app.feeds.lock().unwrap().remove(&id);
    inner.sent.remove(&id);
    inner.clear.retain(|&c| c != id);
    for t in inner.tiles.iter_mut().filter(|t| **t == Some(id)) {
        *t = None;
    }
    inner.changed();
    Ok(())
}

#[tauri::command]
fn set_layout(app: State<App>, layout: usize) -> Result<(), String> {
    if !LAYOUTS.contains(&layout) {
        return Err("unsupported layout".into());
    }
    let mut inner = app.inner.lock().unwrap();
    inner.layout = layout;
    inner.solo = None;
    inner.changed();
    Ok(())
}

/// Show a device in a tile (moving it if it's already shown elsewhere), or clear the tile with id = None.
#[tauri::command]
fn assign_tile(app: State<App>, tile: usize, id: Option<u64>) -> Result<(), String> {
    let mut inner = app.inner.lock().unwrap();
    if tile >= MAX_TILES {
        return Err("bad tile".into());
    }
    if let Some(id) = id {
        inner.index(id)?;
        for t in inner.tiles.iter_mut().filter(|t| **t == Some(id)) {
            *t = None;
        }
    }
    inner.tiles[tile] = id;
    inner.changed();
    Ok(())
}

#[tauri::command]
fn set_solo(app: State<App>, tile: Option<usize>) {
    let mut inner = app.inner.lock().unwrap();
    inner.solo = tile.filter(|&t| t < MAX_TILES);
    inner.sync_live();
}

/// Tile sizes in device pixels, so each decoder makes JPEGs no bigger than what's shown.
#[tauri::command]
fn set_view_sizes(app: State<App>, sizes: Vec<(u64, u32, u32)>) {
    let mut inner = app.inner.lock().unwrap();
    for (id, w, h) in sizes {
        let b = media::jpeg_box(w, h);
        if inner.boxes.get(&id) != Some(&b)
            && let Some(d) = inner.devices.iter().find(|d| d.id == id)
        {
            d.send(Cmd::View(b));
            inner.boxes.insert(id, b);
        }
    }
}

#[tauri::command]
fn set_clear(app: State<App>, id: u64, clear: bool) -> Result<(), String> {
    let mut inner = app.inner.lock().unwrap();
    inner.index(id)?;
    inner.clear.retain(|&c| c != id);
    if clear {
        inner.clear.push(id);
    }
    inner.changed();
    Ok(())
}

fn send(app: &State<App>, id: u64, cmd: Cmd) -> Result<(), String> {
    let inner = app.inner.lock().unwrap();
    inner.devices[inner.index(id)?].send(cmd);
    Ok(())
}

#[tauri::command]
fn ptz(app: State<App>, id: u64, dir: String) -> Result<(), String> {
    let dir = match dir.as_str() {
        "up" => Direction::Up,
        "down" => Direction::Down,
        "left" => Direction::Left,
        "right" => Direction::Right,
        "stop" => Direction::Stop,
        _ => return Err("bad direction".into()),
    };
    send(&app, id, Cmd::Ptz(dir))
}

#[tauri::command]
fn snapshot(app: State<App>, id: u64) -> Result<(), String> {
    send(&app, id, Cmd::Snapshot)
}

#[tauri::command]
fn record(app: State<App>, id: u64, on: bool) -> Result<(), String> {
    send(&app, id, Cmd::Record(on))
}

#[derive(Serialize)]
struct FoundDto {
    #[serde(flatten)]
    found: scan::Found,
    /// Already in the device list (matched by UID or IP).
    added: bool,
}

#[tauri::command]
async fn scan_lan(app: State<'_, App>) -> Result<Vec<FoundDto>, String> {
    let found = app.rt.spawn(scan::scan(std::time::Duration::from_secs(3))).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    let inner = app.inner.lock().unwrap();
    Ok(found
        .into_iter()
        .map(|f| {
            let ip = f.ip.to_string();
            let added = inner.devices.iter().any(|d| d.config.uid.eq_ignore_ascii_case(&f.uid) || d.config.host == ip);
            FoundDto { found: f, added }
        })
        .collect())
}

#[tauri::command]
fn open_media_folder() {
    let dir = device::media_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
}

fn main() {
    env_logger::init();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let feeds = stream::Feeds::default();
    let token = stream::token();
    let port = rt.block_on(stream::serve(feeds.clone(), token.clone())).expect("start stream server");

    let saved: Saved = std::fs::read(config_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let app = App {
        rt,
        inner: Mutex::new(Inner { devices: vec![], tiles: vec![None; MAX_TILES], layout: 4, clear: vec![], sent: HashMap::new(), solo: None, hidden: false, boxes: HashMap::new(), next_id: 0 }),
        feeds,
        port,
        token,
    };
    {
        let mut inner = app.inner.lock().unwrap();
        let ids: Vec<u64> = saved.devices.into_iter().map(|c| app.spawn(&mut inner, c)).collect();
        inner.layout = if LAYOUTS.contains(&saved.layout) { saved.layout } else { 4 };
        for (t, i) in saved.tiles.iter().take(MAX_TILES).enumerate() {
            inner.tiles[t] = i.and_then(|i| ids.get(i).copied());
        }
        inner.clear = saved.clear.iter().filter_map(|&i| ids.get(i).copied()).collect();
        inner.sync_live();
    }

    tauri::Builder::default()
        .manage(app)
        .invoke_handler(tauri::generate_handler![
            get_state, device_config, save_device, remove_device, set_layout, assign_tile,
            set_clear, ptz, snapshot, record, open_media_folder, scan_lan, set_solo, set_view_sizes
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Resized(_) = event {
                let hidden = window.is_minimized().unwrap_or(false);
                let app = window.state::<App>();
                let mut inner = app.inner.lock().unwrap();
                if inner.hidden != hidden {
                    inner.hidden = hidden;
                    inner.sync_live();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("run tauri app");
}
