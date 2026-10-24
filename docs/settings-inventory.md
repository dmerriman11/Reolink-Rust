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
| REA/Ghidra on `bcsdk.dll` | Command table (msg id ↔ SDK command) and XML builders for masks/motion | [baichuan-command-ids.md](baichuan-command-ids.md) |

REA limitation: the semantic graph keeps almost no string literals for minified bundles, so field-level
inventory comes from the locale strings and from the cameras' own XML, not from the REA graph.

## Camera settings (Argus 4 Pro, firmware v3.0.0.4978)

Readable: 26 VideoInput, 37 ServerPort, 38 Ntp, 40 Ddns, 42 Email, 44 Osd, 46 MotionDetect, 52 Shelter,
54 RecordCfg, 56 Compression, 68 Ftp, 70 FtpTask, 76 Ip, 80 VersionInfo, 81 RecordSchedule, 93 LinkType,
97 Upnp, 101 AutoReboot, 102 HddInfo, 104 SystemGeneral, 106 Dst, 114 Uid, 115 WifiSignal, 116 Wifi,
120 OnlineUsers, 122 Performance, 132 VideoInputAdv, 146 StreamInfoList, 199 Support, 208 LedState,
212 rfAlarmCfg (PIR), 217 EmailTask, 219 PushTask, 232 AudioTask, 253 BatteryInfo, 299 AiCfg, 319 Timelapse.

Rejected (400/405, not supported by this model): 50, 58, 78, 83, 124, 133, 190, 228, 252, 291, 342; 287 empty.

### Write ids: from the SDK, not guesses

`bcsdk.dll` (shipped with 8.21.31) holds the full command table: 454 records of (Baichuan msg id, `E_BC_CMD`)
in `.data` at `0x180576afc–0x18057836c`, decoded with REA/Ghidra (the DLL bit was cleared in an analysis copy
so Ghidra loads it as an executable). Full table: [baichuan-command-ids.md](baichuan-command-ids.md). It agrees
with the neolink dissector everywhere both have an entry, with one exception: the SDK labels auto-reboot
GET 100 / SET 101, but the cameras answer the config on 101, so the app reads 101 and writes 100 (dissector order).

Field formats come from the SDK's XML builders, not from guesses:
- Privacy masks (`FUN_1801185b0`): `<Shelter>` with `topLeftX = x<<16 | refW` (likewise Y/width/height); the
  camera answers in lowercase `<shelterList>`, max 8 areas. The app edits on a 0..1000 reference frame.
