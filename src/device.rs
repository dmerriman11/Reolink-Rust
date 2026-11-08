//! One worker task per device: connect, login, stream, PTZ, snapshot, record.
//!
//! Unlike the official client there is no idle/battery disconnect: the session is held open
//! with pings for as long as the app runs, and any drop (battery camera or not) reconnects
//! automatically and resumes the live view that was showing.

use crate::media::{Decoder, Pushed, Raw, Recorder};
use neolink_core::bcmedia::model::VideoType;
use anyhow::{Context as _, Result};
use neolink_core::bc_protocol::{
    BcCamera, BcCameraOpt, ConnectionProtocol, Credentials, DiscoveryMethods, Direction, MotionData,
    MotionStatus, StreamData, StreamKind,
};
use neolink_core::bcmedia::model::BcMedia;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::watch;

/// Tray "Pause recording": no manual or scheduled recording starts while set.
pub static RECORDING_PAUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Latest decoded JPEG for the live view; None when not streaming.
pub type Frame = Option<Arc<Vec<u8>>>;

/// Where a camera's live view goes: JPEGs for MJPEG <img> tiles, encoded frames for WebCodecs tiles.
#[derive(Default)]
pub struct Views {
    pub jpeg: watch::Sender<Frame>,
    pub raw: crate::media::RawFeed,
    pub audio: crate::audio::Feed,
}

