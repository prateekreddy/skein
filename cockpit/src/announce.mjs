// What to announce, and on which channel. The decision only — the speaking and the notifying are
// the caller's, because a function that reaches for `speechSynthesis` is one that has to be mocked
// rather than called.
//
// **Voice and alerts are independent switches**, and that is the property this exists to keep. They
// were once one condition, and turning alerts off silenced the voice — a bug that is invisible in
// the source unless you are looking for it, which is exactly why asserting it by matching the page's
// text was standing in for a test.
//
// Two outputs from one rule, so they cannot drift into disagreeing about what is worth interrupting
// somebody for.

// How long a person must have been away before anything speaks. Being at the board is not being
// interrupted; announcing what is on screen in front of them is a nag.
export const OWED_GRACE_MS = 20_000;

// Past a couple, a count is all anybody can act on — and six banners stacked on a lock screen is the
// same nag wearing a different hat.
export const NAME_AT_MOST = 2;

// Decide what this tick should announce.
//
// `fresh` is the boxes newly owed that have not been announced yet — the caller keeps that
// bookkeeping, because it is state and this is not. Returns what to do, never what was done.
export function announcementsFor(fresh, opts) {
  const { voiceOn = false, alertsOn = false, awayForMs = 0, speakingAlready = false } = opts || {};
  const quiet = { notes: [], say: "", beep: false };

  // Nothing to announce *with*. The caller keeps its backlog rather than clearing it, so turning a
  // channel on tells you what you missed instead of starting from silence — which is why this
  // returns "say nothing" rather than "there is nothing".
  if (!voiceOn && !alertsOn) return quiet;
  if (awayForMs < OWED_GRACE_MS) return quiet;
  if (!fresh || !fresh.length) return quiet;
  // Another sentence is being read on this tick. Let it stand alone and say this on the next one.
  if (speakingAlready) return quiet;

  const notes = !alertsOn
    ? []
    : fresh.length <= NAME_AT_MOST
      ? fresh.map(b => ({ body: `${b.name} needs you`, tag: b.name }))
      : [{ body: `${fresh.length} boxes need you`, tag: "skein-owed" }];

  return {
    notes,
    // **Independent.** `voiceOn` is read on its own and not alongside `alertsOn`; a reader who has
    // to check both to know whether the voice speaks is reading the bug this had.
    say: voiceOn ? sentenceFor(fresh) : "",
    beep: alertsOn && notes.length > 0,
  };
}

// The sentence, for however many boxes. Names them while naming them is useful and counts them
// after that, because a list of nine box names read aloud is not something anybody hears.
export function sentenceFor(fresh) {
  if (!fresh || !fresh.length) return "";
  if (fresh.length === 1) return `${fresh[0].name} needs you`;
  if (fresh.length <= NAME_AT_MOST) return `${fresh.map(b => b.name).join(" and ")} need you`;
  return `${fresh.length} boxes need you`;
}
