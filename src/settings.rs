//! Camera settings: a catalog of pages/fields mapped onto Baichuan messages, and an engine that reads them and
//! writes changes read-modify-write, then reads back to verify the camera applied them.
//!
//! Message ids come from the official SDK's command table (docs/baichuan-command-ids.md) and captured traffic;
//! where they disagreed (auto-reboot), the camera's own behaviour decided. Value domains, validation rules and
//! binary formats (motion zones, privacy masks) come from the official client and SDK (docs/settings-inventory.md).
//! Fields a camera doesn't answer (message rejected or element missing) are left out, so each camera shows
//! exactly what it supports.

use crate::xmledit;
use base64::Engine as _;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Msg {
    pub get: u32,
    pub set: Option<u32>,
    /// Needs the `<Extension><channelId>` block.
    pub channel: bool,
}

const fn m(get: u32, set: Option<u32>, channel: bool) -> Msg {
    Msg { get, set, channel }
}

const VERSION: Msg = m(80, None, false);
const UID: Msg = m(114, None, false);
const NET_INFO: Msg = m(76, None, false);
const BATTERY: Msg = m(253, None, true);
const STORAGE: Msg = m(102, None, false);
const SIGNAL: Msg = m(115, None, true);
const PERF: Msg = m(122, None, false);
const IMAGE: Msg = m(26, Some(25), true);
const OSD: Msg = m(44, Some(45), true);
const ENCODE: Msg = m(56, Some(57), true);
const STREAMS: Msg = m(146, None, true);
const PIR: Msg = m(212, Some(213), true);
const LEDS: Msg = m(208, Some(209), true);
const RECORD: Msg = m(54, Some(55), true);
const EMAIL_TASK: Msg = m(217, Some(216), true);
const PUSH_TASK: Msg = m(219, Some(218), true);
const GENERAL: Msg = m(104, Some(105), false);
const NTP: Msg = m(38, Some(39), false);
const DST: Msg = m(106, Some(107), false);
// The SDK table labels 100 as GET, but on the camera 101 returns the config and 100 only acknowledges.
const AUTO_REBOOT: Msg = m(101, Some(100), false);
const MOTION: Msg = m(46, Some(47), true);
const MASKS: Msg = m(52, Some(53), true);
const REC_SCHEDULE: Msg = m(81, Some(82), true);
const FTP_TASK: Msg = m(70, Some(71), true);
const SIREN_TASK: Msg = m(232, Some(231), true);
const EMAIL: Msg = m(42, Some(43), true);
const FTP: Msg = m(68, Some(69), true);
const NETWORK: Msg = m(76, Some(77), true);
const PORTS: Msg = m(37, Some(36), true);
const UPNP: Msg = m(97, Some(98), true);
const DDNS: Msg = m(40, Some(41), true);
const AI: Msg = m(299, Some(300), true);
const TIMELAPSE: Msg = m(319, Some(320), true);
// Found by the official-setter audit (docs/settings-inventory.md); write ids from the SDK command table.
const DN_THRESHOLD: Msg = m(296, Some(297), true);
const AI_DENOISE: Msg = m(439, Some(440), true);
const STITCH: Msg = m(417, Some(418), true);
const PUSH_CFG: Msg = m(363, Some(364), true);
const SPEAKER: Msg = m(264, Some(265), true);
const POWER_SAVING: Msg = m(423, Some(424), true);
const AUTO_UPDATE: Msg = m(195, Some(196), true);
const P2P: Msg = m(210, Some(211), true);
const ACCESS_USERS: Msg = m(511, None, true);
/// Test commands: the current Email/Ftp XML sent to the SDK's EMAILTEST (141) / FTP_TEST (194).
pub const EMAIL_TEST: Msg = m(42, Some(141), true);
pub const FTP_TEST: Msg = m(68, Some(194), true);
pub const SYSTEM_GENERAL: Msg = GENERAL;

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

