//! Camera settings: a catalog of pages/fields mapped onto Baichuan messages, and an engine that reads them and
//! writes changes read-modify-write, then reads back to verify the camera applied them.
//!
//! Message ids and value domains come from the official client and captured traffic; see
//! docs/settings-inventory.md. Writes use only ids confirmed by captured official-client traffic.
//! Fields a camera doesn't answer (message rejected or element missing) are left out, so each camera
//! shows exactly what it supports.

use crate::xmledit;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Msg {
    pub get: u32,
    pub set: Option<u32>,
    /// Needs the `<Extension><channelId>` block.
    pub channel: bool,
}

const VERSION: Msg = Msg { get: 80, set: None, channel: false };
const UID: Msg = Msg { get: 114, set: None, channel: false };
const NET: Msg = Msg { get: 76, set: None, channel: false };
const BATTERY: Msg = Msg { get: 253, set: None, channel: true };
const STORAGE: Msg = Msg { get: 102, set: None, channel: false };
const SIGNAL: Msg = Msg { get: 115, set: None, channel: true };
const PERF: Msg = Msg { get: 122, set: None, channel: false };
const IMAGE: Msg = Msg { get: 26, set: Some(25), channel: true };
const OSD: Msg = Msg { get: 44, set: Some(45), channel: true };
const ENCODE: Msg = Msg { get: 56, set: Some(57), channel: true };
const STREAMS: Msg = Msg { get: 146, set: None, channel: true };
const PIR: Msg = Msg { get: 212, set: Some(213), channel: true };
const LEDS: Msg = Msg { get: 208, set: Some(209), channel: true };
const RECORD: Msg = Msg { get: 54, set: Some(55), channel: true };
const EMAIL_TASK: Msg = Msg { get: 217, set: Some(216), channel: true };
const PUSH_TASK: Msg = Msg { get: 219, set: Some(218), channel: true };
const GENERAL: Msg = Msg { get: 104, set: Some(105), channel: false };
const NTP: Msg = Msg { get: 38, set: Some(39), channel: false };
const DST: Msg = Msg { get: 106, set: Some(107), channel: false };
const AUTO_REBOOT: Msg = Msg { get: 101, set: Some(100), channel: false };

