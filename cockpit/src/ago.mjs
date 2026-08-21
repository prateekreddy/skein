// How long ago, in the words the board uses.
//
// **Formatted here rather than on the server**, and the reason is that the stream stopped re-sending
// a box that only got older. Change detection ignores the clock-derived fields — a box's age moves
// every second whether or not anything happened to it, and leaving it in made every box change on
// every tick — so a server-formatted string would freeze between real changes and a fleet that had
// been quiet for ten minutes would say "2m ago" for ever.
//
// The server sends `age_secs`: the age **at the moment of the observation**. The client knows when
// it received it. So the age is that plus the time since, recomputed on a timer that costs nothing
// and needs no traffic at all.
//
// One formatter, in one language. It was `util::ago` in Rust; two that can disagree is the shape
// §11.7 exists to end, and this is the one the person reads.

// `secs` may be null — `age_secs` is absent when a box's `lastSeen` is unparseable or missing.
// "We do not know" and "just now" are different things and this is the field that says so, so it
// renders as `?` exactly as the server's did, and never as `0s ago`.
export function ago(secs) {
  if (secs === null || secs === undefined || Number.isNaN(secs)) return "?";
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

// A row's age now: what it was when it was observed, plus how long ago that was.
//
// `sinceMs` is how long the row has been held, in milliseconds. Clamped at zero, because a clock
// that steps backwards should make a row look freshly seen rather than make it say `-3s ago`.
export function ageNow(secs, sinceMs) {
  if (secs === null || secs === undefined || Number.isNaN(secs)) return "?";
  return ago(secs + Math.max(0, Math.floor((sinceMs || 0) / 1000)));
}
