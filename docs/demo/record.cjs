// Records docs/demo.webp (animated; a GIF of the same clip is ~5x larger): the real UI (ui/, demo mode) with synthetic camera feeds and a visible cursor.
// Needs Playwright and ffmpeg. Run from the repo root:  NODE_PATH=<dir with playwright> node docs/demo/record.cjs
const { chromium } = require("playwright");
const { execFileSync } = require("child_process");
const fs = require("fs");
const http = require("http");
const path = require("path");

const ROOT = path.resolve(__dirname, "../..");
const OUT = path.join(ROOT, "docs", "demo.webp");
const W = 1100, H = 660;

// ---------- synthetic feeds: 640x360 SVG scenes, animated with SMIL so they move inside an <img> ----------
const MONO = "Consolas, monospace";
// OSD clock whose seconds tick: each digit is a clipped strip stepped with discrete animateTransform.
function clock(x, y, prefix, startSec, fill) {
  const cw = 14 * 0.55;
  const strip = (dx, n, dur, start) => {
    const steps = Array.from({ length: n }, (_, k) => `0 ${-18 * ((start + k) % n)}`).join(";");
    const digits = Array.from({ length: n }, (_, k) => `<tspan x="0" y="${14 + 18 * k}">${k}</tspan>`).join("");
    return `<svg x="${x + dx}" y="${y - 14}" width="${cw}" height="18"><g><animateTransform attributeName="transform" type="translate" calcMode="discrete" dur="${dur}s" repeatCount="indefinite" values="${steps}"/><text font-family="${MONO}" font-size="14" fill="${fill}">${digits}</text></g></svg>`;
  };
  const px = prefix.length * cw;
  return `<text x="${x}" y="${y}" font-family="${MONO}" font-size="14" fill="${fill}">${prefix}</text>`
    + strip(px, 6, 60, Math.floor(startSec / 10)) + strip(px + cw, 10, 10, startSec % 10);
}
function scene(body, { name, ir, sec }) {
  const ink = "#f2f2f2";
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 640 360" width="640" height="360">
<defs>
<radialGradient id="vig" cx="50%" cy="50%" r="75%"><stop offset="60%" stop-color="#000" stop-opacity="0"/><stop offset="100%" stop-color="#000" stop-opacity=".55"/></radialGradient>
<filter id="grain"><feTurbulence type="fractalNoise" baseFrequency=".9" numOctaves="2" seed="4"/><feColorMatrix values="0 0 0 0 .5  0 0 0 0 .5  0 0 0 0 .5  0 0 0 .5 0"/></filter>
</defs>
<g ${ir ? 'style="filter:grayscale(1) contrast(1.05)"' : ""}>${body}</g>
<rect width="640" height="360" filter="url(#grain)" opacity="${ir ? 0.22 : 0.1}"/>
<rect width="640" height="360" fill="url(#vig)"/>
${clock(14, 62, "10/25/2026 Sun 21:14:", sec, ink)}
<text x="626" y="346" text-anchor="end" font-family="${MONO}" font-size="14" fill="${ink}">${name}</text>
</svg>`;
}
const PERSON = `<g fill="#0c0d0d"><circle cx="0" cy="-58" r="9"/><path d="M-11 -46 h22 l4 30 h-6 l-2 -16 l-2 16 l3 46 h-8 l-4 -40 l-4 40 h-8 l3 -46 l-2 -16 l-2 16 h-6 z"/></g>`;
const SCENES = {
  1: scene(`
<rect width="640" height="360" fill="#2a2e2c"/>
<rect y="0" width="640" height="150" fill="#151716"/>
<path d="M0 150 L640 140 L640 360 L0 360 Z" fill="#4a514c"/>
<rect x="400" y="40" width="240" height="140" fill="#6c706c"/><path d="M380 50 L520 -10 L660 50 Z" fill="#383b39"/>
<rect x="470" y="88" width="44" height="92" fill="#2b2d2c"/><circle cx="530" cy="96" r="18" fill="#fffbe6" opacity=".55"/><circle cx="530" cy="96" r="5" fill="#fff"/>
<path d="M470 180 L514 180 L620 360 L380 360 Z" fill="#7b7f7b"/>
<g fill="#202321"><ellipse cx="420" cy="180" rx="34" ry="16"/><ellipse cx="590" cy="182" rx="40" ry="18"/></g>
<rect x="88" y="50" width="12" height="120" fill="#1a1c1b"/><circle cx="94" cy="58" r="58" fill="#1f2220"/>
<g transform="translate(-40 300)">
  <animateTransform attributeName="transform" type="translate" values="-40 300; 700 255" dur="9s" repeatCount="indefinite"/>
  <g><animateTransform attributeName="transform" type="translate" values="0 0;0 -3;0 0" dur=".55s" repeatCount="indefinite"/>${PERSON}</g>
</g>`, { name: "Front Yard", ir: true, sec: 7 }),
  2: scene(`
<defs><linearGradient id="sky" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#8fb1cf"/><stop offset="1" stop-color="#dde6ea"/></linearGradient></defs>
<rect width="640" height="200" fill="url(#sky)"/>
<path d="M0 150 q60 -40 120 -8 q50 -30 110 0 q70 -36 140 -4 q60 -30 130 2 q70 -26 140 0 V200 H0 Z" fill="#5d7a5a"/>
<rect y="190" width="640" height="170" fill="#6f8f4f"/>
<rect x="60" y="80" width="280" height="150" fill="#d8cbb0"/><path d="M40 90 L200 20 L360 90 Z" fill="#7a5d4a"/>
<rect x="110" y="130" width="180" height="100" fill="#ece8e0"/><g stroke="#cfc9bd" stroke-width="2">${[150, 170, 190, 210].map((y) => `<line x1="110" y1="${y}" x2="290" y2="${y}"/>`).join("")}</g>
<path d="M110 230 L290 230 L470 360 L-40 360 Z" fill="#b9b6b0"/>
<g transform="translate(150 250)"><rect x="0" y="20" width="200" height="50" rx="14" fill="#2e4a6b"/><path d="M30 22 L60 -8 H140 L172 22 Z" fill="#26405e"/><path d="M42 20 L66 -2 H98 V20 Z M104 20 V-2 H136 L160 20 Z" fill="#a9c2d4"/><g fill="#141414"><circle cx="45" cy="72" r="16"/><circle cx="160" cy="72" r="16"/></g><rect x="186" y="34" width="12" height="8" rx="2" fill="#f5d27a"/></g>
<g transform="translate(540 230)"><rect x="-7" y="-60" width="14" height="70" fill="#5a4330"/>
  <g><animateTransform attributeName="transform" type="rotate" values="-1.5 0 0;1.5 0 0;-1.5 0 0" dur="5s" repeatCount="indefinite"/><circle cx="0" cy="-110" r="62" fill="#3f6b38"/><circle cx="-30" cy="-80" r="40" fill="#4a7a41"/><circle cx="32" cy="-86" r="42" fill="#467339"/></g></g>`,
    { name: "Driveway", sec: 41 }),
  3: scene(`
<rect width="640" height="360" fill="#2c302d"/><rect y="230" width="640" height="130" fill="#3e443f"/>
<g fill="#5b5f5a">${Array.from({ length: 22 }, (_, i) => `<rect x="${i * 30}" y="110" width="24" height="130"/>`).join("")}</g>
<rect x="250" y="104" width="150" height="140" fill="#6d716c"/><rect x="262" y="116" width="126" height="116" fill="#474b47"/>
<circle cx="80" cy="70" r="80" fill="#1d201e"/><circle cx="600" cy="80" r="90" fill="#1d201e"/>`, { name: "Back Gate", ir: true, sec: 23 }),
  4: scene(`
<rect width="640" height="360" fill="#5c574f"/><rect y="250" width="640" height="110" fill="#77736b"/>
<ellipse cx="320" cy="0" rx="200" ry="70" fill="#fff4d6" opacity=".25"/><rect x="290" y="0" width="60" height="8" fill="#fffbe9"/>
<g fill="#3d3a35"><rect x="30" y="70" width="180" height="8"/><rect x="30" y="140" width="180" height="8"/><rect x="30" y="210" width="180" height="8"/></g>
<g><rect x="40" y="40" width="50" height="30" fill="#a8743f"/><rect x="100" y="48" width="40" height="22" fill="#4b6a8a"/><rect x="50" y="112" width="70" height="28" fill="#8d8a7e"/><rect x="130" y="104" width="44" height="36" fill="#a8743f"/><rect x="44" y="180" width="60" height="30" fill="#6b4b2e"/></g>
<rect x="380" y="170" width="220" height="16" fill="#6b4b2e"/><rect x="390" y="186" width="10" height="70" fill="#4a3a28"/><rect x="580" y="186" width="10" height="70" fill="#4a3a28"/>
<rect x="420" y="80" width="150" height="80" fill="#4a463f"/><g stroke="#2d2b27" stroke-width="3">${[440, 470, 500, 530, 550].map((x) => `<line x1="${x}" y1="92" x2="${x}" y2="140"/>`).join("")}</g>`,
    { name: "Garage", sec: 52 }),
};
const svgUrl = (id) => "data:image/svg+xml," + encodeURIComponent(SCENES[id] || SCENES[1]);

// ---------- page setup: feeds, snapshot, visible cursor ----------
const INIT = `
window.__DEMO_SCENES = ${JSON.stringify(Object.fromEntries(Object.keys(SCENES).map((id) => [id, svgUrl(id)])))};
window.__TAURI__ = { core: { invoke: (cmd, args) => cmd === "camera_snapshot"
  ? Promise.resolve(window.__DEMO_SCENES[args.id] || window.__DEMO_SCENES[1])
  : demoInvoke(cmd, args) } };
addEventListener("DOMContentLoaded", () => {
  const s = document.createElement("style");
  s.textContent = \`
    #demo-cursor { position: fixed; left: 0; top: 0; z-index: 2147483647; pointer-events: none;
      transition: transform 650ms cubic-bezier(0.77, 0, 0.175, 1); }
    #demo-cursor svg { display: block; filter: drop-shadow(0 1px 2px rgba(0,0,0,.6)); }
    .demo-ripple { position: fixed; z-index: 2147483646; pointer-events: none; width: 34px; height: 34px; margin: -17px 0 0 -17px;
      border-radius: 50%; border: 2px solid #f4b13e; transform: scale(.6); opacity: .9;
      transition: transform 380ms cubic-bezier(0.23, 1, 0.32, 1), opacity 380ms cubic-bezier(0.23, 1, 0.32, 1); }\`;
  document.head.append(s);
  const c = document.createElement("div");
  c.id = "demo-cursor";
  c.innerHTML = '<svg width="20" height="26" viewBox="0 0 20 26"><path d="M1 1 L1 20 L6 15.5 L9.5 24 L13 22.5 L9.5 14.5 L16 14.5 Z" fill="#fff" stroke="#111" stroke-width="1.4" stroke-linejoin="round"/></svg>';
  c.style.transform = "translate(${W * 0.62}px, ${H * 0.55}px)";
  document.body.append(c);
});
window.__demoMove = (x, y) => { document.getElementById("demo-cursor").style.transform = \`translate(\${x}px, \${y}px)\`; };
window.__demoRipple = (x, y) => {
  const r = document.createElement("div");
  r.className = "demo-ripple"; r.style.left = x + "px"; r.style.top = y + "px";
  document.body.append(r);
  requestAnimationFrame(() => requestAnimationFrame(() => { r.style.transform = "scale(1.4)"; r.style.opacity = "0"; }));
  setTimeout(() => r.remove(), 450);
};`;

// Static server for ui/ plus /live/<id> feeds. An offline camera's stream request is held until it reconnects.
const held = [];
let gateOnline = false;
function serve() {
  return new Promise((resolve) => {
    const srv = http.createServer((req, res) => {
      const url = new URL(req.url, "http://x");
      const live = url.pathname.match(/^\/live\/(\d+)/);
      if (live) {
        if (live[1] === "3" && !gateOnline) return held.push(res);
        res.writeHead(200, { "content-type": "image/svg+xml" });
        return res.end(SCENES[live[1]] || SCENES[1]);
      }
      const file = path.join(ROOT, "ui", url.pathname === "/" ? "index.html" : url.pathname);
      if (!file.startsWith(path.join(ROOT, "ui")) || !fs.existsSync(file)) { res.writeHead(404); return res.end(); }
      const type = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".png": "image/png", ".svg": "image/svg+xml" }[path.extname(file)];
      res.writeHead(200, { "content-type": type || "application/octet-stream" });
      fs.createReadStream(file).pipe(res);
    });
    srv.listen(0, "127.0.0.1", () => resolve(srv));
  });
}

module.exports = { INIT, SCENES, serve, W, H, setGateOnline: (v) => { gateOnline = v; } };
if (require.main === module) (async () => {
  const srv = await serve();
  const tmp = fs.mkdtempSync(path.join(require("os").tmpdir(), "reolink-demo-"));
  const browser = await chromium.launch({ channel: "msedge" }); // Edge = the WebView2 engine the app runs in
  const ctx = await browser.newContext({ viewport: { width: W, height: H }, recordVideo: { dir: tmp, size: { width: W, height: H } } });
  await ctx.addInitScript(INIT);
  const page = await ctx.newPage();
  const t0 = Date.now();
  await page.goto(`http://127.0.0.1:${srv.address().port}/`);
  await page.waitForSelector(".tile img.ok");
  await page.waitForTimeout(900); // let the entry animations settle before the clip starts

  const wait = (ms) => page.waitForTimeout(ms);
  const centerOf = async (loc) => { const b = await loc.boundingBox(); return [b.x + b.width / 2, b.y + b.height / 2]; };
  // Glide the visible cursor, then move the real mouse there (hover states follow it).
  const moveTo = async (loc) => {
    const [x, y] = await centerOf(loc);
    await page.evaluate(([x, y]) => window.__demoMove(x, y), [x, y]);
    await wait(700);
    await page.mouse.move(x, y);
    return [x, y];
  };
  const click = async (loc, opts = {}) => {
    const [x, y] = await moveTo(loc);
    await wait(150);
    await page.evaluate(([x, y]) => window.__demoRipple(x, y), [x, y]);
    await page.mouse.click(x, y, opts);
  };
  const deviceRow = (name) => page.locator(".device", { hasText: name });
  // Hovering a row slides its menu in over where the name was, so aim at the status dot on the far left.
  const deviceName = (name) => deviceRow(name).locator(".dot");

  const start = (Date.now() - t0) / 1000;
  await wait(1800);                                                    // 2 stacked: motion + REC on Front Yard
  await click(page.locator('.layout-btn[data-n="4"]'));               // 2x2 layout
  await wait(900);
  await click(deviceName("Garage NVR ch1"));                          // fill a tile from the device list
  await wait(800);
  await click(deviceName("Back Gate"));                              // offline camera...
  await wait(1400);
  gateOnline = true;
  held.splice(0).forEach((res) => { res.writeHead(200, { "content-type": "image/svg+xml" }); res.end(SCENES[3]); });
  await page.evaluate(() => Object.assign(demoInvoke.state.devices[2], { online: true, status: "Online", live: "sub", battery: 12 }));
  await wait(1500);                                                    // ...reconnects on its own
  const front = page.locator(".tile").first();
  await moveTo(front);
  await wait(150);
  await page.evaluate(([x, y]) => window.__demoRipple(x, y), await centerOf(front));
  await page.mouse.dblclick(...(await centerOf(front)));             // single view
  await wait(2000);
  await page.keyboard.press("Escape");
  await wait(700);
  await moveTo(deviceName("Front Yard"));                             // hover menu
  await wait(400);
  await click(deviceRow("Front Yard").locator(".device-menu button", { hasText: "Settings" }));
  await wait(700);
  await click(page.locator("#settings-nav button", { hasText: "Motion" }));
  await page.waitForSelector(".zone-grid");
  await wait(900);
  // Clear the area, then paint the lawn over the live snapshot in zigzag passes.
  await page.evaluate(() => document.querySelector(".frame-editor .editor-tools").scrollIntoView({ behavior: "smooth", block: "end" }));
  await wait(600);
  await click(page.locator(".frame-editor .ghost-btn", { hasText: "Clear" }));
  await wait(400);
  const g = await page.locator(".zone-grid").boundingBox();
  const at = (fx, fy) => [g.x + g.width * fx, g.y + g.height * fy];
  const first = at(0.03, 11.5 / 22);
  await page.evaluate(([x, y]) => window.__demoMove(x, y), first);
  await wait(700);
  await page.mouse.move(...first);
  await page.mouse.down(); // real press starts the paint; the drag itself runs in-page, in step with the frames
  await page.evaluate(async ({ g, rows }) => {
    const cur = document.getElementById("demo-cursor");
    cur.style.transition = "none";
    const grid = document.querySelector(".zone-grid");
    let last = null;
    const moveTo = (x, y) => {
      // Sub-steps every 3px so no cell between two frames is skipped.
      const [px, py] = last || [x, y];
      const n = Math.max(1, Math.ceil(Math.hypot(x - px, y - py) / 3));
      for (let i = 1; i <= n; i++) {
        const cx = px + (x - px) * i / n, cy = py + (y - py) * i / n;
        grid.dispatchEvent(new PointerEvent("pointermove", { clientX: cx, clientY: cy, bubbles: true }));
      }
      last = [x, y];
      cur.style.transform = `translate(${x}px, ${y}px)`;
    };
    const frame = () => new Promise(requestAnimationFrame);
    for (let r = 0; r < rows; r++) {
      const y = g.y + g.height * (11.5 + r) / 22;
      const [x0, x1] = r % 2 ? [0.97, 0.03] : [0.03, 0.97];
      const t0 = performance.now();
      for (let t = 0; t < 1; t = (performance.now() - t0) / 220) {
        moveTo(g.x + g.width * (x0 + (x1 - x0) * t), y);
        await frame();
      }
      moveTo(g.x + g.width * x1, y);
      if (r < rows - 1) moveTo(g.x + g.width * x1, g.y + g.height * (12.5 + r) / 22);
    }
    cur.style.transition = "";
  }, { g, rows: 10 });
  await page.mouse.up();
  await wait(2000);
  const end = (Date.now() - t0) / 1000;

  const video = await page.video().path();
  await ctx.close();
  await browser.close();
  srv.close();

  execFileSync("ffmpeg", ["-y", "-v", "error", "-ss", String(start), "-t", String(end - start), "-i", video,
    "-vf", "fps=15", "-c:v", "libwebp_anim", "-quality", "75", "-compression_level", "6", "-loop", "0", OUT], { stdio: "inherit" });
  fs.rmSync(tmp, { recursive: true, force: true });
  console.log(`${OUT}  ${(fs.statSync(OUT).size / 1e6).toFixed(1)} MB, ${(end - start).toFixed(1)} s`);
})();
