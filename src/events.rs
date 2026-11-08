//! Motion event log: one JSON line per motion start and per motion end, appended next to devices.json.
//! Also the desktop notification a motion start raises.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Events older than this are dropped at startup.
pub const KEEP_DAYS: i64 = 30;
/// Minimum gap between two notifications for the same camera.
pub const NOTIFY_GAP: Duration = Duration::from_secs(60);

/// A motion event. `end` is None while the motion (plus post roll) is still running, or when the session dropped
/// before the camera reported the end.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Event {
    /// Camera name (the runtime device id doesn't survive a restart).
    pub name: String,
    /// Unix ms.
    pub start: i64,
    pub end: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<String>,
}

/// One line of the file: a start (`end: None`) or the end of the start with the same name and start time.
#[derive(Serialize, Deserialize)]
struct Line {
    name: String,
    start: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    end: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ai: Option<String>,
}

pub fn path() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("Reolink-rs").join("events.jsonl")
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

static FILE: Mutex<()> = Mutex::new(());

fn append(line: &Line) {
    let _g = FILE.lock().unwrap();
    let p = path();
    let r = std::fs::create_dir_all(p.parent().unwrap_or(Path::new("."))).and_then(|_| {
        let mut f = std::fs::OpenOptions::new().append(true).create(true).open(&p)?;
        writeln!(f, "{}", serde_json::to_string(line).unwrap())
    });
    if let Err(e) = r {
        log::warn!("event log: {e}");
    }
}

/// Record a motion start; returns its start time for `end`.
pub fn start(name: &str, ai: Option<String>) -> i64 {
    let start = now_ms();
    append(&Line { name: name.into(), start, end: None, ai });
    start
}

pub fn end(name: &str, start: i64, end: i64) {
    append(&Line { name: name.into(), start, end: Some(end), ai: None });
}

/// Merge start and end lines into events, in file order. Unknown or broken lines are skipped.
fn merge<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<Event> {
    let mut events: Vec<Event> = vec![];
    let mut index: HashMap<(String, i64), usize> = HashMap::new();
    for l in lines.filter_map(|l| serde_json::from_str::<Line>(l).ok()) {
        let key = (l.name.clone(), l.start);
        match (l.end, index.get(&key)) {
            (Some(end), Some(&i)) => events[i].end = Some(end),
            (Some(_), None) => {} // end whose start was pruned or lost
            (None, _) => {
                index.insert(key, events.len());
                events.push(Event { name: l.name, start: l.start, end: None, ai: l.ai });
            }
        }
    }
    events
}

fn read(p: &Path) -> String {
    let _g = FILE.lock().unwrap();
    std::fs::read_to_string(p).unwrap_or_default()
}

/// Events overlapping [from, to) in unix ms, newest first.
pub fn range(from: i64, to: i64) -> Vec<Event> {
    let text = read(&path());
    let mut v: Vec<Event> = merge(text.lines()).into_iter().filter(|e| e.start < to && e.end.unwrap_or(e.start) >= from).collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.start));
    v
}

/// Keep only the lines of events that started at or after `cutoff` (unix ms).
fn pruned(text: &str, cutoff: i64) -> String {
    text.lines().filter(|l| serde_json::from_str::<Line>(l).is_ok_and(|l| l.start >= cutoff)).map(|l| format!("{l}\n")).collect()
}

pub fn prune() {
    let p = path();
    let text = read(&p);
    let kept = pruned(&text, now_ms() - KEEP_DAYS * 86_400_000);
    if kept.len() != text.len() {
        let _g = FILE.lock().unwrap();
        if let Err(e) = std::fs::write(&p, kept) {
            log::warn!("event log prune: {e}");
        }
    }
}

// ---------- notifications ----------

pub static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
/// Shows and focuses the main window (main.rs sets it); a clicked notification calls it.
pub static SHOW_WINDOW: OnceLock<fn(&tauri::AppHandle)> = OnceLock::new();
static LAST_NOTIFIED: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

