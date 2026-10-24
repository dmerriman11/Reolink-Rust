# Settings inventory (official client 8.21.31 → Reolink-rs)

How this was established, so later phases can extend it without redoing the work.

## Sources

| Source | What it gave | Reference |
|---|---|---|
| REA `analyze-javascript-application` on `remoteConfig` + `wifiSettings` bundles | 93 camera-settings routes (one per settings page) | Evidence `ev_cf222709726d0b3254e4ae1937608d69c6b649ea4d7b3513a884260f9463684b` |
| REA on `localSettings` bundle | Shell only; settings components live in shared chunks (minified webpack names nothing) | Evidence `ev_864c8ed9e073f0d88508179616202a526ef84770a8f8c7dedcbe81a147dc4362` |
| English locale chunk (`976.js`, 7145 strings) | Labels for every page and field | `localSettings_*`, `remoteConfig_*` keys |
| Main process `loadSettings` (`220.js`) | Client settings applied at startup | volume, hardware acceleration, auto-add devices, alarm beep, local record folder/schedule |
| neolink `dissector/baichuan.lua` + `messages.md` | Baichuan message ids and captured XML (read and write) | Write payload = the read's `<body>` (verified for msg 25/26) |
| `examples/settings_dump.rs` against both Argus 4 Pro cameras | What these cameras actually answer | 38 readable messages each |

REA limitation: the semantic graph keeps almost no string literals for minified bundles, so field-level
inventory comes from the locale strings and from the cameras' own XML, not from the REA graph.

## Camera settings (Argus 4 Pro, firmware v3.0.0.4978)

Readable: 26 VideoInput, 37 ServerPort, 38 Ntp, 40 Ddns, 42 Email, 44 Osd, 46 MotionDetect, 52 Shelter,
54 RecordCfg, 56 Compression, 68 Ftp, 70 FtpTask, 76 Ip, 80 VersionInfo, 81 RecordSchedule, 93 LinkType,
97 Upnp, 101 AutoReboot, 102 HddInfo, 104 SystemGeneral, 106 Dst, 114 Uid, 115 WifiSignal, 116 Wifi,
120 OnlineUsers, 122 Performance, 132 VideoInputAdv, 146 StreamInfoList, 199 Support, 208 LedState,
212 rfAlarmCfg (PIR), 217 EmailTask, 219 PushTask, 232 AudioTask, 253 BatteryInfo, 299 AiCfg, 319 Timelapse.

Rejected (400/405, not supported by this model): 50, 58, 78, 83, 124, 133, 190, 228, 252, 291, 342; 287 empty.

Writes are implemented only where the write id is confirmed by captured official-client traffic
(dissector "(write)" entries): 25, 39, 45, 55, 57, 100, 105, 107, 209, 213, 216, 218, plus reboot (23).
Unconfirmed write ids (Wi-Fi 116, AudioTask 232, AiCfg 299, Timelapse 319) are shown read-only.

### Implemented camera settings (`src/settings.rs`)

| Page | Fields | Messages |
|---|---|---|
| Device info | model, firmware, hardware, build, serial, UID, IP, MAC, Wi-Fi signal, battery %, charging, power source, battery temperature, SD size/free, camera CPU | 80, 114, 76, 115, 253, 102, 122 (read-only) |
| Display | camera name on video, show name, show date/time, watermark, brightness, contrast, saturation, hue, sharpness, day/night, anti-flicker | 44/45, 26/25 |
| Stream | Clear/Fluent resolution (read-only), frame rate and bitrate (options from the camera's StreamInfoList), audio per stream | 56/57, 146 |
| Detection & alerts | PIR on, PIR sensitivity 1–100, reduce false alarms, push on, email on, SD retention (official labels), post-motion record time | 212/213, 219/218, 217/216, 54/55 |
| Lights | infrared lights auto/on/off, infrared brightness 0–100, status LED | 208/209 |
| System | device name, time zone, date format, time format, NTP on/server/interval, DST on, login lock; "sync time with this PC" | 104/105, 38/39, 106/107 |
| Maintenance | automatic reboot on/day/hour; "reboot camera" | 101/100, 23 |

Every write is validated against the field's domain, applied read-modify-write, then read back; a value
the camera didn't keep is reported per field. Verified on both cameras with `examples/settings_check.rs --write`
and through the app's IPC.

Not yet covered (official pages): email/FTP server settings and passwords, users, network (DDNS, ports, IP),
recording/alarm schedules, privacy mask, motion zones, AI detection, siren/audio alarm, time-lapse, Wi-Fi network.

## Client (local) settings

| Official | Reolink-rs |
|---|---|
| Save captured images to / Record videos to | Implemented (separate folders) |
| Video file duration | Implemented (recordings split into segments) |
| Folder space warning threshold | Implemented (warns when free space is low) |
| Run at startup | Implemented (per-user Run key) |
| Hardware acceleration | Implemented (GPU decode toggle) |
| Auto-add devices (LAN search loop) | Implemented as "scan network for cameras" toggle |
| Download path | Not applicable until playback/download exists |
| Alarm beep, volume | Not applicable until alarms/live audio exist |
| Auto login, automatic client update | Not applicable (no Reolink account or update server) |
| Local record schedule, record stream type, folder size cap | Not implemented: needs always-on streams (drains battery cameras) or deleting user files |
