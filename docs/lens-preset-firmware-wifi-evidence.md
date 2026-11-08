# Lens, presets, firmware and Wi-Fi parity evidence

Inspected 2026-11-07. No camera connections or commands were made during this work.
Source: installed `C:/Program Files/Reolink/resources/app` (client 8.21.31).
`libBCSDKWrapper.dll` SHA-256:
`8d5ae81fd21acb52ed8b5d4e67452ac50b255999748ca7eb2bb6b06c09becf89`.
Addresses below are virtual addresses at image base `0x180000000`.
Disassembly was read statically using pefile/capstone, without loading/executing the DLL.

## Implemented: optical lens controls

- Official SDK command 2108 maps to Baichuan **18** in the DLL table at
  `0x180576ba4`, independently checked against this installed binary.
- XML builder `0x1800eaee0` emits `PtzControl version="1.1"`, `channelId`,
  `speed`, `command`. Branches at `0x1800eb041/04a/053/05c` emit
  `zoomInc`, `zoomDec`, `focusInc`, `focusDec`; stop is at `0x1800eafdc`.
- `220.js`, `focusFar` wrapper uses `PTZ_FOCUSDEC`; `focusNear` uses
  `PTZ_FOCUSINC`. Do not infer near/far from increment/decrement names.
- `BCSDK_GetSupportZoomAndFocus` at `0x18001ac10` calls `0x1800605a0`.
  Its type checks admit 1, 2, 4, 5 and reject 0 and 3. This implementation
  additionally requires channel Support and readable positive zoom/focus ranges
  from message 294, and fails closed when either query fails. Older lens cameras
  without a readable range may therefore not expose controls.
- Uses existing held PTZ input, stop-on-release/cancel/blur, per-device worker,
  and live-media draining. Capabilities are also checked in the worker.
- Autofocus is NOT added: automatic-focus configuration and the PTZ `auto`
  operation must not be confused with a one-shot focus action. The one-shot
  action's payload/capability has not been confirmed.

## Implemented: preset rename/delete

- Existing captured-protocol message **19** / `PtzPreset` is reused.
- Official `js/livePTZ-6e7966c982e3d6af.js` has
  `iPtzCmdRecord:{add:PTZ_SETPOS,move:PTZ_TOPOS,delete:PTZ_DELPOS,modify:-1}`.
  `handlePreset` sends a single preset through `setPresets`.
- DLL builder `0x18011aa20`, branches `0x18011ab6b..0x18011aba6`:
  SDK command 15 emits `setPos`, 16 emits `toPos`, 17 emits `delPos`;
  other values (including rename -1) OMIT the command element.
  Rename must not call setPos, which would overwrite the stored position.
- Same builder rejects IDs above 63 (`0x18011ab3e`) and handles 64 entries.
  Allocation now uses 0..63; the old 1..64 allocation could send invalid slot 64.
- Name storage occupies 32 bytes in the SDK struct; input is limited to 31 UTF-8
  bytes, with empty/control-character names rejected. Serde escapes XML names.
- UI deletion requires confirmation; rename's prompt explicitly preserves the
  position. Existing preset IDs must be present before either write.

## Blocked: manual firmware update

The native table confirms SDK UPGRADE 2091 maps to **67** and also **411**
(records `0x180576d84`, `0x18057793c`). An ID alone does not establish which
transport path is appropriate. `220.js` calls `BCSDK_RemoteUpgradeFirmware`
with a native struct containing source path, upgrade-config flag, file/current/
send/packet sizes, channel bitmap, device type, strategy and ack counters.
Progress comes from SDK callbacks, not a proven standalone XML response.

Still needed: confirmed prepare/chunk/ack/finalize sequence and payloads,
PAK header and hardware/model/version validation, file size limits, battery
admission rule, and post-reboot success verification. No firmware picker,
validator, command, mock upload, or upload path was added: exposing a guessed
flow would violate the task's safety rule. The existing E1 firmware capture
in `neolink_core/src/bc/samples/e1_firmwareupgrade.pcapng` may help future work
but was not decoded/validated in this task.

## Blocked: Wi-Fi switching

Independently verified native-table mappings:
- GET_WIFI_INFO 2073 -> **116**, record `0x180576f94`.
- SET_WIFI_INFO 2074 -> **117**, record `0x180576fa0`.
- GET_SCAN_AP 2135 -> **198**, record `0x180577108`.

Official `remoteConfig-6e7966c982e3d6af.js` requests
`getWifiCfg({bScanAp:true})`; `220.js` wraps `BCSDK_RemoteGetWifiCfg`,
`BCSDK_RemoteSetWifiCfg` and `BCSDK_RemoteWifiTest`. UI scan results contain
SSID (`udid`), band/type, protocol, encryption, and signal. It is not safe to
assume `bScanAp` is a wire XML field or that the Wi-Fi test is read-only.

Still needed: trace scan request orchestration and exact set/test XML,
password/encryption/band representation and reconnect behavior. No scan,
Wi-Fi test or Wi-Fi write was sent, and no switching path was added.

## Hardware acceptance still required

On a suitable optical PTZ camera: ability visibility, hold/release/blur stop,
zoom direction and focus near/far; rename after moving elsewhere must retain
the old stored position; delete only a disposable preset and refresh/reconnect
to verify persistence. Do not test firmware/Wi-Fi writes on the user's Argus.

## Verification

- `node --test tests/*.test.js`: 38 passed.
- `cargo test -p neolink_core --lib lens_preset_tests`: 2 passed.
- `cargo test --all-targets`: main binary 62 passed, 2 failed because `ffprobe`
  was not on PATH (`src/media.rs:598`).
- `cargo test --all-targets -- --skip media::tests::h264_recording_keeps_camera_time
  --skip media::tests::h265_recording_keeps_camera_time`: passed, including example
  test targets. No examples were run as camera probes.
- Browser/native UI interaction and hardware behavior have not been tested.
- `cargo build --release`: passed (3m 05s); output `target/release/reolink-rs.exe`.
