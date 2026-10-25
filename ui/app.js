"use strict";

const LAYOUTS = [
  { n: 1, cols: 1, rows: 1 },
  { n: 2, cols: 1, rows: 2 }, // two cameras stacked vertically
  { n: 4, cols: 2, rows: 2 },
  { n: 9, cols: 3, rows: 3 },
  { n: 16, cols: 4, rows: 4 },
];

const ICON = {
  camera: '<svg viewBox="0 0 24 24"><path d="M4 8h3l2-3h6l2 3h3v11H4z"/><circle cx="12" cy="13" r="3.5"/></svg>',
  rec: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="6"/></svg>',
  close: '<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>',
  wifi: '<svg viewBox="0 0 24 24"><path class="a3" d="M2.5 9.5a14 14 0 0 1 19 0"/><path class="a2" d="M5.5 13a9.5 9.5 0 0 1 13 0"/><path class="a1" d="M8.6 16.4a4.8 4.8 0 0 1 6.8 0"/><circle cx="12" cy="19.6" r="1.4" fill="currentColor" stroke="none"/></svg>',
  plus: '<svg viewBox="0 0 24 24"><rect x="3" y="5" width="18" height="14" rx="2"/><path d="M12 9v6M9 12h6"/></svg>',
};

const invoke = window.__TAURI__ ? window.__TAURI__.core.invoke : demoInvoke;
const $ = (sel) => document.querySelector(sel);
const el = (tag, cls, html) => { const e = document.createElement(tag); if (cls) e.className = cls; if (html) e.innerHTML = html; return e; };

let state = { devices: [], layout: 4, tiles: [], stream_base: "", token: "" };
let selected = null; // tile index, or null when no tile is highlighted
let solo = null;
let ptzActive = null;
const tiles = [];

const device = (id) => state.devices.find((d) => d.id === id);

async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    toast(String(e));
    throw e;
  }
}

// ---------- toast ----------
let toastTimer;
function toast(msg) {
  const t = $("#toast");
  t.textContent = msg;
  t.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("show"), 4500);
}

// ---------- layout picker ----------
function buildLayouts() {
  const box = $("#layouts");
  for (const l of LAYOUTS) {
    const b = el("button", "layout-btn");
    b.setAttribute("role", "radio");
    b.title = l.n === 2 ? "2 cameras, stacked" : `${l.n} cameras`;
    const g = el("span", "glyph");
    g.style.gridTemplateColumns = `repeat(${l.cols}, 1fr)`;
    g.style.gridTemplateRows = `repeat(${l.rows}, 1fr)`;
    for (let i = 0; i < l.n; i++) g.append(el("span"));
    b.append(g);
    b.onclick = () => { solo = null; call("set_layout", { layout: l.n }).then(refresh); };
    b.dataset.n = l.n;
    box.append(b);
  }
}

// ---------- device list ----------
let devicesKey = null;
function renderDevices() {
  const list = $("#device-list");
  $("#empty-hint").hidden = state.devices.length > 0;
  const visible = state.tiles.slice(0, state.layout);
  // Rebuild only on change: re-inserting rows every poll restarts their fade-in and flickers.
  const key = JSON.stringify([visible, state.devices.map((d) => [d.id, d.name, d.online, d.status, d.battery, d.charging, d.wifi_dbm])]);
  if (key === devicesKey) return;
  const firstRender = devicesKey === null;
  devicesKey = key;
  list.classList.toggle("settled", !firstRender);
  list.replaceChildren(...state.devices.map((d, i) => {
    const li = el("li", "device" + (visible.includes(d.id) ? " shown" : ""));
    li.style.animationDelay = `${i * 40}ms`;
    li.tabIndex = 0;
    li.title = d.status;
    li.append(el("span", "dot" + (d.online ? " on" : "")));
    li.append(Object.assign(el("span", "device-name"), { textContent: d.name }));
    const meta = el("span", "device-meta");
    if (d.online && d.wifi_level != null) {
      const w = el("span", `wifi lvl-${d.wifi_level}`, ICON.wifi);
      w.title = `Wi-Fi ${d.wifi_dbm} dBm`;
      w.setAttribute("aria-label", `Wi-Fi signal ${d.wifi_level} of 3 (${d.wifi_dbm} dBm)`);
      meta.append(w);
    }
    if (d.battery != null) {
      const b = el("span", "battery" + (d.battery <= 20 && !d.charging ? " low" : ""));
      const i = el("i");
      i.style.setProperty("--pct", d.battery);
      b.append(i, `${d.battery}%${d.charging ? " ⚡" : ""}`);
      meta.append(b);
    }
    li.append(meta);
    if (!d.online) li.append(Object.assign(el("span", "device-status"), { textContent: d.status }));
    const menu = el("span", "device-menu");
    const gear = Object.assign(el("button"), { textContent: "Settings" });
    gear.onclick = (e) => { e.stopPropagation(); openCameraSettings(d.id); };
    const edit = Object.assign(el("button"), { textContent: "Edit" });
    edit.onclick = (e) => { e.stopPropagation(); openDialog(d.id); };
    const del = Object.assign(el("button", "danger"), { textContent: "Remove" });
    del.onclick = (e) => {
      e.stopPropagation();
      // Rescan once the camera has dropped our session, so it shows up again under "Found nearby".
      if (confirm(`Remove ${d.name}?`)) call("remove_device", { id: d.id }).then(() => { refresh(); setTimeout(() => scanLan(), 1500); });
    };
    menu.append(gear, edit, del);
    li.append(menu);
    const show = () => {
      if (PB.on) { PB.cam = safeName(d.name); renderPlayback(); return; } // in playback, a device row picks its timeline
      const free = state.tiles.slice(0, state.layout).findIndex((t) => t == null);
      const tile = free >= 0 ? free : (selected ?? 0);
      call("assign_tile", { tile, id: d.id }).then(refresh);
    };
    li.onclick = show;
    li.onkeydown = (e) => { if (e.key === "Enter") show(); };
    return li;
  }));
}

// ---------- tiles ----------
// Single view: the backend stops streams for the hidden tiles and may switch the shown one to Clear.
function setSolo(idx) {
  solo = idx;
  renderTiles();
  call("set_solo", { tile: idx });
}

// Report each streaming tile's size in device pixels so JPEGs are made no bigger than shown.
let sizeTimer;
function reportSizes() {
  clearTimeout(sizeTimer);
  sizeTimer = setTimeout(() => {
    const dpr = window.devicePixelRatio || 1;
    const sizes = [];
    tiles.forEach((t, idx) => {
      const id = state.tiles[idx];
      if (id == null || !t.root.isConnected || t.root.offsetParent === null) return;
      const r = t.root.getBoundingClientRect();
      sizes.push([id, Math.round(r.width * dpr), Math.round(r.height * dpr)]);
    });
    if (sizes.length) invoke("set_view_sizes", { sizes }).catch(() => {});
  }, 250);
}
// Per tile, so layout and single-view changes count too, not just window resizes.
const sizeObserver = new ResizeObserver(reportSizes);

// ---------- live video: WebCodecs ----------
// Tiles decode the camera's own H.264/H.265 here (on the GPU where it can) instead of ffmpeg turning every frame
// into a JPEG. A camera whose codec the WebView can't decode falls back to the MJPEG <img> for the session.
const mjpegOnly = new Set();

// Annex-B NAL units (start codes stripped).
function nalUnits(b) {
  const out = [];
  let start = -1;
  for (let i = 0; i + 2 < b.length; i++) {
    if (b[i] === 0 && b[i + 1] === 0 && b[i + 2] === 1) {
      if (start >= 0) out.push(b.subarray(start, i - (i > 0 && b[i - 1] === 0 ? 1 : 0)));
      start = i + 3;
      i += 2;
    }
  }
  if (start >= 0) out.push(b.subarray(start));
  return out;
}

const hex = (v, n = 2) => v.toString(16).toUpperCase().padStart(n, "0");

// A NAL unit's payload without emulation-prevention bytes (00 00 03 -> 00 00).
function rbsp(n, from) {
  const r = [];
  for (let i = from; i < n.length; i++) {
    if (n[i] === 3 && r.length >= 2 && r[r.length - 1] === 0 && r[r.length - 2] === 0) continue;
    r.push(n[i]);
  }
  return r;
}

// Fixed-width and Exp-Golomb reads over an RBSP.
function bitReader(r) {
  let p = 0;
  const u = (n) => { let v = 0; for (let i = 0; i < n; i++, p++) v = v * 2 + ((r[p >> 3] >> (7 - (p & 7))) & 1); return v; };
  const ue = () => { let z = 0; while (u(1) === 0 && z < 32) z++; return 2 ** z - 1 + u(z); };
  return { u, ue };
}

// HEVC picture size from the SPS, after the conformance-window crop.
function hevcSize(r) {
  const b = bitReader(r);
  b.u(4);
  const subLayers = b.u(3);
  b.u(1);
  b.u(88 + 8); // general profile_tier_level and level
  const present = [];
  for (let i = 0; i < subLayers; i++) present.push([b.u(1), b.u(1)]);
  if (subLayers > 0) for (let i = subLayers; i < 8; i++) b.u(2);
  for (const [profile, level] of present) b.u((profile ? 88 : 0) + (level ? 8 : 0));
  b.ue(); // sps id
  const chroma = b.ue();
  if (chroma === 3) b.u(1);
  let width = b.ue(), height = b.ue();
  if (b.u(1)) {
    const [sx, sy] = [chroma === 1 || chroma === 2 ? 2 : 1, chroma === 1 ? 2 : 1];
    const [l, rt, t, bt] = [b.ue(), b.ue(), b.ue(), b.ue()];
    width -= sx * (l + rt);
    height -= sy * (t + bt);
  }
  return { width, height };
}