impl Views {
    fn stop(&self) {
        self.jpeg.send_replace(None);
        self.raw.clear();
        self.audio.clear();
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DeviceConfig {
    pub name: String,
    /// IP or hostname; optional when a UID is given.
    pub host: String,
    pub uid: String,
    pub port: Option<u16>,
    pub username: String,
    /// Held in the OS credential store under `secret` (see secrets.rs); in devices.json only if the store refused it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    /// NVR channel, 0 for a standalone camera.
    pub channel: u8,
    /// Names this camera's entry in the credential store.
    #[serde(default)]
    pub secret: String,
    /// The camera has logged in with AES before; a weaker offer later means something on the network is
    /// interfering, so it is refused. Editing the camera clears this.
    #[serde(default)]
    pub aes: bool,
}

/// Reply channel for request/response commands.
pub type Reply<T> = tokio::sync::oneshot::Sender<Result<T, String>>;

#[derive(Clone, Serialize)]
pub struct PtzPreset {
    pub id: u8,
    pub name: String,
}

pub enum PresetAction {
    List,
    Go(u8),
    Save(String),
    Rename(u8, String),
    Delete(u8),
}

fn optical_ptz(ptz_type: Option<u32>) -> bool {
    matches!(ptz_type, Some(1 | 2 | 4 | 5))
}

fn free_preset_id(presets: &[PtzPreset]) -> Result<u8, String> {
    // Official SDK preset builder accepts IDs 0..63.
    (0..64).find(|id| !presets.iter().any(|p| p.id == *id))
        .ok_or_else(|| "All 64 preset slots are occupied".into())
}

async fn preset_command(cam: &BcCamera, action: PresetAction) -> Result<Vec<PtzPreset>, String> {
    if let PresetAction::Save(name) | PresetAction::Rename(_, name) = &action {
        if name.trim().is_empty() || name.trim().len() > 31 || name.chars().any(char::is_control) {
            return Err("Enter a preset name of 1-31 UTF-8 bytes without control characters".into());
        }
    }
    let xml = cam.get_ptz_preset().await.map_err(|e| e.to_string())?;
    let mut presets: Vec<_> = xml.preset_list.preset.into_iter()
        .map(|p| PtzPreset { id: p.id, name: p.name.unwrap_or_else(|| format!("Preset {}", p.id)) })
        .collect();
    match action {
        PresetAction::List => {},
        PresetAction::Go(id) => {
            if !presets.iter().any(|p| p.id == id) { return Err("Preset no longer exists; refresh the list".into()); }
            cam.moveto_ptz_preset(id).await.map_err(|e| e.to_string())?;
        },
        PresetAction::Rename(id, name) => {
            let preset = presets.iter_mut().find(|p| p.id == id)
                .ok_or("Preset no longer exists; refresh the list")?;
            let name = name.trim().to_owned();
            cam.rename_ptz_preset(id, name.clone()).await.map_err(|e| e.to_string())?;
            preset.name = name;
        },
        PresetAction::Delete(id) => {
            if !presets.iter().any(|p| p.id == id) { return Err("Preset no longer exists; refresh the list".into()); }
            cam.delete_ptz_preset(id).await.map_err(|e| e.to_string())?;
            presets.retain(|p| p.id != id);
        },
        PresetAction::Save(name) => {
            let id = free_preset_id(&presets)?;
            let name = name.trim().to_owned();
            cam.set_ptz_preset(id, name.clone()).await.map_err(|e| e.to_string())?;
            presets.push(PtzPreset { id, name });
        },
    }
    presets.sort_by_key(|p| p.id);
    Ok(presets)
}

#[cfg(test)]
mod preset_tests {
    use super::*;
    #[test]
    fn parses_preset_lists_without_commands() {
        use neolink_core::bc::xml::PtzPreset;
        let xml = r#"<PtzPreset version="1.1"><channelId>3</channelId><presetList><preset><id>1</id><name>Gate &amp; yard</name></preset><preset><id>4</id><command>toPos</command></preset></presetList></PtzPreset>"#;
        let parsed: PtzPreset = quick_xml::de::from_str(xml).unwrap();
        assert_eq!(parsed.channel_id, 3);
        assert_eq!(parsed.preset_list.preset[0].name.as_deref(), Some("Gate & yard"));
        assert!(parsed.preset_list.preset[0].command.is_empty());
        assert_eq!(parsed.preset_list.preset[1].name, None);
        let empty: PtzPreset = quick_xml::de::from_str(r#"<PtzPreset version="1.1"><channelId>0</channelId><presetList/></PtzPreset>"#).unwrap();
        assert!(empty.preset_list.preset.is_empty());
    }
    #[test]
    fn optical_lens_requires_known_ability() {
        for t in [None, Some(0), Some(3), Some(6), Some(u32::MAX)] { assert!(!optical_ptz(t)); }
        for t in [1, 2, 4, 5] { assert!(optical_ptz(Some(t))); }
    }
    #[test]
    fn lowest_free_slot_never_overwrites() {
        let p = |id| PtzPreset { id, name: String::new() };
        assert_eq!(free_preset_id(&[]), Ok(0));
        assert_eq!(free_preset_id(&[p(3), p(1), p(1), p(0), p(255)]), Ok(2));
        let mut full: Vec<_> = (0..64).map(p).collect();
        assert!(free_preset_id(&full).is_err());
        full.pop();
        assert_eq!(free_preset_id(&full), Ok(63));
    }
}

pub enum Cmd {
    PtzPreset { action: PresetAction, reply: Reply<Vec<PtzPreset>> },
    SdSearch { day: String, reply: Reply<Vec<crate::sd::Clip>> },
    /// Download a clip to `dest`, reporting into `progress` (see sd::Progress); replaces no reply.
    SdDownload { start: String, end: String, dest: PathBuf, progress: crate::sd::Shared },
    /// Camera user accounts (messages 58/59); see users_command.
    Users { action: UserAction, reply: Reply<Vec<UserInfo>> },
    /// Read (`set: None`) or write a settings message; replies with the camera's XML (empty for writes).
    Xml { msg: crate::settings::Msg, set: Option<String>, reply: Reply<String> },
    Reboot(Reply<()>),
    /// A JPEG still for the settings editors (motion area, privacy masks).
    SnapshotJpeg(Reply<Vec<u8>>),
    Live(Option<StreamKind>),
    Ptz(Direction),
    Light(bool),
    Siren,
    /// Open the camera's speaker for the microphone; see crate::talk.
    Talk(Reply<crate::talk::Session>),
    Snapshot,
    Record(bool),
    /// Largest JPEG (device pixels) the tile can show; see media::jpeg_box.
    View((u32, u32)),
    /// Internal: the local recording schedule's wish changed (see prefs::Prefs::local_record).
    Auto,
    /// Drop the session and connect again now (live view and recording resume), or end the wait when offline.
    Reconnect,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserInfo {
    pub name: String,
    pub admin: bool,
    /// The account this app signs in with; it can't be changed or removed from here.
    pub own: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum UserAction {
    List,
    Add { name: String, password: String, admin: bool },
    Password { name: String, password: String },
    Delete { name: String },
}

/// Limits from the SDK structs (BC_MAX_NAME_LEN 32 incl. NUL) and the camera's own password rule.
pub fn validate_user(name: &str, password: Option<&str>) -> Result<(), String> {
    if name.is_empty() || name.len() > 31 || name.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("User name: 1–31 characters, no spaces".into());
    }
    if let Some(p) = password && (p.len() < 6 || p.len() > 31 || p.chars().any(|c| c.is_control() || c.is_whitespace())) {
        return Err("Password: 6–31 characters, no spaces".into());
    }
    Ok(())
}

/// Which users may be changed: never the account this app uses (its stored credentials would stop working) and
/// never an administrator's removal, so the camera can't be locked out from here.
fn user_change_allowed(action: &UserAction, users: &[UserInfo]) -> Result<(), String> {
    let target = match action { UserAction::Password { name, .. } | UserAction::Delete { name } => name, _ => return Ok(()) };
    let Some(u) = users.iter().find(|u| &u.name == target) else { return Err(format!("No user {target}")) };
    if u.own { return Err("This is the account Reolink-rs signs in with; change it in the camera's settings (Edit camera) instead".into()); }
    if u.admin && matches!(action, UserAction::Delete { .. }) { return Err("Administrator accounts can't be deleted from here".into()); }
    Ok(())
}

/// List, add, re-password or delete camera users; always answers with the list as the camera has it afterwards.
async fn users_command(cam: &BcCamera, login: &str, action: UserAction) -> Result<Vec<UserInfo>, String> {
    let list = |l: neolink_core::bc::xml::UserList| -> Vec<UserInfo> {
        l.user_list.unwrap_or_default().into_iter().map(|u| UserInfo { own: u.user_name == login, admin: u.user_level == 1, name: u.user_name }).collect()
    };
    let wait = Duration::from_secs(20);
    let users = list(tokio::time::timeout(wait, cam.get_users()).await.map_err(|_| "camera didn't answer")?.map_err(|e| match e {
        neolink_core::Error::CameraServiceUnavailable { .. } => "This camera has no user accounts to manage (battery cameras refuse the user list)".to_string(),
        e => e.to_string(),
    })?);
    user_change_allowed(&action, &users)?;
    let r = match &action {
        UserAction::List => return Ok(users),
        UserAction::Add { name, password, admin } => { validate_user(name, Some(password))?; tokio::time::timeout(wait, cam.add_user(name.clone(), password.clone(), u8::from(*admin))).await }
        UserAction::Password { name, password } => { validate_user(name, Some(password))?; tokio::time::timeout(wait, cam.modify_user(name.clone(), password.clone())).await }
        UserAction::Delete { name } => tokio::time::timeout(wait, cam.delete_user(name.clone())).await,
    };
    r.map_err(|_| "camera didn't answer")?.map_err(|e| e.to_string())?;
    Ok(list(tokio::time::timeout(wait, cam.get_users()).await.map_err(|_| "camera didn't answer")?.map_err(|e| e.to_string())?))
}

#[cfg(test)]
mod user_tests {
    use super::*;
    fn users() -> Vec<UserInfo> {
        vec![UserInfo { name: "admin".into(), admin: true, own: true }, UserInfo { name: "boss".into(), admin: true, own: false }, UserInfo { name: "kid".into(), admin: false, own: false }]
    }
    #[test]
    fn own_and_admin_accounts_are_protected() {
        let u = users();
        assert!(user_change_allowed(&UserAction::Delete { name: "admin".into() }, &u).is_err());
        assert!(user_change_allowed(&UserAction::Password { name: "admin".into(), password: "x".into() }, &u).is_err());
        assert!(user_change_allowed(&UserAction::Delete { name: "boss".into() }, &u).is_err());
        assert!(user_change_allowed(&UserAction::Password { name: "boss".into(), password: "x".into() }, &u).is_ok());
        assert!(user_change_allowed(&UserAction::Delete { name: "kid".into() }, &u).is_ok());
        assert!(user_change_allowed(&UserAction::Delete { name: "nobody".into() }, &u).is_err());
        assert!(user_change_allowed(&UserAction::Add { name: "new".into(), password: "secret1".into(), admin: false }, &u).is_ok());
    }
    #[test]
    fn user_fields_have_sdk_limits() {
        assert!(validate_user("guest", Some("secret1")).is_ok());
        assert!(validate_user("", Some("secret1")).is_err());
        assert!(validate_user("a b", Some("secret1")).is_err());
        assert!(validate_user(&"x".repeat(32), None).is_err());
        assert!(validate_user("guest", Some("short")).is_err());
    }
}

#[derive(Default)]
pub struct State {
    pub status: String,
    pub online: bool,
    pub battery: Option<u32>,
    pub charging: bool,
    /// Wi-Fi RSSI in dBm; None for wired cameras or before the first reading.
    pub wifi_dbm: Option<i32>,
    /// Audio has arrived on the current camera stream.
    pub audio: bool,
    pub lens_zoom: bool,
    pub lens_focus: bool,
    pub live: Option<StreamKind>,
    pub recording: Option<PathBuf>,
    pub message: Option<String>,
    /// The camera currently reports motion.
    pub motion: bool,
    /// Motion starts seen since the app started; the UI beeps when it grows.
    pub motion_events: u64,
    /// This session is AES encrypted; the app pins the camera to AES from then on (DeviceConfig::aes).
    pub aes: bool,
    /// When the live stream last delivered a video frame (or started): the UI shows a stalled picture from this.
    pub last_frame: Option<Instant>,
    /// Until when a manual light-on holds; the camera turns it off by itself then.
    pub light_until: Option<Instant>,
}

pub struct Device {
    /// Runtime-only id; stable while the app runs, unlike the list index.
    pub id: u64,
    pub config: DeviceConfig,
    pub views: Arc<Views>,
    /// Settings messages this camera refused to write; see settings::Camera::writable.
    pub read_only: Arc<Mutex<std::collections::HashSet<u32>>>,
    pub state: Arc<Mutex<State>>,
    tx: UnboundedSender<Cmd>,
    task: tokio::task::JoinHandle<()>,
}

impl Device {
    pub fn spawn(rt: &tokio::runtime::Handle, id: u64, config: DeviceConfig) -> Self {
        let state = Arc::new(Mutex::new(State { status: "Connecting".into(), ..Default::default() }));
        let (tx, rx) = unbounded_channel();
        let views = Arc::new(Views::default());
        let task = rt.spawn(run(config.clone(), rx, state.clone(), views.clone()));
        Self { id, config, views, read_only: Default::default(), state, tx, task }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn sender(&self) -> UnboundedSender<Cmd> {
        self.tx.clone()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// False for the -10 dBm placeholder Argus cameras report for ~5 s after connecting; real Wi-Fi is below -20.
pub fn real_rssi(dbm: i32) -> bool {
    dbm <= -20
}

/// Signal bars 0..=3 using the official client's thresholds (calcSignalStrength).
pub fn wifi_level(dbm: i32) -> u8 {
    match dbm {
        d if d < -70 => 0,
        d if d < -50 => 1,
        d if d < -40 => 2,
        _ => 3,
    }
}

/// Local time as YYYY-MM-DD_HH-MM-SS for file names.
// chrono, not time: time's now_local() refuses (falls back to UTC) on Linux/macOS once tokio has started threads.
fn stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string()
}

/// A warning when the recording drive is below the configured free-space threshold.
fn space_warning() -> Option<String> {
    let p = crate::prefs::get();
    let free = crate::prefs::free_bytes(&p.record_dir)?;
    let limit = p.space_warning_gb as u64 * 1_000_000_000;
    (p.space_warning_gb > 0 && free < limit)
        .then(|| format!("Low disk space: {:.1} GB free in {}", free as f64 / 1e9, p.record_dir.display()))
}

fn safe_name(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

/// Desired state that survives reconnects.
struct Wanted {
    live: Option<StreamKind>,
    record: bool,
    jpeg_box: (u32, u32),
}

async fn run(cfg: DeviceConfig, mut rx: UnboundedReceiver<Cmd>, state: Arc<Mutex<State>>, views: Arc<Views>) {
    let mut wanted = Wanted { live: None, record: false, jpeg_box: crate::media::DEFAULT_BOX };
    let mut aes = cfg.aes; // pinned once seen, for this run too
    let mut backoff = 1;
    'run: loop {
        let started = Instant::now();
        let err = match session(&cfg, &mut rx, &state, &views, &mut wanted, &mut aes).await {
            Ok(()) => return, // command channel closed: device removed
            Err(e) => e,
        };
        // A session that ran a while, or a reconnect the user asked for, starts the backoff over.
        if started.elapsed() > Duration::from_secs(60) || err.to_string() == RECONNECT {
            backoff = 1;
        }
        {
            let mut s = state.lock().unwrap();
            s.online = false;
            s.audio = false;
            s.recording = None;
            s.status = format!("Reconnecting in {backoff}s: {err:#}");
        }
        views.stop();
        log::warn!("{}: {err:#}", cfg.name);
        // Keep commands flowing while offline so the UI's wishes apply on reconnect.
        let sleep = tokio::time::sleep(Duration::from_secs(backoff));
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => break,
                cmd = rx.recv() => match cmd {
                    None => return,
                    Some(Cmd::Live(k)) => { wanted.live = k; state.lock().unwrap().live = k; }
                    Some(Cmd::Record(r)) => wanted.record = r,
                    Some(Cmd::View(b)) => wanted.jpeg_box = b,
                    Some(Cmd::SdSearch { reply, .. }) => { let _ = reply.send(Err("camera is offline".into())); }
                    Some(Cmd::SdDownload { progress, .. }) => { let mut p = progress.lock().unwrap(); p.done = true; p.error = Some("camera is offline".into()); }
                    Some(Cmd::Users { reply, .. }) => { let _ = reply.send(Err("camera is offline".into())); }
                    Some(Cmd::Xml { reply, .. }) => { let _ = reply.send(Err("camera is offline".into())); }
                    Some(Cmd::PtzPreset { reply, .. }) => { let _ = reply.send(Err("camera is offline".into())); }
                    Some(Cmd::Reboot(reply)) => { let _ = reply.send(Err("camera is offline".into())); }
                    Some(Cmd::SnapshotJpeg(reply)) => { let _ = reply.send(Err("camera is offline".into())); }
                    Some(Cmd::Reconnect) => { backoff = 1; continue 'run; }
                    Some(_) => {}
                },
            }
        }
        backoff = (backoff * 2).min(30);
    }
}

/// Log in once with `cfg` over the LAN (no relay) and return the camera's own name, for adding found cameras.
pub async fn probe_login(cfg: &DeviceConfig) -> Result<String> {
    // LAN only (the camera was just found here), but keep the UID: Argus cameras won't take a LAN session without it.
    let opts = BcCameraOpt { discovery: DiscoveryMethods::Local, max_discovery_retries: 3, ..camera_opts(cfg)? };
    let cam = BcCamera::new(&opts).await.context("connect")?;
    cam.login().await.context("login")?;
    let general = cam.get_xml(104, false).await.unwrap_or_default();
    let name = general.split("<deviceName>").nth(1).and_then(|r| r.split("</deviceName>").next()).unwrap_or("").trim().to_string();
    let _ = cam.logout().await;
    Ok(name)
}

fn camera_opts(cfg: &DeviceConfig) -> Result<BcCameraOpt> {
    let host = cfg.host.trim();
    let uid = cfg.uid.trim();
    let addrs: Vec<IpAddr> = if host.is_empty() {
        vec![]
    } else if let Ok(ip) = host.parse() {
        vec![ip]
    } else {
        use std::net::ToSocketAddrs;
        (host, 0).to_socket_addrs().with_context(|| format!("resolve {host}"))?.map(|a| a.ip()).collect()
    };
    anyhow::ensure!(!addrs.is_empty() || !uid.is_empty(), "need an IP/host or a UID");
    Ok(BcCameraOpt {
        name: cfg.name.clone(),
        channel_id: cfg.channel,
        addrs,
        uid: (!uid.is_empty()).then(|| uid.to_string()),
        port: cfg.port,
        protocol: ConnectionProtocol::TcpUdp,
        // Same reach as the official client: LAN first, then Reolink's P2P relay for UID devices.
        discovery: if uid.is_empty() { DiscoveryMethods::Local } else { DiscoveryMethods::Relay },
        max_discovery_retries: 10,
        credentials: Credentials {
            username: cfg.username.clone(),
            password: (!cfg.password.is_empty()).then(|| cfg.password.clone()),
        },
        debug: false,
    })
}

struct Live {
    kind: StreamKind,
    video_us: u32,
    stream: StreamData,
    decoder: Option<Decoder>,
    recorder: Option<Recorder>,
    /// Recorder starts on the next video frame, once the codec is known.
    record: bool,
    /// Decoder output box; a change takes effect at the next keyframe.
    jpeg_box: (u32, u32),
    /// False for a stream that only feeds a scheduled recording: nothing on screen, so nothing is decoded.
    show: bool,
}

async fn next_media(live: &mut Option<Live>) -> Result<Result<BcMedia, neolink_core::Error>, neolink_core::Error> {
    match live {
        Some(l) => l.stream.get_data().await,
        None => std::future::pending().await,
    }
}

/// Await a camera request while still taking media off the live stream. neolink's poller blocks once a stream's
/// buffers are full, and then no reply gets through, including the one `fut` is waiting for.
async fn draining<T>(
    fut: impl std::future::Future<Output = T>,
    live: &mut Option<Live>,
    name: &str,
    state: &Arc<Mutex<State>>,
    views: &Views,
) -> Result<T> {
    tokio::pin!(fut);
    loop {
        tokio::select! {
            r = &mut fut => return Ok(r),
            m = next_media(live) => {
                let media = m.context("stream")?.context("stream data")?;
                on_media(live.as_mut().expect("live set"), media, name, state, views);
            }
        }
    }
}

/// How long a click keeps the light on (seconds), so a forgotten light doesn't drain a battery camera.
const LIGHT_SECS: u16 = 180;
/// A camera that doesn't answer these in time would otherwise hold up its worker.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

/// Open the camera's speaker and start sending it whatever the session is given.
async fn start_talk(cam: Arc<BcCamera>, channel: u8, name: String, state: Arc<Mutex<State>>) -> Result<crate::talk::Session, String> {
    let ability = tokio::time::timeout(CONTROL_TIMEOUT, cam.talk_ability())
        .await
        .map_err(|_| "The camera didn't answer about its speaker".to_string())?
        .map_err(|e| format!("This camera can't talk: {e}"))?;
    let audio = ability.audio_config_list.into_iter().map(|c| c.audio_config).find(|c| c.audio_type == "adpcm" && c.length_per_encoder >= 2);
    let (Some(audio), Some(duplex), Some(mode)) = (audio, ability.duplex_list.into_iter().next(), ability.audio_stream_mode_list.into_iter().next()) else {
        return Err("This camera can't talk".into());
    };
    let (block, rate) = (audio.length_per_encoder as usize / 2, audio.sample_rate as u32);
    let config = neolink_core::bc::xml::TalkConfig {
        version: "1.1".into(),
        channel_id: channel,
        duplex: duplex.duplex,
        audio_stream_mode: mode.audio_stream_mode,
        audio_config: audio,
    };
    // A talk left open by a crash, or by another client, blocks this one. The official client ends it the same way.
    let _ = tokio::time::timeout(CONTROL_TIMEOUT, cam.talk_stop()).await;
    let (tx, rx) = crossbeam_channel::bounded(8); // about half a second of audio at most
    let rt = tokio::runtime::Handle::current();
    // talk_stream waits on the channel without yielding, so it gets a thread of its own.
    tokio::task::spawn_blocking(move || {
        if let Err(e) = rt.block_on(cam.talk_stream(rx, config)) {
            log::warn!("{name}: talk: {e}");
            state.lock().unwrap().message = Some(format!("Talking stopped: {e}"));
        }
    });
    Ok(crate::talk::Session::new(tx, block, rate))
}

/// A camera that doesn't answer a ping in this long is gone; reconnect instead of waiting forever.
const PING_TIMEOUT: Duration = Duration::from_secs(20);
/// A live stream with no video for this long is restarted by reconnecting, even while pings still work.
const STALL_RESTART: Duration = Duration::from_secs(20);
/// The session error for Cmd::Reconnect.
const RECONNECT: &str = "reconnect requested";

async fn next_motion(m: &mut Option<MotionData>) -> Result<MotionStatus, neolink_core::Error> {
    match m {
        Some(m) => m.next_motion().await,
        None => std::future::pending().await,
    }
}

/// Whether the local recording schedule wants this camera recorded right now.
fn auto_record_wanted(p: &crate::prefs::Prefs, moving: bool) -> bool {
    use chrono::{Datelike as _, Timelike as _};
    let now = chrono::Local::now();
    !RECORDING_PAUSED.load(std::sync::atomic::Ordering::Relaxed)
        && p.scheduled(now.weekday().num_days_from_sunday() as u8, now.hour() as u8) && (p.local_record == "continuous" || moving)
}

async fn session(
    cfg: &DeviceConfig,
    rx: &mut UnboundedReceiver<Cmd>,
    state: &Arc<Mutex<State>>,
    views: &Views,
    wanted: &mut Wanted,
    aes: &mut bool,
) -> Result<()> {
    let set = |f: &dyn Fn(&mut State)| f(&mut state.lock().unwrap());
    set(&|s| s.status = "Connecting".into());
    // Shared so settings requests can run as their own tasks without pausing the live stream.
    let cam = std::sync::Arc::new(BcCamera::new(&camera_opts(cfg)?).await.context("connect")?);
    set(&|s| s.status = "Logging in".into());
    cam.login().await.context("login")?;
    // BCEncrypt is a fixed XOR. A camera that has done AES doesn't go back: refuse rather than send video and
    // settings (camera passwords included) in the clear to whatever answered.
    if cam.is_aes() {
        *aes = true;
    } else if *aes {
        anyhow::bail!("camera offered weaker encryption than the AES it used before; refused in case something on the network is intercepting (edit the camera to accept it)");
    }

    // Fail closed: require advertised optical PTZ and a readable lens range.
    let lens = tokio::time::timeout(CONTROL_TIMEOUT, async {
        let support = cam.get_support().await.ok()?;
        let item = support.items.iter().find(|i| i.chn_id == u32::from(cfg.channel))?;
        if !optical_ptz(item.ptz_type) { return None; }
        let lens = cam.get_zoom().await.ok()?;
        Some((lens.zoom.max_pos > lens.zoom.min_pos, lens.focus.max_pos > lens.focus.min_pos))
    }).await.ok().flatten().unwrap_or((false, false));
    let battery = cam.battery_info().await.ok();
    set(&|s| {
        s.lens_zoom = lens.0;
        s.lens_focus = lens.1;
        s.online = true;
        s.status = "Online".into();
        s.aes = cam.is_aes();
        s.battery = battery.as_ref().map(|b| b.battery_percent);
        s.charging = battery.as_ref().is_some_and(|b| b.charge_status == "charging");
    });

    let mut live: Option<Live> = None;
    let mut ping = tokio::time::interval(Duration::from_secs(5));
    let mut battery_poll = tokio::time::interval(Duration::from_secs(60));
    let mut wifi_poll = tokio::time::interval(Duration::from_secs(30));
    let mut wifi_supported = true; // cleared once a wired camera rejects the query
    let mut space_poll = tokio::time::interval(Duration::from_secs(60));
    // Motion events drive the alarm beep and motion recording; not every camera offers them.
    let mut motion = match cam.listen_on_motion().await {
        Ok(m) => Some(m),
        Err(e) => {
            log::info!("{}: no motion events: {e}", cfg.name);
            None
        }
    };
    let mut motion_until: Option<Instant> = None;
    let mut motion_start: Option<i64> = None; // the open event in the motion log
    let mut auto = false; // the local recording schedule is recording this camera
    let mut schedule_tick = tokio::time::interval(Duration::from_secs(1));
    // Resume what was on screen (and recording) before a drop.
    let mut pending = vec![Cmd::Live(wanted.live)];

    loop {
        let cmd = if let Some(c) = pending.pop().map(Some) {
            c
        } else {
            tokio::select! {
                c = rx.recv() => match c { None => return Ok(()), c => c },
                _ = ping.tick() => {
                    let r = tokio::time::timeout(PING_TIMEOUT, draining(cam.ping(), &mut live, &cfg.name, state, views)).await;
                    r.context("ping: no answer")??.context("ping")?;
                    if live.is_some() && state.lock().unwrap().last_frame.is_some_and(|t| t.elapsed() > STALL_RESTART) {
                        anyhow::bail!("no video for {} s", STALL_RESTART.as_secs());
                    }
                    None
                }
                _ = battery_poll.tick(), if battery.is_some() => {
                    if let Ok(b) = draining(cam.battery_info(), &mut live, &cfg.name, state, views).await? {
                        set(&|s| { s.battery = Some(b.battery_percent); s.charging = b.charge_status == "charging"; });
                    }
                    None
                }
                _ = space_poll.tick(), if live.as_ref().is_some_and(|l| l.recorder.is_some()) => {
                    if let Some(w) = space_warning() {
                        set(&|s| s.message = Some(w.clone()));
                    }
                    None
                }
                _ = wifi_poll.tick(), if wifi_supported => {
                    match draining(cam.wifi_signal(), &mut live, &cfg.name, state, views).await? {
                        // Not measured yet right after connecting: try again shortly instead of showing it.
                        Ok(Some(d)) if !real_rssi(d) => wifi_poll.reset_after(Duration::from_secs(5)),
                        Ok(dbm) => set(&|s| s.wifi_dbm = dbm),
                        Err(neolink_core::Error::CameraServiceUnavailable { .. }) => wifi_supported = false,
                        Err(e) => log::debug!("{}: wifi signal: {e}", cfg.name),
                    }
                    None
                }
                ev = next_motion(&mut motion) => {
                    match ev {
                        Ok(MotionStatus::Start(_)) => {
                            motion_until = None;
                            let (fresh, on_screen) = {
                                let mut s = state.lock().unwrap();
                                let fresh = !s.motion;
                                if fresh {
                                    s.motion_events += 1;
                                }
                                s.motion = true;
                                (fresh, s.live.is_some())
                            };
                            if fresh {
                                let ai = motion.as_ref().and_then(|m| m.last_ai());
                                motion_start = Some(crate::events::start(&cfg.name, ai.clone()));
                                crate::events::notify(&cfg.name, ai.as_deref(), on_screen);
                            }
                        }
                        Ok(MotionStatus::Stop(_)) => {
                            motion_until = Some(Instant::now() + Duration::from_secs(crate::prefs::MOTION_POST_SECS));
                            set(&|s| s.motion = false);
                            if let Some(start) = motion_start.take() {
                                crate::events::end(&cfg.name, start, crate::events::now_ms() + crate::prefs::MOTION_POST_SECS as i64 * 1000);
                            }
                        }
                        Ok(MotionStatus::NoChange(_)) => {}
                        Err(e) => {
                            log::warn!("{}: motion events stopped: {e}", cfg.name);
                            motion = None;
                        }
                    }
                    None
                }
                _ = schedule_tick.tick() => {
                    let moving = state.lock().unwrap().motion || motion_until.is_some_and(|t| Instant::now() < t);
                    let want = auto_record_wanted(&crate::prefs::get(), moving);
                    (want != auto).then(|| { auto = want; Cmd::Auto })
                }
                m = next_media(&mut live) => {
                    let media = m.context("stream")?.context("stream data")?;
                    on_media(live.as_mut().expect("live set"), media, &cfg.name, state, views);
                    None
                }
            }
        };
        let Some(cmd) = cmd else { continue };
        match cmd {
            Cmd::Live(kind) => {
                wanted.live = kind;
                let record = wanted.record || auto;
                // Off screen while the schedule records: keep a stream for the recording only. Recordings always
                // take the main stream, so they never drop to the sub stream when the tile is small.
                let want = kind.or(auto.then_some(StreamKind::Main)).map(|k| if record { StreamKind::Main } else { k });
                if live.as_ref().map(|l| l.kind) != want {
                    if kind.is_none() && live.is_some() {
                        log::info!("{}: live stopped", cfg.name);
                    }
                    // Wait for the camera to confirm the stop: dropping StreamData stops it in the background, and on an
                    // Argus a stop still in flight left the stream started next sending nothing (main, sub, main in 1 s).
                    if let Some(mut old) = live.take() {
                        let _ = old.stream.shutdown().await;
                    }
                    set(&|s| s.audio = false);
                    views.audio.clear();
                    if let Some(k) = want {
                        log::info!("{}: live {k}{}", cfg.name, if kind.is_none() { " (recording only)" } else { "" });
                        let stream = cam.start_video(k, 0, false).await.context("start live")?;
                        set(&|s| s.last_frame = Some(Instant::now())); // a stream that never sends counts from here
                        live = Some(Live { kind: k, video_us: 0, stream, decoder: None, recorder: None, record, jpeg_box: wanted.jpeg_box, show: true });
                    }
                }
                if let Some(l) = live.as_mut() {
                    l.show = kind.is_some();
                    if !l.show {
                        l.decoder = None;
                    }
                }
                if kind.is_none() {
                    views.stop();
                }
                set(&|s| {
                    s.live = live.as_ref().filter(|l| l.show).map(|l| l.kind);
                    if live.is_none() {
                        s.recording = None;
                    }
                });
            }
            Cmd::Record(on) => {
                if on && RECORDING_PAUSED.load(std::sync::atomic::Ordering::Relaxed) {
                    set(&|s| s.message = Some("Recording is paused (Resume in the top bar or the tray menu)".into()));
                    continue;
                }
                wanted.record = on;
                log::info!("{}: record {}", cfg.name, if on { "on" } else { "off" });
                let Some(l) = live.as_mut() else {
                    set(&|s| s.message = Some("Start live view before recording".into()));
                    wanted.record = false;
                    continue;
                };
                l.record = on || auto;
                if !l.record {
                    l.recorder = None;
                    set(&|s| s.recording = None);
                }
                pending.push(Cmd::Live(wanted.live)); // to the main stream for recording, or back when it ends
            }
            Cmd::Auto => {
                log::info!("{}: scheduled recording {}", cfg.name, if auto { "starts" } else { "ends" });
                if let Some(l) = live.as_mut() {
                    l.record = wanted.record || auto;
                    if !l.record {
                        l.recorder = None;
                        set(&|s| s.recording = None);
                    }
                }
                // Starts (or, when the schedule ends, stops) a recording-only stream, and picks the stream to use.
                pending.push(Cmd::Live(wanted.live));
            }
            Cmd::Reconnect => anyhow::bail!(RECONNECT),
            Cmd::View(b) => {
                wanted.jpeg_box = b;
                if let Some(l) = live.as_mut() {
                    l.jpeg_box = b;
                }
            }
            Cmd::Ptz(dir) => {
                let allowed = {
                    let s = state.lock().unwrap();
                    match dir {
                        Direction::ZoomIn | Direction::ZoomOut => s.lens_zoom,
                        Direction::FocusNear | Direction::FocusFar => s.lens_focus,
                        _ => true,
                    }
                };
                if !allowed { set(&|s| s.message = Some("This camera does not advertise that lens control".into())); continue; }

                if let Err(e) = draining(cam.send_ptz(dir, 32.0), &mut live, &cfg.name, state, views).await? {
                    set(&|s| s.message = Some(format!("PTZ: {e}")));
                }
            }
            Cmd::PtzPreset { action, reply } => {
                // Serialize list-and-save with other controls while continuing to consume live media.
                let result = draining(tokio::time::timeout(CONTROL_TIMEOUT, preset_command(&cam, action)),
                    &mut live, &cfg.name, state, views).await?
                    .unwrap_or_else(|_| Err("Preset request timed out; refresh before retrying a save".into()));
                if let Err(e) = &result { set(&|s| s.message = Some(format!("PTZ presets: {e}"))); }
                let _ = reply.send(result);
            }
            Cmd::Light(on) => {
                let res = draining(cam.set_floodlight_manual(on, LIGHT_SECS), &mut live, &cfg.name, state, views).await?;
                match res {
                    Ok(()) => set(&|s| s.light_until = on.then(|| Instant::now() + Duration::from_secs(LIGHT_SECS as u64))),
                    Err(e) => set(&|s| s.message = Some(format!("Light: {e}"))),
                }
            }
            Cmd::Siren => {
                let res = draining(tokio::time::timeout(CONTROL_TIMEOUT, cam.siren()), &mut live, &cfg.name, state, views).await?;
                let failed = match res {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e.to_string()),
                    Err(_) => Some("the camera didn't answer".to_string()),
                };
                if let Some(e) = failed {
                    set(&|s| s.message = Some(format!("Siren: {e}")));
                }
            }
            Cmd::Talk(reply) => {
                let (cam, channel, name, shared) = (cam.clone(), cfg.channel, cfg.name.clone(), state.clone());
                tokio::spawn(async move { let _ = reply.send(start_talk(cam, channel, name, shared).await); });
            }
            Cmd::SdSearch { day, reply } => {
                let cam=cam.clone(); let channel=cfg.channel;
                tokio::spawn(async move { let result=tokio::time::timeout(Duration::from_secs(80),crate::sd::search(&cam,channel,&day)).await.unwrap_or_else(|_|Err("SD search timed out".into())); let _=reply.send(result); });
            }
            Cmd::SdDownload { start, end, dest, progress } => {
                let cam=cam.clone();let channel=cfg.channel;
                let shared = progress.clone();
                let task = tokio::spawn(async move {
                    let r = crate::sd::download(cam,channel,&start,&end,dest,&shared).await;
                    let mut p = shared.lock().unwrap();
                    p.done = true;
                    match r { Ok(f) => p.file = Some(f), Err(e) => p.error = Some(e) }
                });
                progress.lock().unwrap().abort = Some(task.abort_handle());
            }
            Cmd::Users { action, reply } => {
                let (cam, login) = (cam.clone(), cfg.username.clone());
                tokio::spawn(async move { let _ = reply.send(users_command(&cam, &login, action).await); });
            }
            Cmd::Xml { msg, set, reply } => {
                let cam = cam.clone();
                tokio::spawn(async move {
                    let res = match set {
                        None => cam.get_xml(msg.get, msg.channel).await,
                        Some(xml) => match msg.set {
                            Some(id) => cam.set_xml(id, msg.channel, xml).await.map(|_| String::new()),
                            None => Err(neolink_core::Error::Other("setting is read-only")),
                        },
                    };
                    let _ = reply.send(res.map_err(|e| e.to_string()));
                });
            }
            Cmd::SnapshotJpeg(reply) => {
                let cam = cam.clone();
                tokio::spawn(async move {
                    let _ = reply.send(cam.get_snapshot().await.map_err(|e| e.to_string()));
                });
            }
            Cmd::Reboot(reply) => {
                let res = draining(cam.reboot(), &mut live, &cfg.name, state, views).await?.map_err(|e| e.to_string());
                let _ = reply.send(res);
            }
            Cmd::Snapshot => {
                let res = draining(cam.get_snapshot(), &mut live, &cfg.name, state, views).await?.map_err(anyhow::Error::from).and_then(|jpg| {
                    let dir = crate::prefs::get().snapshot_dir;
                    std::fs::create_dir_all(&dir)?;
                    let path = dir.join(format!("{}_{}.jpg", safe_name(&cfg.name), stamp()));
                    std::fs::write(&path, jpg)?;
                    anyhow::Ok(path)
                });
                let msg = match res {
                    Ok(p) => format!("Snapshot saved: {}", p.display()),
                    Err(e) => format!("Snapshot failed: {e:#}"),
                };
                set(&|s| s.message = Some(msg.clone()));
            }
        }
    }
}

fn on_media(live: &mut Live, media: BcMedia, name: &str, state: &Arc<Mutex<State>>, views: &Views) {
    let (codec, data, key, us) = match media {
        BcMedia::Iframe(f) => (f.video_type, f.data, true, f.microseconds),
        BcMedia::Pframe(f) => (f.video_type, f.data, false, f.microseconds),
        BcMedia::Aac(_) | BcMedia::Adpcm(_) => {
            state.lock().unwrap().audio = true;
            if live.show { views.audio.push(media, live.video_us); }
            return;
        }
        _ => return,
    };
    live.video_us = us;
    state.lock().unwrap().last_frame = Some(Instant::now());

    if live.record && live.recorder.is_none() {
        let prefs = crate::prefs::get();
        let dir = prefs.record_dir.clone();
        let res = std::fs::create_dir_all(&dir).and_then(|_| Recorder::new(codec, &dir, &safe_name(name), prefs.segment_minutes));
        let mut s = state.lock().unwrap();
        match res {
            Ok(r) => {
                s.recording = Some(r.path.clone());
                live.recorder = Some(r);
                if let Some(w) = space_warning() {
                    s.message = Some(w);
                }
            }
            Err(e) => {
                live.record = false;
                s.message = Some(format!("Recording failed (is ffmpeg installed?): {e}"));
            }
        }
    }
    if let Some(r) = live.recorder.as_mut() {
        match r.push(&data, key, us) {
            Pushed::Queued => {}
            Pushed::Overloaded => {
                log::warn!("{name}: recording can't keep up, dropping frames");
                state.lock().unwrap().message =
                    Some("Recording is falling behind (disk too slow?): some video is being skipped".into());
            }
            Pushed::Dead => {
                live.recorder = None;
                live.record = false;
                let mut s = state.lock().unwrap();
                s.recording = None;
                s.message = Some("Recording stopped: ffmpeg quit writing (disk full or removed?)".into());
            }
        }
    }
    if !live.show {
        return;
    }
    // WebCodecs tiles decode the camera's own frames; ffmpeg runs only while an MJPEG <img> is watching.
    let hevc = matches!(codec, VideoType::H265);
    if views.jpeg.receiver_count() == 0 {
        live.decoder = None;
        views.raw.push(Raw { hevc, key, us, data });
        return;
    }
    views.raw.push(Raw { hevc, key, us, data: data.clone() });

    // Swap decoders only at a keyframe so the new one starts clean; the old one keeps the tile live until then.
    let stale = live.decoder.as_ref().is_some_and(|d| {
        std::mem::discriminant(&d.codec) != std::mem::discriminant(&codec) || (key && d.jpeg_box != live.jpeg_box)
    });
    if stale {
        live.decoder = None;
    }
    if live.decoder.is_none() {
        if !key {
            return;
        }
        let tx = views.jpeg.clone();
        let on_frame = move |jpeg: Vec<u8>| {
            tx.send_replace(Some(Arc::new(jpeg)));
        };
        match Decoder::new(codec, live.jpeg_box, on_frame) {
            Ok(d) => live.decoder = Some(d),
            Err(e) => {
                state.lock().unwrap().message = Some(format!("Decoder failed (is ffmpeg installed?): {e}"));
                return;
            }
        }
    }
    live.decoder.as_mut().unwrap().push(data, key);
}

#[cfg(test)]
mod tests {
    use super::wifi_level;

    #[test]
    fn wifi_levels_match_official_client() {
        assert_eq!([-80, -71, -70, -58, -51, -50, -41, -40, -30].map(wifi_level), [0, 0, 1, 1, 1, 2, 2, 3, 3]);
        assert!(!super::real_rssi(-10) && super::real_rssi(-20) && super::real_rssi(-59));
    }
}
