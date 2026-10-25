#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod device;
mod media;
mod prefs;
mod recordings;
mod xmledit;
mod scan;
mod settings;
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
    #[serde(default)]
    prefs: prefs::Prefs,
    /// Cameras the user removed, kept so they can be re-added from "Found nearby" without retyping the login.
    #[serde(default)]
    removed: Vec<DeviceConfig>,
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
    /// Playback view open: the live grid is off screen, so it shouldn't stream either.
    paused: bool,
    /// Last JPEG box sent to each device.
    boxes: HashMap<u64, (u32, u32)>,
    next_id: u64,
    removed: Vec<DeviceConfig>,
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
        let visible: Vec<u64> = match (self.hidden || self.paused, self.solo) {
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
            prefs: prefs::get(),
            removed: self.removed.clone(),
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
    motion: bool,
    motion_events: u64,
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
                motion: s.motion,
                motion_events: s.motion_events,
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
    let old = inner.devices.remove(i); // Drop aborts its worker
    let cfg = old.config.clone();
    inner.removed.retain(|d| !(d.uid.eq_ignore_ascii_case(&cfg.uid) && d.host == cfg.host));
    inner.removed.push(cfg);
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

/// Add a camera found on the LAN without asking for its login when one is known: its own login if it was removed
/// earlier, else the login the user's other cameras use, else the factory default (admin, no password). Tries at most
/// three logins so the camera's lock-out after repeated failures isn't reached. Ok(false): ask the user.
#[tauri::command]
async fn add_found(app: State<'_, App>, uid: String, ip: String) -> Result<bool, String> {
    let candidates: Vec<DeviceConfig> = {
        let inner = app.inner.lock().unwrap();
        let fresh = |username: &str, password: &str| DeviceConfig {
            name: String::new(), host: ip.clone(), uid: uid.clone(), port: None,
            username: username.into(), password: password.into(), channel: 0,
        };
        let mut c: Vec<DeviceConfig> = inner.removed.iter()
            .filter(|d| d.uid.eq_ignore_ascii_case(&uid) || d.host == ip)
            .map(|d| DeviceConfig { host: ip.clone(), uid: uid.clone(), ..d.clone() })
            .collect();
        c.extend(inner.devices.iter().map(|d| fresh(&d.config.username, &d.config.password)));
        c.push(fresh("admin", ""));
        let mut seen = std::collections::HashSet::new();
        c.retain(|d| seen.insert((d.username.clone(), d.password.clone())));
        c.truncate(3);
        c
    };
    for mut cfg in candidates {
        let probe = { let cfg = cfg.clone(); app.rt.spawn(async move { tokio::time::timeout(std::time::Duration::from_secs(15), device::probe_login(&cfg)).await }) };
        let Ok(Ok(Ok(name))) = probe.await else { continue };
        if cfg.name.is_empty() {
            cfg.name = if name.is_empty() { format!("Camera {}", ip.rsplit('.').next().unwrap_or(&ip)) } else { name };
        }
        let mut inner = app.inner.lock().unwrap();
        if inner.devices.iter().any(|d| d.config.uid.eq_ignore_ascii_case(&uid)) {
            return Ok(true); // added meanwhile
        }
        inner.removed.retain(|d| !d.uid.eq_ignore_ascii_case(&uid));
        app.spawn(&mut inner, cfg);
        inner.changed();
        return Ok(true);
    }
    Ok(false)
}

/// The settings engine's view of a device: requests go through its worker, which owns the connection.
/// `.1` holds messages this camera refused to write (400/405), shared for as long as the device exists.
struct DeviceCam(tokio::sync::mpsc::UnboundedSender<Cmd>, std::sync::Arc<std::sync::Mutex<std::collections::HashSet<u32>>>);

impl DeviceCam {
    async fn request(&self, msg: settings::Msg, set: Option<String>) -> Result<String, String> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.0.send(Cmd::Xml { msg, set, reply }).map_err(|_| "device was removed".to_string())?;
        match tokio::time::timeout(std::time::Duration::from_secs(20), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("device restarted; try again".into()),
            Err(_) => Err("camera didn't answer in time (asleep or out of range?)".into()),
        }
    }
}

