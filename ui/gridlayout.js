// The live grid's arithmetic, apart from the DOM so it can run under node (tests/gridlayout.test.js).

// Reorder the first `n` tile records so each camera in `wanted` stays on the record playing it: no rebuild, no new
// decoder. Records of cameras no longer wanted fill the gaps. Returns whether anything moved.
function followCameras(tiles, wanted, n) {
  let moved = false;
  for (let i = 0; i < n; i++) {
    if (wanted[i] == null || tiles[i].dev === wanted[i]) continue;
    const j = tiles.findIndex((t, k) => k < n && t.dev === wanted[i]);
    if (j < 0) continue;
    [tiles[i], tiles[j]] = [tiles[j], tiles[i]];
    moved = true;
  }
  return moved;
}

// Move the border after track `i` by `delta` px, taking only from its neighbour. The minimum shrinks with the space
// available, so it can always be met when every track is at it.
function resizeTracks(px, i, delta, min) {
  const floor = Math.min(min, px.reduce((a, b) => a + b, 0) / px.length / 2);
  const pair = px[i] + px[i + 1];
  if (pair <= floor * 2) return px;
  const next = [...px];
  next[i] = Math.min(Math.max(px[i] + delta, floor), pair - floor);
  next[i + 1] = pair - next[i];
  return next;
}

// Saved proportions come back from storage, so anything unexpected means equal sizes.
const validTracks = (v, len) => Array.isArray(v) && v.length === len && v.every((x) => Number.isFinite(x) && x > 0);

if (typeof module !== "undefined") module.exports = { followCameras, resizeTracks, validTracks };
