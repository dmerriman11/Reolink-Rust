"use strict";
// Motion event log helpers shared by the Playback timeline and the Events list; tested in tests/events.test.js.

const POST_ROLL_MS = 30_000; // as prefs::MOTION_POST_SECS: an event with no end yet shows this long

// Where an event ends on the timeline: its logged end, or start plus the post roll while it has none.
// shortcut: an open event (motion still running, or its session dropped) shows as the post roll only; read State.motion if that misleads
const eventEnd = (ev) => ev.end ?? ev.start + POST_ROLL_MS;

// Newest first, optionally one camera only.
function filterEvents(events, name) {
  return events.filter((ev) => !name || ev.name === name).sort((a, b) => b.start - a.start);
}

// Marker spans for one camera's row across the view window [v0, v1), as {left, width} percentages. Events that
// touch or overlap merge into one span so a busy day doesn't draw hundreds of slivers.
function markerLayout(events, v0, v1) {
  const span = v1 - v0;
  const spans = events
    .map((ev) => [Math.max(ev.start, v0), Math.min(eventEnd(ev), v1)])
    .filter(([a, b]) => b > a)
    .sort((a, b) => a[0] - b[0]);
  const merged = [];
  for (const [a, b] of spans) {
    const last = merged.at(-1);
    if (last && a <= last[1]) last[1] = Math.max(last[1], b);
    else merged.push([a, b]);
  }
  return merged.map(([a, b]) => ({ left: ((a - v0) / span) * 100, width: ((b - a) / span) * 100 }));
}

if (typeof module !== "undefined") module.exports = { POST_ROLL_MS, eventEnd, filterEvents, markerLayout };