impl settings::Camera for DeviceCam {
    fn get(&self, msg: settings::Msg) -> impl std::future::Future<Output = Result<String, String>> + Send {
        self.request(msg, None)
    }
    async fn set(&self, msg: settings::Msg, xml: String) -> Result<(), String> {
        let r = self.request(msg, Some(xml)).await.map(|_| ());
        if let Err(e) = &r
            && settings::is_refusal(e)
        {
            self.1.lock().unwrap().insert(msg.set.unwrap_or(msg.get));
        }
        r
    }
    fn writable(&self, msg: settings::Msg) -> bool {
        !self.1.lock().unwrap().contains(&msg.set.unwrap_or(msg.get))
    }
}

fn device_cam(app: &State<'_, App>, id: u64) -> Result<DeviceCam, String> {
    let inner = app.inner.lock().unwrap();
    let d = &inner.devices[inner.index(id)?];
    Ok(DeviceCam(d.sender(), d.read_only.clone()))
}

#[derive(Serialize)]
struct PageInfo {
    id: &'static str,
    title: &'static str,
}

#[tauri::command]
fn camera_pages() -> Vec<PageInfo> {
    settings::PAGES.iter().map(|p| PageInfo { id: p.id, title: p.title }).collect()
}

#[tauri::command]
async fn camera_settings(app: State<'_, App>, id: u64, page: String) -> Result<settings::PageView, String> {
    let cam = device_cam(&app, id)?;
    let page = settings::page(&page).ok_or("unknown settings page")?;
    Ok(settings::read_page(&cam, page).await)
}

#[tauri::command]
async fn save_camera_settings(
    app: State<'_, App>,
    id: u64,
    page: String,
    changes: std::collections::BTreeMap<String, String>,
) -> Result<Vec<settings::WriteResult>, String> {
    let cam = device_cam(&app, id)?;
    let page = settings::page(&page).ok_or("unknown settings page")?;
    Ok(settings::write_page(&cam, page, &changes).await)
}

/// Set the camera clock to this PC's time, expressed in the camera's own time zone.
#[tauri::command]
async fn sync_camera_time(app: State<'_, App>, id: u64) -> Result<(), String> {
    use settings::Camera;
    let cam = device_cam(&app, id)?;
    let xml = cam.get(settings::SYSTEM_GENERAL).await?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs() as i64;
    cam.set(settings::SYSTEM_GENERAL, settings::with_clock(&xml, now)?).await
}

/// Have the camera send a test email / try the FTP server with its current settings.
#[tauri::command]
async fn camera_test(app: State<'_, App>, id: u64, which: String) -> Result<(), String> {
    let cam = device_cam(&app, id)?;
    let test = match which.as_str() {
        "email" => settings::EMAIL_TEST,
        "ftp" => settings::FTP_TEST,
        _ => return Err("unknown test".into()),
    };
    settings::run_test(&cam, test).await.map_err(|e| format!("Test failed: {e}"))
}

/// A still from the camera as a data: URL, for drawing motion areas and privacy masks over.
#[tauri::command]
async fn camera_snapshot(app: State<'_, App>, id: u64) -> Result<String, String> {
    use base64::Engine as _;
    let tx = device_cam(&app, id)?.0;
    let (reply, rx) = tokio::sync::oneshot::channel();
    tx.send(Cmd::SnapshotJpeg(reply)).map_err(|_| "device was removed".to_string())?;
    let jpeg = tokio::time::timeout(std::time::Duration::from_secs(20), rx)
        .await
        .map_err(|_| "camera didn't answer in time".to_string())?
        .map_err(|_| "device restarted; try again".to_string())??;
    Ok(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpeg)))
}

#[tauri::command]
async fn reboot_camera(app: State<'_, App>, id: u64) -> Result<(), String> {
    let tx = device_cam(&app, id)?.0;
    let (reply, rx) = tokio::sync::oneshot::channel();
    tx.send(Cmd::Reboot(reply)).map_err(|_| "device was removed".to_string())?;
    tokio::time::timeout(std::time::Duration::from_secs(20), rx)
        .await
        .map_err(|_| "camera didn't answer in time".to_string())?
        .map_err(|_| "device restarted; try again".to_string())?
}