/// Allowed characters, from the official client's validators.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Charset {
    Any,
    /// Host names and IPs.
    Host,
    Email,
    /// /^[^\\\/'"`^$?;<>|& ]*$/ (DDNS names and passwords)
    Ddns,
    /// /^[a-zA-Z0-9\\\/]*$/ (FTP remote directory)
    FtpPath,
}

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Info(Fmt),
    /// Slider.
    Range { min: i64, max: i64 },
    /// Plain number box (ports).
    Number { min: i64, max: i64 },
    Toggle,
    Choice(Opts),
    Text { max: usize, charset: Charset, required: bool },
    /// Never sent to the UI; an empty value leaves the camera's password unchanged.
    Password { max: usize, charset: Charset },
    Ipv4 { required: bool },
    /// typeScheduleList: path is the list; value is JSON {type: 168 x "0"/"1"} (7 days x 24 h, Sunday first).
    Schedule,
    /// Motion area: path is the MD element; value is JSON {"w","h","cells"} with cells row-major "0"/"1".
    Zone,
    /// Privacy masks: path is the Shelter element; value is JSON {"enable","max","rects":[[x,y,w,h],...]} in 0..1000.
    Masks,
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
const PORT: Kind = Kind::Number { min: 1, max: 65535 };
const MD_SENS: Kind = Kind::Range { min: 1, max: 50 };
const MD_SENS_DEFAULT: &str = "MD/sensInfoNew/sensitivityDefault";
const fn text(max: usize, charset: Charset, required: bool) -> Kind {
    Kind::Text { max, charset, required }
}
const fn pass(max: usize, charset: Charset) -> Kind {
    Kind::Password { max, charset }
}
const WEEKDAYS: &[(&str, &str)] = &[
    ("Sunday", "Sunday"), ("Monday", "Monday"), ("Tuesday", "Tuesday"), ("Wednesday", "Wednesday"),
    ("Thursday", "Thursday"), ("Friday", "Friday"), ("Saturday", "Saturday"),
];
/// Sizes of the coordinate space masks are expressed in; the camera rescales from it (the SDK packs
/// each coordinate as value<<16 | reference size).
pub const MASK_REF: i64 = 1000;

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
            f("ip", "IP address", NET_INFO, "Ip/ip", INFO),
            f("mac", "MAC address", NET_INFO, "Ip/mac", INFO),
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
            f("osd_name", "Camera name on video", OSD, "OsdChannelName/name", text(31, Charset::Any, true)),
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
            f("dn_level", "Day/night switch level", DN_THRESHOLD, "DayNightThreshold/thresholdval/cur", Kind::Range { min: 0, max: 100 }),
            f("ai_denoise", "AI noise reduction", AI_DENOISE, "aiDenoise/enable", Kind::Toggle),
            // Dual-lens cameras: where the two images are joined (ranges as the camera reports them).
            f("stitch_dist", "Stitching distance (m)", STITCH, "BinoStitch/distance/cur", Kind::Range { min: 2, max: 20 }),
            f("stitch_x", "Stitching horizontal offset", STITCH, "BinoStitch/xpixel/cur", Kind::Range { min: -100, max: 100 }),
            f("stitch_y", "Stitching vertical offset", STITCH, "BinoStitch/ypixel/cur", Kind::Range { min: -100, max: 100 }),
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
        title: "Detection & recording",
        fields: &[
            f("pir_on", "Motion sensor (PIR)", PIR, "rfAlarmCfg/enable", Kind::Toggle),
            f("pir_sens", "PIR sensitivity", PIR, "rfAlarmCfg/sensiValue", Kind::Range { min: 1, max: 100 }),
            f("pir_reduce", "Reduce false alarms", PIR, "rfAlarmCfg/reduceFalseAlarm", Kind::Toggle),
            f("push_on", "Push notifications", PUSH_TASK, "PushTask/enable", Kind::Toggle),
            f("push_interval", "Minimum time between push notifications (s)", PUSH_CFG, "PushCfg/interval", Kind::Number { min: 0, max: 3600 }),
            f("rich_push", "Rich notifications (with picture)", PUSH_CFG, "PushCfg/richNotificationEnable", Kind::Toggle),
            f("email_on", "Email alerts", EMAIL_TASK, "EmailTask/enable", Kind::Toggle),
            f("rec_retention", "SD card recordings", RECORD, "RecordCfg/cycle",
                Kind::Choice(Opts::Retention(RECORD, "RecordCfg/cyclelist/item"))),
            f("rec_post", "Keep recording after motion (s)", RECORD, "RecordCfg/recordDelayTime",
                Kind::Choice(Opts::List(RECORD, "RecordCfg/timeList/time"))),
        ],
    },
    Page {
        id: "motion",
        title: "Motion detection",
        fields: &[
            f("md_on", "Motion detection", MOTION, "MD/enable", Kind::Toggle),
            f("md_zone", "Detection area", MOTION, "MD", Kind::Zone),
            // The official client edits sensInfoNew whenever the camera reports it (iSensVerion != 0);
            // shortcut: older cameras with only the legacy sensitivityInfoList don't show this field.
            f("md_sens", "Sensitivity", MOTION, MD_SENS_DEFAULT, MD_SENS),
        ],
    },
    Page {
        id: "privacy",
        title: "Privacy masks",
        fields: &[f("masks", "Masked areas", MASKS, "Shelter", Kind::Masks)],
    },
    Page {
        id: "schedules",
        title: "Schedules",
        fields: &[
            f("rec_on", "SD card recording", REC_SCHEDULE, "Record/enable", Kind::Toggle),
            f("rec_sched", "Recording schedule", REC_SCHEDULE, "Record/typeScheduleList", Kind::Schedule),
            f("push_sched", "Push notification schedule", PUSH_TASK, "PushTask/typeScheduleList", Kind::Schedule),
            f("email_sched", "Email alert schedule", EMAIL_TASK, "EmailTask/typeScheduleList", Kind::Schedule),
            f("ftp_sched", "FTP upload schedule", FTP_TASK, "FtpTask/typeScheduleList", Kind::Schedule),
            f("siren_on", "Siren on detection", SIREN_TASK, "AudioTask/enable", Kind::Toggle),
            f("siren_sched", "Siren schedule", SIREN_TASK, "AudioTask/typeScheduleList", Kind::Schedule),
        ],
    },
    Page {
        id: "email",
        title: "Email",
        fields: &[
            f("smtp_server", "SMTP server", EMAIL, "Email/smtpServer", text(127, Charset::Host, false)),
            f("smtp_port", "SMTP port", EMAIL, "Email/smtpPort", PORT),
            f("smtp_ssl", "SSL/TLS", EMAIL, "Email/ssl", Kind::Toggle),
            f("smtp_user", "Sender address / username", EMAIL, "Email/userName", text(127, Charset::Email, false)),
            f("smtp_pass", "Password", EMAIL, "Email/password", pass(127, Charset::Any)),
            f("smtp_nick", "Sender name", EMAIL, "Email/sendNickname", text(127, Charset::Any, false)),
            f("rcpt1", "Recipient 1", EMAIL, "Email/address1", text(127, Charset::Email, false)),
            f("rcpt2", "Recipient 2", EMAIL, "Email/address2", text(127, Charset::Email, false)),
            f("rcpt3", "Recipient 3", EMAIL, "Email/address3", text(127, Charset::Email, false)),
            f("attach", "Attach picture", EMAIL, "Email/attachment", Kind::Toggle),
        ],
    },
    Page {
        id: "ftp",
        title: "FTP",
        fields: &[
            f("ftp_on", "FTP upload", FTP_TASK, "FtpTask/enable", Kind::Toggle),
            f("ftp_server", "FTP server", FTP, "Ftp/server", text(127, Charset::Host, false)),
            f("ftp_port", "Port", FTP, "Ftp/port", PORT),
            f("ftp_anon", "Anonymous login", FTP, "Ftp/nonymous", Kind::Toggle),
            f("ftp_user", "Username", FTP, "Ftp/userName", text(127, Charset::Any, false)),
            f("ftp_pass", "Password", FTP, "Ftp/password", pass(127, Charset::Any)),
            f("ftp_dir", "Remote folder", FTP, "Ftp/remoteDir", text(127, Charset::FtpPath, false)),
        ],
    },
    Page {
        id: "network",
        title: "Network",
        fields: &[
            f("dhcp", "Get IP address automatically (DHCP)", NETWORK, "Dhcp/enable", Kind::Toggle),
            f("net_ip", "IP address", NETWORK, "Ip/ip", Kind::Ipv4 { required: true }),
            f("net_mask", "Subnet mask", NETWORK, "Ip/mask", Kind::Ipv4 { required: true }),
            f("net_gw", "Gateway", NETWORK, "Ip/gateway", Kind::Ipv4 { required: true }),
            f("auto_dns", "Get DNS automatically", NETWORK, "AutoDns/enable", Kind::Toggle),
            f("dns1", "Primary DNS", NETWORK, "Dns/dns1", Kind::Ipv4 { required: false }),
            f("dns2", "Secondary DNS", NETWORK, "Dns/dns2", Kind::Ipv4 { required: false }),
            f("upnp", "UPnP", UPNP, "Upnp/enable", Kind::Toggle),
            f("port_server", "Client port", PORTS, "ServerPort/serverPort", INFO),
            f("http_on", "HTTP", PORTS, "HttpPort/enable", Kind::Toggle),
            f("http_port", "HTTP port", PORTS, "HttpPort/httpPort", PORT),
            f("https_on", "HTTPS", PORTS, "HttpsPort/enable", Kind::Toggle),
            f("https_port", "HTTPS port", PORTS, "HttpsPort/httpsPort", PORT),
            f("rtsp_on", "RTSP", PORTS, "RtspPort/enable", Kind::Toggle),
            f("rtsp_port", "RTSP port", PORTS, "RtspPort/rtspPort", PORT),
            f("rtmp_on", "RTMP", PORTS, "RtmpPort/enable", Kind::Toggle),
            f("rtmp_port", "RTMP port", PORTS, "RtmpPort/rtmpPort", PORT),
            f("onvif_on", "ONVIF", PORTS, "OnvifPort/enable", Kind::Toggle),
            f("onvif_port", "ONVIF port", PORTS, "OnvifPort/onvifPort", PORT),
            f("ddns_on", "DDNS", DDNS, "Ddns/enable", Kind::Toggle),
            f("ddns_type", "DDNS provider", DDNS, "Ddns/ddnsType", INFO),
            f("ddns_name", "DDNS domain", DDNS, "Ddns/ddnsName", text(127, Charset::Ddns, false)),
            f("ddns_user", "DDNS username", DDNS, "Ddns/userName", text(127, Charset::Ddns, false)),
            f("ddns_pass", "DDNS password", DDNS, "Ddns/password", pass(127, Charset::Ddns)),
        ],
    },
    Page {
        id: "smart",
        title: "Smart detection & time-lapse",
        fields: &[
            f("ai_types", "Detects", AI, "AiCfg/detectType", INFO),
            f("tl_overwrite", "Time-lapse: overwrite oldest files", TIMELAPSE, "timelapseCfg/overwrite", Kind::Toggle),
            f("tl_max", "Time-lapse: max files", TIMELAPSE, "timelapseCfg/maxFileNum", INFO),
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
        id: "audio",
        title: "Speaker & siren",
        fields: &[
            f("spk_volume", "Speaker volume", SPEAKER, "audioCfg/volume", Kind::Range { min: 0, max: 100 }),
            f("pre_alarm", "Pre-recorded alarm sound", SPEAKER, "audioCfg/preAlarm", Kind::Toggle),
        ],
    },
    Page {
        id: "power",
        title: "Power saving",
        fields: &[
            f("ps_on", "Power-saving schedule", POWER_SAVING, "PowerSaving/enable", Kind::Toggle),
            f("ps_sched", "Power-saving hours", POWER_SAVING, "PowerSaving/timeTable", Kind::Schedule),
        ],
    },
    Page {
        id: "system",
        title: "System",
        fields: &[
            f("name", "Device name", GENERAL, "SystemGeneral/deviceName", text(31, Charset::Any, true)),
            f("tz", "Time zone", GENERAL, "SystemGeneral/timeZone", Kind::Choice(Opts::TimeZones)),
            f("date_fmt", "Date format", GENERAL, "SystemGeneral/osdFormat",
                Kind::Choice(Opts::Fixed(&[("MDY", "MM/DD/YYYY"), ("DMY", "DD/MM/YYYY"), ("YMD", "YYYY/MM/DD")]))),
            f("time_fmt", "Time format", GENERAL, "SystemGeneral/timeFormat",
                Kind::Choice(Opts::Fixed(&[("1", "12-hour"), ("0", "24-hour")]))),
            f("ntp_on", "Sync time from internet (NTP)", NTP, "Ntp/enable", Kind::Toggle),
            f("ntp_server", "NTP server", NTP, "Ntp/server", text(63, Charset::Host, true)),
            f("ntp_interval", "NTP sync interval", NTP, "Ntp/synchronizeInterval",
                Kind::Choice(Opts::Fixed(&[("60", "Every hour"), ("360", "Every 6 hours"), ("720", "Every 12 hours"), ("1440", "Daily"), ("10080", "Weekly")]))),
            f("dst_on", "Daylight saving time", DST, "Dst/enable", Kind::Toggle),
            f("login_lock", "Lock out after repeated failed logins", GENERAL, "SystemGeneral/loginLock", Kind::Toggle),
            f("auto_update", "Automatic firmware updates", AUTO_UPDATE, "AutoUpdate/enable", Kind::Toggle),
            f("p2p", "Reolink P2P (remote access by UID)", P2P, "PTOP/enable", Kind::Toggle),
            // 58 (user list) is refused by battery cameras; 511 only reports how many extra users the camera takes.
            f("access_users", "Additional users supported", ACCESS_USERS, "accessUserList/maxAccessUserNum", Kind::Info(Fmt::Plain)),
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
    /// False once this camera has refused writes to the message (e.g. Argus cameras reject motion-area
    /// writes with 400); its fields are then shown read-only instead of offering edits that can't work.
    fn writable(&self, _msg: Msg) -> bool {
        true
    }
}

/// The camera refused the message itself (400/405), as opposed to a transport failure.
pub fn is_refusal(err: &str) -> bool {
    err.contains("returned code 400") || err.contains("returned code 405")
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
    /// info | range | number | toggle | choice | text | password | ipv4 | schedule | zone | masks
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

// ---------- formatting ----------

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

// ---------- compound values: schedules, motion zones, privacy masks ----------

pub const SCHEDULE_LEN: usize = 7 * 24;

/// Key for schedules stored as one bare 168-char table (e.g. PowerSaving/timeTable) rather than a typed list.
const PLAIN_TABLE: &str = "hours";

fn plain_table(xml: &str, path: &str) -> Option<String> {
    xmledit::get(xml, path).filter(|t| t.len() == 168 && t.chars().all(|c| c == '0' || c == '1'))
}

fn read_schedule(xml: &str, path: &str) -> Option<String> {
    if let Some(t) = plain_table(xml, path) {
        return serde_json::to_string(&BTreeMap::from([(PLAIN_TABLE, t)])).ok();
    }
    let types = xmledit::get_all(xml, &format!("{path}/item/type"));
    let tables = xmledit::get_all(xml, &format!("{path}/item/valueTable"));
    if types.is_empty() || types.len() != tables.len() {
        return None;
    }
    let map: BTreeMap<String, String> = types.into_iter().zip(tables).collect();
    serde_json::to_string(&map).ok()
}

fn write_schedule(xml: &str, path: &str, value: &str) -> Result<String, String> {
    let wanted: BTreeMap<String, String> = serde_json::from_str(value).map_err(|_| "invalid schedule".to_string())?;
    if plain_table(xml, path).is_some() {
        let t = wanted.get(PLAIN_TABLE).ok_or("invalid schedule")?;
        return xmledit::set(xml, path, t).map_err(|e| e.to_string());
    }
    let types = xmledit::get_all(xml, &format!("{path}/item/type"));
    let mut out = xml.to_string();
    for (t, table) in wanted {
        let i = types.iter().position(|x| *x == t).ok_or_else(|| format!("schedule has no {t:?} row"))?;
        out = xmledit::set(&out, &format!("{path}/item[{i}]/valueTable"), &table).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn schedule_summary(value: &str) -> String {
    let map: BTreeMap<String, String> = serde_json::from_str(value).unwrap_or_default();
    map.iter()
        .map(|(t, table)| format!("{t} {}/168 h", table.chars().filter(|c| *c == '1').count()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct ZoneValue {
    w: usize,
    h: usize,
    cells: String,
}

/// MD scope: a columns x rows bitmap (row-major, first cell in the high bit, base64), of which the top-left
/// width x height cells are the camera's detection grid. Format from the SDK's encoder (FUN_1801a69b0).
fn zone_geometry(xml: &str, md: &str) -> Option<(usize, usize, usize, usize, Vec<u8>)> {
    let num = |p: &str| xmledit::get(xml, &format!("{md}/{p}"))?.parse::<usize>().ok();
    let (w, h, cols, rows) = (num("width")?, num("height")?, num("scope/columns")?, num("scope/rows")?);
    let table = xmledit::get(xml, &format!("{md}/scope/valueTable"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(table.split_whitespace().collect::<String>()).ok()?;
    (w <= cols && h <= rows && bytes.len() * 8 >= cols * rows).then_some((w, h, cols, rows, bytes))
}

fn bit(bytes: &[u8], i: usize) -> bool {
    bytes[i / 8] & (0x80 >> (i % 8)) != 0
}

fn read_zone(xml: &str, md: &str) -> Option<String> {
    let (w, h, cols, _, bytes) = zone_geometry(xml, md)?;
    let cells: String = (0..h).flat_map(|r| (0..w).map(move |c| (r, c))).map(|(r, c)| if bit(&bytes, r * cols + c) { '1' } else { '0' }).collect();
    serde_json::to_string(&ZoneValue { w, h, cells }).ok()
}

fn write_zone(xml: &str, md: &str, value: &str) -> Result<String, String> {
    let z: ZoneValue = serde_json::from_str(value).map_err(|_| "invalid detection area".to_string())?;
    let (w, h, cols, _, mut bytes) = zone_geometry(xml, md).ok_or("camera's detection area couldn't be read")?;
    if (z.w, z.h) != (w, h) || z.cells.len() != w * h {
        return Err("detection area doesn't match the camera's grid".into());
    }
    // Only the detection grid changes; bits outside it are kept as the camera had them.
    for (i, ch) in z.cells.chars().enumerate() {
        let n = (i / w) * cols + i % w;
        if ch == '1' {
            bytes[n / 8] |= 0x80 >> (n % 8);
        } else {
            bytes[n / 8] &= !(0x80 >> (n % 8));
        }
    }
    let table = base64::engine::general_purpose::STANDARD.encode(&bytes);
    xmledit::set(xml, &format!("{md}/scope/valueTable"), &table).map_err(|e| e.to_string())
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct MasksValue {
    enable: String,
    max: usize,
    /// [x, y, w, h] in 0..MASK_REF.
    rects: Vec<[i64; 4]>,
}

/// Unpack an SDK mask coordinate (value<<16 | reference size) into the 0..MASK_REF space.
fn unpack(v: i64) -> i64 {
    let (coord, reference) = (v >> 16, v & 0xffff);
    if reference == 0 { coord } else { (coord * MASK_REF + reference / 2) / reference }
}

fn read_masks(xml: &str, root: &str) -> Option<String> {
    let enable = xmledit::get(xml, &format!("{root}/enable"))?;
    let max = xmledit::get(xml, &format!("{root}/maxNum")).and_then(|v| v.parse().ok()).unwrap_or(4);
    let mut rects = vec![];
    // The camera answers with <shelterList>; the SDK writes <ShelterList>. Accept either.
    for list in ["shelterList", "ShelterList"] {
        let items = format!("{root}/{list}/Shelter");
        let on = xmledit::get_all(xml, &format!("{items}/enable"));
        for (i, e) in on.iter().enumerate() {
            if e != "1" {
                continue;
            }
            let num = |p: &str| xmledit::get(xml, &format!("{items}[{i}]/{p}")).and_then(|v| v.parse::<i64>().ok());
            if let (Some(x), Some(y), Some(w), Some(h)) = (num("topLeftX"), num("topLeftY"), num("width"), num("height")) {
                rects.push([unpack(x), unpack(y), unpack(w), unpack(h)]);
            }
        }
    }
    serde_json::to_string(&MasksValue { enable, max, rects }).ok()
}

/// The full Shelter write as the SDK builds it (BaichuanConfigurator, FUN_1801185b0).
fn write_masks(xml: &str, root: &str, value: &str) -> Result<String, String> {
    let v: MasksValue = serde_json::from_str(value).map_err(|_| "invalid privacy masks".to_string())?;
    let channel = xmledit::get(xml, &format!("{root}/channelId")).unwrap_or_else(|| "0".into());
    let max = xmledit::get(xml, &format!("{root}/maxNum")).and_then(|v| v.parse().ok()).unwrap_or(v.max);
    let mut list = String::new();
    for i in 0..max {
        match v.rects.get(i) {
            Some([x, y, w, h]) => list.push_str(&format!(
                "<Shelter>\n<id>{i}</id>\n<enable>1</enable>\n<topLeftX>{}</topLeftX>\n<topLeftY>{}</topLeftY>\n<width>{}</width>\n<height>{}</height>\n</Shelter>\n",
                x << 16 | MASK_REF, y << 16 | MASK_REF, w << 16 | MASK_REF, h << 16 | MASK_REF
            )),
            None => list.push_str(&format!("<Shelter>\n<id>{i}</id>\n<enable>0</enable>\n</Shelter>\n")),
        }
    }
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<body>\n<Shelter version=\"1.1\">\n<channelId>{channel}</channelId>\n<enable>{}</enable>\n<ShelterList>\n{list}</ShelterList>\n</Shelter>\n</body>\n",
        v.enable
    ))
}

fn masks_close(a: &str, b: &str) -> bool {
    // Camera rescaling can round a coordinate by a unit or two.
    match (serde_json::from_str::<MasksValue>(a), serde_json::from_str::<MasksValue>(b)) {
        (Ok(a), Ok(b)) => {
            a.enable == b.enable
                && a.rects.len() == b.rects.len()
                && a.rects.iter().zip(&b.rects).all(|(p, q)| p.iter().zip(q).all(|(u, v)| (u - v).abs() <= 3))
        }
        _ => false,
    }
}

fn zone_summary(value: &str) -> String {
    serde_json::from_str::<ZoneValue>(value)
        .map(|z| format!("{} of {} areas active", z.cells.chars().filter(|c| *c == '1').count(), z.w * z.h))
        .unwrap_or_default()
}

/// The field's current value from its message's XML, in the form the UI edits.
fn extract(field: &Field, xml: &str) -> Option<String> {
    match field.kind {
        Kind::Schedule => read_schedule(xml, field.path),
        Kind::Zone => read_zone(xml, field.path),
        Kind::Masks => read_masks(xml, field.path),
        // Present but never revealed.
        Kind::Password { .. } => xmledit::get(xml, field.path).map(|_| String::new()),
        _ => xmledit::get(xml, field.path),
    }
}

/// `xml` with the field set to `value` (already validated).
fn apply(field: &Field, xml: &str, value: &str) -> Result<String, String> {
    match field.kind {
        Kind::Schedule => write_schedule(xml, field.path, value),
        Kind::Zone => write_zone(xml, field.path, value),
        Kind::Masks => write_masks(xml, field.path, value),
        // The camera stores empty strings as a single space.
        Kind::Text { .. } | Kind::Ipv4 { .. } if value.is_empty() => xmledit::set(xml, field.path, " ").map_err(|e| e.to_string()),
        // Like the official "default sensitivity" control: the default and every time period take the value.
        _ if field.path == MD_SENS_DEFAULT => {
            let mut x = xmledit::set(xml, field.path, value).map_err(|e| e.to_string())?;
            for i in 0..xmledit::get_all(xml, "MD/sensInfoNew/sensInfoList/sensInfo").len() {
                x = xmledit::set(&x, &format!("MD/sensInfoNew/sensInfoList/sensInfo[{i}]/sensitivity"), value).map_err(|e| e.to_string())?;
            }
            Ok(x)
        }
        _ => xmledit::set(xml, field.path, value).map_err(|e| e.to_string()),
    }
}

/// What a successful write must read back as.
fn applied(field: &Field, after: &str, value: &str) -> bool {
    match field.kind {
        Kind::Masks => read_masks(after, field.path).is_some_and(|got| masks_close(&got, value)),
        Kind::Password { .. } => xmledit::get(after, field.path).as_deref() == Some(value),
        _ => extract(field, after).as_deref() == Some(value),
    }
}

/// The camera's text for the first `<tag>` element (with its children), or None.
fn element<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let start = [format!("<{tag}>"), format!("<{tag} "), format!("<{tag}/")].iter().filter_map(|t| xml.find(t.as_str())).min()?;
    let rest = &xml[start..];
    let end = match rest.find(&format!("</{tag}>")) {
        Some(i) => i + tag.len() + 3,
        None => rest.find("/>")? + 2,
    };
    Some(&rest[..end])
}

/// What to send for a write. Most messages take the edited document whole; motion (47) must be the SDK's
/// partial `<MD>` (FUN_18014e120): channelId plus only the groups being changed. Argus cameras answer 400 to the
/// whole document read back from 46 (its empty timeBlockList/handleException aren't accepted as input).
fn outgoing(msg: Msg, xml: String, edits: &[(&Field, &String)]) -> Result<String, String> {
    if msg != MOTION {
        return Ok(xml);
    }
    let mut groups = vec![];
    for (f, _) in edits {
        let tags: &[&str] = match f.key {
            "md_on" => &["enable"],
            "md_zone" => &["width", "height", "scope"],
            "md_sens" => &["sensInfoNew"],
            _ => return Err(format!("{} can't be written", f.label)),
        };
        for t in tags {
            let e = element(&xml, t).ok_or_else(|| format!("camera's motion settings have no {t}"))?;
            if !groups.contains(&e) {
                groups.push(e);
            }
        }
    }
    let channel = element(&xml, "channelId").ok_or("camera's motion settings have no channelId")?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>
<body>
<MD version=\"1.1\">
{channel}
{}
</MD>
</body>
",
        groups.join("
")
    ))
}

// ---------- engine ----------

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

/// Read a page. Each distinct message is requested once, concurrently; fields the camera doesn't provide are omitted.
pub async fn read_page<C: Camera>(cam: &C, page: &Page) -> PageView {
    let mut msgs: Vec<Msg> = page.fields.iter().map(|f| f.msg).collect();
    for f in page.fields {
        match f.kind {
            Kind::Choice(Opts::StreamTable(..)) => msgs.push(STREAMS),
            Kind::Choice(Opts::List(m, _) | Opts::Retention(m, _)) => msgs.push(m),
            _ => {}
        }
    }
    msgs.sort_by_key(|m| (m.get, m.set));
    msgs.dedup();
    let replies = futures::future::join_all(msgs.iter().map(|m| cam.get(*m))).await;
    let mut cache: HashMap<Msg, Option<String>> = msgs.into_iter().zip(replies.into_iter().map(Result::ok)).collect();

    let mut fields = vec![];
    for field in page.fields {
        let Some(xml) = fetch(cam, field.msg, &mut cache).await else { continue };
        let Some(value) = extract(field, &xml) else { continue };
        let writable = field.msg.set.is_some() && cam.writable(field.msg);
        let (kind, min, max, max_len, opts, display) = match field.kind {
            Kind::Info(fmt) => ("info", None, None, None, vec![], format_value(fmt, &value)),
            Kind::Password { .. } if !writable => continue,
            Kind::Zone if !writable => ("info", None, None, None, vec![], zone_summary(&value)),
            Kind::Masks | Kind::Schedule if !writable => ("info", None, None, None, vec![], "set in the Reolink app".into()),
            _ if !writable => ("info", None, None, None, vec![], value.clone()),
            Kind::Range { min, max } => ("range", Some(min), Some(max), None, vec![], value.clone()),
            Kind::Number { min, max } => ("number", Some(min), Some(max), None, vec![], value.clone()),
            Kind::Toggle => ("toggle", None, None, None, vec![], value.clone()),
            Kind::Text { max, .. } => ("text", None, None, Some(max), vec![], value.clone()),
            Kind::Password { max, .. } => ("password", None, None, Some(max), vec![], String::new()),
            Kind::Ipv4 { .. } => ("ipv4", None, None, Some(15), vec![], value.clone()),
            Kind::Schedule => ("schedule", None, None, None, vec![], schedule_summary(&value)),
            Kind::Zone => ("zone", None, None, None, vec![], String::new()),
            Kind::Masks => ("masks", None, None, None, vec![], String::new()),
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

fn charset_ok(charset: Charset, v: &str) -> bool {
    match charset {
        Charset::Any => true,
        Charset::Host => v.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:".contains(c)),
        Charset::Email => {
            v.is_empty()
                || (v.chars().all(|c| !c.is_whitespace())
                    && v.split_once('@').is_some_and(|(a, d)| !a.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.')))
        }
        Charset::Ddns => !v.chars().any(|c| "\\/'\"`^$?;<>|& ".contains(c)),
        Charset::FtpPath => v.chars().all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '\\'),
    }
}

fn charset_hint(charset: Charset) -> &'static str {
    match charset {
        Charset::Any => "",
        Charset::Host => " (letters, digits, . - _ :)",
        Charset::Email => " (an email address)",
        Charset::Ddns => " (no spaces or \\ / ' \" ` ^ $ ? ; < > | &)",
        Charset::FtpPath => " (letters, digits and /)",
    }
}

fn parse_ipv4(v: &str) -> Option<u32> {
    v.parse::<std::net::Ipv4Addr>().ok().map(u32::from)
}

/// Check a requested value against the field's domain before anything is sent to the camera.
pub fn validate(field: &Field, value: &str, options: &[Choice]) -> Result<(), String> {
    let label = field.label;
    let text_ok = |max: usize, charset: Charset, required: bool| {
        if required && value.trim().is_empty() {
            Err(format!("{label} can't be empty"))
        } else if value.chars().count() > max {
            Err(format!("{label} can be at most {max} characters"))
        } else if value.chars().any(|c| c.is_control()) || !charset_ok(charset, value) {
            Err(format!("{label} has invalid characters{}", charset_hint(charset)))
        } else {
            Ok(())
        }
    };
    match field.kind {
        Kind::Info(_) => Err(format!("{label} is read-only")),
        Kind::Range { min, max } | Kind::Number { min, max } => match value.parse::<i64>() {
            Ok(v) if (min..=max).contains(&v) => Ok(()),
            _ => Err(format!("{label} must be a whole number from {min} to {max}")),
        },
        Kind::Toggle => if value == "0" || value == "1" { Ok(()) } else { Err(format!("{label} must be on or off")) },
        Kind::Text { max, charset, required } => text_ok(max, charset, required),
        Kind::Password { max, charset } => text_ok(max, charset, false),
        Kind::Ipv4 { required } => {
            if value.is_empty() && !required || parse_ipv4(value).is_some() {
                Ok(())
            } else {
                Err(format!("{label} must be an IPv4 address like 192.168.1.20"))
            }
        }
        Kind::Choice(_) => {
            if options.iter().any(|c| c.value == value) { Ok(()) } else { Err(format!("{label} has no option {value:?}")) }
        }
        Kind::Schedule => {
            let map: BTreeMap<String, String> = serde_json::from_str(value).map_err(|_| format!("{label} is malformed"))?;
            if map.values().all(|t| t.len() == SCHEDULE_LEN && t.chars().all(|c| c == '0' || c == '1')) {
                Ok(())
            } else {
                Err(format!("{label} must have 168 hourly slots per type"))
            }
        }
        Kind::Zone => {
            let z: ZoneValue = serde_json::from_str(value).map_err(|_| format!("{label} is malformed"))?;
            if z.cells.len() == z.w * z.h && z.cells.chars().all(|c| c == '0' || c == '1') {
                Ok(())
            } else {
                Err(format!("{label} is malformed"))
            }
        }
        Kind::Masks => {
            let v: MasksValue = serde_json::from_str(value).map_err(|_| format!("{label} is malformed"))?;
            let in_frame = v.rects.iter().all(|[x, y, w, h]| {
                *x >= 0 && *y >= 0 && *w > 0 && *h > 0 && x + w <= MASK_REF && y + h <= MASK_REF
            });
            if !(v.enable == "0" || v.enable == "1") || v.rects.len() > v.max || !in_frame {
                Err(format!("{label}: at most {} areas, each inside the picture", v.max))
            } else {
                Ok(())
            }
        }
    }
}

/// Whole-page checks that single fields can't express. Network: a static address must be usable.
fn validate_page(page: &Page, merged: &BTreeMap<&str, String>) -> Result<(), String> {
    if page.id != "network" || merged.get("dhcp").map(String::as_str) != Some("0") {
        return Ok(());
    }
    let ip = merged.get("net_ip").and_then(|v| parse_ipv4(v)).ok_or("A static IP address is required")?;
    let mask = merged.get("net_mask").and_then(|v| parse_ipv4(v)).ok_or("A subnet mask is required")?;
    let gw = merged.get("net_gw").and_then(|v| parse_ipv4(v)).ok_or("A gateway is required")?;
    if mask == 0 || (!mask).wrapping_add(1) & !mask != 0 {
        return Err("Subnet mask must be contiguous, like 255.255.255.0".into());
    }
    let host = ip & !mask;
    if host == 0 || host == !mask {
        return Err("IP address can't be the network or broadcast address".into());
    }
    if ip & mask != gw & mask || gw == ip {
        return Err("Gateway must be a different address on the same subnet as the IP address".into());
    }
    Ok(())
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
    // Blank passwords mean "keep the current one".
    let changes: BTreeMap<&String, &String> = changes
        .iter()
        .filter(|(k, v)| !(v.is_empty() && page.fields.iter().any(|f| f.key == k.as_str() && matches!(f.kind, Kind::Password { .. }))))
        .collect();

    // Page-level validation sees the camera's current values with the requested changes on top.
    let mut merged: BTreeMap<&str, String> = BTreeMap::new();
    for f in page.fields {
        if let Some(v) = changes.get(&f.key.to_string()) {
            merged.insert(f.key, (*v).clone());
        } else if let Some(xml) = fetch(cam, f.msg, &mut cache).await
            && let Some(v) = extract(f, &xml) {
                merged.insert(f.key, v);
            }
    }
    if let Err(e) = validate_page(page, &merged) {
        return changes.keys().map(|k| WriteResult { key: (*k).clone(), ok: false, error: Some(e.clone()) }).collect();
    }

    for (key, value) in changes {
        let Some(field) = page.fields.iter().find(|f| f.key == key.as_str()) else {
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
        let current = fetch(cam, field.msg, &mut cache).await.and_then(|x| extract(field, &x));
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
            match apply(f, &xml, v) {
                Ok(x) => xml = x,
                Err(e) => edit_err = Some(e),
            }
        }
        if let Some(e) = edit_err {
            fail(&mut results, &edits, e);
            continue;
        }
        let xml = match outgoing(msg, xml, &edits) {
            Ok(x) => x,
            Err(e) => {
                fail(&mut results, &edits, e);
                continue;
            }
        };
        if let Err(e) = cam.set(msg, xml).await {
            let why = if is_refusal(&e) {
                "this camera doesn't allow changing this setting (it refused the write)".to_string()
            } else {
                format!("camera rejected the change: {e}")
            };
            fail(&mut results, &edits, why);
            continue;
        }
        let after = cam.get(msg).await.ok();
        for (f, v) in edits {
            let ok = after.as_deref().is_some_and(|x| applied(f, x, v));
            let error = (!ok).then(|| match f.kind {
                Kind::Password { .. } => "camera didn't keep the new password".to_string(),
                _ => format!(
                    "camera kept {:?} instead of {v:?}",
                    after.as_deref().and_then(|x| extract(f, x)).unwrap_or_default()
                ),
            });
            results.push(WriteResult { key: f.key.into(), ok, error });
        }
    }
    results
}

/// Send the camera's current Email/Ftp settings to its test command (141/194); the camera reports success.
pub async fn run_test<C: Camera>(cam: &C, test: Msg) -> Result<(), String> {
    let xml = cam.get(test).await?;
    cam.set(test, xml).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory camera: per-message XML; a write replaces it unless a path is "sticky" (camera refuses).
    struct Fake {
        xml: Mutex<HashMap<u32, String>>,
        refuse_writes_to: Option<&'static str>,
        sets: Mutex<Vec<u32>>,
        /// Masks: answer with the camera's lowercase <shelterList> form after a write.
        masks_echo: bool,
        /// Refuse writes to this get-id with 400, like Argus cameras do for motion (47).
        refuse_msg: Option<u32>,
        refused: Mutex<Vec<u32>>,
    }

    impl Fake {
        fn new(msgs: &[(u32, &str)]) -> Self {
            Fake {
                xml: Mutex::new(msgs.iter().map(|(i, x)| (*i, x.to_string())).collect()),
                refuse_writes_to: None,
                sets: Mutex::new(vec![]),
                masks_echo: false,
                refuse_msg: None,
                refused: Mutex::new(vec![]),
            }
        }

        fn apply(&self, msg: Msg, xml: String) -> Result<(), String> {
            if self.refuse_msg == Some(msg.get) {
                self.refused.lock().unwrap().push(msg.get);
                return Err(format!("Msg of type {} returned code 400", msg.set.unwrap()));
            }
            self.sets.lock().unwrap().push(msg.set.unwrap());
            let mut map = self.xml.lock().unwrap();
            let mut new = xml;
            if let Some(path) = self.refuse_writes_to {
                let old = xmledit::get(&map[&msg.get], path).unwrap();
                new = xmledit::set(&new, path, &old).unwrap();
            }
            if msg == MOTION {
                // Cameras refuse the whole document read back from 46 and merge the partial one.
                if new.contains("<timeBlockList") || new.contains("<handleException") || new.contains("<sensitivityInfoList") {
                    return Err("Msg of type 47 returned code 400".into());
                }
                let mut merged = map[&msg.get].clone();
                for t in ["enable", "width", "height", "scope", "sensInfoNew"] {
                    if let (Some(old), Some(part)) = (element(&merged, t).map(str::to_string), element(&new, t)) {
                        merged = merged.replacen(&old, part, 1);
                    }
                }
                new = merged;
            }
            if self.masks_echo {
                new = new.replace("ShelterList", "shelterList").replace("<channelId>0</channelId>", "<channelId>0</channelId>\n<maxNum>8</maxNum>");
            }
            map.insert(msg.get, new);
            Ok(())
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
        fn writable(&self, msg: Msg) -> bool {
            !self.refused.lock().unwrap().contains(&msg.get)
        }
    }

    const LEDS_XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<body>\n<LedState version=\"1.1\">\n<channelId>0</channelId>\n<ledVersion>0</ledVersion>\n<IRLedBrightness>80</IRLedBrightness>\n<state>close</state>\n<lightState>open</lightState>\n</LedState>\n</body>\n";
    const STREAMS_XML: &str = "<body><StreamInfoList version=\"1.1\"><StreamInfo><channelBits>1</channelBits><encodeTable><type>mainStream</type><framerateTable>15,12,10</framerateTable><bitrateTable>1024,1536,2048,3072,4096</bitrateTable></encodeTable><encodeTable><type>subStream</type><framerateTable>15,10</framerateTable><bitrateTable>256,512,1024</bitrateTable></encodeTable></StreamInfo></StreamInfoList></body>";
    const ENCODE_XML: &str = "<body><Compression version=\"1.1\"><mainStream><resolutionName>5120*1440</resolutionName><frame>15</frame><bitRate>4096</bitRate></mainStream><subStream><resolutionName>1536*432</resolutionName><frame>15</frame><bitRate>1024</bitRate></subStream></Compression></body>";
    const GENERAL_XML: &str = "<body><SystemGeneral version=\"1.1\"><timeZone>25200</timeZone><osdFormat>MDY</osdFormat><year>2026</year><month>10</month><day>24</day><hour>13</hour><minute>17</minute><second>47</second><timeFormat>1</timeFormat><deviceName>Backyard</deviceName></SystemGeneral></body>";

    fn ones(n: usize) -> String {
        "1".repeat(n)
    }

    fn schedule_xml(root: &str) -> String {
        format!(
            "<body><{root} version=\"1.1\"><channelId>0</channelId><enable>1</enable><typeScheduleList><item><type>people</type><valueTable>{}</valueTable></item><item><type>vehicle</type><valueTable>{}</valueTable></item></typeScheduleList></{root}></body>",
            ones(168), ones(168)
        )
    }

    /// 96x64 scope with an 80x22 grid, every grid cell on (like the cameras' default).
    fn md_xml() -> String {
        let mut bytes = vec![0u8; 96 * 64 / 8];
        for r in 0..22 {
            for c in 0..80 {
                let n = r * 96 + c;
                bytes[n / 8] |= 0x80 >> (n % 8);
            }
        }
        let table = base64::engine::general_purpose::STANDARD.encode(&bytes);
        format!(
            "<body><MD version=\"1.1\"><channelId>0</channelId><enable>1</enable><width>80</width><height>22</height><scope><columns>96</columns><rows>64</rows><valueTable>{table}</valueTable></scope><sensitivityInfoList><sensitivityInfo><id>0</id><sensitivity>10</sensitivity><beginHour>0</beginHour><beginMinute>0</beginMinute><endHour>6</endHour><endMinute>0</endMinute></sensitivityInfo></sensitivityInfoList><sensInfoNew><sensitivityDefault>10</sensitivityDefault><sensInfoList><sensInfo><priority>0</priority><sensitivity>10</sensitivity><beginHour>0</beginHour><beginMinute>0</beginMinute><endHour>6</endHour><endMinute>0</endMinute></sensInfo></sensInfoList></sensInfoNew><timeBlockList /><handleException><handleType>none</handleType></handleException></MD></body>"
        )
    }

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    fn changes(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect()
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
        let r = run(write_page(&cam, p, &changes(&[("ir_bright", "55"), ("ir", "auto")])));
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
            let r = run(write_page(&cam, p, &changes(&[(k, v)])));
            assert!(!r[0].ok, "{k}={v} should be rejected");
        }
        assert!(cam.sets.lock().unwrap().is_empty());
    }

    #[test]
    fn reports_when_camera_ignores_a_write() {
        let mut cam = Fake::new(&[(208, LEDS_XML)]);
        cam.refuse_writes_to = Some("LedState/IRLedBrightness");
        let r = run(write_page(&cam, page("lights").unwrap(), &changes(&[("ir_bright", "10")])));
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
    fn schedules_read_and_write_one_row() {
        let cam = Fake::new(&[(81, &schedule_xml("Record"))]);
        let p = page("schedules").unwrap();
        let v = run(read_page(&cam, p));
        let s = v.fields.iter().find(|f| f.key == "rec_sched").unwrap();
        assert_eq!(s.display, "people 168/168 h, vehicle 168/168 h");
        // Turn off Sunday 00:00-01:00 for vehicles only.
        let mut map: BTreeMap<String, String> = serde_json::from_str(&s.value).unwrap();
        map.get_mut("vehicle").unwrap().replace_range(0..1, "0");
        let r = run(write_page(&cam, p, &changes(&[("rec_sched", &serde_json::to_string(&map).unwrap())])));
        assert!(r[0].ok, "{r:?}");
        let xml = cam.xml.lock().unwrap()[&81].clone();
        assert_eq!(xmledit::get_all(&xml, "Record/typeScheduleList/item/valueTable"), [ones(168), format!("0{}", ones(167))]);
        // Malformed schedules are refused before sending.
        let bad = run(write_page(&cam, p, &changes(&[("rec_sched", "{\"people\":\"10\"}")])));
        assert!(!bad[0].ok);
    }

    #[test]
    fn motion_zone_roundtrips_bits_msb_first() {
        let cam = Fake::new(&[(46, &md_xml())]);
        let p = page("motion").unwrap();
        let v = run(read_page(&cam, p));
        let zone = v.fields.iter().find(|f| f.key == "md_zone").unwrap();
        let mut z: ZoneValue = serde_json::from_str(&zone.value).unwrap();
        assert_eq!((z.w, z.h), (80, 22));
        assert!(z.cells.chars().all(|c| c == '1'));
        assert_eq!(v.fields.iter().find(|f| f.key == "md_sens").unwrap().value, "10");
        // Clear the first cell of the second row; it must land at bit 96 (byte 12, high bit).
        z.cells.replace_range(80..81, "0");
        let r = run(write_page(&cam, p, &changes(&[("md_zone", &serde_json::to_string(&z).unwrap()), ("md_sens", "25")])));
        assert!(r.iter().all(|r| r.ok), "{r:?} (the fake refuses whole documents like the cameras)");
        let xml = cam.xml.lock().unwrap()[&46].clone();
        let bytes = base64::engine::general_purpose::STANDARD.decode(xmledit::get(&xml, "MD/scope/valueTable").unwrap()).unwrap();
        assert_eq!(bytes[12], 0x7f);
        assert_eq!(bytes[11], 0x00, "cells outside the 80-wide grid stay as they were");
        assert_eq!(xmledit::get(&xml, MD_SENS_DEFAULT).as_deref(), Some("25"));
        assert_eq!(xmledit::get(&xml, "MD/sensInfoNew/sensInfoList/sensInfo[0]/sensitivity").as_deref(), Some("25"), "periods follow the default");
        assert_eq!(xmledit::get(&xml, "MD/sensitivityInfoList/sensitivityInfo[0]/sensitivity").as_deref(), Some("10"), "legacy list untouched");
        assert!(!run(write_page(&cam, p, &changes(&[("md_sens", "51")])))[0].ok, "official range is 1-50");
    }

    #[test]
    fn motion_writes_send_only_the_changed_groups() {
        let edited = md_xml();
        let field = |k: &str| page("motion").unwrap().fields.iter().find(|f| f.key == k).unwrap();
        let v = String::new();
        let doc = outgoing(MOTION, edited.clone(), &[(field("md_sens"), &v)]).unwrap();
        assert!(doc.contains("<channelId>0</channelId>") && doc.contains("<sensInfoNew>"), "{doc}");
        for absent in ["<enable>", "<scope>", "<timeBlockList", "<handleException>", "<sensitivityInfoList>"] {
            assert!(!doc.contains(absent), "{absent} in {doc}");
        }
        let doc = outgoing(MOTION, edited, &[(field("md_zone"), &v), (field("md_on"), &v)]).unwrap();
        assert!(["<enable>1</enable>", "<width>80</width>", "<height>22</height>", "<valueTable>"].iter().all(|t| doc.contains(t)), "{doc}");
        assert!(!doc.contains("sensInfoNew"));
    }

    #[test]
    fn refused_writes_become_read_only_for_that_camera() {
        let mut cam = Fake::new(&[(46, &md_xml())]);
        cam.refuse_msg = Some(46);
        let p = page("motion").unwrap();
        let r = run(write_page(&cam, p, &changes(&[("md_sens", "20")])));
        assert!(!r[0].ok && r[0].error.as_deref().unwrap().contains("doesn't allow changing"), "{r:?}");
        let v = run(read_page(&cam, p));
        assert!(v.fields.iter().all(|f| f.kind == "info"), "motion fields shown read-only after the refusal");
        assert_eq!(v.fields.iter().find(|f| f.key == "md_zone").unwrap().display, "1760 of 1760 areas active");
    }

    #[test]
    fn plain_time_tables_edit_like_typed_schedules() {
        let xml = format!("<body><PowerSaving version=\"1.1\"><enable>0</enable><timeTable>{}</timeTable></PowerSaving></body>", "0".repeat(168));
        let cam = Fake::new(&[(423, &xml)]);
        let p = page("power").unwrap();
        let v = run(read_page(&cam, p));
        let sched = v.fields.iter().find(|f| f.key == "ps_sched").unwrap();
        assert_eq!(sched.kind, "schedule");
        let mut m: BTreeMap<String, String> = serde_json::from_str(&sched.value).unwrap();
        m.insert("hours".into(), format!("{}{}", "1".repeat(8), "0".repeat(160)));
        let r = run(write_page(&cam, p, &changes(&[("ps_sched", &serde_json::to_string(&m).unwrap()), ("ps_on", "1")])));
        assert!(r.iter().all(|r| r.ok), "{r:?}");
        let after = cam.xml.lock().unwrap()[&423].clone();
        assert!(xmledit::get(&after, "PowerSaving/timeTable").unwrap().starts_with("111111110"));
        assert_eq!(xmledit::get(&after, "PowerSaving/enable").as_deref(), Some("1"));
    }

    #[test]
    fn privacy_masks_use_the_sdk_layout() {
        let read = "<body><Shelter version=\"1.1\"><channelId>0</channelId><enable>0</enable><maxNum>8</maxNum><shelterList /></Shelter></body>";
        let mut cam = Fake::new(&[(52, read)]);
        cam.masks_echo = true;
        let p = page("privacy").unwrap();
        let v = run(read_page(&cam, p));
        assert_eq!(v.fields[0].value, r#"{"enable":"0","max":8,"rects":[]}"#);
        let want = r#"{"enable":"1","max":8,"rects":[[100,200,300,150]]}"#;
        let r = run(write_page(&cam, p, &changes(&[("masks", want)])));
        assert!(r[0].ok, "{r:?}");
        let xml = cam.xml.lock().unwrap()[&52].clone();
        // x<<16 | reference, exactly as the SDK packs it; 8 entries, the unused ones disabled.
        assert!(xml.contains(&format!("<topLeftX>{}</topLeftX>", 100 << 16 | 1000)));
        assert_eq!(xmledit::get_all(&xml, "Shelter/shelterList/Shelter/enable").len(), 8);
        // Out-of-frame or too many areas are refused.
        assert!(!run(write_page(&cam, p, &changes(&[("masks", r#"{"enable":"1","max":8,"rects":[[900,0,200,10]]}"#)])))[0].ok);
    }

    #[test]
    fn passwords_are_never_shown_and_blank_keeps_them() {
        let ftp = "<body><Ftp version=\"1.1\"><server>ftp.example.com</server><port>21</port><nonymous>0</nonymous><remoteDir> </remoteDir><userName>cam</userName><password>s3cret</password></Ftp></body>";
        let cam = Fake::new(&[(68, ftp)]);
        let p = page("ftp").unwrap();
        let v = run(read_page(&cam, p));
        let pw = v.fields.iter().find(|f| f.key == "ftp_pass").unwrap();
        assert_eq!((pw.kind, pw.value.as_str(), pw.display.as_str()), ("password", "", ""));
        assert!(!serde_json::to_string(&v).unwrap().contains("s3cret"));
        // Blank = unchanged: nothing is written at all.
        assert!(run(write_page(&cam, p, &changes(&[("ftp_pass", "")]))).is_empty());
        assert!(cam.sets.lock().unwrap().is_empty());
        let r = run(write_page(&cam, p, &changes(&[("ftp_pass", "n3w"), ("ftp_dir", "cams/front")])));
        assert!(r.iter().all(|r| r.ok), "{r:?}");
        assert!(!run(write_page(&cam, p, &changes(&[("ftp_dir", "bad dir!")])))[0].ok, "official path rule");
    }

    #[test]
    fn network_rejects_settings_that_would_strand_the_camera() {
        let net = "<body><Dhcp version=\"1.1\"><enable>1</enable></Dhcp><AutoDns version=\"1.1\"><enable>1</enable></AutoDns><Ip version=\"1.1\"><ip>192.168.1.254</ip><mask>255.255.255.0</mask><mac>00:00:00:00:00:00</mac><gateway>192.168.1.1</gateway></Ip><Dns version=\"1.1\"><dns1>9.9.9.9</dns1><dns2> </dns2></Dns></body>";
        let cam = Fake::new(&[(76, net)]);
        let p = page("network").unwrap();
        for bad in [
            vec![("dhcp", "0"), ("net_gw", "10.0.0.1")],
            vec![("dhcp", "0"), ("net_mask", "255.0.255.0")],
            vec![("dhcp", "0"), ("net_ip", "192.168.1.255")],
            vec![("net_ip", "300.1.1.1")],
        ] {
            let r = run(write_page(&cam, p, &changes(&bad)));
            assert!(r.iter().all(|r| !r.ok), "{bad:?} should be refused");
        }
        assert!(cam.sets.lock().unwrap().is_empty());
        let r = run(write_page(&cam, p, &changes(&[("dhcp", "0"), ("net_ip", "192.168.1.60"), ("dns2", "")])));
        assert!(r.iter().all(|r| r.ok), "{r:?}");
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
        let mut keys = std::collections::HashSet::new();
        for p in PAGES {
            for f in p.fields {
                assert!(keys.insert((p.id, f.key)), "duplicate key {}", f.key);
                if !matches!(f.kind, Kind::Info(_)) {
                    assert!(f.msg.set.is_some(), "{} is editable but has no write id", f.key);
                }
            }
        }
    }
}