// The WebCodecs codec string for a keyframe, read from its SPS (ISO/IEC 14496-15 Annex E for HEVC), plus the HEVC
// picture size; null if it has no SPS.
function streamInfo(frame, hevc) {
  for (const n of nalUnits(frame)) {
    if (!hevc && (n[0] & 0x1f) === 7 && n.length > 3) return { codec: `avc1.${hex(n[1])}${hex(n[2])}${hex(n[3])}` };
    if (hevc && ((n[0] >> 1) & 0x3f) === 33) {
      const r = rbsp(n, 2);
      if (r.length < 14) return null;
      // r[0]: VPS id, sub-layers; then profile_tier_level: space/tier/profile, 32 compat bits, 48 constraint bits, level.
      const space = ["", "A", "B", "C"][r[1] >> 6], tier = r[1] & 0x20 ? "H" : "L", profile = r[1] & 0x1f;
      let compat = ((r[2] << 24) | (r[3] << 16) | (r[4] << 8) | r[5]) >>> 0, rev = 0;
      for (let i = 0; i < 32; i++) { rev = ((rev << 1) | (compat & 1)) >>> 0; compat >>>= 1; }
      const cons = r.slice(6, 12);
      while (cons.length && cons[cons.length - 1] === 0) cons.pop();
      const codec = [`hvc1.${space}${profile}`, rev.toString(16).toUpperCase(), `${tier}${r[12]}`, ...cons.map((c) => hex(c))].join(".");
      return { codec, ...hevcSize(r) };
    }
  }
  return null;
}

// Play GET /raw/<id> (see stream.rs: 1-byte flags, 4-byte length, access unit) into `canvas`.
function rawPlayer(canvas, url, onFirst, onFail) {
  const ctx = canvas.getContext("2d");
  const abort = new AbortController();
  let dec = null, sig = null, waitKey = true, ts = 0, shown = false, size = null;
  const stop = () => { abort.abort(); try { dec?.close(); } catch { /* already closed */ } dec = null; };
  const fail = (why) => { if (abort.signal.aborted) return; console.warn("WebCodecs, using MJPEG:", why); stop(); onFail(); };
  const draw = (frame) => {
    // WebView2's hardware HEVC decoder can report Chromium's default 1280x720 as the visible area of a larger
    // picture; crop to the size the SPS gives instead, so the tile isn't just the top-left corner.
    if (size && frame.displayWidth !== size.width && size.width <= frame.codedWidth && size.height <= frame.codedHeight) {
      const fixed = new VideoFrame(frame, { visibleRect: { x: 0, y: 0, ...size } });
      frame.close();
      frame = fixed;
    }
    if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
      canvas.width = frame.displayWidth;
      canvas.height = frame.displayHeight;
    }
    ctx.drawImage(frame, 0, 0);
    frame.close();
    if (!shown) { shown = true; onFirst(); }
  };
  async function onFrame(flags, data) {
    const key = (flags & 1) !== 0;
    if (key) {
      const hevc = (flags & 2) !== 0;
      const info = streamInfo(data, hevc);
      const codec = info?.codec;
      // Reconfigure when the stream changes (main/sub switch, new resolution): its parameter sets (VPS/SPS/PPS) do.
      const params = nalUnits(data).filter((n) => (hevc ? [32, 33, 34].includes((n[0] >> 1) & 0x3f) : [7, 8].includes(n[0] & 0x1f)));
      const s = codec && codec + params.map((n) => n.join()).join("|");
      if (codec && s !== sig) {
        size = info.width ? { width: info.width, height: info.height } : null;
        // No description: Annex B with in-band parameter sets. The size hint keeps hardware decoders from guessing.
        const config = { codec, optimizeForLatency: true, ...(size && { codedWidth: size.width, codedHeight: size.height }) };
        const { supported } = await VideoDecoder.isConfigSupported(config).catch(() => ({ supported: false }));
        if (!supported) return fail(`${codec} not supported`);
        dec ||= new VideoDecoder({ output: draw, error: fail });
        dec.configure(config);
        sig = s;
        waitKey = true;
      }
    }
    if (!dec || dec.state !== "configured" || (waitKey && !key)) return;
    if (dec.decodeQueueSize > 8) { waitKey = true; return; } // falling behind: drop to the next keyframe
    waitKey = false;
    dec.decode(new EncodedVideoChunk({ type: key ? "key" : "delta", timestamp: (ts += 1000), data }));
  }
  (async () => {
    const reader = (await fetch(url, { signal: abort.signal, cache: "no-store" })).body.getReader();
    let buf = new Uint8Array(0);
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return; // device removed or edited: the tile gets a new stream
      const next = new Uint8Array(buf.length + value.length);
      next.set(buf);
      next.set(value, buf.length);
      buf = next;
      let at = 0;
      while (buf.length - at >= 5) {
        const len = new DataView(buf.buffer, at + 1, 4).getUint32(0);
        if (buf.length - at - 5 < len) break;
        await onFrame(buf[at], buf.subarray(at + 5, at + 5 + len));
        at += 5 + len;
      }
      buf = buf.subarray(at);
    }
  })().catch((e) => { if (!abort.signal.aborted) console.warn("live stream:", e); });
  return { stop };
}

// Point a tile at a camera's video, keeping its connection while it shows the same camera.
function showVideo(t, d) {
  const raw = state.webcodecs && "VideoDecoder" in window && !mjpegOnly.has(d.id);
  const src = `${state.stream_base}/${raw ? "raw" : "live"}/${d.id}?t=${state.token}`;
  if (t.src === src) return raw ? t.canvas : t.img;
  stopVideo(t);
  t.src = src;
  if (raw) {
    const ready = () => { t.canvas.classList.add("ok"); t.loading?.remove(); };
    const fallback = () => { mjpegOnly.add(d.id); t.key = null; renderTiles(); };
    t.player = rawPlayer(t.canvas, src, ready, fallback);
    return t.canvas;
  }
  t.img.onload = () => { t.img.classList.add("ok"); t.loading?.remove(); };
  t.img.onerror = () => t.img.classList.remove("ok");
  t.img.src = src;
  return t.img;
}

function stopVideo(t) {
  t.player?.stop();
  t.player = null;
  t.src = "";
  t.img.removeAttribute("src");
  t.img.classList.remove("ok");
  t.canvas.classList.remove("ok");
}

function makeTile(idx) {
  const t = { root: el("div", "tile"), img: el("img"), canvas: el("canvas"), key: null, src: "", player: null, loading: null };
  sizeObserver.observe(t.root);
  t.img.alt = "";
  t.img.draggable = false;
  t.root.onclick = () => { selected = idx; renderTiles(); renderPtz(); };
  t.root.ondblclick = (e) => {
    if (e.target.closest(".tile-bar")) return;
    setSolo(solo === idx ? null : idx);
  };
  return t;
}

function renderTiles() {
  const grid = $("#grid");
  const l = LAYOUTS.find((x) => x.n === state.layout) || LAYOUTS[2];
  grid.style.setProperty("--cols", l.cols);
  grid.style.setProperty("--rows", l.rows);
  grid.classList.toggle("solo", solo != null);
  if (selected >= l.n) selected = 0;
  while (tiles.length < l.n) tiles.push(makeTile(tiles.length));
  // Only touch the grid when the tile count changes; re-inserting tiles restarts their entry
  // animation and detaches the MJPEG <img>, which showed as a black flash on every poll.
  if (grid.children.length !== l.n) grid.replaceChildren(...tiles.slice(0, l.n).map((t) => t.root));

  tiles.slice(0, l.n).forEach((t, idx) => {
    t.root.classList.toggle("selected", idx === selected);
    t.root.classList.toggle("solo", idx === solo);
    t.root.style.animationDelay = `${idx * 35}ms`;
    const d = device(state.tiles[idx]);
    t.root.classList.toggle("motion", !!d?.motion);
    // Rebuild a tile only when what it shows changes; the <img> keeps its stream connection otherwise.
    const key = d ? [d.id, d.online, d.live, d.clear, d.recording, d.live ? "" : d.status].join("|") : "empty";
    if (key === t.key) return;
    t.key = key;
    reportSizes(); // the tile may now show another camera
    t.root.replaceChildren();

    if (!d) {
      stopVideo(t);
      t.root.append(el("div", "tile-empty", `${ICON.plus}<span>Pick a device</span>`));
      return;
    }
    const loading = el("div", "tile-msg", d.online ? '<div><div class="spinner"></div>Starting stream…</div>' : "");
    if (!d.online) loading.textContent = d.status;
    t.loading = loading;
    const view = showVideo(t, d);
    // A picture already showing needs no spinner; an offline status always shows.
    t.root.append(...(d.online && view.classList.contains("ok") ? [] : [loading]), view);

    const head = el("div", "tile-head");
    head.append(Object.assign(el("span"), { textContent: d.name }));
    if (d.live) head.append(Object.assign(el("span", "chip live"), { textContent: d.live === "main" ? "HD" : "SD" }));
    if (d.recording) head.append(Object.assign(el("span", "chip rec"), { textContent: "REC" }));
    t.root.append(head);

    const bar = el("div", "tile-bar");
    const seg = el("div", "seg");
    for (const [label, clear] of [["Fluent", false], ["Clear", true]]) {
      const b = Object.assign(el("button"), { textContent: label });
      b.setAttribute("aria-pressed", String(d.clear === clear));
      if (clear) b.title = "HD in single view and 1–2 camera layouts; smaller tiles use Fluent to save bandwidth";
      b.onclick = (e) => { e.stopPropagation(); call("set_clear", { id: d.id, clear }).then(refresh); };
      seg.append(b);
    }
    const snap = el("button", "bar-btn", `${ICON.camera}`);
    snap.title = "Snapshot";
    snap.onclick = (e) => { e.stopPropagation(); call("snapshot", { id: d.id }); };
    const rec = el("button", "bar-btn" + (d.recording ? " recording" : ""), `${ICON.rec}<span>${d.recording ? "Stop" : "Record"}</span>`);
    rec.onclick = (e) => { e.stopPropagation(); call("record", { id: d.id, on: !d.recording }).then(refresh); };
    const close = el("button", "bar-btn", ICON.close);
    close.title = "Close live view";
    close.onclick = (e) => {
      e.stopPropagation();
      if (solo === idx) setSolo(null);
      call("assign_tile", { tile: idx, id: null }).then(refresh);
    };
    bar.append(seg, snap, rec, el("span", "spacer"), close);
    t.root.append(bar);
  });
  // Drop streams for tiles hidden by a smaller layout.
  tiles.slice(l.n).forEach((t) => { stopVideo(t); t.key = null; });
}

