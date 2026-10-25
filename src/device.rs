//! One worker task per device: connect, login, stream, PTZ, snapshot, record.
//!
//! Unlike the official client there is no idle/battery disconnect: the session is held open
//! with pings for as long as the app runs, and any drop (battery camera or not) reconnects
//! automatically and resumes the live view that was showing.

use crate::media::{Decoder, Recorder};
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

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DeviceConfig {
    pub name: String,
    /// IP or hostname; optional when a UID is given.
    pub host: String,
    pub uid: String,
    pub port: Option<u16>,
    pub username: String,
    // shortcut: stored in plain text in %APPDATA%; move to Windows Credential Manager before sharing builds.
    pub password: String,
    /// NVR channel, 0 for a standalone camera.
    pub channel: u8,
}

/// Reply channel for request/response commands.
pub type Reply<T> = tokio::sync::oneshot::Sender<Result<T, String>>;

pub enum Cmd {
    /// Read (`set: None`) or write a settings message; replies with the camera's XML (empty for writes).
    Xml { msg: crate::settings::Msg, set: Option<String>, reply: Reply<String> },
    Reboot(Reply<()>),
    /// A JPEG still for the settings editors (motion area, privacy masks).
    SnapshotJpeg(Reply<Vec<u8>>),
    Live(Option<StreamKind>),
    Ptz(Direction),
    Snapshot,
    Record(bool),
    /// Largest JPEG (device pixels) the tile can show; see media::jpeg_box.
    View((u32, u32)),
    /// Internal: the local recording schedule's wish changed (see prefs::Prefs::local_record).
    Auto,
}

#[derive(Default)]
pub struct State {
    pub status: String,
    pub online: bool,
    pub battery: Option<u32>,
    pub charging: bool,
    /// Wi-Fi RSSI in dBm; None for wired cameras or before the first reading.
    pub wifi_dbm: Option<i32>,
    pub live: Option<StreamKind>,
    pub recording: Option<PathBuf>,
    pub message: Option<String>,
    /// The camera currently reports motion.
    pub motion: bool,
    /// Motion starts seen since the app started; the UI beeps when it grows.
    pub motion_events: u64,
}

