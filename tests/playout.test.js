// `node --test tests/playout.test.js` — live player timing, replayed against frame arrivals recorded from real cameras
// (tests/fixtures: arrival ms, camera ms, keyframe, bytes; from examples/stream_timing).
const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { Playout, framer, decoderConfigs } = require("../ui/playout.js");

const trace = (name) => fs.readFileSync(path.join(__dirname, "fixtures", name), "utf8").trim().split("\n")
  .map((l) => l.split(",").map(Number)).map(([at, cam]) => ({ at, us: Math.round(cam * 1000) >>> 0 }));

// When each frame reaches the screen: at its due time, or on arrival if that's later, and always in order.
function replay(frames) {
  const p = new Playout();
  let shown = -Infinity;
  return frames.map((f) => {
    const { due, overflow } = p.add(f.us, f.at);
    shown = Math.max(due, f.at, shown);
    return { at: f.at, shown, overflow };
  });
}
const gaps = (t) => t.slice(1).map((v, i) => v - t[i]);
const after = (played, ms) => played.filter((f) => f.at >= played[0].at + ms);
const steady = (n, stepMs, start = 0) => Array.from({ length: n }, (_, i) => ({ at: i * stepMs, us: (start + i * stepMs * 1000) >>> 0 }));

for (const name of ["backyard_main.csv", "front_door_main.csv"]) {
  test(`${name}: the keyframe stalls no longer reach the screen`, () => {
    const frames = trace(name);
    assert.ok(Math.max(...gaps(frames.map((f) => f.at))) > 500, "the recording really stalls");
    const played = replay(frames);
    assert.ok(played.every((f) => !f.overflow), "no frame is dropped");
    const shown = gaps(after(played, 5000).map((f) => f.shown));
    assert.ok(Math.max(...shown) < 150, `longest gap on screen ${Math.max(...shown)} ms`);
    const latency = Math.max(...after(played, 5000).map((f) => f.shown - f.at));
    assert.ok(latency < 1700, `a frame waited ${latency} ms`);
  });
}

test("the camera's u32 microsecond clock wrapping changes nothing", () => {
  const shown = gaps(replay(steady(300, 66.667, 2 ** 32 - 5_000_000)).map((f) => f.shown));
  assert.ok(shown.every((g) => g > 60 && g < 73), `gaps ${Math.min(...shown)}..${Math.max(...shown)}`);
});

test("a camera with no usable clock still plays at its arrival rate", () => {
  const played = replay(steady(600, 66.667).map((f) => ({ ...f, us: 0 })));
  assert.ok(played.every((f) => !f.overflow));
  const shown = gaps(after(played, 5000).map((f) => f.shown));
  assert.ok(shown.every((g) => g > 60 && g < 73), `gaps ${Math.min(...shown)}..${Math.max(...shown)}`);
});

test("a jump in the camera's clock (stream switch) neither drops frames nor adds delay", () => {
  const a = steady(150, 66.667), b = steady(150, 66.667, 900_000_000).map((f) => ({ ...f, at: f.at + 10_000 }));
  const played = replay([...a, ...b]);
  assert.ok(played.every((f) => !f.overflow));
  const waited = played.at(-1).shown - played.at(-1).at;
  assert.ok(waited < 1100, `waited ${waited} ms after the jump`);
  assert.ok(gaps(played.map((f) => f.shown)).every((g) => g >= 0));
});

test("the delay settles to what the link needs, without visible speed-up", () => {
  const played = replay(steady(15 * 90, 66.667));
  const shown = gaps(after(played, 1000).map((f) => f.shown));
  assert.ok(shown.every((g) => g > 62 && g < 71), `gaps ${Math.min(...shown)}..${Math.max(...shown)}`);
  const end = played.at(-1);
  assert.ok(end.shown - end.at < 200, `still ${end.shown - end.at} ms behind after 90 s`);
});

test("a link slower than the video gives up the backlog instead of falling behind for ever", () => {
  // Frames arrive every 100 ms but cover 67 ms: a third of a second further behind every second.
  const played = replay(Array.from({ length: 400 }, (_, i) => ({ at: i * 100, us: Math.round(i * 66667) >>> 0 })));
  const first = played.findIndex((f) => f.overflow);
  assert.ok(first > 0, "overflow reported");
  assert.ok(played[first].at < 25_000, `only after ${played[first].at} ms`);
  const end = played.at(-1);
  assert.ok(end.shown - end.at < 6000, `still ${end.shown - end.at} ms behind`);
});

test("H.264 avoids the hardware decoder's reorder delay, and can still fall back to it", () => {
  const [first, fallback, none] = decoderConfigs("avc1.640033", null);
  assert.equal(first.hardwareAcceleration, "prefer-software");
  assert.equal(first.optimizeForLatency, true);
  assert.equal(fallback.hardwareAcceleration, undefined);
  assert.equal(none, undefined);
});

test("HEVC stays on the hardware decoder, with its size hint", () => {
  assert.deepEqual(decoderConfigs("hvc1.1.6.L150", { width: 5120, height: 1440 }),
    [{ codec: "hvc1.1.6.L150", optimizeForLatency: true, codedWidth: 5120, codedHeight: 1440 }]);
});

test("framer rebuilds frames however the bytes are split", async () => {
  const frame = (flags, us, body) => {
    const b = Buffer.alloc(9 + body.length);
    b[0] = flags; b.writeUInt32BE(body.length, 1); b.writeUInt32BE(us, 5); b.set(body, 9);
    return b;
  };
  const bytes = Buffer.concat([frame(1, 4_000_000_000, Buffer.from("key")), frame(0, 66_667, Buffer.from("delta!")), frame(6, 0, Buffer.alloc(0))]);
  for (const size of [1, 2, 7, 9, 10, bytes.length]) {
    const got = [];
    const parse = framer(async (flags, us, data) => got.push([flags, us, Buffer.from(data).toString()]));
    for (let i = 0; i < bytes.length; i += size) await parse(new Uint8Array(bytes.subarray(i, i + size)));
    assert.deepEqual(got, [[1, 4_000_000_000, "key"], [0, 66_667, "delta!"], [6, 0, ""]], `split every ${size}`);
  }
});