// ---------- PTZ ----------
function renderPtz() {
  const d = device(state.tiles[selected]);
  $("#ptz-target").textContent = d ? d.name : "Select a live tile";
  $("#dpad").classList.toggle("disabled", !d || !d.online);
}

// ---------- device panel and full screen ----------
function setupView() {
  const toggle = $("#sidebar-toggle");
  const apply = (collapsed) => {
    document.body.classList.toggle("sidebar-collapsed", collapsed);
    toggle.setAttribute("aria-expanded", String(!collapsed));
    toggle.title = collapsed ? "Show device panel" : "Hide device panel";
  };
  try { apply(localStorage.getItem("sidebarCollapsed") === "1"); } catch { /* storage unavailable */ }
  // Clicking anywhere in the device panel (device rows included) drops the tile highlight.
  $(".sidebar").addEventListener("click", () => { selected = null; renderTiles(); renderPtz(); });
  toggle.onclick = () => {
    const collapsed = !document.body.classList.contains("sidebar-collapsed");
    apply(collapsed);
    try { localStorage.setItem("sidebarCollapsed", collapsed ? "1" : "0"); } catch { /* ignore */ }
  };

  // The window goes full screen too (outside Tauri, the browser's own full screen).
  const win = window.__TAURI__?.window?.getCurrentWindow?.();
  const setFull = async (on) => {
    document.body.classList.toggle("fullscreen", on);
    try {
      if (win) await win.setFullscreen(on);
      else if (on) await document.documentElement.requestFullscreen?.();
      else if (document.fullscreenElement) await document.exitFullscreen();
    } catch (e) { console.warn("full screen", e); }
  };
  $("#fullscreen").onclick = () => setFull(true);
  $("#fs-exit").onclick = () => setFull(false);
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && document.body.classList.contains("fullscreen") && !document.querySelector("dialog[open]")) setFull(false);
  });
  document.addEventListener("fullscreenchange", () => { if (!document.fullscreenElement && !win) document.body.classList.remove("fullscreen"); });
}

function setupPtz() {
  const panel = $("#ptz");
  const toggle = $("#ptz-toggle");
  const apply = (collapsed) => {
    panel.classList.toggle("collapsed", collapsed);
    toggle.setAttribute("aria-expanded", String(!collapsed));
  };
  try { apply(localStorage.getItem("ptzCollapsed") === "1"); } catch { /* storage unavailable */ }
  toggle.onclick = () => {
    const collapsed = !panel.classList.contains("collapsed");
    apply(collapsed);
    try { localStorage.setItem("ptzCollapsed", collapsed ? "1" : "0"); } catch { /* ignore */ }
  };

  const stop = () => {
    if (!ptzActive) return;
    ptzActive.btn.classList.remove("active");
    invoke("ptz", { id: ptzActive.id, dir: "stop" }).catch(() => {});
    ptzActive = null;
  };
  for (const btn of document.querySelectorAll("#dpad button")) {
    btn.onpointerdown = (e) => {
      const d = device(state.tiles[selected]);
      if (!d) return;
      btn.setPointerCapture(e.pointerId);
      btn.classList.add("active");
      ptzActive = { id: d.id, btn };
      call("ptz", { id: d.id, dir: btn.dataset.dir }).catch(stop);
    };
    btn.onpointerup = btn.onpointercancel = btn.onlostpointercapture = stop;
  }
}


// ---------- settings (camera + app) ----------
// One dialog renders pages of fields: {key,label,kind,value,display,min,max,max_len,options}.
// kind: info | range | toggle | choice | text | folder | number
const S = { mode: null, deviceId: null, pages: [], page: null, view: null, original: {}, values: {}, errors: {}, busy: false };

function openSettingsDialog() {
  const d = $("#settings-dialog");
  if (!d.open) d.showModal();
}

async function openCameraSettings(id) {
  const d = device(id);
  Object.assign(S, { mode: "camera", deviceId: id, view: null, pages: await call("camera_pages") });
  $("#settings-kicker").textContent = d ? d.name : "Camera";
  openSettingsDialog();
  showSettingsPage(S.pages[0].id);
}

async function openAppSettings() {
  Object.assign(S, { mode: "app", deviceId: null, view: null, pages: [{ id: "app", title: "App" }] });
  $("#settings-kicker").textContent = "Reolink-rs";
  openSettingsDialog();
  showSettingsPage("app");
}

const LOCAL_RECORD = [["off", "Off (record only when I press Record)"], ["motion", "When a camera detects motion"], ["continuous", "Continuously"]];
const SEGMENTS = [[0, "One file per recording"], [5, "5 minutes"], [10, "10 minutes"], [15, "15 minutes"], [30, "30 minutes"], [60, "60 minutes"]];

function prefsToView(p) {
  const opt = (pairs) => pairs.map(([v, l]) => ({ value: String(v), label: l }));
  return {
    id: "app", title: "App settings",
    fields: [
      { key: "snapshot_dir", label: "Save snapshots to", kind: "folder", value: p.snapshot_dir },
      { key: "record_dir", label: "Save recordings to", kind: "folder", value: p.record_dir },
      { key: "segment_minutes", label: "Video file duration", kind: "choice", value: String(p.segment_minutes), options: opt(SEGMENTS) },
      { key: "space_warning_gb", label: "Warn when free space is below (GB, 0 = off)", kind: "number", value: String(p.space_warning_gb), min: 0, max: 10000 },
      { key: "hardware_decode", label: "Hardware video decoding", kind: "toggle", value: p.hardware_decode ? "1" : "0" },
      { key: "scan_network", label: "Find new cameras on the network", kind: "toggle", value: p.scan_network ? "1" : "0" },
      { key: "run_at_startup", label: "Run at Windows startup", kind: "toggle", value: p.run_at_startup ? "1" : "0" },
      { key: "alarm_beep", label: "Beep when a camera detects motion", kind: "toggle", value: p.alarm_beep ? "1" : "0" },
      { key: "alarm_volume", label: "Beep volume", kind: "range", value: String(p.alarm_volume), min: 0, max: 100 },
      { key: "local_record", label: "Record to this PC automatically", kind: "choice", value: p.local_record, options: opt(LOCAL_RECORD) },
      { key: "record_schedule", label: "Automatic recording hours", kind: "schedule", value: JSON.stringify({ record: p.record_schedule }) },
    ],
  };
}

function viewToPrefs(values) {
  return {
    snapshot_dir: values.snapshot_dir, record_dir: values.record_dir,
    segment_minutes: +values.segment_minutes, space_warning_gb: Math.max(0, Math.round(+values.space_warning_gb || 0)),
    hardware_decode: values.hardware_decode === "1", scan_network: values.scan_network === "1",
    run_at_startup: values.run_at_startup === "1",
    alarm_beep: values.alarm_beep === "1", alarm_volume: Math.round(+values.alarm_volume || 0),
    local_record: values.local_record, record_schedule: JSON.parse(values.record_schedule).record,
  };
}

async function showSettingsPage(pageId) {
  if (dirty() && !confirm("Discard unsaved changes?")) return;
  S.page = pageId;
  S.view = null;
  S.errors = {};
  $("#settings-nav").replaceChildren(...S.pages.map((p) => {
    const b = Object.assign(el("button"), { textContent: p.title, type: "button" });
    if (p.id === pageId) b.setAttribute("aria-current", "page");
    b.onclick = () => showSettingsPage(p.id);
    return b;
  }));
  $("#settings-title").textContent = S.pages.find((p) => p.id === pageId).title;
  $("#settings-body").replaceChildren(el("div", "settings-loading", '<div><div class="spinner"></div>Reading settings…</div>'));
  setStatus("");
  renderSettingsActions();
  updateFooter();
  let view;
  try {
    view = S.mode === "camera"
      ? await invoke("camera_settings", { id: S.deviceId, page: pageId })
      : prefsToView(await invoke("get_prefs"));
  } catch (e) {
    $("#settings-body").replaceChildren(Object.assign(el("p", "settings-empty"), { textContent: String(e) }));
    return;
  }
  if (S.page !== pageId) return; // user moved on while this loaded
  S.view = view;
  S.original = Object.fromEntries(view.fields.map((f) => [f.key, f.value]));
  S.values = { ...S.original };
  renderSettingsBody();
}

function dirty() {
  return !!S.view && Object.keys(S.values).some((k) => S.values[k] !== S.original[k]);
}

function setStatus(text, err = false) {
  const s = $("#settings-status");
  s.textContent = text;
  s.classList.toggle("err", err);
}

function updateFooter() {
  $("#settings-save").disabled = !dirty() || S.busy;
  $("#settings-revert").disabled = !dirty() || S.busy;
  for (const row of document.querySelectorAll("#settings-body .set-row")) {
    row.classList.toggle("changed", S.values[row.dataset.key] !== S.original[row.dataset.key]);
  }
}

