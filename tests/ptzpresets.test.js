const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const source = fs.readFileSync(require.resolve("../ui/app.js"), "utf8");

function harness(call) {
  const nodes = new Map();
  const node = () => ({ hidden: false, disabled: false, value: "", textContent: "", children: [],
    replaceChildren() { this.children = []; }, append(child) { this.children.push(child); },
    setAttribute() {} });
  const $ = (key) => { if (!nodes.has(key)) nodes.set(key, node()); return nodes.get(key); };
  const context = vm.createContext({ $, call, document: { createElement: node },
    selected: 0, state: { tiles: [1, 2] }, device: (id) => ({ id, online: true }),
    ptzEl: { parentNode: null, querySelector: $ }, tiles: [{ root: { append() {} } }, { root: { append() {} } }] });
  vm.runInContext(source.slice(source.indexOf("function renderPtz()"), source.indexOf("// ---------- device panel")), context);
  vm.runInContext("renderPtz()", context);
  return { context, $, run: (code) => vm.runInContext(code, context) };
}

test("late preset response cannot populate a different camera", async () => {
  let finish;
  const h = harness(() => new Promise((r) => { finish = r; }));
  const pending = h.run('runPreset("ptz_presets")');
  h.run("selected = 1; renderPtz()");
  finish([{ id: 1, name: "First camera" }]);
  await pending;
  assert.equal(h.$("#preset-form").hidden, true);
  assert.equal(h.$("#preset-list").children.length, 0);
  assert.equal(h.$("#preset-refresh").disabled, false);
});

test("preset names use text, controls recover after failure, duplicate submissions are ignored", async () => {
  let finish, calls = 0;
  const h = harness(() => { calls++; return new Promise((r) => { finish = r; }); });
  const pending = h.run('runPreset("ptz_presets")');
  await h.run('runPreset("ptz_presets")');
  assert.equal(calls, 1);
  finish([{ id: 1, name: "<img src=x>" }]);
  await pending;
  assert.equal(h.$("#preset-list").children[0].textContent, "<img src=x>");
  h.context.call = async () => { throw new Error("camera refused"); };
  h.$("#preset-name").value = "Keep my draft";
  await h.run('runPreset("ptz_preset_save", { name: "Keep my draft" })');
  assert.match(h.$("#preset-status").textContent, /camera refused/);
  assert.equal(h.$("#preset-controls").disabled, false);
  assert.equal(h.$("#preset-name").value, "Keep my draft");
});

test("demo presets persist per camera and fill the lowest free slot", async () => {
  const context = vm.createContext({ TextEncoder });
  vm.runInContext(source.slice(source.indexOf("function demoInvoke(")), context);
  const invoke = (cmd, args) => context.demoInvoke(cmd, args);
  assert.equal((await invoke("ptz_presets", { id: 1 })).length, 1);
  const saved = await invoke("ptz_preset_save", { id: 1, name: " Yard " });
  assert.equal(saved[1].id, 0);
  assert.equal(saved[1].name, "Yard");
  assert.equal((await invoke("ptz_presets", { id: 2 })).length, 1);
  await assert.rejects(invoke("ptz_preset_go", { id: 1, presetId: 64 }));
  await assert.rejects(invoke("ptz_preset_save", { id: 1, name: " " }));
  await assert.rejects(invoke("ptz_presets", { id: 3 }));
});


test("preset delete requires confirmation and rename cancellation does not write", async () => {
  let calls = 0;
  const h = harness(async () => { calls++; return [{ id: 0, name: "Gate" }]; });
  h.context.confirm = () => false;
  h.context.prompt = () => null;
  await h.run('runPreset("ptz_presets")');
  h.$("#preset-list").children[1].onclick();
  h.$("#preset-list").children[2].onclick();
  assert.equal(calls, 1);
  h.context.confirm = () => true;
  h.$("#preset-list").children[2].onclick();
  assert.equal(calls, 2);
});

test("demo rename/delete preserve other positions and reject stale IDs", async () => {
  const context = vm.createContext({ TextEncoder });
  vm.runInContext(source.slice(source.indexOf("function demoInvoke(")), context);
  const invoke = (cmd, args) => context.demoInvoke(cmd, { id: 1, ...args });
  await invoke("ptz_preset_save", { name: "Gate" });
  const renamed = await invoke("ptz_preset_rename", { presetId: 0, name: "Side gate" });
  assert.equal(renamed.find(p => p.id === 0).name, "Side gate");
  await assert.rejects(invoke("ptz_preset_rename", { presetId: 0, name: "x".repeat(32) }));
  const remaining = await invoke("ptz_preset_delete", { presetId: 0 });
  assert.equal(remaining.length, 1);
  assert.equal(remaining[0].id, 1);
  await assert.rejects(invoke("ptz_preset_delete", { presetId: 0 }));
});

test("lens controls stay hidden without advertised capability", () => {
  const h = harness(async () => []);
  assert.equal(h.$("#lens-zoom").hidden, true);
  assert.equal(h.$("#lens-focus").hidden, true);
  h.context.device = id => ({ id, online: true, lens_zoom: true, lens_focus: true });
  h.run("renderPtz()");
  assert.equal(h.$("#lens-zoom").hidden, false);
  assert.equal(h.$("#lens-focus").hidden, false);
});
