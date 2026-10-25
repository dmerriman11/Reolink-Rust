// Writes still screenshots to docs/screenshots/: same demo UI and synthetic feeds as record.cjs.
// Run from the repo root:  NODE_PATH=<dir with playwright> node docs/demo/screenshots.cjs
const { chromium } = require("playwright");
const fs = require("fs");
const path = require("path");
const { INIT, serve, W, H, setGateOnline } = require("./record.cjs");

const OUT = path.resolve(__dirname, "../screenshots");

(async () => {
  setGateOnline(true);
  const srv = await serve();
  fs.mkdirSync(OUT, { recursive: true });
  const browser = await chromium.launch({ channel: "msedge" });
  const page = await browser.newPage({ viewport: { width: W, height: H }, deviceScaleFactor: 2 });
  await page.addInitScript(INIT + `addEventListener("DOMContentLoaded", () => document.getElementById("demo-cursor").remove());`);
  await page.goto(`http://127.0.0.1:${srv.address().port}/`);
  await page.waitForSelector(".tile img.ok");
  await page.evaluate(() => Object.assign(demoInvoke.state.devices[2], { online: true, status: "Online", live: "sub" }));
  const shot = async (name) => { await page.mouse.move(W - 2, H - 2); await page.waitForTimeout(900); await page.screenshot({ path: path.join(OUT, name) }); console.log(name); };

  await page.click('.layout-btn[data-n="4"]');
  for (const name of ["Garage NVR ch1", "Back Gate"]) await page.locator(".device", { hasText: name }).locator(".dot").click();
  await page.waitForFunction(() => document.querySelectorAll(".tile img.ok").length >= 4);
  await shot("grid.png");

  await page.locator(".tile").nth(1).dblclick();
  await shot("single.png");
  await page.keyboard.press("Escape");

  await page.locator(".device", { hasText: "Front Yard" }).locator(".dot").hover();
  await page.locator(".device", { hasText: "Front Yard" }).locator(".device-menu button", { hasText: "Settings" }).click();
  await page.click('#settings-nav button:has-text("Motion")');
  await page.waitForSelector(".zone-grid");
  await shot("settings-motion.png");
  await page.click('#settings-nav button:has-text("Display")');
  await shot("settings-display.png");

  await browser.close();
  srv.close();
})();