function control(f) {
  const c = el("div", "set-control");
  const set = (v) => { S.values[f.key] = String(v); delete S.errors[f.key]; updateFooter(); };
  const id = `set-${f.key}`;
  switch (f.kind) {
    case "range": {
      const r = Object.assign(el("input"), { type: "range", id, min: f.min, max: f.max, step: 1, value: S.values[f.key] });
      const out = Object.assign(el("span", "range-val"), { textContent: S.values[f.key] });
      r.oninput = () => { out.textContent = r.value; set(r.value); };
      c.append(r, out);
      break;
    }
    case "toggle": {
      const w = el("label", "switch");
      const i = Object.assign(el("input"), { type: "checkbox", id, checked: S.values[f.key] === "1" });
      i.onchange = () => set(i.checked ? "1" : "0");
      w.append(i, el("span"));
      c.append(w);
      break;
    }
    case "choice": {
      const s = Object.assign(el("select"), { id });
      for (const o of f.options) s.append(Object.assign(el("option"), { value: o.value, textContent: o.label, selected: o.value === S.values[f.key] }));
      s.onchange = () => set(s.value);
      c.append(s);
      break;
    }
    case "text":
    case "number": {
      const i = Object.assign(el("input"), { type: f.kind === "number" ? "number" : "text", id, value: S.values[f.key] });
      if (f.max_len) i.maxLength = f.max_len;
      if (f.kind === "number") Object.assign(i, { min: f.min, max: f.max, step: 1 });
      i.oninput = () => set(i.value);
      c.append(i);
      break;
    }
    case "password": {
      const i = Object.assign(el("input"), { type: "password", id, value: S.values[f.key], placeholder: "Unchanged", autocomplete: "new-password" });
      if (f.max_len) i.maxLength = f.max_len;
      i.oninput = () => set(i.value);
      c.append(i);
      break;
    }
    case "ipv4": {
      const i = Object.assign(el("input"), { type: "text", id, value: S.values[f.key], maxLength: 15, inputMode: "decimal", placeholder: "0.0.0.0", spellcheck: false });
      i.oninput = () => set(i.value.trim());
      c.append(i);
      break;
    }
    case "schedule": c.append(scheduleEditor(f, set)); break;
    case "zone": c.append(zoneEditor(f, set)); break;
    case "masks": c.append(masksEditor(f, set)); break;
    case "folder": {
      const p = Object.assign(el("span", "path-val"), { textContent: S.values[f.key], title: S.values[f.key] });
      const b = Object.assign(el("button", "ghost-btn"), { type: "button", textContent: "Change…", id });
      b.onclick = async () => {
        const picked = await invoke("pick_folder", { current: S.values[f.key] });
        if (picked) { p.textContent = picked; p.title = picked; set(picked); }
      };
      c.append(p, b);
      break;
    }
    default:
      c.append(Object.assign(el("span", "set-info"), { textContent: f.display || f.value || "—", id }));
  }
  return c;
}

function renderSettingsBody() {
  const body = $("#settings-body");
  if (!S.view.fields.length) {
    body.replaceChildren(Object.assign(el("p", "settings-empty"), { textContent: "This camera doesn't offer these settings, or is offline." }));
    updateFooter();
    return;
  }
  body.replaceChildren(...S.view.fields.map((f) => {
    const row = el("div", "set-row");
    row.dataset.key = f.key;
    const label = Object.assign(el("label", "set-label"), { textContent: f.label, htmlFor: `set-${f.key}` });
    if (["schedule", "zone", "masks"].includes(f.kind)) row.classList.add("wide");
    row.append(label, control(f));
    if (S.errors[f.key]) row.append(Object.assign(el("div", "set-error"), { textContent: S.errors[f.key] }));
    return row;
  }));
  updateFooter();
}

function renderSettingsActions() {
  const box = $("#settings-actions");
  box.replaceChildren();
  const action = (label, fn) => {
    const b = Object.assign(el("button", "ghost-btn"), { type: "button", textContent: label });
    b.onclick = async () => {
      b.disabled = true;
      try { await fn(); } catch (e) { setStatus(String(e), true); } finally { b.disabled = false; }
    };
    box.append(b);
  };
  if (S.mode !== "camera") return;
  if (S.page === "system") {
    action("Sync time with this PC", async () => {
      await invoke("sync_camera_time", { id: S.deviceId });
      setStatus("Camera clock set to this PC's time.");
    });
  }
  if (S.page === "email") {
    action("Send test email", async () => {
      if (dirty()) throw new Error("Save your changes first; the test uses the camera's saved settings.");
      setStatus("Sending test email…");
      await invoke("camera_test", { id: S.deviceId, which: "email" });
      setStatus("Test email sent. Check the recipient inbox.");
    });
  }
  if (S.page === "ftp") {
    action("Test FTP", async () => {
      if (dirty()) throw new Error("Save your changes first; the test uses the camera's saved settings.");
      setStatus("Testing FTP…");
      await invoke("camera_test", { id: S.deviceId, which: "ftp" });
      setStatus("FTP test succeeded.");
    });
  }
  if (S.page === "maintenance") {
    action("Reboot camera", async () => {
      if (!confirm("Reboot the camera now? It will be offline for about a minute, then reconnect automatically.")) return;
      await invoke("reboot_camera", { id: S.deviceId });
      setStatus("Camera is rebooting; it will reconnect automatically.");
    });
  }
}

async function saveSettings() {
  const changes = Object.fromEntries(Object.keys(S.values).filter((k) => S.values[k] !== S.original[k]).map((k) => [k, S.values[k]]));
  S.busy = true;
  updateFooter();
  // Argus cameras apply motion changes and then drop the connection; the save waits for the reconnect to verify.
  setStatus(S.page === "motion" ? "Saving… motion changes take 1–3 minutes while the camera reconnects." : "Saving…");
  try {
    if (S.mode === "app") {
      const p = await invoke("save_prefs", { prefs: viewToPrefs(S.values) });
      appPrefs = p;
      S.view = prefsToView(p);
      S.original = Object.fromEntries(S.view.fields.map((f) => [f.key, f.value]));
      S.values = { ...S.original };
      renderSettingsBody();
      setStatus("Saved.");
      return;
    }
    const results = await invoke("save_camera_settings", { id: S.deviceId, page: S.page, changes });
    const errors = Object.fromEntries(results.filter((r) => !r.ok).map((r) => [r.key, r.error]));
    const failed = Object.keys(errors).length;
    // Re-read so the form shows what the camera actually has now.
    S.view = await invoke("camera_settings", { id: S.deviceId, page: S.page });
    S.original = Object.fromEntries(S.view.fields.map((f) => [f.key, f.value]));
    S.values = { ...S.original };
    S.errors = errors;
    renderSettingsBody();
    setStatus(failed ? `${failed} setting${failed > 1 ? "s" : ""} not saved; see below.` : "Saved and verified on the camera.", failed > 0);
  } catch (e) {
    setStatus(String(e), true);
  } finally {
    S.busy = false;
    updateFooter();
  }
}

function setupSettings() {
  $("#app-settings").onclick = openAppSettings;
  $("#settings-save").onclick = saveSettings;
  $("#settings-revert").onclick = () => { S.values = { ...S.original }; S.errors = {}; renderSettingsBody(); setStatus(""); };
  const close = () => {
    if (dirty() && !confirm("Discard unsaved changes?")) return;
    S.view = null;
    $("#settings-dialog").close();
  };
  $("#settings-close").onclick = close;
  $("#settings-dialog").addEventListener("cancel", (e) => { e.preventDefault(); close(); });
}


// ---------- rich editors: schedules, motion area, privacy masks ----------
const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

// Paint by dragging: the first cell decides whether the drag turns cells on or off.
function paintable(grid, onPaint) {
  let painting = null;
  grid.addEventListener("pointerdown", (e) => {
    const cell = e.target.closest("[data-i]");
    if (!cell) return;
    grid.setPointerCapture(e.pointerId);
    painting = cell.classList.contains("on") ? "0" : "1";
    onPaint(+cell.dataset.i, painting);
  });
  grid.addEventListener("pointermove", (e) => {
    if (painting === null) return;
    const cell = document.elementFromPoint(e.clientX, e.clientY)?.closest("[data-i]");
    if (cell && grid.contains(cell)) onPaint(+cell.dataset.i, painting);
  });
  const stop = () => { painting = null; };
  grid.addEventListener("pointerup", stop);
  grid.addEventListener("pointercancel", stop);
}

function scheduleEditor(f, set) {
  const wrap = el("div", "sched");
  const value = JSON.parse(S.values[f.key]);
  const types = Object.keys(value);
  let current = types[0];
  const tabs = el("div", "seg sched-tabs");
  const grid = el("div", "sched-grid");
  const commit = () => set(JSON.stringify(value));
  const draw = () => {
    for (const b of tabs.children) b.setAttribute("aria-pressed", String(b.dataset.t === current));
    const table = value[current];
    grid.replaceChildren(el("span", "sched-corner"));
    for (let h = 0; h < 24; h++) grid.append(Object.assign(el("span", "sched-hour"), { textContent: h % 3 === 0 ? String(h) : "" }));
    DAYS.forEach((d, day) => {
      grid.append(Object.assign(el("span", "sched-day"), { textContent: d }));
      for (let h = 0; h < 24; h++) {
        const i = day * 24 + h;
        const c = el("span", "sched-cell" + (table[i] === "1" ? " on" : ""));
        c.dataset.i = i;
        c.title = `${d} ${String(h).padStart(2, "0")}:00`;
        grid.append(c);
      }
    });
  };
  for (const t of types) {
    const b = Object.assign(el("button"), { type: "button", textContent: t.replace("_", " / ") });
    b.dataset.t = t;
    b.onclick = () => { current = t; draw(); };
    tabs.append(b);
  }
  paintable(grid, (i, v) => {
    const t = value[current];
    if (t[i] === v) return;
    value[current] = t.slice(0, i) + v + t.slice(i + 1);
    grid.querySelector(`[data-i="${i}"]`).classList.toggle("on", v === "1");
    commit();
  });
  const fill = (v) => { value[current] = v.repeat(168); draw(); commit(); };
  const tools = el("div", "editor-tools");
  tools.append(
    Object.assign(el("button", "ghost-btn"), { type: "button", textContent: "All day, every day", onclick: () => fill("1") }),
    Object.assign(el("button", "ghost-btn"), { type: "button", textContent: "Clear", onclick: () => fill("0") }),
  );
  wrap.append(tabs, grid, tools);
  draw();
  return wrap;
}

// Snapshot behind the zone/mask editors, fetched once per dialog page.
async function snapshotInto(img, note) {
  try {
    img.src = await invoke("camera_snapshot", { id: S.deviceId });
  } catch (e) {
    note.textContent = "No snapshot available; drawing on a blank frame.";
  }
}