/// Whether a camera last notified at `last` may notify again at `now`.
pub fn notify_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= NOTIFY_GAP)
}

/// "Front Door: motion (people)" as a desktop notification, unless off, throttled, or the camera is being watched
/// live in the focused window.
pub fn notify(name: &str, ai: Option<&str>, live: bool) {
    use tauri::Manager as _;
    if !crate::prefs::get().notify_motion {
        return;
    }
    let Some(app) = APP.get() else { return };
    let focused = app.get_webview_window("main").is_some_and(|w| w.is_focused().unwrap_or(false) && w.is_visible().unwrap_or(false));
    if live && focused {
        return;
    }
    let now = Instant::now();
    {
        let mut g = LAST_NOTIFIED.lock().unwrap();
        let last = g.get_or_insert_with(HashMap::new);
        if !notify_due(last.get(name).copied(), now) {
            return;
        }
        last.insert(name.into(), now);
    }
    let body = match ai {
        Some(t) => format!("motion ({t})"),
        None => "motion".to_string(),
    };
    // Windows: the plugin has no click handler, so the toast goes through the WinRT crate it wraps; a click shows
    // the window and the page opens that camera (the "open-camera" event).
    #[cfg(windows)]
    {
        use tauri_winrt_notification::Toast;
        // As the plugin: the AppUserModelID only exists for the installed app (its Start-menu shortcut).
        let exe = std::env::current_exe().unwrap_or_default();
        let installed = !exe.parent().is_some_and(|d| d.ends_with("target/debug") || d.ends_with("target/release"));
        let id = if installed { app.config().identifier.clone() } else { Toast::POWERSHELL_APP_ID.to_string() };
        let (handle, camera) = (app.clone(), name.to_string());
        let toast = Toast::new(&id).title(name).text1(&body).on_activated(move |_| {
            use tauri::Emitter as _;
            if let Some(show) = SHOW_WINDOW.get() {
                show(&handle);
            }
            let _ = handle.emit("open-camera", &camera);
            Ok(())
        });
        if let Err(e) = toast.show() {
            log::warn!("{name}: notification: {e}");
        }
    }
    #[cfg(not(windows))]
    {
        use tauri_plugin_notification::NotificationExt as _;
        if let Err(e) = app.notification().builder().title(name).body(body).show() {
            log::warn!("{name}: notification: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = concat!(
        r#"{"name":"Front","start":1000,"ai":"people"}"#, "\n",
        r#"{"name":"Back","start":1500}"#, "\n",
        r#"{"name":"Front","start":1000,"end":4000}"#, "\n",
        r#"{"name":"Gone","start":200,"end":900}"#, "\n",
        "not json\n",
        r#"{"name":"Front","start":9000}"#, "\n",
    );

    #[test]
    fn end_lines_close_their_start_and_strays_are_skipped() {
        let ev = merge(LOG.lines());
        assert_eq!(ev.len(), 3);
        assert_eq!(ev[0], Event { name: "Front".into(), start: 1000, end: Some(4000), ai: Some("people".into()) });
        assert_eq!(ev[1].end, None);
        assert_eq!(ev[2].start, 9000);
    }

    #[test]
    fn pruning_keeps_whole_lines_from_the_cutoff_on() {
        let kept = pruned(LOG, 1000);
        assert_eq!(kept.lines().count(), 4);
        assert!(!kept.contains("Gone") && !kept.contains("not json"));
        assert_eq!(pruned(&kept, 1000), kept);
    }

    #[test]
    fn notifications_are_throttled_per_camera() {
        let t0 = Instant::now();
        assert!(notify_due(None, t0));
        assert!(!notify_due(Some(t0), t0 + Duration::from_secs(59)));
        assert!(notify_due(Some(t0), t0 + NOTIFY_GAP));
    }
}
