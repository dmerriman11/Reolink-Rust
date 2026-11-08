// Live audio timing, apart from the WebView APIs so it can run under node (tests/audioplayout.test.js).
// Each chunk starts at its camera time plus the offset the tile's video Playout (playout.js) holds frames by, so the
// sound stays with the picture however long the video is delayed. Without video (MJPEG fallback, no frame yet) it
// keeps a small jitter buffer of its own.
const JITTER_MS = 300;
const AUDIO_JUMP_MS = 5000; // as playout.js: a bigger step in the camera's clock is a new timeline
// A chunk this close to the end of the last one continues it, so the seams stay sample-exact while the video's offset
// slews (it moves ~3%, which would otherwise cut or gap the sound every few chunks). Within lip-sync tolerance.
const SYNC_MS = 120;

class AudioPlayout {
  last = null; // the last stamp, in camera microseconds
  media = 0; // the camera's clock in ms, unwrapped (own timeline only)
  offset = null; // own timeline: played at media + offset
  end = -Infinity; // when the last scheduled chunk ends, screen clock

  // Screen time to start a chunk stamped `us` (u32 camera microseconds) lasting `dur` ms that arrived at `now`.
  // `video`: the tile's video Playout, or null. `drop`: it is already late. `joined`: it starts where the last one ends, so play it on that sample. `reset`: the timeline changed, so stop
  // whatever is still scheduled.
  add(us, dur, now, video) {
    let reset = false;
    if (this.last !== null) {
      const delta = ((us - this.last) | 0) / 1000; // signed 32-bit, so the clock wrapping is just the next step
      if (Math.abs(delta) > AUDIO_JUMP_MS) { reset = true; this.offset = null; }
      else this.media += delta;
    }
    this.last = us;
    let at;
    if (video?.offset != null) at = video.media + ((us - video.last) | 0) / 1000 + video.offset;
    else {
      const late = now - this.media;
      if (this.offset === null || late > this.offset) this.offset = late + JITTER_MS;
      at = this.media + this.offset;
    }
    let drop = false, joined = false;
    if (Math.abs(at - this.end) < SYNC_MS) { at = this.end; joined = true; }
    else if (at < this.end) drop = true; // ahead of the picture by more than the tolerance: skip a chunk to fall back in line
    drop ||= at < now;
    if (drop) return { at, drop, reset, joined: false };
    this.end = at + dur;
    return { at, drop, reset, joined };
  }
}

if (typeof module !== "undefined") module.exports = { AudioPlayout };