function zoneEditor(f, set) {
  const z = JSON.parse(S.values[f.key]);
  const wrap = el("div", "frame-editor");
  const stage = el("div", "frame-stage");
  const img = el("img", "frame-img");
  const note = el("p", "editor-note", "");
  const grid = el("div", "zone-grid");
  grid.style.gridTemplateColumns = `repeat(${z.w}, 1fr)`;
  grid.style.gridTemplateRows = `repeat(${z.h}, 1fr)`;
  for (let i = 0; i < z.w * z.h; i++) {
    const c = el("span", "zone-cell" + (z.cells[i] === "1" ? " on" : ""));
    c.dataset.i = i;
    grid.append(c);
  }
  const commit = () => set(JSON.stringify(z));
  paintable(grid, (i, v) => {
    if (z.cells[i] === v) return;
    z.cells = z.cells.slice(0, i) + v + z.cells.slice(i + 1);
    grid.children[i].classList.toggle("on", v === "1");
    commit();
  });
  const fill = (v) => {
    z.cells = v.repeat(z.w * z.h);
    for (const c of grid.children) c.classList.toggle("on", v === "1");
    commit();
  };
  const tools = el("div", "editor-tools");
  tools.append(
    Object.assign(el("button", "ghost-btn"), { type: "button", textContent: "Whole picture", onclick: () => fill("1") }),
    Object.assign(el("button", "ghost-btn"), { type: "button", textContent: "Clear", onclick: () => fill("0") }),
    Object.assign(el("span", "editor-hint"), { textContent: "Drag to paint where motion is detected." }),
  );
  stage.append(img, grid);
  wrap.append(stage, tools, note);
  snapshotInto(img, note);
  return wrap;
}

function masksEditor(f, set) {
  const v = JSON.parse(S.values[f.key]);
  const REF = 1000;
  const wrap = el("div", "frame-editor");
  const stage = el("div", "frame-stage masks");
  const img = el("img", "frame-img");
  const note = el("p", "editor-note", "");
  const layer = el("div", "mask-layer");
  const commit = () => set(JSON.stringify(v));
  const enable = el("label", "switch");
  const box = Object.assign(el("input"), { type: "checkbox", checked: v.enable === "1" });
  box.onchange = () => { v.enable = box.checked ? "1" : "0"; commit(); };
  enable.append(box, el("span"));
  const count = el("span", "editor-hint");
  const draw = () => {
    layer.replaceChildren();
    v.rects.forEach(([x, y, w, h], i) => {
      const r = el("div", "mask-rect");
      Object.assign(r.style, { left: `${x / 10}%`, top: `${y / 10}%`, width: `${w / 10}%`, height: `${h / 10}%` });
      const x_ = Object.assign(el("button", "mask-del"), { type: "button", textContent: "×", title: "Remove" });
      x_.onpointerdown = (e) => e.stopPropagation();
      x_.onclick = () => { v.rects.splice(i, 1); draw(); commit(); };
      r.append(x_);
      layer.append(r);
    });
    count.textContent = `${v.rects.length} of ${v.max} areas. Drag on the picture to add one.`;
  };
  let start = null;
  const pos = (e) => {
    const b = layer.getBoundingClientRect();
    return [Math.min(REF, Math.max(0, Math.round(((e.clientX - b.left) / b.width) * REF))),
            Math.min(REF, Math.max(0, Math.round(((e.clientY - b.top) / b.height) * REF)))];
  };
  let ghost = null;
  layer.addEventListener("pointerdown", (e) => {
    if (v.rects.length >= v.max || e.target.closest(".mask-del")) return;
    layer.setPointerCapture(e.pointerId);
    start = pos(e);
    ghost = el("div", "mask-rect ghost");
    layer.append(ghost);
  });
  layer.addEventListener("pointermove", (e) => {
    if (!start) return;
    const [x2, y2] = pos(e);
    Object.assign(ghost.style, {
      left: `${Math.min(start[0], x2) / 10}%`, top: `${Math.min(start[1], y2) / 10}%`,
      width: `${Math.abs(x2 - start[0]) / 10}%`, height: `${Math.abs(y2 - start[1]) / 10}%`,
    });
  });
  layer.addEventListener("pointerup", (e) => {
    if (!start) return;
    const [x2, y2] = pos(e);
    const r = [Math.min(start[0], x2), Math.min(start[1], y2), Math.abs(x2 - start[0]), Math.abs(y2 - start[1])];
    start = null;
    if (r[2] >= 10 && r[3] >= 10) { v.rects.push(r); if (v.enable !== "1") { v.enable = "1"; box.checked = true; } commit(); }
    draw();
  });
  const tools = el("div", "editor-tools");
  const lbl = Object.assign(el("span", "editor-hint"), { textContent: "Masks on" });
  tools.append(enable, lbl, count);
  stage.append(img, layer);
  wrap.append(stage, tools, note);
  draw();
  snapshotInto(img, note);
  return wrap;
}

// ---------- LAN discovery ----------
let scanning = false;
async function scanLan(auto = false) {
  if (scanning) return;
  if (auto) {
    try { if (!(await invoke("get_prefs")).scan_network) return; } catch { /* default: scan */ }
  }
  scanning = true;
  $("#rescan").classList.add("spinning");
  try {
    const found = (await invoke("scan_lan")).filter((f) => !f.added);
    $("#nearby").hidden = found.length === 0;
    $("#nearby-list").replaceChildren(...found.map((f) => {
      const li = el("li", "nearby-item");
      li.tabIndex = 0;
      li.title = "Add this camera";
      li.append(
        Object.assign(el("span", "nearby-ip"), { textContent: f.ip + (f.battery != null ? `  ·  ${f.battery}%` : "") }),
        Object.assign(el("span", "nearby-add"), { textContent: "Add" }),
        Object.assign(el("span", "nearby-uid"), { textContent: f.uid }),
      );
      // One click: re-adds a removed camera, or uses a login that works; only otherwise ask for it.
      const add = async () => {
        li.classList.add("busy");
        const added = await invoke("add_found", { uid: f.uid, ip: f.ip }).catch(() => false);
        li.classList.remove("busy");
        if (added) { toast(`Added ${f.ip}`); refresh(); scanLan(); }
        else openDialog(null, { name: `Camera ${f.ip.split(".").pop()}`, host: f.ip, uid: f.uid });
      };
      li.onclick = add;
      li.onkeydown = (e) => { if (e.key === "Enter") add(); };
      return li;
    }));
  } catch (e) {
    console.warn("LAN scan failed", e);
  } finally {
    scanning = false;
    $("#rescan").classList.remove("spinning");
  }
}

// ---------- add / edit dialog ----------
let editing = null;
async function openDialog(id = null, prefill = null) {
  editing = id;
  const form = $("#device-form");
  form.reset();
  $("#form-error").textContent = "";
  $("#dialog-title").textContent = id == null ? "Add device" : "Edit device";
  form.password.placeholder = id == null ? "" : "Leave blank to keep current";
  if (id != null) {
    const cfg = await call("device_config", { id });
    for (const k of ["name", "host", "uid", "username", "channel"]) form[k].value = cfg[k] ?? "";
    form.port.value = cfg.port ?? "";
  }
  if (prefill) for (const [k, v] of Object.entries(prefill)) form[k].value = v;
  $("#device-dialog").showModal();
  (prefill ? form.password : form.name).focus();
}

function setupDialog() {
  const form = $("#device-form");
  $("#add-device").onclick = () => openDialog();
  $("#dialog-cancel").onclick = () => $("#device-dialog").close();
  form.onsubmit = async (e) => {
    e.preventDefault();
    const port = form.port.value.trim();
    if (port && !(/^\d+$/.test(port) && +port > 0 && +port < 65536)) {
      $("#form-error").textContent = "Port must be 1–65535";
      return;
    }
    const config = {
      name: form.name.value, host: form.host.value, uid: form.uid.value,
      port: port ? +port : null, username: form.username.value, password: form.password.value,
      channel: Math.max(0, Math.min(63, +form.channel.value || 0)),
    };
    try {
      await invoke("save_device", { id: editing, config });
      $("#device-dialog").close();
      refresh();
      scanLan(); // drop the newly added camera from "Found nearby"
    } catch (err) {
      $("#form-error").textContent = String(err);
    }
  };
}

// ---------- motion alarm ----------
let appPrefs = null;
const motionSeen = new Map();
let audio = null;

// A short two-tone beep (official "alarm beep"), at the user's volume.
function beep(volume) {
  audio ||= new AudioContext();
  const t = audio.currentTime;
  const gain = audio.createGain();
  gain.gain.setValueAtTime(0.0001, t);
  gain.gain.exponentialRampToValueAtTime(Math.max(0.0001, volume / 100 * 0.4), t + 0.02);
  gain.gain.exponentialRampToValueAtTime(0.0001, t + 0.5);
  gain.connect(audio.destination);
  [[880, 0], [660, 0.25]].forEach(([f, at]) => {
    const o = audio.createOscillator();
    o.frequency.value = f;
    o.connect(gain);
    o.start(t + at);
    o.stop(t + at + 0.22);
  });
}

async function checkMotion() {
  let fresh = false;
  for (const d of state.devices) {
    const seen = motionSeen.get(d.id);
    if (seen !== undefined && d.motion_events > seen) fresh = true;
    motionSeen.set(d.id, d.motion_events);
  }
  if (!fresh) return;
  try { appPrefs ||= await invoke("get_prefs"); } catch { return; }
  if (appPrefs.alarm_beep && appPrefs.alarm_volume > 0) beep(appPrefs.alarm_volume);
}

// ---------- playback (NVR) ----------
// Recordings are the MP4 segments in the recording folder, named <camera>_<YYYY-MM-DD_HH-MM-SS>.mp4 (local time).
const SPEEDS = [0.25, 0.5, 1, 2, 4, 8, 16];
const FRAME = 1 / 15; // shortcut: assumes Reolink's usual 15 fps; read the stream's real rate if steps feel off
const SEC = 1000, MIN = 60 * SEC, HOUR = 60 * MIN;
const TICKS = [10 * SEC, 30 * SEC, MIN, 5 * MIN, 10 * MIN, 15 * MIN, 30 * MIN, HOUR, 2 * HOUR, 3 * HOUR];
const NAME_RE = /^(.*)_(\d{4})-(\d\d)-(\d\d)_(\d\d)-(\d\d)-(\d\d)\.mp4$/i;
const PB = { on: false, opened: false, dir: "", files: [], cams: [], cam: null, cur: null, day: 0, v0: 0, v1: 0, mark: [null, null], seek: 0, autoplay: false, speed: 1, timer: null, heads: [] };
const video = $("#pb-video");

