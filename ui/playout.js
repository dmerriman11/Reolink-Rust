// The live player's timing, apart from the WebView APIs so it can run under node (tests/playout.test.js).

// A camera sends a keyframe no faster than its uplink allows (about 530 KB/s measured on an Argus 4 Pro: ~1 s for a
// main-stream keyframe, every 2 s), and the frames behind it then arrive in a burst. Showing frames as they arrive
// shows that as freeze, fast-forward, freeze. So each frame is shown at its time on the camera's clock plus an offset
// large enough that the latest recent frame would still have made it.
const SEED_MS = 1000; // a stall will come: absorb the first one before it has been measured
const MARGIN_MS = 120; // on top of the worst lateness seen, for the next keyframe being a little bigger
const WINDOW_MS = 30000; // must span several keyframes, or the offset would sag between them
const SLEW = 0.03; // the offset moves at most this many ms per ms, so 3% off speed: too little to see
const JUMP_MS = 5000; // a bigger step in the camera's clock is a new timeline (stream switch, camera restart)
const MAX_DELAY_MS = 5000; // further behind than this and the link can't carry the stream: skip ahead

class Playout {
  media = 0; // the camera's clock in ms, unwrapped
  step = 1000 / 15; // ms between the last two frames with a usable clock
  last = null; // the last stamp, in camera microseconds
  offset = null; // shown at media + offset
  seen = []; // [arrival, arrival - media] for the window
  lastDue = -Infinity;
  lastAt = 0;

  // The screen time for a frame stamped `us` (u32 microseconds) that arrived at `now`. `overflow`: everything held so
  // far should be thrown away, as the link is falling further and further behind.
  add(us, now) {
    let delay = null; // kept across a new timeline: the link hasn't changed
    if (this.last !== null) {
      const delta = ((us - this.last) | 0) / 1000; // signed 32-bit, so the clock wrapping is just the next step
      if (Math.abs(delta) > JUMP_MS) {
        delay = this.offset - Math.min(...this.seen.map((s) => s[1]));
        this.seen = [];
        this.offset = null;
        this.media += this.step;
      } else {
        if (delta > 0) this.step = delta;
        this.media += this.step;
      }
    }
    this.last = us;

    const late = now - this.media; // how far behind the camera's clock this frame came in
    this.seen.push([now, late]);
    while (now - this.seen[0][0] > WINDOW_MS) this.seen.shift();
    let worst = -Infinity, best = Infinity;
    for (const [, l] of this.seen) { if (l > worst) worst = l; if (l < best) best = l; }

    const overflow = worst - best > MAX_DELAY_MS;
    if (overflow) {
      this.seen = [[now, late]];
      this.offset = null;
      delay = MARGIN_MS;
    }
    if (this.offset === null) this.offset = late + (delay ?? SEED_MS);
    else if (late > this.offset) this.offset = late + MARGIN_MS; // missed its time: a freeze either way, so step now
    else {
      const room = SLEW * (now - this.lastAt);
      this.offset += Math.max(-room, Math.min(room, worst + MARGIN_MS - this.offset));
    }
    this.lastAt = now;
    this.lastDue = Math.max(this.lastDue, this.media + this.offset);
    return { due: this.lastDue, overflow };
  }
}

// What to try for a stream, best first. No description: Annex B with in-band parameter sets. The size hint keeps
// hardware decoders from guessing. The sub stream's H.264 (Argus 4 Pro) has no VUI, so nothing says its frames are never
// reordered: a hardware decoder holds back up to 16 of them, a 1.07 s freeze and then a burst, every 2 s. Software
// decoding with optimizeForLatency hands each one over at once.
// shortcut: all H.264 goes to software, heavy on a 4K H.264 main stream; add the VUI to the SPS if a camera needs that.
function decoderConfigs(codec, box) {
  const plain = { codec, optimizeForLatency: true, ...(box && { codedWidth: box.width, codedHeight: box.height }) };
  return codec.startsWith("avc1") ? [{ ...plain, hardwareAcceleration: "prefer-software" }, plain] : [plain];
}

// Split /raw's byte stream (stream.rs: 1 flag byte, 4-byte length, 4-byte camera time, all big-endian, then the
// payload) into frames, however the chunks fall. Each payload is allocated once and every byte copied once, so big
// keyframes spread over many chunks stay cheap.
const FRAME_HEAD = 9;
function framer(onFrame) {
  const head = new Uint8Array(FRAME_HEAD);
  const view = new DataView(head.buffer);
  let have = 0; // header bytes so far
  let body = null; // payload being filled
  let filled = 0;
  return async (chunk) => {
    let at = 0;
    while (at < chunk.length) {
      if (!body) {
        const n = Math.min(FRAME_HEAD - have, chunk.length - at);
        head.set(chunk.subarray(at, at + n), have);
        have += n;
        at += n;
        if (have < FRAME_HEAD) return;
        body = new Uint8Array(view.getUint32(1));
        filled = 0;
      }
      const n = Math.min(body.length - filled, chunk.length - at);
      body.set(chunk.subarray(at, at + n), filled);
      filled += n;
      at += n;
      if (filled === body.length) {
        const frame = body;
        body = null;
        have = 0;
        await onFrame(head[0], view.getUint32(5), frame);
      }
    }
  };
}

if (typeof module !== "undefined") module.exports = { Playout, framer, decoderConfigs };
