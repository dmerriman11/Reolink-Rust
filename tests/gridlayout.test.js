// `node --test tests/gridlayout.test.js`
const test = require("node:test");
const assert = require("node:assert");
const { followCameras, resizeTracks, validTracks, clampZoom, zoomAround } = require("../ui/gridlayout.js");

const records = (...devs) => devs.map((dev) => ({ dev }));
const devs = (tiles) => tiles.map((t) => t.dev);

test("a swap moves the records, so both players survive", () => {
  const tiles = records(1, 2, 3, 4);
  const [a, c] = [tiles[0], tiles[2]];
  assert.equal(followCameras(tiles, [3, 2, 1, 4], 4), true);
  assert.equal(tiles[0], c);
  assert.equal(tiles[2], a);
});

test("a camera moved to an empty tile takes its record there", () => {
  const tiles = records(1, null);
  assert.equal(followCameras(tiles, [null, 1], 2), true);
  assert.deepEqual(devs(tiles), [null, 1]);
});

test("nothing moves when the cameras are where they were", () => {
  assert.equal(followCameras(records(1, null, 3), [1, null, 3], 3), false);
});

test("a new camera leaves the record it replaces in place", () => {
  const tiles = records(1, 2);
  assert.equal(followCameras(tiles, [5, 2], 2), false);
  assert.deepEqual(devs(tiles), [1, 2]);
});

test("records past the layout are not used", () => {
  const tiles = records(1, 2, 3);
  assert.equal(followCameras(tiles, [3, 2], 2), false);
});

test("a border only moves its two tracks and stops at the minimum", () => {
  assert.deepEqual(resizeTracks([400, 400, 400], 0, 100, 120), [500, 300, 400]);
  assert.deepEqual(resizeTracks([400, 400, 400], 0, 1000, 120), [680, 120, 400]);
  assert.deepEqual(resizeTracks([400, 400, 400], 1, -1000, 120), [400, 120, 680]);
});

test("the minimum adapts to a small window", () => {
  assert.deepEqual(resizeTracks([100, 100], 0, -1000, 120), [50, 150]);
});

test("a pair already below the minimum doesn't jump", () => {
  assert.deepEqual(resizeTracks([60, 60, 1000], 0, 30, 120), [60, 60, 1000]);
});

test("a zoomed picture stays inside the tile", () => {
  assert.deepEqual(clampZoom(2, 0.4, 0.6), { s: 2, x: 0.4, y: 0.6 });
  assert.deepEqual(clampZoom(2, 0.1, 0.9), { s: 2, x: 0.25, y: 0.75 });
  assert.deepEqual(clampZoom(1, 0, 1), { s: 1, x: 0.5, y: 0.5 });
});

test("zooming keeps the point under the pointer in place", () => {
  const screen = (z, p, axis) => 0.5 + (p - z[axis]) * z.s;
  const before = { s: 2, x: 0.4, y: 0.5 };
  const after = zoomAround(before, 0.45, 0.55, 1.5, 8);
  assert.equal(after.s, 3);
  assert.ok(Math.abs(screen(after, 0.45, "x") - screen(before, 0.45, "x")) < 1e-9);
  assert.ok(Math.abs(screen(after, 0.55, "y") - screen(before, 0.55, "y")) < 1e-9);
});

test("zoom stops at its limits, and all the way out is the whole picture", () => {
  assert.equal(zoomAround({ s: 6, x: 0.5, y: 0.5 }, 0.5, 0.5, 2, 8).s, 8);
  assert.deepEqual(zoomAround({ s: 2, x: 0.3, y: 0.7 }, 0.2, 0.8, 0.1, 8), { s: 1, x: 0.5, y: 0.5 });
});

test("bad saved sizes are refused", () => {
  assert.equal(validTracks([1, 2], 2), true);
  for (const bad of [null, "12", [1], [1, 0], [1, -1], [1, NaN], [1, "2"]]) assert.equal(validTracks(bad, 2), false);
});