#[derive(Clone, Copy, Debug)]
pub enum Fmt {
    Plain,
    Suffix(&'static str),
    Bytes,
    /// Camera seconds west of UTC shown as UTC±h:mm.
    TimeZone,
    /// Wi-Fi RSSI; the camera reports a -10 dBm placeholder for a few seconds after connecting.
    Dbm,
}

#[derive(Clone, Copy, Debug)]
pub enum Opts {
    Fixed(&'static [(&'static str, &'static str)]),
    /// Every value at a path of a message, e.g. RecordCfg/timeList/time.
    List(Msg, &'static str),
    /// One of a stream's tables in StreamInfoList (stream type, table element).
    StreamTable(&'static str, &'static str),
    /// Time zones as camera seconds west of UTC.
    TimeZones,
    /// SD-card retention values listed by the camera (1 overwrite, 0 no overwrite, -N keep N days).
    Retention(Msg, &'static str),
}

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Info(Fmt),
    Range { min: i64, max: i64 },
    Toggle,
    Choice(Opts),
    Text { max: usize },
}

#[derive(Clone, Copy, Debug)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub msg: Msg,
    pub path: &'static str,
    pub kind: Kind,
}

pub struct Page {
    pub id: &'static str,
    pub title: &'static str,
    pub fields: &'static [Field],
}

const fn f(key: &'static str, label: &'static str, msg: Msg, path: &'static str, kind: Kind) -> Field {
    Field { key, label, msg, path, kind }
}

const INFO: Kind = Kind::Info(Fmt::Plain);
const PCT: Kind = Kind::Info(Fmt::Suffix("%"));
const IMG: Kind = Kind::Range { min: 0, max: 255 };
const WEEKDAYS: &[(&str, &str)] = &[
    ("Sunday", "Sunday"), ("Monday", "Monday"), ("Tuesday", "Tuesday"), ("Wednesday", "Wednesday"),
    ("Thursday", "Thursday"), ("Friday", "Friday"), ("Saturday", "Saturday"),
];

pub static PAGES: &[Page] = &[
    Page {
        id: "info",
        title: "Device info",
        fields: &[
            f("model", "Model", VERSION, "VersionInfo/type", INFO),
            f("firmware", "Firmware", VERSION, "VersionInfo/firmwareVersion", INFO),
            f("hardware", "Hardware", VERSION, "VersionInfo/hardwareVersion", INFO),
            f("build", "Build", VERSION, "VersionInfo/buildDay", INFO),
            f("serial", "Serial number", VERSION, "VersionInfo/serialNumber", INFO),
            f("uid", "UID", UID, "Uid/uid", INFO),
            f("ip", "IP address", NET, "Ip/ip", INFO),
            f("mac", "MAC address", NET, "Ip/mac", INFO),
            f("wifi", "Wi-Fi signal", SIGNAL, "WifiSignal/signal", Kind::Info(Fmt::Dbm)),
            f("battery", "Battery", BATTERY, "BatteryInfo/batteryPercent", PCT),
            f("charging", "Charging", BATTERY, "BatteryInfo/chargeStatus", INFO),
            f("power", "Power source", BATTERY, "BatteryInfo/adapterStatus", INFO),
            f("temp", "Battery temperature", BATTERY, "BatteryInfo/temperature", Kind::Info(Fmt::Suffix(" °C"))),
            f("sd_total", "SD card size", STORAGE, "HddInfoList/HddInfo/capacityV2", Kind::Info(Fmt::Bytes)),
            f("sd_free", "SD card free", STORAGE, "HddInfoList/HddInfo/remainSizeV2", Kind::Info(Fmt::Bytes)),
            f("cpu", "Camera CPU", PERF, "PerformanceInfo/cpuUseRate", PCT),
        ],
    },
    Page {
        id: "display",
        title: "Display",
        fields: &[
            f("osd_name", "Camera name on video", OSD, "OsdChannelName/name", Kind::Text { max: 31 }),
            f("osd_name_on", "Show camera name", OSD, "OsdChannelName/enable", Kind::Toggle),
            f("osd_time_on", "Show date and time", OSD, "OsdDatetime/enable", Kind::Toggle),
            f("watermark", "Watermark", OSD, "OsdChannelName/enWatermark", Kind::Toggle),
            f("bright", "Brightness", IMAGE, "VideoInput/bright", IMG),
            f("contrast", "Contrast", IMAGE, "VideoInput/contrast", IMG),
            f("saturation", "Saturation", IMAGE, "VideoInput/saturation", IMG),
            f("hue", "Hue", IMAGE, "VideoInput/hue", IMG),
            f("sharpen", "Sharpness", IMAGE, "VideoInput/sharpen", IMG),
            f("daynight", "Day / night", IMAGE, "InputAdvanceCfg/DayNight/mode",
                Kind::Choice(Opts::Fixed(&[("auto", "Auto"), ("color", "Always color"), ("blackAndWhite", "Always black & white")]))),
            f("antiflicker", "Anti-flicker", IMAGE, "InputAdvanceCfg/PowerLineFrequency/mode",
                Kind::Choice(Opts::Fixed(&[("50hz", "50 Hz"), ("60hz", "60 Hz")]))),
        ],
    },
    Page {
        id: "stream",
        title: "Stream",
        fields: &[
            f("main_res", "Clear resolution", ENCODE, "Compression/mainStream/resolutionName", INFO),
            f("main_fps", "Clear frame rate", ENCODE, "Compression/mainStream/frame",
                Kind::Choice(Opts::StreamTable("mainStream", "framerateTable"))),
            f("main_kbps", "Clear bitrate (kbps)", ENCODE, "Compression/mainStream/bitRate",
                Kind::Choice(Opts::StreamTable("mainStream", "bitrateTable"))),
            f("sub_res", "Fluent resolution", ENCODE, "Compression/subStream/resolutionName", INFO),
            f("sub_fps", "Fluent frame rate", ENCODE, "Compression/subStream/frame",
                Kind::Choice(Opts::StreamTable("subStream", "framerateTable"))),
            f("sub_kbps", "Fluent bitrate (kbps)", ENCODE, "Compression/subStream/bitRate",
                Kind::Choice(Opts::StreamTable("subStream", "bitrateTable"))),
            // Audio costs ~60 kbit/s of camera Wi-Fi per stream even when nobody listens.
            f("main_audio", "Audio in Clear stream", ENCODE, "Compression/mainStream/audio", Kind::Toggle),
            f("sub_audio", "Audio in Fluent stream", ENCODE, "Compression/subStream/audio", Kind::Toggle),
        ],
    },
    Page {
        id: "detection",
        title: "Detection & alerts",
        fields: &[
            f("pir_on", "Motion sensor (PIR)", PIR, "rfAlarmCfg/enable", Kind::Toggle),
            f("pir_sens", "PIR sensitivity", PIR, "rfAlarmCfg/sensiValue", Kind::Range { min: 1, max: 100 }),
            f("pir_reduce", "Reduce false alarms", PIR, "rfAlarmCfg/reduceFalseAlarm", Kind::Toggle),
            f("push_on", "Push notifications", PUSH_TASK, "PushTask/enable", Kind::Toggle),
            f("email_on", "Email alerts", EMAIL_TASK, "EmailTask/enable", Kind::Toggle),
            f("rec_retention", "SD card recordings", RECORD, "RecordCfg/cycle",
                Kind::Choice(Opts::Retention(RECORD, "RecordCfg/cyclelist/item"))),
            f("rec_post", "Keep recording after motion (s)", RECORD, "RecordCfg/recordDelayTime",
                Kind::Choice(Opts::List(RECORD, "RecordCfg/timeList/time"))),
        ],
    },
    Page {
        id: "lights",
        title: "Lights",
        fields: &[
            f("ir", "Infrared lights", LEDS, "LedState/state",
                Kind::Choice(Opts::Fixed(&[("auto", "Auto"), ("open", "On"), ("close", "Off")]))),
            f("ir_bright", "Infrared brightness", LEDS, "LedState/IRLedBrightness", Kind::Range { min: 0, max: 100 }),
            f("status_led", "Status LED", LEDS, "LedState/lightState",
                Kind::Choice(Opts::Fixed(&[("open", "On"), ("close", "Off")]))),
        ],
    },
    Page {
        id: "system",
        title: "System",
        fields: &[
            f("name", "Device name", GENERAL, "SystemGeneral/deviceName", Kind::Text { max: 31 }),
            f("tz", "Time zone", GENERAL, "SystemGeneral/timeZone", Kind::Choice(Opts::TimeZones)),
            f("date_fmt", "Date format", GENERAL, "SystemGeneral/osdFormat",
                Kind::Choice(Opts::Fixed(&[("MDY", "MM/DD/YYYY"), ("DMY", "DD/MM/YYYY"), ("YMD", "YYYY/MM/DD")]))),
            f("time_fmt", "Time format", GENERAL, "SystemGeneral/timeFormat",
                Kind::Choice(Opts::Fixed(&[("1", "12-hour"), ("0", "24-hour")]))),
            f("ntp_on", "Sync time from internet (NTP)", NTP, "Ntp/enable", Kind::Toggle),
            f("ntp_server", "NTP server", NTP, "Ntp/server", Kind::Text { max: 63 }),
            f("ntp_interval", "NTP sync interval", NTP, "Ntp/synchronizeInterval",
                Kind::Choice(Opts::Fixed(&[("60", "Every hour"), ("360", "Every 6 hours"), ("720", "Every 12 hours"), ("1440", "Daily"), ("10080", "Weekly")]))),
            f("dst_on", "Daylight saving time", DST, "Dst/enable", Kind::Toggle),
            f("login_lock", "Lock out after repeated failed logins", GENERAL, "SystemGeneral/loginLock", Kind::Toggle),
        ],
    },
    Page {
        id: "maintenance",
        title: "Maintenance",
        fields: &[
            f("reboot_on", "Automatic reboot", AUTO_REBOOT, "AutoReboot/enable", Kind::Toggle),
            f("reboot_day", "Reboot day", AUTO_REBOOT, "AutoReboot/weekDay", Kind::Choice(Opts::Fixed(WEEKDAYS))),
            f("reboot_hour", "Reboot hour (0–23)", AUTO_REBOOT, "AutoReboot/hour", Kind::Range { min: 0, max: 23 }),
        ],
    },
];

pub fn page(id: &str) -> Option<&'static Page> {
    PAGES.iter().find(|p| p.id == id)
}

/// How the engine talks to a camera (the device worker in the app, a fake in tests).
/// Futures are `Send` so Tauri can run settings commands on its async runtime.
pub trait Camera: Sync {
    fn get(&self, msg: Msg) -> impl std::future::Future<Output = Result<String, String>> + Send;
    fn set(&self, msg: Msg, xml: String) -> impl std::future::Future<Output = Result<(), String>> + Send;
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

#[derive(Serialize, Debug, Clone)]
pub struct FieldView {
    pub key: &'static str,
    pub label: &'static str,
    /// "info" | "range" | "toggle" | "choice" | "text"
    pub kind: &'static str,
    pub value: String,
    pub display: String,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub max_len: Option<usize>,
    pub options: Vec<Choice>,
}

#[derive(Serialize, Debug, Clone)]
pub struct PageView {
    pub id: &'static str,
    pub title: &'static str,
    pub fields: Vec<FieldView>,
}

fn format_value(fmt: Fmt, v: &str) -> String {
    match fmt {
        Fmt::Plain => v.to_string(),
        Fmt::Suffix(s) => format!("{v}{s}"),
        Fmt::Bytes => v.parse::<f64>().map(|b| format!("{:.1} GB", b / 1e9)).unwrap_or_else(|_| v.into()),
        Fmt::TimeZone => v.parse::<i64>().map(tz_label).unwrap_or_else(|_| v.into()),
        Fmt::Dbm => match v.parse::<i32>() {
            Ok(d) if !crate::device::real_rssi(d) => "measuring…".into(),
            _ => format!("{v} dBm"),
        },
    }
}

/// Official labels for RecordCfg/cycle values.
pub fn retention_label(v: &str) -> String {
    match v.parse::<i64>() {
        Ok(0) => "No overwrite".into(),
        Ok(1) => "Overwrite oldest files".into(),
        Ok(n) if n < 0 => format!("Delete after {} day{}", -n, if n == -1 { "" } else { "s" }),
        _ => v.into(),
    }
}

/// Camera time zone value (seconds west of UTC) as "UTC−7:00".
pub fn tz_label(west_secs: i64) -> String {
    let east = -west_secs;
    let sign = if east < 0 { '−' } else { '+' };
    format!("UTC{sign}{}:{:02}", east.abs() / 3600, east.abs() % 3600 / 60)
}

fn time_zones() -> Vec<Choice> {
    // Whole and the common half/quarter-hour offsets, UTC−12 .. UTC+14.
    let mut east_mins: Vec<i64> = (-12..=14).map(|h| h * 60).collect();
    east_mins.extend([-570, -210, 210, 270, 330, 345, 390, 525, 570, 630, 765, 825]);
    east_mins.sort();
    east_mins.dedup();
    east_mins
        .into_iter()
        .map(|m| {
            let west = -m * 60;
            Choice { value: west.to_string(), label: tz_label(west) }
        })
        .collect()
}

fn stream_table(streams_xml: &str, stream: &str, table: &str) -> Vec<String> {
    // encodeTable blocks are siblings; pair each block's <type> with its table by order.
    let types = xmledit::get_all(streams_xml, "StreamInfoList/StreamInfo/encodeTable/type");
    let tables = xmledit::get_all(streams_xml, &format!("StreamInfoList/StreamInfo/encodeTable/{table}"));
    types
        .iter()
        .zip(tables)
        .find(|(t, _)| t.as_str() == stream)
        .map(|(_, list)| list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

async fn options<C: Camera>(cam: &C, opts: Opts, cache: &mut HashMap<Msg, Option<String>>) -> Vec<Choice> {
    let plain = |v: Vec<String>| v.into_iter().map(|v| Choice { label: v.clone(), value: v }).collect();
    match opts {
        Opts::Fixed(list) => list.iter().map(|(v, l)| Choice { value: (*v).into(), label: (*l).into() }).collect(),
        Opts::TimeZones => time_zones(),
        Opts::List(msg, path) => match fetch(cam, msg, cache).await {
            Some(xml) => plain(xmledit::get_all(&xml, path)),
            None => vec![],
        },
        Opts::Retention(msg, path) => match fetch(cam, msg, cache).await {
            Some(xml) => xmledit::get_all(&xml, path)
                .into_iter()
                .map(|v| Choice { label: retention_label(&v), value: v })
                .collect(),
            None => vec![],
        },
        Opts::StreamTable(stream, table) => match fetch(cam, STREAMS, cache).await {
            Some(xml) => plain(stream_table(&xml, stream, table)),
            None => vec![],
        },
    }
}

async fn fetch<C: Camera>(cam: &C, msg: Msg, cache: &mut HashMap<Msg, Option<String>>) -> Option<String> {
    if let Some(x) = cache.get(&msg) {
        return x.clone();
    }
    let x = cam.get(msg).await.ok();
    cache.insert(msg, x.clone());
    x
}

/// Read a page. Fields whose message or element the camera doesn't provide are omitted.
pub async fn read_page<C: Camera>(cam: &C, page: &Page) -> PageView {
    let mut cache = HashMap::new();
    let mut fields = vec![];
    for field in page.fields {
        let Some(xml) = fetch(cam, field.msg, &mut cache).await else { continue };
        let Some(value) = xmledit::get(&xml, field.path) else { continue };
        let writable = field.msg.set.is_some();
        let (kind, min, max, max_len, opts, display) = match field.kind {
            Kind::Info(fmt) => ("info", None, None, None, vec![], format_value(fmt, &value)),
            _ if !writable => ("info", None, None, None, vec![], value.clone()),
            Kind::Range { min, max } => ("range", Some(min), Some(max), None, vec![], value.clone()),
            Kind::Toggle => ("toggle", None, None, None, vec![], value.clone()),
            Kind::Text { max } => ("text", None, None, Some(max), vec![], value.clone()),
            Kind::Choice(o) => {
                let mut opts = options(cam, o, &mut cache).await;
                if opts.is_empty() {
                    continue; // nothing valid to offer
                }
                // Keep the camera's current value selectable even if it isn't in the known list.
                if !opts.iter().any(|c| c.value == value) {
                    let label = if matches!(o, Opts::TimeZones) { format_value(Fmt::TimeZone, &value) } else { value.clone() };
                    opts.insert(0, Choice { value: value.clone(), label });
                }
                let display = opts.iter().find(|c| c.value == value).map(|c| c.label.clone()).unwrap_or_default();
                ("choice", None, None, None, opts, display)
            }
        };
        fields.push(FieldView { key: field.key, label: field.label, kind, value, display, min, max, max_len, options: opts });
    }
    PageView { id: page.id, title: page.title, fields }
}

/// Check a requested value against the field's domain before anything is sent to the camera.
pub fn validate(field: &Field, value: &str, options: &[Choice]) -> Result<(), String> {
    match field.kind {
        Kind::Info(_) => Err(format!("{} is read-only", field.label)),
        Kind::Range { min, max } => match value.parse::<i64>() {
            Ok(v) if (min..=max).contains(&v) => Ok(()),
            _ => Err(format!("{} must be a whole number from {min} to {max}", field.label)),
        },
        Kind::Toggle => if value == "0" || value == "1" { Ok(()) } else { Err(format!("{} must be on or off", field.label)) },
        Kind::Text { max } => {
            if value.trim().is_empty() {
                Err(format!("{} can't be empty", field.label))
            } else if value.chars().count() > max {
                Err(format!("{} can be at most {max} characters", field.label))
            } else if value.chars().any(|c| c.is_control()) {
                Err(format!("{} contains invalid characters", field.label))
            } else {
                Ok(())
            }
        }
        Kind::Choice(_) => {
            if options.iter().any(|c| c.value == value) { Ok(()) } else { Err(format!("{} has no option {value:?}", field.label)) }
        }
    }
}

/// Requested changes to one message: each field with its new value.
type Edits<'a> = Vec<(&'a Field, &'a String)>;

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct WriteResult {
    pub key: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Apply changes (field key -> value): validate all, then per message read-modify-write and read back to verify.
pub async fn write_page<C: Camera>(cam: &C, page: &Page, changes: &BTreeMap<String, String>) -> Vec<WriteResult> {
    let mut results = vec![];
    let mut cache = HashMap::new();
    let mut by_msg: BTreeMap<u32, (Msg, Edits)> = BTreeMap::new();
    for (key, value) in changes {
        let Some(field) = page.fields.iter().find(|f| f.key == key) else {
            results.push(WriteResult { key: key.clone(), ok: false, error: Some("unknown setting".into()) });
            continue;
        };
        if field.msg.set.is_none() {
            results.push(WriteResult { key: key.clone(), ok: false, error: Some(format!("{} is read-only", field.label)) });
            continue;
        }
        let opts = match field.kind {
            Kind::Choice(o) => options(cam, o, &mut cache).await,
            _ => vec![],
        };
        // The camera's current value is always an acceptable choice (it may predate our option list).
        let current = fetch(cam, field.msg, &mut cache).await.and_then(|x| xmledit::get(&x, field.path));
        let ok = validate(field, value, &opts).or_else(|e| if current.as_deref() == Some(value.as_str()) { Ok(()) } else { Err(e) });
        match ok {
            Ok(()) => by_msg.entry(field.msg.get).or_insert((field.msg, vec![])).1.push((field, value)),
            Err(e) => results.push(WriteResult { key: key.clone(), ok: false, error: Some(e) }),
        }
    }
    for (_, (msg, edits)) in by_msg {
        let fail = |results: &mut Vec<WriteResult>, edits: &[(&Field, &String)], e: String| {
            for (f, _) in edits {
                results.push(WriteResult { key: f.key.into(), ok: false, error: Some(e.clone()) });
            }
        };
        // Always start from a fresh read so concurrent changes on the camera aren't overwritten.
        let mut xml = match cam.get(msg).await {
            Ok(x) => x,
            Err(e) => {
                fail(&mut results, &edits, format!("couldn't read current settings: {e}"));
                continue;
            }
        };
        let mut edit_err = None;
        for (f, v) in &edits {
            match xmledit::set(&xml, f.path, v) {
                Ok(x) => xml = x,
                Err(e) => edit_err = Some(e.to_string()),
            }
        }
        if let Some(e) = edit_err {
            fail(&mut results, &edits, e);
            continue;
        }
        if let Err(e) = cam.set(msg, xml).await {
            fail(&mut results, &edits, format!("camera rejected the change: {e}"));
            continue;
        }
        let after = cam.get(msg).await.ok();
        for (f, v) in edits {
            let got = after.as_deref().and_then(|x| xmledit::get(x, f.path));
            let ok = got.as_deref() == Some(v.as_str());
            results.push(WriteResult {
                key: f.key.into(),
                ok,
                error: (!ok).then(|| format!("camera kept {:?} instead of {v:?}", got.unwrap_or_default())),
            });
        }
    }
    results
}

/// SystemGeneral XML set to `utc_secs` expressed in the camera's own time zone (for "sync time with this PC").
pub fn with_clock(general_xml: &str, utc_secs: i64) -> Result<String, String> {
    let west: i64 = xmledit::get(general_xml, "SystemGeneral/timeZone")
        .and_then(|v| v.parse().ok())
        .ok_or("camera didn't report its time zone")?;
    let local = time::OffsetDateTime::from_unix_timestamp(utc_secs - west).map_err(|e| e.to_string())?;
    let mut xml = general_xml.to_string();
    for (path, v) in [
        ("SystemGeneral/year", local.year() as i64),
        ("SystemGeneral/month", u8::from(local.month()) as i64),
        ("SystemGeneral/day", local.day() as i64),
        ("SystemGeneral/hour", local.hour() as i64),
        ("SystemGeneral/minute", local.minute() as i64),
        ("SystemGeneral/second", local.second() as i64),
    ] {
        xml = xmledit::set(&xml, path, &v.to_string()).map_err(|e| e.to_string())?;
    }
    Ok(xml)
}

pub const SYSTEM_GENERAL: Msg = GENERAL;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory camera: per-message XML; a write replaces it unless the field is "sticky" (camera refuses).
    struct Fake {
        xml: Mutex<HashMap<u32, String>>,
        refuse_writes_to: Option<&'static str>,
        sets: Mutex<Vec<u32>>,
    }

    impl Fake {
        fn new(msgs: &[(u32, &str)]) -> Self {
            Fake { xml: Mutex::new(msgs.iter().map(|(i, x)| (*i, x.to_string())).collect()), refuse_writes_to: None, sets: Mutex::new(vec![]) }
        }
    }

    impl Camera for Fake {
        fn get(&self, msg: Msg) -> impl std::future::Future<Output = Result<String, String>> + Send {
            let r = self.xml.lock().unwrap().get(&msg.get).cloned().ok_or_else(|| "405".into());
            async move { r }
        }
        fn set(&self, msg: Msg, xml: String) -> impl std::future::Future<Output = Result<(), String>> + Send {
            let r = self.apply(msg, xml);
            async move { r }
        }
    }

    impl Fake {
        fn apply(&self, msg: Msg, xml: String) -> Result<(), String> {
            self.sets.lock().unwrap().push(msg.set.unwrap());
            let mut map = self.xml.lock().unwrap();
            let mut new = xml;
            if let Some(path) = self.refuse_writes_to {
                let old = xmledit::get(&map[&msg.get], path).unwrap();
                new = xmledit::set(&new, path, &old).unwrap();
            }
            map.insert(msg.get, new);
            Ok(())
        }
    }

    const LEDS_XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<body>\n<LedState version=\"1.1\">\n<channelId>0</channelId>\n<ledVersion>0</ledVersion>\n<IRLedBrightness>80</IRLedBrightness>\n<state>close</state>\n<lightState>open</lightState>\n</LedState>\n</body>\n";
    const STREAMS_XML: &str = "<body><StreamInfoList version=\"1.1\"><StreamInfo><channelBits>1</channelBits><encodeTable><type>mainStream</type><framerateTable>15,12,10</framerateTable><bitrateTable>1024,1536,2048,3072,4096</bitrateTable></encodeTable><encodeTable><type>subStream</type><framerateTable>15,10</framerateTable><bitrateTable>256,512,1024</bitrateTable></encodeTable></StreamInfo></StreamInfoList></body>";
    const ENCODE_XML: &str = "<body><Compression version=\"1.1\"><mainStream><resolutionName>5120*1440</resolutionName><frame>15</frame><bitRate>4096</bitRate></mainStream><subStream><resolutionName>1536*432</resolutionName><frame>15</frame><bitRate>1024</bitRate></subStream></Compression></body>";
    const GENERAL_XML: &str = "<body><SystemGeneral version=\"1.1\"><timeZone>25200</timeZone><osdFormat>MDY</osdFormat><year>2026</year><month>10</month><day>24</day><hour>13</hour><minute>17</minute><second>47</second><timeFormat>1</timeFormat><deviceName>Backyard</deviceName></SystemGeneral></body>";

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    #[test]
    fn reads_only_supported_fields_with_options() {
        let cam = Fake::new(&[(208, LEDS_XML)]);
        let v = run(read_page(&cam, page("lights").unwrap()));
        let keys: Vec<_> = v.fields.iter().map(|f| f.key).collect();
        assert_eq!(keys, ["ir", "ir_bright", "status_led"]);
        assert_eq!(v.fields[0].display, "Off");
        assert_eq!((v.fields[1].min, v.fields[1].max), (Some(0), Some(100)));
        // Display page: camera answers nothing, so nothing is shown.
        assert!(run(read_page(&cam, page("display").unwrap())).fields.is_empty());
    }

    #[test]
    fn stream_options_come_from_the_camera() {
        let cam = Fake::new(&[(56, ENCODE_XML), (146, STREAMS_XML)]);
        let v = run(read_page(&cam, page("stream").unwrap()));
        let sub_kbps = v.fields.iter().find(|f| f.key == "sub_kbps").unwrap();
        let vals: Vec<_> = sub_kbps.options.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(vals, ["256", "512", "1024"]);
        assert_eq!(v.fields.iter().find(|f| f.key == "main_res").unwrap().kind, "info");
    }

    #[test]
    fn writes_validate_apply_and_verify() {
        let cam = Fake::new(&[(208, LEDS_XML)]);
        let p = page("lights").unwrap();
        let changes: BTreeMap<String, String> =
            [("ir_bright".into(), "55".into()), ("ir".into(), "auto".into())].into_iter().collect();
        let r = run(write_page(&cam, p, &changes));
        assert!(r.iter().all(|r| r.ok), "{r:?}");
        assert_eq!(*cam.sets.lock().unwrap(), vec![209], "one write for both fields of the message");
        let xml = cam.xml.lock().unwrap()[&208].clone();
        assert_eq!(xmledit::get(&xml, "LedState/IRLedBrightness").as_deref(), Some("55"));
        assert_eq!(xmledit::get(&xml, "LedState/ledVersion").as_deref(), Some("0"), "unknown fields preserved");
    }

    #[test]
    fn rejects_out_of_domain_values_without_writing() {
        let cam = Fake::new(&[(208, LEDS_XML)]);
        let p = page("lights").unwrap();
        for (k, v) in [("ir_bright", "101"), ("ir_bright", "x"), ("ir", "strobe"), ("model", "y")] {
            let changes: BTreeMap<String, String> = [(k.into(), v.into())].into_iter().collect();
            let r = run(write_page(&cam, p, &changes));
            assert!(!r[0].ok, "{k}={v} should be rejected");
        }
        assert!(cam.sets.lock().unwrap().is_empty());
    }

    #[test]
    fn reports_when_camera_ignores_a_write() {
        let mut cam = Fake::new(&[(208, LEDS_XML)]);
        cam.refuse_writes_to = Some("LedState/IRLedBrightness");
        let changes: BTreeMap<String, String> = [("ir_bright".into(), "10".into())].into_iter().collect();
        let r = run(write_page(&cam, page("lights").unwrap(), &changes));
        assert!(!r[0].ok);
        assert!(r[0].error.as_deref().unwrap().contains("kept \"80\""));
    }

    #[test]
    fn retention_and_signal_labels_match_official_client() {
        assert_eq!(retention_label("1"), "Overwrite oldest files");
        assert_eq!(retention_label("0"), "No overwrite");
        assert_eq!(retention_label("-7"), "Delete after 7 days");
        assert_eq!(retention_label("-1"), "Delete after 1 day");
        assert_eq!(format_value(Fmt::Dbm, "-10"), "measuring…");
        assert_eq!(format_value(Fmt::Dbm, "-59"), "-59 dBm");
        let rec = "<body><RecordCfg><cycle>-7</cycle><cyclelist><item>0</item><item>1</item><item>-7</item></cyclelist></RecordCfg></body>";
        let cam = Fake::new(&[(54, rec)]);
        let v = run(read_page(&cam, page("detection").unwrap()));
        let r = v.fields.iter().find(|f| f.key == "rec_retention").unwrap();
        assert_eq!(r.display, "Delete after 7 days");
        assert_eq!(r.options.len(), 3);
    }

    #[test]
    fn time_zone_and_clock() {
        assert_eq!(tz_label(25200), "UTC−7:00");
        assert_eq!(tz_label(-19800), "UTC+5:30");
        assert!(time_zones().iter().any(|c| c.value == "25200"));
        // 2026-10-24 20:17:47 UTC in UTC−7 is 13:17:47 local.
        let xml = with_clock(GENERAL_XML, 1792873067).unwrap();
        assert_eq!(xmledit::get(&xml, "SystemGeneral/hour").as_deref(), Some("13"));
        assert_eq!(xmledit::get(&xml, "SystemGeneral/day").as_deref(), Some("24"));
        assert_eq!(xmledit::get(&xml, "SystemGeneral/deviceName").as_deref(), Some("Backyard"));
    }

    #[test]
    fn every_field_path_targets_a_known_message() {
        for p in PAGES {
            for f in p.fields {
                assert!(f.path.contains('/'), "{}", f.key);
                if !matches!(f.kind, Kind::Info(_)) {
                    assert!(f.msg.set.is_some(), "{} is editable but has no write id", f.key);
                }
            }
        }
    }
}