const safeName = (n) => n.replace(/[^\p{L}\p{N}-]/gu, "_"); // as device.rs names the files
const camLabel = (cam) => state.devices.find((d) => safeName(d.name) === cam)?.name ?? cam.replace(/_/g, " ");
const dayOf = (t) => new Date(t).setHours(0, 0, 0, 0);
const addDays = (t, n) => { const d = new Date(t); d.setDate(d.getDate() + n); return d.getTime(); };
const fmtTime = (t, secs = true) => new Date(t).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: secs ? "2-digit" : undefined, hourCycle: "h23" });
const fmtDay = (t) => new Date(t).toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" });
const fmtBytes = (b) => b >= 1e9 ? `${(b / 1e9).toFixed(1)} GB` : `${Math.max(0.1, b / 1e6).toFixed(1)} MB`;
const fmtDur = (ms) => { const s = Math.round(ms / SEC); return s >= 3600 ? `${Math.floor(s / 3600)} h ${Math.floor(s % 3600 / 60)} min` : s >= 60 ? `${Math.floor(s / 60)} min ${s % 60} s` : `${s} s`; };
const isoDay = (t) => { const d = new Date(t); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`; };
const playTime = () => PB.cur ? PB.cur.start + video.currentTime * SEC : null;
const camFiles = (cam) => PB.files.filter((f) => f.cam === cam);
const dayFiles = (cam) => camFiles(cam).filter((f) => f.end > PB.day && f.start < addDays(PB.day, 1));

function fileSrc(name) {
  const sep = PB.dir.includes("\\") ? "\\" : "/";
  const path = PB.dir ? PB.dir.replace(/[\\/]$/, "") + sep + name : name;
  return window.__TAURI__ ? window.__TAURI__.core.convertFileSrc(path) : "demo.mp4"; // design preview: one sample clip
}

function parseFile(f) {
  const m = NAME_RE.exec(f.name);
  if (!m) return null;
  const [, cam, y, mo, d, h, mi, s] = m;
  const start = new Date(+y, mo - 1, +d, +h, +mi, +s).getTime();
  // A finished segment was last written when it closed; the player corrects this once the file loads.
  return { name: f.name, cam, start, end: Math.max(start + SEC, f.modified), size: f.size };
}

async function loadRecordings() {
  let res;
  try { res = await invoke("list_recordings"); } catch { return; }
  PB.dir = res.dir;
  const old = new Map(PB.files.map((f) => [f.name, f]));
  PB.files = res.files.map(parseFile).filter(Boolean).map((f) => {
    const o = old.get(f.name);
    return o && o.size === f.size ? o : f; // keep the object (and its measured length) while the file is unchanged
  }).sort((a, b) => a.start - b.start);
  PB.cams = [...new Set(PB.files.map((f) => f.cam))].sort((a, b) => camLabel(a).localeCompare(camLabel(b)));
  if (PB.cur) PB.cur = PB.files.find((f) => f.name === PB.cur.name) || PB.cur;
  if (!PB.opened) {
    PB.opened = true;
    const last = PB.files.at(-1);
    PB.cam = last?.cam ?? null;
    setDay(dayOf(last ? last.start : Date.now()));
    if (last) openFile(last, 0, false);
    else showMsg("No recordings yet. Press Record on a live tile, or turn on Local recording in Settings.");
  }
  renderPlayback();
}

function setDay(day, center = null) {
  PB.day = day;
  const span = PB.v1 - PB.v0 || Infinity;
  setViewWindow(center == null ? day : center - Math.min(span, addDays(day, 1) - day) / 2, span);
}

function setViewWindow(v0, span) {
  const end = addDays(PB.day, 1);
  span = Math.max(MIN, Math.min(span, end - PB.day));
  PB.v0 = Math.max(PB.day, Math.min(v0, end - span));
  PB.v1 = PB.v0 + span;
  for (const b of document.querySelectorAll(".tl-zoom button")) b.setAttribute("aria-pressed", String(Math.abs(+b.dataset.span * SEC - span) < SEC));
}

function showMsg(text) { $("#pb-msg").textContent = text; $("#pb-msg").hidden = !text; }

// ---------- playback: rendering ----------
function renderPlayback() {
  if (!PB.on) return;
  $("#pb-day").value = isoDay(PB.day);
  const today = PB.cams.flatMap(dayFiles);
  $("#pb-summary").textContent = today.length ? `${today.length} recording${today.length > 1 ? "s" : ""} · ${fmtBytes(today.reduce((n, f) => n + f.size, 0))}` : "No recordings this day";
  $("#pb-cam").textContent = PB.cur ? camLabel(PB.cur.cam) : "";
  $("#pb-export").disabled = !(PB.cur && PB.mark[0] != null && PB.mark[1] > PB.mark[0]);
  renderFiles();
  renderGrid();
  renderTimeline();
  tick();
}

// ---------- playback: thumbnail grid ----------
// Thumbnails are made by ffmpeg on demand (cached on disk by the backend), a few at a time as cards scroll into view.
const thumbs = new Map(); // "<name>|<size>" -> image src, or null when the file has no frame yet
const thumbQueue = [];
let thumbsBusy = 0;
const thumbKey = (f) => `${f.name}|${f.size}`; // a growing file gets another try once it changes
const thumbObserver = new IntersectionObserver((entries) => {
  for (const e of entries) {
    if (!e.isIntersecting) continue;
    thumbObserver.unobserve(e.target);
    thumbQueue.push(e.target);
  }
  pumpThumbs();
}, { rootMargin: "200px" });

function pumpThumbs() {
  while (thumbsBusy < 3 && thumbQueue.length) {
    const card = thumbQueue.shift();
    const f = card._file;
    thumbsBusy++;
    invoke("recording_thumb", { name: f.name })
      .then((path) => path && window.__TAURI__ ? window.__TAURI__.core.convertFileSrc(path) : null, () => null)
      .then((src) => { thumbs.set(thumbKey(f), src); showThumb(card, src); })
      .finally(() => { thumbsBusy--; pumpThumbs(); });
  }
}

function showThumb(card, src) {
  const box = card.querySelector(".pb-thumb");
  if (src) {
    const img = Object.assign(el("img"), { src, alt: "", draggable: false });
    img.onload = () => box.classList.add("ok");
    box.replaceChildren(img);
  } else {
    box.textContent = "No preview";
  }
}

let gridKey = null;
function renderGrid() {
  const grid = $("#pb-grid");
  if (grid.hidden) return;
  const files = PB.cams.flatMap(dayFiles).sort((a, b) => a.start - b.start);
  // Rebuild only when the day's files change; the 10 s refresh would otherwise reload every image.
  const key = JSON.stringify([PB.day, files.map(thumbKey), PB.cur?.name]);
  if (key === gridKey) return;
  gridKey = key;
  thumbQueue.length = 0;
  thumbObserver.disconnect();
  if (!files.length) { grid.replaceChildren(el("p", "pb-grid-empty", "No recordings this day.")); return; }
  grid.replaceChildren(...files.map((f) => {
    const card = el("button", "pb-card" + (f === PB.cur ? " current" : ""));
    card._file = f;
    card.title = `Play ${camLabel(f.cam)} from ${fmtTime(f.start)}`;
    card.append(el("span", "pb-thumb"));
    const meta = el("span", "pb-card-meta");
    meta.append(Object.assign(el("strong"), { textContent: camLabel(f.cam) }), Object.assign(el("span"), { textContent: `${fmtTime(f.start)} · ${fmtDur(f.end - f.start)}` }));
    card.append(meta);
    card.onclick = () => { setGridMode(false); openFile(f, 0, true); };
    if (thumbs.has(thumbKey(f))) showThumb(card, thumbs.get(thumbKey(f)));
    else thumbObserver.observe(card);
    return card;
  }));
}

function setGridMode(on) {
  $("#pb-grid").hidden = !on;
  $("#pb-player").hidden = on;
  $("#pb-mode-player").setAttribute("aria-pressed", String(!on));
  $("#pb-mode-grid").setAttribute("aria-pressed", String(on));
  if (on) { video.pause(); gridKey = null; renderGrid(); }
}

function renderFiles() {
  $("#pb-files-title").textContent = PB.cam ? `${camLabel(PB.cam)} · ${fmtDay(PB.day)}` : "Recordings";
  const files = PB.cam ? dayFiles(PB.cam) : [];
  const list = $("#pb-file-list");
  if (!files.length) { list.replaceChildren(el("li", "pb-empty", "Nothing recorded this day.")); return; }
  list.replaceChildren(...files.map((f) => {
    const li = el("li", "pb-file" + (f === PB.cur ? " current" : ""));
    li.tabIndex = 0;
    li.append(Object.assign(el("span", "pb-file-time"), { textContent: `${fmtTime(f.start)} – ${fmtTime(f.end)}` }));
    li.append(Object.assign(el("span", "pb-file-meta"), { textContent: `${fmtDur(f.end - f.start)} · ${fmtBytes(f.size)}` }));
    const menu = el("span", "device-menu");
    const reveal = Object.assign(el("button"), { textContent: "Show in folder" });
    reveal.onclick = (e) => { e.stopPropagation(); call("reveal_recording", { name: f.name }); };
    const del = Object.assign(el("button", "danger"), { textContent: "Delete" });
    del.onclick = (e) => {
      e.stopPropagation();
      if (!confirm(`Delete the ${camLabel(f.cam)} recording from ${fmtDay(f.start)} ${fmtTime(f.start)}? This can't be undone.`)) return;
      if (f === PB.cur) { video.removeAttribute("src"); video.load(); PB.cur = null; showMsg("Recording deleted."); }
      call("delete_recording", { name: f.name }).finally(loadRecordings);
    };
    menu.append(reveal, del);
    li.append(menu);
    li.onclick = () => openFile(f, 0, true);
    li.onkeydown = (e) => { if (e.key === "Enter") openFile(f, 0, true); };
    return li;
  }));
  list.querySelector(".current")?.scrollIntoView({ block: "nearest" });
}

