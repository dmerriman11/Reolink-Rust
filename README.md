# Reolink-rs

Rust rewrite of the Reolink desktop client (reference: Reolink 8.21.31, Electron), built on
[`neolink_core`](https://github.com/QuantumEntangledAndy/neolink) for the Baichuan protocol and a Tauri 2 UI
(plain HTML/CSS/JS in `ui/`, no Node build). Video reaches the WebView as MJPEG from a token-protected
127.0.0.1 server (`src/stream.rs`); recordings are stream copies at full camera resolution.

**Deliberate difference from the official client:** no battery/idle disconnect. Every device stays
connected (pinged every 5 s) and reconnects automatically, resuming its live view and recording.

`neolink_core/` is a vendored copy (upstream rev 6e05e78) with Windows fixes:
- UDP sockets disable `SIO_UDP_CONNRESET`; otherwise one ICMP "port unreachable" kills discovery with error 10054.
- Stream and connection task errors are surfaced/logged instead of a bare `StreamFinished`.
- Unparseable frames, stray extension payloads and bad UDP datagrams are skipped instead of dropping the connection.
- `get_xml`/`set_xml` raw settings access (read-modify-write keeps fields the typed model doesn't know).

Test a saved camera end to end (stream, decode, ~8 s recording): `cargo run --example probe -- "<device name>" [main|sub]`.
Measure each saved camera's video/audio bitrate per stream: `cargo run --release --example bitrate`.
Validate camera settings (add `--write` for reversible write checks): `cargo run --release --example settings_check`.
Dump every readable settings message as XML (passwords redacted): `cargo run --release --example settings_dump -- <dir>`.

## Run

Requires `ffmpeg.exe` on PATH or next to the exe (used to decode H.264/H.265 and write MP4s).

```bash
cargo run --release
```

Devices are saved to `%APPDATA%\Reolink-rs\devices.json`. Snapshots and recordings go to `%USERPROFILE%\Videos\Reolink-rs`.

## Status

Phase 1 (done): add/edit/remove devices (IP or UID, NVR channel), 1 / 2-stacked / 4 / 9 / 16 live layouts,
Clear/Fluent streams, double-click a tile for single view (Esc to exit), collapsible PTZ panel (hold to move),
snapshot, MP4 recording, battery level.

Phase 2 (done): camera settings (device info, display, stream, detection & alerts, lights, system,
maintenance) and app settings (folders, recording segments, disk-space warning, GPU decoding, network scan,
run at startup). Inventory and protocol notes: [docs/settings-inventory.md](docs/settings-inventory.md).

Next phases toward parity with the official client:
1. Live audio and two-way talk; PTZ presets/zoom/focus
2. Playback: calendar, timeline, SD/NVR recording search and download
3. LAN device discovery and auto-detected NVR channels
4. Remote config pages (display, encoding, recording, alarms, network, users, firmware update)
5. Alarm/motion notifications and event log
6. Local settings, language packs, file encryption tools

## Patching the official client instead

`patch/patch_battery_timeout.py` removes the same disconnect from the installed Electron app.
Run it from an elevated shell; re-run after every Reolink update.

## License

AGPL-3.0-or-later (inherited from `neolink_core`).
