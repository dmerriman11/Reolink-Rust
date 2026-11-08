// `node --test tests/audioplayout.test.js` — live audio timing against the video player's timing.
const { test } = require("node:test");
const assert = require("node:assert/strict");
const { Playout } = require("../ui/playout.js");
const { AudioPlayout } = require("../ui/audioplayout.js");

const DUR = 64; // ms per PCM chunk
const us = (ms) => Math.round(ms * 1000) >>> 0;

// A video stream that has been playing for a while, on time, so its offset has settled.
function video(frames = 150, base = 0) {
  const p = new Playout();
  for (let i = 0; i < frames; i++) p.add(us(base + i * 66.667), i * 66.667);
  return p;
}

test("a chunk plays when the video frame stamped the same time is shown", () => {
  const v = video();
  const a = new AudioPlayout();
  const now = 150 * 66.667, stamp = us(now);
  const { due } = v.add(stamp, now);
  const { at, drop } = a.add(stamp, DUR, now, v);
  assert.ok(!drop);
  assert.ok(Math.abs(at - due) < 1, `audio at ${at}, video due ${due}`);
});

test("audio that arrives after its video frame was shown is dropped", () => {
  const v = video();
  const a = new AudioPlayout();
  const now = 150 * 66.667;
  v.add(us(now), now);
  const { drop } = a.add(us(now), DUR, now + 3000, v);
  assert.ok(drop);
});

test("without video, a 300 ms jitter buffer absorbs a burst", () => {
  const a = new AudioPlayout();
  const first = a.add(us(0), DUR, 1000, null);
  assert.ok(Math.abs(first.at - 1300) < 1, `first chunk at ${first.at}`);
  // Four chunks held up on the link then arriving together still play at the camera's pace.
  const ats = [1, 2, 3, 4].map((i) => a.add(us(i * DUR), DUR, 1000 + 4 * DUR + 5, null));
  assert.ok(ats.every((r) => !r.drop));
  ats.forEach((r, i) => assert.ok(Math.abs(r.at - (1300 + (i + 1) * DUR)) < 1, `chunk ${i} at ${r.at}`));
});

test("chunks whose stamps jitter a little join seamlessly", () => {
  const a = new AudioPlayout();
  let r = a.add(us(0), DUR, 0, null);
  for (let i = 1; i < 20; i++) {
    const prevEnd = r.at + DUR;
    r = a.add(us(i * DUR + (i % 2 ? 8 : -8)), DUR, i * DUR, null);
    assert.equal(r.at, prevEnd, `chunk ${i}`);
    assert.ok(r.joined, `chunk ${i} is played on the last one's end sample`);
  }
});

test("a jump in the camera's clock starts a new timeline and clears the old one", () => {
  const a = new AudioPlayout();
  for (let i = 0; i < 10; i++) a.add(us(i * DUR), DUR, i * DUR, null);
  const r = a.add(us(900_000), DUR, 10 * DUR, null);
  assert.ok(r.reset);
  assert.ok(!r.drop);
  assert.ok(Math.abs(r.at - (10 * DUR + 300)) < 1, `after the jump at ${r.at}`);
});

test("the camera's u32 microsecond clock wrapping changes nothing", () => {
  const a = new AudioPlayout();
  const start = 2 ** 32 - 3 * DUR * 1000;
  let last = null;
  for (let i = 0; i < 10; i++) {
    const r = a.add((start + i * DUR * 1000) >>> 0, DUR, i * DUR, null);
    assert.ok(!r.reset && !r.drop, `chunk ${i}`);
    if (last !== null) assert.ok(Math.abs(r.at - last - DUR) < 1, `gap ${r.at - last}`);
    last = r.at;
  }
});

test("a slewing video offset never cuts the sound: chunks stay seamless, then one is skipped to catch up", () => {
  const a = new AudioPlayout();
  const v = { offset: 1000, media: 0, last: 0 };
  let r = a.add(0, DUR, 0, v), drops = 0;
  for (let i = 1; i < 200; i++) {
    v.offset -= 2; // the video delay shrinking ~3% of real time, as measured on an Argus 4 Pro
    const prevEnd = r.drop ? null : r.at + DUR;
    const next = a.add(us(i * DUR), DUR, i * DUR, v);
    assert.ok(!next.reset, `chunk ${i} cut what was playing`);
    if (next.drop) drops++;
    else if (prevEnd !== null && !r.drop) assert.ok(next.at === prevEnd || Math.abs(next.at - (v.offset + i * DUR)) < 1, `chunk ${i}`);
    if (!next.drop) r = next;
  }
  assert.ok(drops > 0 && drops < 10, `${drops} chunks skipped`);
});