function renderTimeline() {
  const { v0, v1 } = PB, span = v1 - v0;
  const x = (t) => `${((t - v0) / span) * 100}%`;
  const width = (a, b) => `max(2px, ${((b - a) / span) * 100}%)`;
  const step = TICKS.find((s) => span / s <= 12) ?? TICKS.at(-1);
  const scale = el("div", "tl-scale");
  for (let t = PB.day + Math.ceil((v0 - PB.day) / step) * step; t <= v1; t += step) {
    if ((t - v0) / span < 0.03 || (v1 - t) / span < 0.03) continue; // edge labels would be cut off
    const tick = Object.assign(el("span", "tl-tick"), { textContent: fmtTime(t, step < MIN) });
    tick.style.left = x(t);
    scale.append(tick);
  }
  const cursor = el("span", "tl-cursor");
  cursor.hidden = true;
  scale.append(cursor);
  $("#tl-ruler").replaceChildren(scale);
  PB.heads = [];

  const now = Date.now();
  const rows = PB.cams.length ? PB.cams : [null];
  $("#tl-rows").replaceChildren(...rows.map((cam) => {
    const row = el("div", "tl-row" + (cam && cam === PB.cam ? " selected" : ""));
    const label = Object.assign(el("button", "tl-label"), { textContent: cam ? camLabel(cam) : "No recordings" });
    label.disabled = !cam;
    label.onclick = () => { PB.cam = cam; renderPlayback(); };
    const track = el("div", "tl-track");
    track.dataset.cam = cam ?? "";
    for (const f of cam ? camFiles(cam) : []) {
      if (f.end <= v0 || f.start >= v1) continue;
      const seg = el("span", "tl-seg" + (f === PB.cur ? " current" : ""));
      seg.style.left = x(Math.max(f.start, v0));
      seg.style.width = width(Math.max(f.start, v0), Math.min(f.end, v1));
      track.append(seg);
    }
    if (cam && PB.cur?.cam === cam && PB.mark[0] != null) {
      const a = PB.cur.start + PB.mark[0] * SEC, b = PB.mark[1] != null ? PB.cur.start + PB.mark[1] * SEC : a;
      const m = el("span", "tl-mark");
      m.style.left = x(a);
      m.style.width = width(a, b);
      track.append(m);
    }
    if (now > v0 && now < v1) { const n = el("span", "tl-now"); n.style.left = x(now); track.append(n); }
    const head = el("span", "tl-head");
    track.append(head);
    PB.heads.push(head);
    row.append(label, track);
    return row;
  }));
}

// Cheap updates that rebuild nothing: clock, play button, playhead.
function tick() {
  const t = playTime();
  $("#pb-clock").textContent = t == null ? "" : `${fmtDay(t)}  ${fmtTime(t)}`;
  const play = $("#pb-play"), paused = video.paused;
  if (play.dataset.paused !== String(paused)) {
    play.dataset.paused = String(paused);
    play.innerHTML = paused ? '<svg viewBox="0 0 24 24"><path d="M8 5v14l11-7z"/></svg>' : '<svg viewBox="0 0 24 24"><path d="M8 5v14M16 5v14"/></svg>';
    play.title = paused ? "Play (Space)" : "Pause (Space)";
    play.setAttribute("aria-label", paused ? "Play" : "Pause");
  }
  if (t == null) { for (const h of PB.heads) h.hidden = true; return; }
  // Keep the playhead on screen while playing: page the view forward (into the next day if need be).
  if (!paused && (t > PB.v1 || t < PB.v0)) {
    PB.day = dayOf(t);
    setViewWindow(t - (PB.v1 - PB.v0) * 0.1, PB.v1 - PB.v0);
    renderPlayback();
    return;
  }
  const left = `${((t - PB.v0) / (PB.v1 - PB.v0)) * 100}%`;
  for (const h of PB.heads) { h.hidden = t < PB.v0 || t > PB.v1; h.style.left = left; }
}

// ---------- playback: control ----------
const fileAt = (cam, t) => PB.files.find((f) => f.cam === cam && f.start <= t && t < f.end);
const nextFile = (f) => PB.files.find((g) => g.cam === f.cam && g.start > f.start);
const prevFile = (f) => PB.files.findLast((g) => g.cam === f.cam && g.start < f.start);

// Play `cam` at wall time `t`, skipping ahead to its next recording when `t` falls in a gap.
function playAt(cam, t, play = true) {
  PB.cam = cam;
  const f = fileAt(cam, t) || PB.files.find((g) => g.cam === cam && g.start >= t);
  if (!f) { toast(`No ${camLabel(cam)} recording after ${fmtTime(t)}`); renderPlayback(); return; }
  openFile(f, Math.max(0, (t - f.start) / SEC), play);
}

function openFile(f, offset, play) {
  PB.cam = f.cam;
  const at = f.start + offset * SEC;
  if (dayOf(at) !== PB.day) setDay(dayOf(at), at);
  if (PB.cur === f && video.readyState > 0) {
    video.currentTime = offset;
    if (play) video.play().catch(() => {});
  } else {
    PB.cur = f;
    PB.mark = [null, null];
    PB.seek = offset;
    PB.autoplay = play;
    showMsg("");
    video.src = fileSrc(f.name);
  }
  renderPlayback();
}

video.onloadedmetadata = () => {
  const f = PB.cur;
  if (f && Number.isFinite(video.duration) && video.duration > 0) f.end = f.start + video.duration * SEC;
  video.currentTime = Math.min(PB.seek, video.duration || 0);
  if (PB.autoplay) video.play().catch(() => {});
  renderPlayback();
};
video.onerror = () => {
  if (!video.getAttribute("src")) return;
  showMsg(video.error?.code === 4
    ? "Can't play this file. It may still be recording, have been cut off, or use a codec this PC can't decode. Use Show in folder to open it in another player."
    : "Couldn't load this recording.");
};
// Segments play back to back, so a long recording plays through.
video.onended = () => { const n = PB.cur && nextFile(PB.cur); if (n) openFile(n, 0, true); else tick(); };
video.ontimeupdate = tick;
video.onplay = video.onpause = tick;
video.onclick = () => act("play");

function seekBy(secs) {
  const t = playTime();
  if (t == null) return;
  const to = video.currentTime + secs;
  if (to >= 0 && to < (video.duration || 0)) video.currentTime = to;
  else playAt(PB.cur.cam, t + secs * SEC, !video.paused);
}

function setSpeed(s) {
  PB.speed = s;
  video.defaultPlaybackRate = video.playbackRate = s; // the default survives loading the next segment
  $("#pb-speed").value = String(s);
}

function act(a) {
  if (!$("#pb-grid").hidden) setGridMode(false); // transport controls act on the player, so show it
  const f = PB.cur;
  if (!f) {
    const last = PB.cam && camFiles(PB.cam).at(-1);
    if (a === "play" && last) openFile(last, 0, true);
    return;
  }
  switch (a) {
    case "play": if (video.paused) video.play().catch(() => {}); else video.pause(); break;
    case "back": seekBy(-10); break;
    case "fwd": seekBy(10); break;
    case "frame-back": video.pause(); video.currentTime = Math.max(0, video.currentTime - FRAME); break;
    case "frame-fwd": video.pause(); video.currentTime += FRAME; break;
    case "prev": { const p = prevFile(f); if (p) openFile(p, 0, !video.paused); else video.currentTime = 0; break; }
    case "next": { const n = nextFile(f); if (n) openFile(n, 0, !video.paused); break; }
    case "in": PB.mark = [video.currentTime, PB.mark[1] > video.currentTime ? PB.mark[1] : null]; renderPlayback(); break;
    case "out": PB.mark = [PB.mark[0] != null && PB.mark[0] < video.currentTime ? PB.mark[0] : 0, video.currentTime]; renderPlayback(); break;
    case "export":
      toast("Exporting clip…");
      call("export_clip", { name: f.name, from: PB.mark[0], to: PB.mark[1] }).then((p) => toast(`Clip saved: ${p}`));
      break;
    case "still": call("recording_still", { name: f.name, at: video.currentTime }).then((p) => toast(`Snapshot saved: ${p}`)); break;
    case "full":
      if (document.fullscreenElement) document.exitFullscreen();
      else $("#pb-player").requestFullscreen?.().catch(() => {});
      break;
  }
}

function setupTimeline() {
  const rows = $("#tl-rows");
  let drag = null;
  const timeAt = (track, clientX) => { const r = track.getBoundingClientRect(); return PB.v0 + ((clientX - r.left) / r.width) * (PB.v1 - PB.v0); };
  const redraw = () => { renderTimeline(); tick(); };
  rows.addEventListener("pointerdown", (e) => {
    const track = e.target.closest(".tl-track");
    if (!track || e.button !== 0) return;
    track.setPointerCapture(e.pointerId);
    drag = { track, x: e.clientX, v0: PB.v0, moved: false };
  });
  rows.addEventListener("pointermove", (e) => {
    const track = drag?.track || e.target.closest(".tl-track");
    const cursor = $(".tl-cursor");
    if (track && cursor) {
      const t = timeAt(track, e.clientX);
      cursor.textContent = fmtTime(t);
      cursor.style.left = `${((t - PB.v0) / (PB.v1 - PB.v0)) * 100}%`;
      cursor.hidden = t < PB.v0 || t > PB.v1;
    }
    if (!drag) return;
    const dx = e.clientX - drag.x;
    if (Math.abs(dx) > 4) drag.moved = true;
    if (drag.moved) {
      const span = PB.v1 - PB.v0;
      setViewWindow(drag.v0 - (dx / drag.track.getBoundingClientRect().width) * span, span);
      redraw();
    }
  });
  rows.addEventListener("pointerleave", () => { const c = $(".tl-cursor"); if (c) c.hidden = true; });
  rows.addEventListener("pointerup", (e) => {
    if (!drag) return;
    const { track, moved } = drag;
    drag = null;
    if (!moved && track.dataset.cam) playAt(track.dataset.cam, timeAt(track, e.clientX), true);
  });
  rows.addEventListener("wheel", (e) => {
    const track = e.target.closest(".tl-track") || rows.querySelector(".tl-track");
    if (!track) return;
    e.preventDefault();
    const t = timeAt(track, e.clientX), span = PB.v1 - PB.v0;
    const next = Math.max(MIN, Math.min(span * (e.deltaY > 0 ? 1.25 : 0.8), addDays(PB.day, 1) - PB.day));
    setViewWindow(t - (t - PB.v0) * (next / span), next);
    redraw();
  }, { passive: false });
  for (const b of document.querySelectorAll(".tl-zoom button")) {
    b.onclick = () => {
      const span = +b.dataset.span * SEC, t = playTime();
      const center = t != null && dayOf(t) === PB.day ? t : (PB.v0 + PB.v1) / 2;
      setViewWindow(center - span / 2, span);
      redraw();
    };
  }
}

