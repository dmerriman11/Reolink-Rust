// `node --test tests/events.test.js` — motion event list and timeline marker layout.
const { test } = require("node:test");
const assert = require("node:assert/strict");
const { POST_ROLL_MS, eventEnd, filterEvents, markerLayout } = require("../ui/events.js");

const ev = (name, start, end, ai) => ({ name, start, end, ai });

test("an open event shows the post roll; a closed one its logged end", () => {
  assert.equal(eventEnd(ev("a", 1000, null)), 1000 + POST_ROLL_MS);
  assert.equal(eventEnd(ev("a", 1000, 5000)), 5000);
});

test("the list is newest first and filters by camera", () => {
  const all = [ev("Front", 1, 2), ev("Back", 5, 6), ev("Front", 3, 4)];
  assert.deepEqual(filterEvents(all).map((e) => e.start), [5, 3, 1]);
  assert.deepEqual(filterEvents(all, "Front").map((e) => e.start), [3, 1]);
});

test("markers are clipped to the view and touching events merge", () => {
  const v0 = 0, v1 = 1000;
  const m = markerLayout([ev("a", -100, 100), ev("a", 100, 200), ev("a", 500, 600), ev("a", 900, 2000), ev("a", 2000, 3000)], v0, v1);
  assert.deepEqual(m, [{ left: 0, width: 20 }, { left: 50, width: 10 }, { left: 90, width: 10 }]);
});

test("nothing in view draws nothing", () => {
  assert.deepEqual(markerLayout([ev("a", 5000, 6000)], 0, 1000), []);
  assert.deepEqual(markerLayout([], 0, 1000), []);
});