#[tauri::command]
fn get_prefs() -> prefs::Prefs {
    prefs::get()
}

#[tauri::command]
fn save_prefs(app: State<App>, prefs: prefs::Prefs) -> Result<prefs::Prefs, String> {
    prefs.validate()?;
    if prefs.run_at_startup != prefs::get().run_at_startup {
        prefs::apply_run_at_startup(prefs.run_at_startup)?;
    }
    prefs::set(prefs);
    app.inner.lock().unwrap().save();
    Ok(prefs::get())
}

/// Native folder picker, starting in `current`.
#[tauri::command]
async fn pick_folder(window: tauri::Window, current: String) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    window.dialog().file().set_directory(current).set_parent(&window).pick_folder(move |p| {
        let _ = tx.send(p.and_then(|p| p.into_path().ok()).map(|p| p.display().to_string()));
    });
    rx.await.ok().flatten()
}

#[tauri::command]
fn open_media_folder() {
    let dir = prefs::get().record_dir;
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
}

#[tauri::command]
fn set_live_paused(app: State<App>, paused: bool) {
    let mut inner = app.inner.lock().unwrap();
    inner.paused = paused;
    inner.sync_live();
}

#[tauri::command]
fn list_recordings(handle: tauri::AppHandle) -> recordings::Listing {
    let dir = prefs::get().record_dir;
    // The folder is a setting, so the player's file access follows it.
    if let Err(e) = handle.asset_protocol_scope().allow_directory(&dir, false) {
        log::error!("asset scope {}: {e}", dir.display());
    }
    recordings::list(&dir)
}

#[tauri::command]
fn delete_recording(name: String) -> Result<(), String> {
    let path = recordings::resolve(&prefs::get().record_dir, &name)?;
    std::fs::remove_file(&path).map_err(|e| format!("can't delete {name}: {e}"))
}

#[tauri::command]
fn reveal_recording(name: String) -> Result<(), String> {
    let path = recordings::resolve(&prefs::get().record_dir, &name)?;
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    Ok(())
}

#[tauri::command]
async fn export_clip(name: String, from: f64, to: f64) -> Result<String, String> {
    let dir = prefs::get().record_dir;
    let src = recordings::resolve(&dir, &name)?;
    recordings::export_clip(&src, &dir, from, to).await.map(|p| p.display().to_string())
}

#[tauri::command]
async fn recording_still(name: String, at: f64) -> Result<String, String> {
    let prefs = prefs::get();
    let src = recordings::resolve(&prefs.record_dir, &name)?;
    recordings::still(&src, &prefs.snapshot_dir, at).await.map(|p| p.display().to_string())
}

fn main() {
    env_logger::init();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let feeds = stream::Feeds::default();
    let token = stream::token();
    let port = rt.block_on(stream::serve(feeds.clone(), token.clone())).expect("start stream server");

    let saved: Saved = std::fs::read(config_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    prefs::set(saved.prefs.clone());
    let app = App {
        rt,
        inner: Mutex::new(Inner { devices: vec![], tiles: vec![None; MAX_TILES], layout: 4, clear: vec![], sent: HashMap::new(), solo: None, hidden: false, paused: false, boxes: HashMap::new(), next_id: 0, removed: saved.removed.clone() }),
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
        .plugin(tauri_plugin_dialog::init())
        .manage(app)
        .invoke_handler(tauri::generate_handler![
            get_state, device_config, save_device, remove_device, set_layout, assign_tile,
            set_clear, ptz, snapshot, record, open_media_folder, scan_lan, set_solo, set_view_sizes,
            camera_pages, camera_settings, save_camera_settings, sync_camera_time, reboot_camera, camera_test,
            camera_snapshot,
            get_prefs, save_prefs, pick_folder, add_found,
            set_live_paused, list_recordings, delete_recording, reveal_recording, export_clip, recording_still
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