- Motion area (`FUN_18014e120`, `FUN_1801a69b0`): 80×22 grid on a 96×64 canvas, row-major, MSB-first bits, base64.
- Schedules: 7×24 `0/1` tables per alarm type (record, push, email, FTP, siren), Sunday first.
- Official ranges/validators from the client: motion sensitivity 1–50, PIR 1–100, IR brightness 0–100, image
  0–255; DDNS charset `` /^[^\\\/'"`^$?;<>|& ]*$/ ``; FTP path `/^[a-zA-Z0-9\\\/]*$/`; retention 1 = overwrite,
  0 = keep, −N = delete after N days; Wi-Fi reports a −10 dBm placeholder for ~5 s after connecting (ignored).

Write ids confirmed on both cameras (no-op write of every field + reversible change, read back):
25, 36, 39, 41, 43, 45, 53, 55, 57, 69, 71, 77, 82, 98, 100, 105, 107, 209, 213, 216, 218, 231, 300, 320, and
reboot (23, timed below). Email/FTP test: 141/194.

**Motion detection (47): the official client never sends the whole `<MD>` document.** Argus 4 Pro firmware
v3.0.0.4978 answers 400 to the full document read back from 46 (it carries an empty `<timeBlockList/>` and
`<handleException>` the camera doesn't take as input). Traced in both layers:
- JS (REA `analyze_javascript_application` over the remoteConfig/localSettings/wifiSettings bundles, evidence
  `ev_0033c64c57af7836fc8931fd97148f81aeaf680d4add1988c0f48e7d58987c3a`; 3 files, 426,729 AST nodes, application
  coverage complete): `alarmArea.vue` saves through `setMotionAlarmArea` (or `setAlarmAreas` on AI-config
  devices), `motionSensitivity.vue` through `setMotionSensitivityCfg`; `isNewSensitivity` is `iSensVerion != 0`,
  so cameras that report `<sensInfoNew>` are edited there (default sensitivity + optional periods).
- SDK (`FUN_18014e120`, Ghidra via REA): builds `<MD version="1.1"><channelId>` plus only the groups present in
  the request: `enable`/`usepir`, `width`/`height`/`scope`, `timeBlockList`, `handleException`, and *either*
  `sensInfoNew` *or* `sensitivityInfoList`. Msg 345 (`FUN_1801bcbd0`) is the AI variant:
  `<AlarmArea version="1.1"><chn>N</chn><mdArea|personArea|vehicleArea|…><width/><height/><area>base64</area>`
  with a raw width×height bitmap.

The app now sends exactly that partial document (`settings::outgoing`): channelId plus the changed groups.
Sensitivity is the `sensInfoNew` default (1–50); changing it also sets every period, like the official control.
If a camera still refuses a write, its fields turn read-only for that camera with an explanation
(`settings::Camera::writable`).

An earlier hand-made enable-only `<MD>` probe made one camera drop off Baichuan for ~20 minutes (it answered
ping, came back on its own, config unchanged). Live-camera validation of the new partial writes therefore waits
for the owner's go-ahead (`examples/settings_check.rs --write` covers motion).

Intentionally read-only: Wi-Fi network switching (116/117: a wrong value strands a battery camera), users
(58 rejected), AI detection config (342/344 rejected without a request payload).

### Implemented camera settings (`src/settings.rs`, 14 pages, 119 fields per camera)

| Page | Fields | Messages |
|---|---|---|
| Device info | model, firmware, hardware, build, serial, UID, IP, MAC, Wi-Fi signal, battery %, charging, power source, battery temperature, SD size/free, camera CPU | 80, 114, 76, 115, 253, 102, 122 (read-only) |
| Display | camera name on video, show name, show date/time, watermark, brightness, contrast, saturation, hue, sharpness, day/night, anti-flicker | 44/45, 26/25 |
| Stream | Clear/Fluent resolution (read-only), frame rate and bitrate (options from StreamInfoList), audio per stream | 56/57, 146 |
| Detection & recording | PIR on, PIR sensitivity, reduce false alarms, push on, email on, SD retention, post-motion record time | 212/213, 219/218, 217/216, 54/55 |
| Motion detection | on, detection area (painted grid over a live snapshot), sensitivity | 46/47 (partial documents, see above) |
| Privacy masks | on, up to 8 rectangles drawn over a live snapshot | 52/53 |
| Schedules | record on + schedule, push / email / FTP schedules, siren on + schedule (7×24 paint grid per alarm type) | 81/82, 219/218, 217/216, 70/71, 232/231 |
| Email | SMTP server/port/SSL, user, password (write-only), sender name, 3 recipients, attachment; "Send test email" | 42/43, 141 |
| FTP | on, server, port, anonymous, user, password (write-only), remote folder; "Test FTP" | 68/69, 194 |
| Network | DHCP, IP/mask/gateway (validated as a consistent subnet), DNS, UPnP, server/HTTP/HTTPS/RTSP/RTMP/ONVIF ports, DDNS | 76/77, 37/36, 97/98, 40/41 |
| Smart detection & time-lapse | AI detection types (read-only), time-lapse overwrite, max storage | 299/300, 319/320 |
| Lights | infrared auto/on/off, infrared brightness, status LED | 208/209 |
| System | device name, time zone, date/time format, NTP, DST, login lock; "sync time with this PC" | 104/105, 38/39, 106/107 |
| Maintenance | automatic reboot on/day/hour; "reboot camera" | 101/100, 23 |

Passwords are never sent to the UI: the field is blank, and blank means "keep the current password".

Every write is validated against the field's domain, applied read-modify-write, then read back; a value the
camera didn't keep is reported per field. Pages read their messages concurrently.

### Validation (both cameras, `examples/settings_check.rs --write`)

- Read: 14 pages / 119 fields per camera; 40–85 ms per page, network ~260 ms, first contact (info) ~1.9 s.
- No-op write of every writable field: PASS on 13/14 pages; motion reported READ-ONLY (refused, see above).
- Reversible change, verify, restore, verify: PASS for push schedule, privacy mask add/remove, time-lapse
  overwrite, FTP port, email sender name, IR brightness.
- Reboot (`examples/reboot_check.rs`): connection dropped 12 s after the command, back online and streaming-ready
  18 s after it, with no user action; settings readable on both cameras afterwards.

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
| Alarm beep, volume | Implemented: beep on each motion start reported by any camera (motion events, msg 31/33), volume 0–100 |
| Auto login, automatic client update | Not applicable (no Reolink account or update server) |
| Local record schedule | Implemented: off / on motion (plus 30 s after it stops) / continuous, within a 7×24 schedule; records the Clear stream without decoding when no tile shows the camera. Verified on Frontyard (`examples/auto_record_check.rs`): recording in 0.6 s, stopped in 0.4 s |
| Record stream type, folder size cap | Not implemented: Clear stream is always recorded; a size cap would delete user files |
