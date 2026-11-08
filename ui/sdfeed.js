"use strict";
// Feeding a MediaSource from a camera SD download in progress (sd.rs Progress), apart from the WebView APIs so it
// runs under node (tests/sdfeed.test.js). The page polls the download's progress; each track is a fragmented MP4
// file that keeps growing, read in byte ranges and appended to its own SourceBuffer.

// Whether this browser can play every track of the download through MediaSource.
function canFeed(progress, supported) {
  return progress.tracks.length > 0 && progress.tracks.every((t) => supported(t.mime));
}

// The byte ranges still to read: per track, from what has been appended (`have`, by path) to the file's size now.
function pendingRanges(progress, have) {
  return progress.tracks
    .filter((t) => t.bytes > (have[t.path] || 0))
    .map((t) => ({ path: t.path, mime: t.mime, from: have[t.path] || 0, to: t.bytes }));
}

// The download finished and every byte of it has been appended: time to end the stream.
function feedComplete(progress, have) {
  return progress.done && !progress.error && progress.tracks.every((t) => (have[t.path] || 0) >= t.bytes);
}

// What to do with a progress report that can't be fed: "wait" while the download runs without fragments, "file"
// once the finished MP4 is there, "error" when the download failed, or null to keep feeding.
function fallback(progress) {
  if (progress.error) return "error";
  if (progress.done && progress.file && !progress.tracks.length) return "file";
  if (!progress.done && !progress.tracks.length) return "wait";
  return null;
}

if (typeof module !== "undefined") module.exports = { canFeed, pendingRanges, feedComplete, fallback };
