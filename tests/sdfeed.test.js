"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const { canFeed, pendingRanges, feedComplete, fallback } = require("../ui/sdfeed.js");

const v = { path: "c/x.partial.video.mp4", mime: 'video/mp4; codecs="hvc1.1.6.L150.90"', bytes: 3000 };
const a = { path: "c/x.partial.audio.mp4", mime: 'audio/mp4; codecs="mp4a.40.2"', bytes: 500 };

test("feeds only when every track is playable here", () => {
  assert.equal(canFeed({ tracks: [v, a] }, () => true), true);
  assert.equal(canFeed({ tracks: [v, a] }, (m) => m.startsWith("video")), false);
  assert.equal(canFeed({ tracks: [] }, () => true), false);
});

test("pending ranges follow what has been appended, per track", () => {
  assert.deepEqual(pendingRanges({ tracks: [v, a] }, {}), [
    { path: v.path, mime: v.mime, from: 0, to: 3000 },
    { path: a.path, mime: a.mime, from: 0, to: 500 },
  ]);
  assert.deepEqual(pendingRanges({ tracks: [v, a] }, { [v.path]: 3000, [a.path]: 200 }), [{ path: a.path, mime: a.mime, from: 200, to: 500 }]);
  assert.deepEqual(pendingRanges({ tracks: [v, a] }, { [v.path]: 3000, [a.path]: 500 }), []);
});

test("the stream ends once the download is done and fully appended", () => {
  const have = { [v.path]: 3000, [a.path]: 500 };
  assert.equal(feedComplete({ tracks: [v, a], done: false }, have), false);
  assert.equal(feedComplete({ tracks: [v, a], done: true }, { [v.path]: 2000 }), false);
  assert.equal(feedComplete({ tracks: [v, a], done: true }, have), true);
  assert.equal(feedComplete({ tracks: [v, a], done: true, error: "x" }, have), false);
});

test("without fragments the page waits for the file, or reports the error", () => {
  assert.equal(fallback({ tracks: [], done: false }), "wait");
  assert.equal(fallback({ tracks: [], done: true, file: "c/x.mp4" }), "file");
  assert.equal(fallback({ tracks: [v], done: true, file: "c/x.mp4" }), null);
  assert.equal(fallback({ tracks: [], done: true, error: "camera is offline" }), "error");
  assert.equal(fallback({ tracks: [v], done: false }), null);
});
