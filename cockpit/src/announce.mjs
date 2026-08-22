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
//
// **The speech grammar lives here too, and did not at first.** When this module was extracted the
// page kept `sayName`, `forSpeech`, `VERB` and `utteranceFor`, and `sentenceFor` was written fresh —
// so the standing-debt channel quietly stopped saying what a box was asking for and started reading
// box names literally. `era s 6 wants permission. run rm -rf build` became `example-box-1 needs you`. That
// is the sentence that decides whether somebody gets up, and every one of these functions is a rule
// learned out loud. `docs/delivery.md` §5: the architecture is the easy half; the grammar is the
// product. So the grammar is here, where it is tested in node, and there is one of it.

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
// `opts.groupOf` maps a box's state to its board group, and it is an argument rather than an import
// because `cockpit/build.mjs` concatenates these modules and refuses one that imports — what belongs
// here is leaf functions with every input passed in. The sentence needs it: what a box is asking is
// different from that it finished or that it broke.
export function announcementsFor(fresh, opts) {
  const {
    voiceOn = false,
    alertsOn = false,
    awayForMs = 0,
    speakingAlready = false,
    groupOf = null,
  } = opts || {};
  const quiet = { notes: [], say: "", beep: false };

  // Nothing to announce *with*. The caller keeps its backlog rather than clearing it, so turning a
  // channel on tells you what you missed instead of starting from silence — which is why this
  // returns "say nothing" rather than "there is nothing".
  if (!voiceOn && !alertsOn) return quiet;
  if (awayForMs < OWED_GRACE_MS) return quiet;
  if (!fresh || !fresh.length) return quiet;
  // Another sentence is being read on this tick. Let it stand alone and say this on the next one.
  if (speakingAlready) return quiet;

  // A notification is **read**, so it carries the box's real name — the string on the board row and
  // in the tag the browser dedupes on. Only the spoken channel gets `sayName`. The two look like the
  // same string and are not: one is an identifier you click, the other is something said out loud.
  const notes = !alertsOn
    ? []
    : fresh.length <= NAME_AT_MOST
      ? fresh.map(b => ({ body: `${b.name} needs you`, tag: b.name }))
      : [{ body: `${fresh.length} boxes need you`, tag: "skein-owed" }];

  return {
    notes,
    // **Independent.** `voiceOn` is read on its own and not alongside `alertsOn`; a reader who has
    // to check both to know whether the voice speaks is reading the bug this had.
    say: voiceOn ? sentenceFor(fresh, groupOf) : "",
    beep: alertsOn && notes.length > 0,
  };
}

// The sentence, for however many boxes.
//
// One box gets the whole utterance — what it is asking, in the words that distinguish "wants
// permission" from "needs you to sign in" — because with one box there is a decision to describe and
// describing it is the difference between getting up and not. Two get named, spoken. Past that a
// list of names read aloud is not something anybody hears, so it is a count.
export function sentenceFor(fresh, groupOf) {
  if (!fresh || !fresh.length) return "";
  if (fresh.length === 1) {
    // No `groupOf` means the caller could not say which group this is in, and every caller that
    // cannot is asking about a box that is owed — which is what `attn` is.
    return utteranceFor(fresh[0], groupOf ? groupOf(fresh[0].state) : "attn");
  }
  if (fresh.length <= NAME_AT_MOST) {
    return `${fresh.map(b => sayName(b.name)).join(" and ")} need you`;
  }
  return `${fresh.length} boxes need you`;
}

// A box name is written to be read, not spoken: `example-box-3` said literally is three
// words run together, and `example-box-1` comes out as a word. Hyphens and underscores become pauses, and
// a letter/digit boundary gets a space so "S6" is "S 6" rather than a syllable.
export const sayName = n => String(n || "").replace(/[-_]+/g, " ").replace(/([A-Za-z])(\d)/g, "$1 $2");

// What a box that just stopped is actually asking. The kind matters more than the words: "wants
// permission" and "needs you to sign in" want completely different things from you, and a single
// "needs a decision" for both is what makes you go and look — which is the cost this is removing.
export const VERB = {
  permission: "wants permission",
  question: "asks",
  trust: "needs you to trust the folder",
  auth: "needs you to sign in",
};

// Headlines are agent prose, and agent prose is full of things that are unbearable out loud: absolute
// paths, URLs, backticked identifiers, markdown. Speaking `/boxes/example-box-6/tree/src/place.rs`
// costs eight seconds and communicates nothing — the basename communicates all of it. This is the
// single biggest difference between a voice that helps and one that gets muted.
export function forSpeech(text) {
  let s = String(text || "");
  s = s.replace(/`+/g, " ");                                  // code spans: the ticks are noise
  s = s.replace(/\*\*?|__|~~/g, " ");                          // markdown emphasis
  s = s.replace(/https?:\/\/\S+/g, " a link ");                // nobody wants a URL read out
  // A path becomes its basename. Deliberately not every token with a slash in it — "and/or" and
  // "24/7" are words, not paths — so a token qualifies only by looking like one: rooted at /, ~/ or
  // ./, or ending in something with a file extension.
  s = s.replace(/\S+/g, token => {
    if (!token.includes("/")) return token;
    const last = token.split("/").filter(Boolean).pop() || token;
    const isPath = /^[~.]?\//.test(token) || /^[\w.-]+\.[A-Za-z]{1,8}$/.test(last);
    return isPath ? last : token;
  });
  s = s.replace(/\s+/g, " ").trim();
  // Close the gap the strippers left. A backtick becomes a space so `a`+`b` does not fuse into one
  // word — but a span that ended right before punctuation then reads as "build ?", and a speech
  // engine hears that space as a pause in the wrong place.
  s = s.replace(/\s+([,.;:!?])/g, "$1");
  // One breath. A long ask is a reason to look, not something to recite — the row on the board has
  // the full text, and cutting at a sentence boundary keeps what was said grammatical.
  if (s.length > 160) {
    const cut = s.slice(0, 160);
    const stop = Math.max(cut.lastIndexOf(". "), cut.lastIndexOf("? "), cut.lastIndexOf("! "));
    s = (stop > 60 ? cut.slice(0, stop + 1) : cut.replace(/\s+\S*$/, "")) + "…";
  }
  return s;
}

// One box, said in full. `group` is the board group it is in — the caller has it, and passing it
// keeps this a leaf function.
export function utteranceFor(b, group) {
  const name = sayName(b.name);
  const head = forSpeech(b.headline);
  if (group === "error") return head ? `${name} hit an error. ${head}` : `${name} hit an error.`;
  if (group === "done") return `${name} finished.`;
  // A box that ended its turn. Distinct from `attn` on purpose: nothing is blocking it, it has said
  // its piece and stopped, so "needs a decision" would overstate what it is asking for.
  if (group === "waiting") return head ? `${name} is waiting. ${head}` : `${name} is waiting.`;
  const verb = VERB[b.blocked_kind] || (b.pause === "proceed" ? "wants to continue" : "needs a decision");
  // Trust and auth are self-explaining — appending the box's last words to them adds length and no
  // information, because the ask is the state itself rather than anything the agent said.
  if (!head || b.blocked_kind === "trust" || b.blocked_kind === "auth") return `${name} ${verb}.`;
  return `${name} ${verb}. ${head}`;
}
