#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod device;
mod media;
mod prefs;
mod recordings;
mod xmledit;
mod scan;
mod secrets;
mod settings;
mod stream;

use device::{Cmd, Device, DeviceConfig};
use neolink_core::bc_protocol::{Direction, StreamKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

/// %APPDATA% on Windows, ~/.config on Linux, ~/Library/Application Support on macOS.
fn config_path() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("Reolink-rs").join("devices.json")
}

/// The saved config, or defaults when there is none. An unreadable file is moved aside rather than left for the
/// next save to overwrite with an empty device list.
fn load_saved(path: &Path) -> Saved {
    let Ok(bytes) = std::fs::read(path) else { return Saved::default() };
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        let keep = path.with_extension("unreadable.json");
        log::error!("{} is unreadable ({e}); kept as {}", path.display(), keep.display());
        let _ = std::fs::rename(path, &keep);
        Saved::default()
    })
}

/// Replace `path` with `bytes` so a crash or power cut leaves either the old file or the new one, never half of one.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
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
    fn spawn(&self, inner: &mut Inner, mut cfg: DeviceConfig) -> u64 {
        if cfg.secret.is_empty() {
            cfg.secret = secrets::new_id();
        }
        inner.next_id += 1;
        let dev = Device::spawn(self.rt.handle(), inner.next_id, cfg);
        self.feeds.lock().unwrap().insert(dev.id, dev.views.clone());
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
            devices: self.devices.iter().map(|d| secrets::for_disk(&d.config)).collect(),
            layout: self.layout,
            tiles: self.tiles.iter().map(|t| t.and_then(idx)).collect(),
            clear: self.clear.iter().filter_map(|&id| idx(id)).collect(),
            prefs: prefs::get(),
            removed: self.removed.iter().map(secrets::for_disk).collect(),
        };
        let path = config_path();
        if let Err(e) = write_atomic(&path, &serde_json::to_vec_pretty(&saved).unwrap()) {
            log::error!("save {}: {e}", path.display());
        }
    }

    /// Forget removed cameras that `keep` rejects, with their stored passwords (unless a device still uses one).
    fn drop_removed(&mut self, keep: impl Fn(&DeviceConfig) -> bool) {
        let (kept, dropped) = std::mem::take(&mut self.removed).into_iter().partition(|d| keep(d));
        self.removed = kept;
        for d in dropped {
            if !self.devices.iter().any(|x| x.config.secret == d.secret) {
                secrets::delete(&d.secret);
            }
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
    recording_paused: bool,
    /// Tiles may decode with WebCodecs; WebKitGTK's support is too patchy, so Linux keeps MJPEG.
    webcodecs: bool,
}

#[tauri::command]
fn get_state(app: State<App>) -> StateDto {
    let mut inner = app.inner.lock().unwrap();
    inner.sync_live(); // picks up recordings that ended off screen, so their streams stop
    // A camera seen on AES is pinned to it from then on (DeviceConfig::aes); saved the first time.
    let mut pinned = false;
    for d in inner.devices.iter_mut().filter(|d| !d.config.aes) {
        if d.state.lock().unwrap().aes {
            d.config.aes = true;
            pinned = true;
        }
    }
    if pinned {
        inner.save();
    }
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
        recording_paused: device::RECORDING_PAUSED.load(std::sync::atomic::Ordering::Relaxed),
        webcodecs: !cfg!(target_os = "linux"),
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

// (async): off the main thread, which also runs the window; disk writes and child processes there stutter it.
#[tauri::command(async)]
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
    // Never from the page: an id it picked could overwrite another camera's stored password. Editing a camera
    // also clears its AES pin, which is how a user accepts a camera that really did change encryption.
    config.secret.clear();
    config.aes = false;
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
            config.secret = inner.devices[i].config.secret.clone();
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

#[tauri::command(async)]
fn remove_device(app: State<App>, id: u64) -> Result<(), String> {
    let mut inner = app.inner.lock().unwrap();
    let i = inner.index(id)?;
    let old = inner.devices.remove(i); // Drop aborts its worker
    let cfg = old.config.clone();
    inner.drop_removed(|d| !(d.uid.eq_ignore_ascii_case(&cfg.uid) && d.host == cfg.host));
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

#[tauri::command(async)]
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
#[tauri::command(async)]
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

#[tauri::command(async)]
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

#[derive(Serialize)]
struct AddFound {
    added: bool,
    /// When not added: cameras whose saved login could be tried on it, if the user agrees.
    reusable: Vec<String>,
}

/// Add a camera found on the LAN. Only the factory default login (admin, no password) is tried on its own: anything
/// on the network can answer the scan, and a login hands it MD5(password + a nonce it picks), which it can crack
/// offline. With `reuse` (the user agreed), the login of a removed camera with this UID or IP and the logins the
/// other cameras use are tried too. At most three logins, so the camera's lock-out after repeated failures isn't
/// reached.
#[tauri::command]
async fn add_found(app: State<'_, App>, uid: String, ip: String, reuse: bool) -> Result<AddFound, String> {
    // (label, config) for each login, factory default first.
    let candidates: Vec<(String, DeviceConfig)> = {
        let inner = app.inner.lock().unwrap();
        let fresh = |username: &str, password: &str| DeviceConfig {
            host: ip.clone(), uid: uid.clone(), username: username.into(), password: password.into(), ..Default::default()
        };
        let mut c = vec![(String::new(), fresh("admin", ""))];
        c.extend(inner.removed.iter()
            .filter(|d| d.uid.eq_ignore_ascii_case(&uid) || d.host == ip)
            .map(|d| (d.name.clone(), DeviceConfig { host: ip.clone(), uid: uid.clone(), aes: false, ..d.clone() })));
        c.extend(inner.devices.iter().map(|d| (d.config.name.clone(), fresh(&d.config.username, &d.config.password))));
        let mut seen = std::collections::HashSet::new();
        c.retain(|(_, d)| seen.insert((d.username.clone(), d.password.clone())));
        c.truncate(3);
        c
    };
    let (default, saved) = candidates.split_at(1);
    let tries = if reuse { &candidates[..] } else { default };
    for (_, mut cfg) in tries.iter().cloned() {
        let probe = { let cfg = cfg.clone(); app.rt.spawn(async move { tokio::time::timeout(std::time::Duration::from_secs(15), device::probe_login(&cfg)).await }) };
        let Ok(Ok(Ok(name))) = probe.await else { continue };
        if cfg.name.is_empty() {
            cfg.name = if name.is_empty() { format!("Camera {}", ip.rsplit('.').next().unwrap_or(&ip)) } else { name };
        }
        let mut inner = app.inner.lock().unwrap();
        if !inner.devices.iter().any(|d| d.config.uid.eq_ignore_ascii_case(&uid)) {
            app.spawn(&mut inner, cfg);
            inner.drop_removed(|d| !d.uid.eq_ignore_ascii_case(&uid));
            inner.changed();
        } // else: added meanwhile
        return Ok(AddFound { added: true, reusable: vec![] });
    }
    let reusable = if reuse { vec![] } else { saved.iter().map(|(name, _)| name.clone()).collect() };
    Ok(AddFound { added: false, reusable })
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

#[tauri::command(async)]
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

/// Open the system file manager at `path`; with `select`, at its folder with it highlighted where the platform can.
fn reveal(path: &Path, select: bool) {
    use std::process::Command;
    let mut cmd;
    if cfg!(windows) {
        cmd = Command::new("explorer");
        if select {
            cmd.arg(format!("/select,{}", path.display()));
        } else {
            cmd.arg(path);
        }
    } else if cfg!(target_os = "macos") {
        cmd = Command::new("open");
        if select {
            cmd.arg("-R");
        }
        cmd.arg(path);
    } else {
        // xdg-open can't highlight a file, so open its folder.
        cmd = Command::new("xdg-open");
        cmd.arg(if select { path.parent().unwrap_or(path) } else { path });
    }
    if let Err(e) = cmd.spawn() {
        log::error!("open {}: {e}", path.display());
    }
}

#[tauri::command(async)]
fn open_media_folder() {
    let dir = prefs::get().record_dir;
    let _ = std::fs::create_dir_all(&dir);
    reveal(&dir, false);
}

#[tauri::command]
fn set_live_paused(app: State<App>, paused: bool) {
    let mut inner = app.inner.lock().unwrap();
    inner.paused = paused;
    inner.sync_live();
}

#[tauri::command(async)]
fn list_recordings(handle: tauri::AppHandle) -> recordings::Listing {
    let listing = recordings::list(&prefs::get().record_dir);
    // The player may read these recordings and nothing else: the folder is a setting that can point anywhere.
    for f in &listing.files {
        allow_asset(&handle, &listing.dir.join(&f.name));
    }
    listing
}

fn allow_asset(handle: &tauri::AppHandle, file: &Path) {
    if let Err(e) = handle.asset_protocol_scope().allow_file(file) {
        log::error!("asset scope {}: {e}", file.display());
    }
}

#[tauri::command(async)]
fn delete_recording(name: String) -> Result<(), String> {
    let dir = prefs::get().record_dir;
    let path = recordings::resolve(&dir, &name)?;
    std::fs::remove_file(&path).map_err(|e| format!("can't delete {name}: {e}"))?;
    let _ = std::fs::remove_file(recordings::thumb_dir(&dir).join(name).with_extension("jpg"));
    Ok(())
}

#[tauri::command]
async fn recording_thumb(handle: tauri::AppHandle, name: String) -> Result<String, String> {
    let dir = prefs::get().record_dir;
    let src = recordings::resolve(&dir, &name)?;
    let thumb = recordings::thumb(&src, &dir).await?;
    allow_asset(&handle, &thumb);
    Ok(thumb.display().to_string())
}

#[tauri::command(async)]
fn reveal_recording(name: String) -> Result<(), String> {
    let path = recordings::resolve(&prefs::get().record_dir, &name)?;
    reveal(&path, true);
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

/// Track whether the window can be seen (minimized or tucked in the tray), so hidden windows don't stream.
fn sync_hidden(window: &tauri::Window) {
    let hidden = window.is_minimized().unwrap_or(false) || !window.is_visible().unwrap_or(true);
    set_hidden(&window.state::<App>(), hidden);
}

fn set_hidden(app: &App, hidden: bool) {
    let mut inner = app.inner.lock().unwrap();
    if inner.hidden != hidden {
        inner.hidden = hidden;
        inner.sync_live();
    }
}

fn show_window(handle: &tauri::AppHandle) {
    if let Some(w) = handle.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        set_hidden(&handle.state::<App>(), false);
    }
}

fn set_recording_paused(handle: &tauri::AppHandle, paused: bool) {
    device::RECORDING_PAUSED.store(paused, std::sync::atomic::Ordering::Relaxed);
    if paused {
        // Scheduled recording stops on its next check; manual ones stop here.
        for d in &handle.state::<App>().inner.lock().unwrap().devices {
            d.send(Cmd::Record(false));
        }
    }
}

fn tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let show = MenuItem::with_id(app, "show", "Show Reolink-rs", true, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, "pause", "Pause recording", true, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &pause, &PredefinedMenuItem::separator(app)?, &quit])?;
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().expect("bundle icon").clone())
        .tooltip("Reolink-rs")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |handle, e| match e.id().as_ref() {
            "show" => show_window(handle),
            "pause" => set_recording_paused(handle, pause.is_checked().unwrap_or(false)),
            "quit" => {
                // Stop every device first: closing the recorders lets ffmpeg finish their MP4s.
                handle.state::<App>().inner.lock().unwrap().devices.clear();
                std::thread::sleep(std::time::Duration::from_millis(500)); // shortcut: fixed grace for the aborted tasks to drop; wait on them if files still come out cut
                handle.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn main() {
    env_logger::init();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let feeds = stream::Feeds::default();
    let token = stream::token();
    let port = rt.block_on(stream::serve(feeds.clone(), token.clone())).expect("start stream server");

    let mut saved = load_saved(&config_path());
    // Passwords still in the file (saved before the credential store) move into the store at the save below.
    let migrate = saved.devices.iter().chain(&saved.removed).any(|c| !c.password.is_empty() || c.secret.is_empty());
    for c in saved.devices.iter_mut().chain(saved.removed.iter_mut()) {
        secrets::load(c);
    }
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
        if migrate {
            inner.save();
        }
    }

    tauri::Builder::default()
        // Must be first: a second launch shows this instance's window and exits before setting anything up.
        .plugin(tauri_plugin_single_instance::init(|handle, _, _| show_window(handle)))
        .plugin(tauri_plugin_dialog::init())
        .manage(app)
        .invoke_handler(tauri::generate_handler![
            get_state, device_config, save_device, remove_device, set_layout, assign_tile,
            set_clear, ptz, snapshot, record, open_media_folder, scan_lan, set_solo, set_view_sizes,
            camera_pages, camera_settings, save_camera_settings, sync_camera_time, reboot_camera, camera_test,
            camera_snapshot,
            get_prefs, save_prefs, pick_folder, add_found,
            set_live_paused, list_recordings, delete_recording, reveal_recording, export_clip, recording_still, recording_thumb
        ])
        .setup(|app| Ok(tray(app)?))
        // Minimizing or closing tucks the window into the tray; Quit is in the tray menu.
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
                sync_hidden(window);
            }
            tauri::WindowEvent::Resized(_) => {
                if window.is_minimized().unwrap_or(false) {
                    let _ = window.hide();
                }
                sync_hidden(window);
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("run tauri app");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_saves_atomically_and_unreadable_files_are_kept() {
        let dir = std::env::temp_dir().join("reolink-rs-config-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("devices.json");
        assert!(load_saved(&path).devices.is_empty(), "no file: defaults");

        let cam = DeviceConfig { name: "Front".into(), host: "192.168.1.9".into(), ..Default::default() };
        let saved = Saved { devices: vec![cam], layout: 4, ..Default::default() };
        write_atomic(&path, &serde_json::to_vec(&saved).unwrap()).unwrap();
        write_atomic(&path, &serde_json::to_vec(&saved).unwrap()).unwrap(); // replaces an existing file
        assert_eq!(load_saved(&path).devices[0].name, "Front");
        assert!(!path.with_extension("tmp").exists());

        // A half-written file loads as defaults but is moved aside, so the next save can't erase the cameras in it.
        std::fs::write(&path, br#"{"devices":[{"name":"Fro"#).unwrap();
        assert!(load_saved(&path).devices.is_empty());
        assert!(!path.exists());
        assert!(std::fs::read_to_string(dir.join("devices.unreadable.json")).unwrap().contains("Fro"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
