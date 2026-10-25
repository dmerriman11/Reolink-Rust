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
let selected = 0;
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
      const free = state.tiles.slice(0, state.layout).findIndex((t) => t == null);
      const tile = free >= 0 ? free : selected;
      selected = tile;
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
new ResizeObserver(reportSizes).observe(document.querySelector("#grid"));
function makeTile(idx) {
  const t = { root: el("div", "tile"), img: el("img"), key: null };
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
    t.root.replaceChildren();

    if (!d) {
      t.img.removeAttribute("src");
      t.root.append(el("div", "tile-empty", `${ICON.plus}<span>Pick a device</span>`));
      return;
    }
    const src = `${state.stream_base}/live/${d.id}?t=${state.token}`;
    if (t.img.dataset.src !== src) { t.img.classList.remove("ok"); t.img.src = src; t.img.dataset.src = src; }
    const loading = el("div", "tile-msg", d.online ? '<div><div class="spinner"></div>Starting stream…</div>' : "");
    if (!d.online) loading.textContent = d.status;
    t.img.onload = () => { t.img.classList.add("ok"); loading.remove(); };
    t.img.onerror = () => t.img.classList.remove("ok");
    t.root.append(loading, t.img);

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
  tiles.slice(l.n).forEach((t) => { t.img.removeAttribute("src"); t.img.dataset.src = ""; t.key = null; });
}

// ---------- PTZ ----------
function renderPtz() {
  const d = device(state.tiles[selected]);
  $("#ptz-target").textContent = d ? d.name : "Select a live tile";
  $("#dpad").classList.toggle("disabled", !d || !d.online);
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

// ---------- refresh loop ----------
async function refresh() {
  try {
    state = await invoke("get_state");
  } catch (e) {
    return;
  }
  checkMotion();
  for (const d of state.devices) if (d.message) toast(d.message);
  for (const b of document.querySelectorAll(".layout-btn")) {
    b.setAttribute("aria-checked", String(+b.dataset.n === state.layout));
  }
  renderDevices();
  renderTiles();
  renderPtz();
  reportSizes();
}

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && solo != null && !$("#device-dialog").open) setSolo(null);
});
$("#media-folder").onclick = () => call("open_media_folder");

buildLayouts();
setupPtz();
setupDialog();
refresh();
setInterval(refresh, 800);
$("#rescan").onclick = () => scanLan();
setupSettings();
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
  if (cmd === "scan_lan") return Promise.resolve([{ uid: "95270000EXAMPLEA", ip: "192.168.1.254", battery: 99, added: false }]);
  if (cmd === "set_layout") demo.layout = args.layout;
  if (cmd === "assign_tile") demo.tiles[args.tile] = args.id;
  return Promise.resolve(cmd === "get_state" ? structuredClone(demo) : null);
}