pub struct Device {
    /// Runtime-only id; stable while the app runs, unlike the list index.
    pub id: u64,
    pub config: DeviceConfig,
    pub frames: watch::Receiver<Frame>,
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
        let (frame_tx, frames) = watch::channel(None);
        let task = rt.spawn(run(config.clone(), rx, state.clone(), frame_tx));
        Self { id, config, frames, read_only: Default::default(), state, tx, task }
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

async fn run(cfg: DeviceConfig, mut rx: UnboundedReceiver<Cmd>, state: Arc<Mutex<State>>, frames: watch::Sender<Frame>) {
    let mut wanted = Wanted { live: None, record: false, jpeg_box: crate::media::DEFAULT_BOX };
    let mut backoff = 1;
    loop {
        let err = match session(&cfg, &mut rx, &state, &frames, &mut wanted).await {
            Ok(()) => return, // command channel closed: device removed
            Err(e) => e,
        };
        {
            let mut s = state.lock().unwrap();
            s.online = false;
            s.recording = None;
            s.status = format!("Reconnecting in {backoff}s: {err:#}");
        }
        frames.send_replace(None);
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
                    Some(Cmd::Xml { reply, .. }) => { let _ = reply.send(Err("camera is offline".into())); }
                    Some(Cmd::Reboot(reply)) => { let _ = reply.send(Err("camera is offline".into())); }
                    Some(Cmd::SnapshotJpeg(reply)) => { let _ = reply.send(Err("camera is offline".into())); }
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
    frames: &watch::Sender<Frame>,
) -> Result<T> {
    tokio::pin!(fut);
    loop {
        tokio::select! {
            r = &mut fut => return Ok(r),
            m = next_media(live) => {
                let media = m.context("stream")?.context("stream data")?;
                on_media(live.as_mut().expect("live set"), media, name, state, frames);
            }
        }
    }
}

/// A camera that doesn't answer a ping in this long is gone; reconnect instead of waiting forever.
const PING_TIMEOUT: Duration = Duration::from_secs(20);

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
    frames: &watch::Sender<Frame>,
    wanted: &mut Wanted,
) -> Result<()> {
    let set = |f: &dyn Fn(&mut State)| f(&mut state.lock().unwrap());
    set(&|s| s.status = "Connecting".into());
    // Shared so settings requests can run as their own tasks without pausing the live stream.
    let cam = std::sync::Arc::new(BcCamera::new(&camera_opts(cfg)?).await.context("connect")?);
    set(&|s| s.status = "Logging in".into());
    cam.login().await.context("login")?;

    let battery = cam.battery_info().await.ok();
    set(&|s| {
        s.online = true;
        s.status = "Online".into();
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
                    let r = tokio::time::timeout(PING_TIMEOUT, draining(cam.ping(), &mut live, &cfg.name, state, frames)).await;
                    r.context("ping: no answer")??.context("ping")?;
                    None
                }
                _ = battery_poll.tick(), if battery.is_some() => {
                    if let Ok(b) = draining(cam.battery_info(), &mut live, &cfg.name, state, frames).await? {
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
                    match draining(cam.wifi_signal(), &mut live, &cfg.name, state, frames).await? {
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
                            set(&|s| {
                                if !s.motion {
                                    s.motion_events += 1;
                                }
                                s.motion = true;
                            });
                        }
                        Ok(MotionStatus::Stop(_)) => {
                            motion_until = Some(Instant::now() + Duration::from_secs(crate::prefs::MOTION_POST_SECS));
                            set(&|s| s.motion = false);
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
                    on_media(live.as_mut().expect("live set"), media, &cfg.name, state, frames);
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
                    live = None; // dropping StreamData sends the stop to the camera
                    if let Some(k) = want {
                        log::info!("{}: live {k}{}", cfg.name, if kind.is_none() { " (recording only)" } else { "" });
                        let stream = cam.start_video(k, 0, false).await.context("start live")?;
                        live = Some(Live { kind: k, stream, decoder: None, recorder: None, record, jpeg_box: wanted.jpeg_box, show: true });
                    }
                }
                if let Some(l) = live.as_mut() {
                    l.show = kind.is_some();
                    if !l.show {
                        l.decoder = None;
                    }
                }
                if kind.is_none() {
                    frames.send_replace(None);
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
                    set(&|s| s.message = Some("Recording is paused (tray menu)".into()));
                    continue;
                }
                wanted.record = on;
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
            Cmd::View(b) => {
                wanted.jpeg_box = b;
                if let Some(l) = live.as_mut() {
                    l.jpeg_box = b;
                }
            }
            Cmd::Ptz(dir) => {
                if let Err(e) = draining(cam.send_ptz(dir, 32.0), &mut live, &cfg.name, state, frames).await? {
                    set(&|s| s.message = Some(format!("PTZ: {e}")));
                }
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
                let res = draining(cam.reboot(), &mut live, &cfg.name, state, frames).await?.map_err(|e| e.to_string());
                let _ = reply.send(res);
            }
            Cmd::Snapshot => {
                let res = draining(cam.get_snapshot(), &mut live, &cfg.name, state, frames).await?.map_err(anyhow::Error::from).and_then(|jpg| {
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

fn on_media(live: &mut Live, media: BcMedia, name: &str, state: &Arc<Mutex<State>>, frames: &watch::Sender<Frame>) {
    let (codec, data, key) = match media {
        BcMedia::Iframe(f) => (f.video_type, f.data, true),
        BcMedia::Pframe(f) => (f.video_type, f.data, false),
        _ => return, // shortcut: live audio playback not implemented yet (phase 2)
    };

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
    if let Some(r) = live.recorder.as_mut()
        && !r.push(&data, key)
    {
        live.recorder = None;
        live.record = false;
        let mut s = state.lock().unwrap();
        s.recording = None;
        s.message = Some("Recording stopped: ffmpeg quit writing (disk full or removed?)".into());
    }
    if !live.show {
        return;
    }

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
        let tx = frames.clone();
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