function setView(playback) {
  PB.on = playback;
  document.body.classList.toggle("playback", playback);
  $("#nvr").hidden = !playback;
  $("#view-live").setAttribute("aria-pressed", String(!playback));
  $("#view-playback").setAttribute("aria-pressed", String(playback));
  // The live grid is off screen during playback, so stop its streams (scheduled recording carries on).
  invoke("set_live_paused", { paused: playback }).catch(() => {});
  clearInterval(PB.timer);
  if (playback) {
    loadRecordings();
    PB.timer = setInterval(loadRecordings, 10_000); // new segments show up while cameras record
  } else {
    video.pause();
    if (document.fullscreenElement) document.exitFullscreen();
  }
  try { localStorage.setItem("view", playback ? "playback" : "live"); } catch { /* ignore */ }
}

function setupPlayback() {
  $("#view-live").onclick = (e) => { e.stopPropagation(); setView(false); };
  $("#view-playback").onclick = (e) => { e.stopPropagation(); setView(true); };
  $("#pb-media").onclick = () => call("open_media_folder");
  $("#pb-mode-player").onclick = () => setGridMode(false);
  $("#pb-mode-grid").onclick = () => setGridMode(true);
  $("#pb-speed").append(...SPEEDS.map((s) => Object.assign(el("option"), { value: String(s), textContent: `${s}×` })));
  $("#pb-speed").value = "1";
  $("#pb-speed").onchange = (e) => setSpeed(+e.target.value);
  for (const b of document.querySelectorAll("#pb-transport [data-act]")) b.onclick = () => act(b.dataset.act);
  $("#pb-day").onchange = (e) => {
    if (!e.target.value) return;
    const [y, m, d] = e.target.value.split("-");
    setDay(new Date(+y, m - 1, +d).getTime());
    renderPlayback();
  };
  $("#pb-prev-day").onclick = () => { setDay(addDays(PB.day, -1)); renderPlayback(); };
  $("#pb-next-day").onclick = () => { setDay(addDays(PB.day, 1)); renderPlayback(); };
  $("#pb-latest").onclick = () => {
    const last = (PB.cam && camFiles(PB.cam).at(-1)) || PB.files.at(-1);
    if (last) openFile(last, 0, true);
  };
  setupTimeline();
  document.addEventListener("keydown", (e) => {
    if (!PB.on || document.querySelector("dialog[open]") || e.target.closest?.("input, select, textarea") || e.ctrlKey || e.metaKey || e.altKey) return;
    const keys = { " ": "play", ArrowLeft: "back", ArrowRight: "fwd", ",": "frame-back", ".": "frame-fwd", i: "in", o: "out", f: "full" };
    const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
    if (keys[k]) { e.preventDefault(); act(keys[k]); }
    else if (k === "[" || k === "]") setSpeed(SPEEDS[Math.max(0, Math.min(SPEEDS.length - 1, SPEEDS.indexOf(PB.speed) + (k === "]" ? 1 : -1)))]);
  });
  let resume = false;
  try { resume = localStorage.getItem("view") === "playback"; } catch { /* storage unavailable */ }
  if (resume) setView(true);
}

// ---------- refresh loop ----------
async function refresh() {
  try {
    state = await invoke("get_state");
  } catch (e) {
    return;
  }
  checkMotion();
  $("#rec-paused").hidden = !state.recording_paused;
  for (const d of state.devices) if (d.message) toast(d.message);
  for (const b of document.querySelectorAll(".layout-btn")) {
    b.setAttribute("aria-checked", String(+b.dataset.n === state.layout));
  }
  renderDevices();
  renderTiles();
  renderPtz();
}

document.addEventListener("keydown", (e) => {
  // In full screen, Esc leaves full screen first (setupView).
  if (e.key === "Escape" && solo != null && !$("#device-dialog").open && !document.body.classList.contains("fullscreen")) setSolo(null);
});
$("#media-folder").onclick = () => call("open_media_folder");

buildLayouts();
setupView();
window.__TAURI__?.app?.getVersion().then((v) => { $("#app-version").textContent = `v${v}`; }).catch(() => {});
setupPtz();
setupDialog();
refresh();
setInterval(refresh, 800);
$("#rescan").onclick = () => scanLan();
setupSettings();
setupPlayback();
scanLan(true);
setInterval(() => scanLan(true), 30_000);

// ---------- demo data when opened outside Tauri (design preview only) ----------
function demoInvoke(cmd, args) {
  const demo = demoInvoke.state ||= {
    devices: [
      { id: 1, name: "Front Yard", online: true, status: "Online", battery: 100, charging: false, wifi_dbm: -70, wifi_level: 1, live: "sub", clear: false, recording: true, motion: true, motion_events: 3 },
      { id: 2, name: "Driveway", online: true, status: "Online", battery: 64, charging: true, wifi_dbm: -45, wifi_level: 2, live: "main", clear: true, recording: false },
      { id: 3, name: "Back Gate", online: false, status: "Reconnecting in 4s: connect: Timed out", battery: 12, charging: false, live: null, clear: false, recording: false },
      { id: 4, name: "Garage NVR ch1", online: true, status: "Online", battery: null, charging: false, live: null, clear: false, recording: false },
    ],
    layout: 2, tiles: [1, 2, null, null, ...Array(12).fill(null)], stream_base: "", token: "",
  };
  if (cmd === "get_prefs") return Promise.resolve({ snapshot_dir: "C:\\Users\\you\\Videos\\Reolink-rs", record_dir: "C:\\Users\\you\\Videos\\Reolink-rs", segment_minutes: 30, space_warning_gb: 5, hardware_decode: true, scan_network: true, run_at_startup: false, alarm_beep: true, alarm_volume: 60, local_record: "motion", record_schedule: "1".repeat(168) });
  if (cmd === "camera_pages") return Promise.resolve([{ id: "display", title: "Display" }, { id: "motion", title: "Motion" }, { id: "privacy", title: "Privacy masks" }, { id: "schedules", title: "Schedules" }, { id: "email", title: "Email" }]);
  const day = "0".repeat(8) + "1".repeat(12) + "0".repeat(4);
  const rich = {
    motion: [{ key: "md_sens", label: "Sensitivity", kind: "range", value: "40", display: "40", min: 1, max: 50, options: [] },
      { key: "md_zone", label: "Detection area", kind: "zone", value: JSON.stringify({ w: 80, h: 22, cells: "0".repeat(80 * 6) + "1".repeat(80 * 16) }), display: "", options: [] }],
    privacy: [{ key: "masks", label: "Privacy masks", kind: "masks", value: JSON.stringify({ enable: "1", max: 8, rects: [[600, 80, 250, 200]] }), display: "", options: [] }],
    schedules: [{ key: "push_sched", label: "Push notifications", kind: "schedule", value: JSON.stringify({ motion: day.repeat(7), timing: "0".repeat(168) }), display: "", options: [] }],
    email: [{ key: "smtp_host", label: "SMTP server", kind: "text", value: "smtp.example.com", display: "", max_len: 127, options: [] },
      { key: "smtp_pass", label: "Password", kind: "password", value: "", display: "", max_len: 31, options: [] },
      { key: "ip", label: "Static IP", kind: "ipv4", value: "192.168.1.50", display: "", options: [] }],
  };
  if (cmd === "camera_settings" && rich[args.page]) return Promise.resolve({ id: args.page, title: "", fields: rich[args.page] });
  if (cmd === "camera_snapshot") return Promise.reject("demo");
  if (cmd === "camera_settings") return Promise.resolve({ id: args.page, title: "", fields: [
    { key: "osd_name", label: "Camera name on video", kind: "text", value: "Front door", display: "Front door", max_len: 31, options: [] },
    { key: "osd_time_on", label: "Show date and time", kind: "toggle", value: "1", display: "1", options: [] },
    { key: "bright", label: "Brightness", kind: "range", value: "128", display: "128", min: 0, max: 255, options: [] },
    { key: "daynight", label: "Day / night", kind: "choice", value: "color", display: "Always color", options: [{ value: "auto", label: "Auto" }, { value: "color", label: "Always color" }] },
    { key: "model", label: "Model", kind: "info", value: "Argus 4 Pro", display: "Argus 4 Pro", options: [] }] });
  if (cmd === "list_recordings") {
    // A day of motion clips plus one continuous camera, all named after demo.mp4 next to the page.
    const day = new Date().setHours(0, 0, 0, 0), files = [];
    const name = (cam, t) => { const d = new Date(t), p = (n) => String(n).padStart(2, "0"); return `${cam}_${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}_${p(d.getHours())}-${p(d.getMinutes())}-${p(d.getSeconds())}.mp4`; };
    for (let h = 6; h < 22; h += 1.7) files.push({ name: name("Front_Yard", day + h * 3600e3), size: 38e6, modified: day + h * 3600e3 + 9 * 60e3 });
    for (let h = 0; h < 12; h += 0.5) files.push({ name: name("Driveway", day + h * 3600e3), size: 410e6, modified: day + (h + 0.5) * 3600e3 });
    return Promise.resolve({ dir: "", files });
  }
  if (cmd === "scan_lan") return Promise.resolve([{ uid: "95270000EXAMPLEA", ip: "192.168.1.254", battery: 99, added: false }]);
  if (cmd === "set_layout") demo.layout = args.layout;
  if (cmd === "assign_tile") demo.tiles[args.tile] = args.id;
  return Promise.resolve(cmd === "get_state" ? structuredClone(demo) : null);
}
