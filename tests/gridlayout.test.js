// `node --test tests/gridlayout.test.js`
const test = require("node:test");
const assert = require("node:assert");
const { followCameras, resizeTracks, validTracks } = require("../ui/gridlayout.js");

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

test("bad saved sizes are refused", () => {
  assert.equal(validTracks([1, 2], 2), true);
  for (const bad of [null, "12", [1], [1, 0], [1, -1], [1, NaN], [1, "2"]]) assert.equal(validTracks(bad, 2), false);
});
