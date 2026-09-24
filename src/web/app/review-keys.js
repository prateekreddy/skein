// ---- the keyboard's state (SKEIN-151/159, docs/review-ux.md §6) -------------------------------
// Selection is a PR NUMBER — an rk key, never an index (§6 focus rule 1): an index drifts off the
// row you are looking at every time a summary lands or a lane re-sorts. `revSelAt` remembers where
// that key sat in nav order at the last render, so a selected row that LEAVES the list can hand
// the selection to the row that took its place by position — and `revFlash` makes that hand-off
// visible for one render rather than silent.
let revSel = null;             // rk of the selected row, or a stack's own key — never an index
let revSelAt = 0;              // where the selection sat in revNav at the last render
let revNav = [];               // every navigable key (rows and stack heads) in display order, rebuilt by renderReview
let revStacks = new Map();     // stack key -> its stack, as of the last render — what → enters and n/N skip when finished
let revFlash = "";             // the key whose row flashes once this render: a selection moved by position
let revChord = "";             // "g" while a g-chord waits for its second key (the meaning lives in shortcutFor)
let revLastActKey = "";        // the last act revHold held — what a queue-mode u takes back

// What you typed survives a reload. Reported live on PR 577: notes were written, a draft came
// back, the page was reloaded — and both were gone, because the composer was pure page state.
// Saved per (repo, PR, kind) on every keystroke and every draft, restored on reopen, cleared only
// when the thing is actually posted.
function revComposeStore(c) { return `skein.revdraft.${c.repo}#${c.number}.${c.kind}`; }
/// Save one composer's words — **the composer the save is FOR, not the one on screen** (SKEIN-841).
///
/// `c` defaults to the open composer, which is what a keystroke means: `oninput` is the box the
/// reader is typing in. An answer means the other thing. `revAsk` and `revDraft` capture `c` at the
/// press, and this read the global instead, so an answer that landed after the reader had opened
/// another composer was written onto an object nothing on the page referenced any more and saved
/// nowhere — while the composer they had switched TO had its draft written under its own key, at a
/// moment it had not asked for. Re-opening the ask then restored the pre-answer copy below, so a
/// model call the reader had paid for was gone with nothing said. The deferral of the PAINT is
/// right and is untouched (see `revComposePaint`): an answer nobody is looking at must not take a
/// caret, and it must still be KEPT.
function revComposeSave(c = revComposing) {
  if (!c) return;
  // `outside` and `draftedFrom` too (SKEIN-819): the line saying an answer or a draft came from
  // outside its box has to survive a reload exactly as the text it is about does, or a reload is a
  // way to lose the warning and keep the words it warns about.
  try {
    localStorage.setItem(revComposeStore(c), JSON.stringify({
      text: c.text, answer: c.answer, outside: c.outside || "", draftedFrom: c.draftedFrom || "",
    }));
  } catch {}
}
function revCompose(repo, number, kind) {
  let saved = null;
  try { saved = JSON.parse(localStorage.getItem(revComposeStore({ repo, number, kind })) || "null"); } catch {}
  revComposing = {
    repo, number, kind, text: (saved && saved.text) || "", answer: (saved && saved.answer) || "",
    outside: (saved && saved.outside) || "", draftedFrom: (saved && saved.draftedFrom) || "", busy: false,
  };
  renderReviewNow();
  setTimeout(() => document.getElementById("rev-compose")?.focus(), 30);
}
function revComposeClose() { revComposing = null; renderReviewNow(); }

function revComposeHtml(pr) {
  const c = revComposing;
  if (!c || c.number !== pr.number || c.repo !== pr.repo_id) return "";
  const asking = c.kind === "ask";
  const label = asking ? "Ask about this PR — the answer stays between you and skein."
    : c.kind === "comment" ? "Comment — posted to GitHub under your name."
    : "Request changes — posted to GitHub under your name, and moves this out of your lane.";
  return `<div class="revcompose">
    <div class="revcl">${esc(label)}</div>
    ${!asking && c.outside ? `<div class="revstale outsidebox"><b>Drafted outside its box — ${esc(c.outside)}. The model did not see the change: check every claim about the code before you post it.</b>
      <button type="button" class="revchip" onclick="revDraftAgain(${pr.number})"${c.busy ? " disabled" : ""}>draft again</button></div>` : ""}
    <textarea id="rev-compose" rows="4" placeholder="${asking ? "what do you want to know?" : "rough notes are fine — skein can draft from them"}"
      oninput="revComposing.text = this.value; revComposeSave()" onkeydown="revComposeKey(event)">${esc(c.text)}</textarea>
    ${c.answer && c.outside ? `<div class="revstale outsidebox"><b>Answered outside its box — ${esc(c.outside)}. It could not look at the code or the earlier reading, so this is from your question alone.</b>
      <button type="button" class="revchip" onclick="revAsk(${pr.number})"${c.busy ? " disabled" : ""}>ask again</button></div>` : ""}
    ${c.answer ? `<div class="revanswer">${marked.parse(c.answer)}</div>` : ""}
    <div class="revacts">
      ${asking
        ? `<button type="button" class="revchip go" onclick="revAsk(${pr.number})"${c.busy ? " disabled" : ""}>${c.busy ? "thinking…" : "ask"}</button>`
        : `<button type="button" class="revchip" onclick="revDraft(${pr.number})"${c.busy ? " disabled" : ""}>${c.busy ? "drafting…" : "draft with skein"}</button>
           <button type="button" class="revchip go" onclick="revAct(${esc(JSON.stringify(pr.repo_id))}, ${pr.number}, ${esc(JSON.stringify(c.kind))})"${c.busy ? " disabled" : ""}>post to GitHub</button>`}
      <button type="button" class="revchip" onclick="revComposeClose()">cancel</button>
      <span class="dim revkeys">${asking ? "⌘↵ asks · Esc closes" : "⌘↵ posts · Esc closes — your words are kept"}</span>
    </div>
  </div>`;
}

/// **⌘↵ presses the composer's primary button, and Esc presses cancel** (SKEIN-606).
///
/// The primary button is the one drawn `.go`: **post to GitHub** for a comment or a request for
/// changes — the same `revAct` the chip calls, so the same 8-second undo receipt, not a shortcut
/// past it — and **ask** for an ask. Esc is **cancel**, `revComposeClose`, which takes the composer
/// off the screen and leaves the words where every keystroke already saved them, so reopening it
/// brings them back. That is what the hint beside the buttons promises.
///
/// **While the composer is busy both keys are ignored**, and swallowed rather than passed on: a
/// second press of the primary button mid-answer would ask twice, and an Esc mid-answer would close
/// the composer an answer is about to be painted into. The chips are `disabled` in that state for
/// the same reason. Ctrl as well as ⌘, because the keyboard a reader has is not the one the hint
/// draws.
///
/// On the textarea rather than on the page's keymap: `shortcutFor` returns nothing while a field
/// has focus (the guard), so this is the only handler that sees these keys here, and it stops them
/// there so nothing behind the composer ever does.
function revComposeKey(ev) {
  const c = revComposing;
  if (!c) return;
  const send = ev.key === "Enter" && (ev.metaKey || ev.ctrlKey);
  const close = ev.key === "Escape";
  if (!send && !close) return;
  ev.preventDefault();
  ev.stopPropagation();
  if (c.busy) return;
  if (close) { revComposeClose(); return; }
  if (c.kind === "ask") revAsk(c.number);
  else revAct(c.repo, c.number, c.kind);
}

/// Ask, privately. The answer replaces nothing and is posted nowhere.
function revAsk(number) {
  const c = revComposing;
  if (!c || !c.text.trim()) { toast("ask something first"); return; }
  c.busy = true; renderReviewNow();
  revPost(c.repo, number, "ask", c.text).then(d => {
    c.busy = false;
    // `c`, named — not whatever composer is open by the time this runs (SKEIN-841).
    // `outside` is the server's reason this answer did not come from the box (SKEIN-819) — empty,
    // and so cleared, when it did, which is what makes "ask again" able to retire the line.
    if (d.ok) { c.answer = d.text; c.outside = d.outside || ""; revComposeSave(c); } else toast(d.error || "no answer");
    revComposePaint(c);
  });
}

/// Draft into the box you are about to send. It replaces the notes rather than appending, because
/// the notes were the input to it — and you can still edit every word before it goes.
function revDraft(number) {
  const c = revComposing;
  if (!c || !c.text.trim()) { toast("say roughly what you want to tell them"); return; }
  c.busy = true; renderReviewNow();
  // The notes this draft is made FROM, kept so "draft again" drafts from them rather than from
  // the draft that replaced them in the box (SKEIN-819).
  const notes = c.text;
  revPost(c.repo, number, "draft", c.text).then(d => {
    c.busy = false;
    // `c`, named — the identical shape, and the identical loss (SKEIN-841).
    if (d.ok) {
      c.text = d.text; c.outside = d.outside || ""; c.draftedFrom = notes;
      revComposeSave(c);
    } else toast(d.error || "could not draft");
    revComposePaint(c);
  });
}

/// "draft again", on the line that says a draft was written outside its box (SKEIN-819): the same
/// press as "draft with skein", from the notes the draft was made from. What is in the box now is
/// the draft itself, and drafting from a draft would be a model rewording its own words about a
/// change it did not see.
function revDraftAgain(number) {
  const c = revComposing;
  if (!c || c.busy) return;
  if (c.draftedFrom) c.text = c.draftedFrom;
  revDraft(number);
}

/// The answer to a composer's own press, painted NOW (SKEIN-416).
///
/// `revAsk` and `revDraft` both waited on `renderReview`, and §6 rule 2 holds that render while the
/// pane owns a caret or a live selection — which is the composer's ordinary state, since the box
/// the reader typed the question into is the box they are looking at while skein thinks. Pressing
/// "draft with skein" then did nothing anybody could see for the length of a model call and,
/// on success, for ever: `c.text` had the drafted review in it and only a render puts it in the
/// textarea. Worse than invisible — the next keystroke's `oninput` writes the STALE box back over
/// `c.text`, so the draft was silently thrown away by the reader carrying on typing.
///
/// `revPendingPaint` and `revRepaintRow` are surgical because a receipt has one element to swap; an
/// answer changes the composer's box, its answer panel and its chips at once, so this is the whole
/// pane and the caret's position is what it costs. That is the same trade `renderReviewNow`'s
/// comment above spells out, and the reader asked for it.
///
/// **Forced only while this composer is still the one on screen.** `c` is captured at the press: by
/// the time the answer lands the reader may have cancelled it, or opened another PR's composer, and
/// nothing here is drawn any more. Forcing then would take a caret out of a box for an answer
/// nobody is waiting to see, which is the harm rule 2 exists to prevent.
function revComposePaint(c) {
  if (revComposing === c) renderReviewNow(); else renderReview();
}

function revPost(repo, number, kind, body, draftedAt) {
  return fetch(`/api/repos/${encodeURIComponent(repo)}/review/${number}/act`, {
    method: "POST", headers: { "content-type": "application/json" },
    // `drafted_at` is the revision the reader was looking at. Empty means "assume current"; a merge
    // sends the sha its own question quoted, so the server refuses against what was on screen.
    body: JSON.stringify({ kind, body: body || "", drafted_at: draftedAt || "" }),
  }).then(r => r.json()).catch(e => ({ ok: false, error: String(e.message || e).split("\n")[0] }));
}

/// The acts that reach GitHub under your name. Merge asks first — it is the one thing here that
/// cannot be undone from this pane, and an approval you did not mean is a sentence to your
/// colleague where a merge you did not mean is a commit on the base branch.
///
/// SKEIN-162: a verdict does not post on the press. The payload is assembled HERE, exactly as it
/// would have been sent, and held for REV_UNDO_MS — the control you pressed becomes the receipt
/// (`✓ approved · undo · 7s`), and undo cancels a request that never left the machine. Merge keeps
/// its confirm and posts immediately: it already asks, and a second window on top of a dialog
/// would be two undos for the one act that cannot be undone anyway.
function revAct(repo, number, kind) {
  const key = repo + "#" + number;
  const pr = ((revQueue && revQueue.prs) || []).find(x => rk(x) === key) || {};
  // **The revision the reader is looking at, taken once** (SKEIN-365). The question below names it
  // and the request sends it, out of the one value — so `prwork::merge_by_hand` refusing with "you
  // read abc1234, #41 is now at def5678" quotes back the sha the dialog put in front of the reader,
  // instead of one nothing on screen ever said.
  //
  // The queue's own row is where that sha comes from, and it is what the server would guess for
  // itself when nothing is sent (`prq::remembered_head`) — so this is never staler than sending
  // nothing.
  const mergeHead = kind === "merge" ? pr.head_sha || "" : "";
  if (kind === "merge") {
    // **The last place a mistake can be caught** (SKEIN-365). "Merge #41?" said the one thing the
    // reader already knew: they pressed it. What the chip cannot tell them is WHICH commit lands
    // and WHERE — and a merge is the single press this pane cannot take back.
    //
    // Both facts fall away together when they are not known, rather than naming a base nobody
    // checked: a question that guesses is worse than a short one, on the press that is final.
    const sha = mergeHead.slice(0, 7);
    const into = pr.base_ref || "";
    const lands = sha
      ? `${sha} is the commit on screen, and the one skein sends: if the branch moved since you read it, nothing is merged and skein says so.`
      : "skein cannot name the commit on screen, so the merge is checked against whatever it last saw for this pull request.";
    if (!confirmed(`Merge #${number}${sha ? ` (${sha})` : ""}${into ? ` into ${into}` : ""}?\n\n${lands} Approving is separate and has already happened if you did it.`)) return;
  }
  const mine = revComposing && revComposing.number === number && revComposing.repo === repo;
  const body = mine ? revComposing.text : "";
  const verdict = kind === "approve" || kind === "request-changes" || kind === "comment";
  // Only a merge names a revision. It used to send "" — which left the server to fall back to the
  // queue's remembered sha, a sha that lags exactly when a pull request has moved since the last
  // poll, and the merge was refused for not having read the code the reader had just been shown
  // (SKEIN-365). A verdict needs none: it carries no line numbers to anchor.
  const draftedAt = kind === "merge" ? mergeHead : "";
  const p = {
    repo, number, kind, verdict,
    // The draft's localStorage key, kept until the post truly lands: undo reopens the composer
    // with every word intact, because nothing was removed on a press that posted nothing.
    mineStore: mine ? revComposeStore(revComposing) : "",
    url: pr.url || `https://github.com/${repo}/pull/${number}`,
    send: () => revPost(repo, number, kind, body, draftedAt),
    state: "waiting", left: Math.round(REV_UNDO_MS / 1000), timer: null, tick: null, error: "", said: "",
  };
  // The composer closes now — the receipt replaces it — but only the on-screen copy.
  if (mine) revComposing = null;
  if (kind === "merge") {
    // Immediate — but a verdict still inside its window must not survive to double-post.
    const had = revPending.get(key);
    if (had) { if (had.timer) clearTimeout(had.timer); if (had.tick) clearTimeout(had.tick); }
    revPending.set(key, p);
    revFire(key);
    return;
  }
  revHold(key, p);
}

// --- SKEIN-162: hold, undo, fire — feedback where the eye is (docs/review-ux.md §7.1) ----------

/// Start (or replace) the undo window for one PR's act. The timer is global, not the reading
/// view's: closing the reading or moving to another PR during the countdown must NOT silently
/// drop a decision you made, so the act still fires when the window lapses, wherever you are by
/// then. (Closing the whole page is the one way out — the hold is client-side by design; that is
/// what makes undo free.) A second press inside the window replaces the pending act wholesale.
function revHold(key, p) {
  const had = revPending.get(key);
  if (had) { if (had.timer) clearTimeout(had.timer); if (had.tick) clearTimeout(had.tick); }
  revPending.set(key, p);
  revLastActKey = key;   // what a queue-mode `u` takes back — every held act passes through here
  p.timer = setTimeout(() => revFire(key), REV_UNDO_MS);
  p.tick = setTimeout(() => revTick(key), 1000);
  revPendingPaint(key);
}

/// Cancel a held act. Nothing was posted, so there is nothing to clean up: the draft and the line
/// notes are exactly as they were, and the bar returns to its normal state.
function revUndo(key) {
  const p = revPending.get(key);
  if (!p || p.state !== "waiting") return;
  clearTimeout(p.timer);
  if (p.tick) clearTimeout(p.tick);
  revPending.delete(key);
  revPendingPaint(key);
}

/// The visible countdown. Cosmetic and deliberately decoupled from the fire timer: a display that
/// drifts a frame costs nothing, where a fire that depended on the display would.
function revTick(key) {
  const p = revPending.get(key);
  if (!p || p.state !== "waiting") return;
  p.left = Math.max(0, p.left - 1);
  const spans = (revpane && revpane.querySelectorAll) ? revpane.querySelectorAll("[data-undo-left]") : [];
  for (const el of spans) if (el.getAttribute("data-undo-left") === key) el.textContent = `${p.left}s`;
  if (p.left > 0) p.tick = setTimeout(() => revTick(key), 1000);
}

/// The window lapsed (or try-again was pressed): the act goes out, byte-identical to what the
/// press assembled. Success cleans up what the press deliberately left alone, marks the row done
/// in place, and never calls loadReview(true) — the server's cache invalidation still runs on the
/// act, so the next natural load agrees; the 2-4s full rebuild is exactly what this replaces.
function revFire(key) {
  const p = revPending.get(key);
  if (!p || p.state === "posting" || p.state === "posted") return;
  p.state = "posting";
  p.timer = null;
  if (p.tick) { clearTimeout(p.tick); p.tick = null; }
  revPendingPaint(key);
  p.send().then(d => {
    if (!d || !d.ok) {
      // Failure STAYS. The bar wears the refusal until acted on, and the row keeps its accent —
      // a verdict that silently did not land is the worst outcome this surface can produce.
      p.state = "failed";
      p.error = (d && d.error) || "that did not go through";
      // You may have moved on since the window lapsed, and the row may be folded, filtered out or
      // scrolled away — so say it once out loud too. The row's own receipt is still there when you
      // come back to it.
      //
      // **With `p.url`, which is what makes it findable** (SKEIN-417). A verdict fires eight
      // seconds AFTER the press, so being elsewhere when it is refused is the ordinary case, not
      // the edge — and a toast is the only thing said at the moment it happens. The url buys two
      // things from `toast` at once: 8000ms instead of 3500ms, which is the difference between a
      // notice a reader catches and one they do not, and a way to GitHub, where a refusal can
      // actually be understood. It is the same `p.url` the receipt's "open on GitHub ↗" uses, so
      // the two ways to the pull request cannot disagree.
      toast(`#${p.number} refused: ${p.error}`, p.url);
      revPendingPaint(key);
      return;
    }
    p.state = "posted";
    p.said = d.text || "";
    // Only now that it is real: the saved draft has done its job.
    if (p.mineStore) { try { localStorage.removeItem(p.mineStore); } catch {} }
    revMarkDone(key, p.kind);
    revPendingPaint(key);
  });
}

/// try again — the same payload, not a re-assembly: what you decided is what posts.
function revRetry(key) {
  const p = revPending.get(key);
  if (!p || p.state !== "failed") return;
  p.error = "";
  revFire(key);
}

/// What the pressed control becomes, in every state of the held act. The success path never
/// toasts — this line, in the control you pressed, IS the feedback.
function revReceiptHtml(key, p) {
  const did = { approve: "approved", "request-changes": "changes requested", comment: "commented",
                archive: "set aside", merge: "merged",
                // The one write this panel grants (SKEIN-305). Replies stay on GitHub.
                resolve: "thread resolved",
                // Not a verdict and not a post — a read that REPLACES the review you have vetted,
                // so the receipt says what is about to be lost rather than what is about to be sent
                // (SKEIN-293).
                reread: "reading it again — this replaces the review you vetted" }[p.kind] || p.kind;
  if (p.state === "waiting") {
    return `<span class="revreceipt">✓ ${esc(did)}</span>
      <button type="button" class="revchip" onclick="revUndo(${esc(JSON.stringify(key))})">undo (u)</button>
      <span class="revreceipt"><span class="undoleft" data-undo-left="${esc(key)}">${p.left}s</span></span>`;
  }
  if (p.state === "posting") {
    return `<span class="revreceipt">✓ ${p.kind === "reread" ? "reading it again…" : `${esc(did)} — posting…`}</span>`;
  }
  if (p.state === "posted") {
    return `<span class="revreceipt">✓ ${p.kind === "reread" ? "read again" : `${esc(did)}${p.said ? ` — ${esc(p.said)}` : ""}`}</span>`;
  }
  return `<span class="revreceipt failed">✗ ${p.kind === "archive" ? "could not set aside"
      : p.kind === "reread" ? "could not read it again" : "GitHub refused"}: ${esc(p.error)}</span>
      <button type="button" class="revchip" onclick="revRetry(${esc(JSON.stringify(key))})">try again</button>${
      p.kind === "archive" ? "" : `
      ${link(p.url, "open on GitHub ↗")}`}`;
}

/// The queue row marks done IN PLACE — green dot, struck title — and does not vanish; vanishing
/// rows in a list you are navigating destroy your place. The pr object itself is updated so any
/// later full render agrees, and the row leaves on the next natural load.
function revMarkDone(key, kind) {
  const pr = ((revQueue && revQueue.prs) || []).find(x => rk(x) === key);
  if (pr) {
    // A read decides nothing (SKEIN-293). Without this arm a held re-read landed and marked the row
    // "you commented" — a verdict nobody gave, on the row it was about to be read on.
    if (kind === "reread") { /* nothing is decided by reading */ }
    else if (kind === "archive") pr.lane = "archived";
    else if (kind !== "merge") {
      pr.my_review = kind === "approve" ? "approved"
        : kind === "request-changes" ? "changes-requested" : "commented";
      pr.review_is_current = true;
      // And GitHub is no longer asking (SKEIN-354): `decided` reads `my_review_requested`, so a row
      // GitHub had RE-requested would go on claiming you after you approved it here — which is
      // exactly what marking a row done in place is for (SKEIN-162). Submitting a review clears the
      // request on GitHub's side too, so this is what the next refetch will say rather than a guess
      // the page is making on its own behalf.
      pr.my_review_requested = false;
    }
  }
  revDecided.add(key);
  revRepaintRow(key);
}

/// The one-row repaint this needs: replace that row's element alone, found by its data-rk hook —
/// never the pane, never the network.
function revRepaintRow(key) {
  if (!revpane || !revpane.querySelector) return;
  const pr = ((revQueue && revQueue.prs) || []).find(x => rk(x) === key);
  if (!pr) return;
  const el = revpane.querySelector(`.revrow${revRkQuery(key)}`);
  if (el) { el.outerHTML = revRow(pr); return; }
  // **A pull request inside a STACK is not a `.revrow`.** `revStackSteps` draws it as a `.step`,
  // and the `.revrow` around it carries the STACK's key — so this selector found nothing, and a
  // press on a stacked row did nothing visible at all. Reported live (SKEIN-284) by an owner whose
  // every open pull request is one 18-step stack: "approve with this review button doesn't really
  // have feedback. So when I click idk if it went through or not." Every test passed throughout,
  // because the suite drove loose rows.
  //
  // A whole repaint rather than a second, step-shaped selector: the surgery is an optimisation of
  // the common case, and an optimisation that cannot find its target must repaint rather than
  // silently do nothing — otherwise the next container to hold a row loses its presses too.
  // `renderReviewNow`, not `renderReview`, because this IS the answer to a press and §6 rule 2's
  // deferral is exactly what would swallow it again.
  //
  // Only when the row is actually ON SCREEN somewhere. A key that matches nothing is a row the
  // reader cannot see — filtered out, or inside a folded stack — and repainting the pane for it
  // would take a caret from a composer over a receipt nobody is looking at.
  if (revpane.querySelector(revRkQuery(key))) renderReviewNow();
}

/// Repaint the receipt where the control was: that pull request's queue row. Surgical on purpose —
/// renderReview() would drop scroll and rebuild the subtree the eye is on, which is the reflow
/// §7.1 exists to end.
///
/// One row and not the pane, always: the receipt has one element to swap, and a full render while
/// somebody is typing is what §6 rule 2 defers — which is how a merge GitHub refused reached
/// `failed` with nothing on screen at all (SKEIN-385). `revRepaintRow` forces `renderReviewNow`
/// where it cannot find the row, for that reason.
function revPendingPaint(key) {
  revRepaintRow(key);
}

if (typeof revpane !== "undefined" && revpane && revpane.addEventListener) {
  // The deferred render (see revRenderHeld) lands the moment the reader lets go: focus leaving
  // the composer or the search input, or the text selection over the pane collapsing. The
  // microtask lets activeElement/selection settle first; revRenderFlush re-asks revRenderHeld, so
  // nothing here decides anything.
  revpane.addEventListener("focusout", () => setTimeout(revRenderFlush, 0));
  document.addEventListener("selectionchange", () => setTimeout(revRenderFlush, 0));
}

// ---- the keyboard, dispatched (SKEIN-151/159, docs/review-ux.md §6) ---------------------------
//
// WHICH key means WHAT is shortcutFor's REVIEW table in cockpit/src/keys.mjs — one table, one
// guard, tested in node; this switch is the only place a review key is acted on. Two of the three
// deliberate absences live here as refusals that speak (§6): `a` on a collapsed row explains itself
// rather than acting, and `m` never arrives at all because merge is unbound in the table — one
// letter must not land a commit on a base branch. (`o`, the third, opens the row HERE, matching the
// fleet's own `o`; GitHub is the `g h` chord.)

const revRkQuery = key => `[data-rk="${key.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"]`;

function revKeyPr(key) {
  return ((revQueue && revQueue.prs) || []).find(x => rk(x) === key) || null;
}

// Move the keyboard's selection WITHOUT a render: the selection is two class flips, and the full
// render path is for when the data moved, not the eye. The state moves even where the DOM stubs
// out (the node worlds), so the next render paints the same answer.
function revKeySelect(key) {
  revSel = key;
  revSelAt = Math.max(0, revNav.indexOf(key));
  if (revpane && revpane.querySelectorAll) {
    for (const el of revpane.querySelectorAll(".revrow.sel, .step.sel")) el.classList.remove("sel");
    const el = revpane.querySelector(`.revrow${revRkQuery(key)}, .step${revRkQuery(key)}`);
    if (el && el.classList) el.classList.add("sel");
  }
  revKeyShowSel();
}

// The selected row kept on screen with 60px of lead (§6's queue table) — enough that the NEXT row
// is already visible when the eye gets there.
function revKeyShowSel() {
  if (!revpane || !revpane.querySelector || !revpane.getBoundingClientRect) return;
  const el = revpane.querySelector(".revrow.sel, .step.sel");
  if (!el || !el.getBoundingClientRect) return;
  const pane = revpane.getBoundingClientRect(), row = el.getBoundingClientRect();
  if (row.top < pane.top + 60) revpane.scrollTop += row.top - pane.top - 60;
  else if (row.bottom > pane.bottom - 60) revpane.scrollTop += row.bottom - pane.bottom + 60;
}

// §6 focus rule 4: an OPENED thing scrolls its expansion to ~25% of the viewport. scrollIntoView
// is not enough when the expansion is 543px tall: measured, expanding the last row put its
// actions at top:969 in a 900px viewport with scrollTop unchanged.
function revKeyShow(el) {
  if (!el || !el.getBoundingClientRect || !revpane || !revpane.getBoundingClientRect) return;
  const pane = revpane.getBoundingClientRect();
  revpane.scrollTop += el.getBoundingClientRect().top - pane.top - pane.height * 0.25;
}

// Has this row had its decision this session? What n/N walk past: a held or landed act, a row
// marked done in place, or a stack with no step left to review.
function revKeyUndecided(key) {
  const st = revStacks.get(key);
  if (st) return !!revStackNext(st) && !st.steps.every(p => revDecided.has(rk(p)));
  return !revDecided.has(key) && !revPending.has(key);
}

function revKey(action) {
  const openStack = revStackOpenKey ? revStacks.get(revStackOpenKey) : null;
  const stepKeys = openStack ? openStack.steps.map(rk) : [];
  switch (action) {
    case "rev-next":
    case "rev-previous": {
      const d = action === "rev-next" ? 1 : -1;
      // Inside an open stack, j/k walk its steps, clamped: the way out is ← or Esc, never falling
      // off the end into an unrelated lane.
      if (openStack && stepKeys.includes(revSel)) {
        revKeySelect(stepKeys[Math.max(0, Math.min(stepKeys.indexOf(revSel) + d, stepKeys.length - 1))]);
        return;
      }
      if (!revNav.length) return;
      const at = revNav.indexOf(revSel);
      const to = at < 0 ? (d > 0 ? 0 : revNav.length - 1) : Math.max(0, Math.min(at + d, revNav.length - 1));
      revKeySelect(revNav[to]);
      return;
    }
    case "rev-first": if (revNav.length) revKeySelect(revNav[0]); return;
    case "rev-last": if (revNav.length) revKeySelect(revNav[revNav.length - 1]); return;
    case "rev-next-undecided":
    case "rev-previous-undecided": {
      if (!revNav.length) return;
      const d = action === "rev-next-undecided" ? 1 : -1;
      const from = revNav.indexOf(revSel);
      for (let i = 1; i <= revNav.length; i++) {
        const k = revNav[(((from + d * i) % revNav.length) + revNav.length) % revNav.length];
        if (revKeyUndecided(k)) { revKeySelect(k); return; }
      }
      return;
    }
    case "rev-into": {
      const st = revSel ? revStacks.get(revSel) : null;
      if (!st) return;
      if (revStackOpenKey !== revSel) toggleRevStack(revSel);
      // Selection lands on the next actionable step — the only place this stack's review can start.
      revKeySelect(rk(revStackNext(st) || st.steps[0]));
      return;
    }
    case "rev-back": {
      if (revStackOpenKey) {
        const head = revStackOpenKey;
        toggleRevStack(head);          // closes it
        revKeySelect(head);            // selection returns to the stack's head row (§6)
        return;
      }
      if (revOpen.size) { toggleRevRow([...revOpen][0]); return; }
      // Nothing to leave: Esc clears the selection, the queue-mode echo of the fleet's deselect.
      revSel = null;
      if (revpane && revpane.querySelectorAll) {
        for (const el of revpane.querySelectorAll(".revrow.sel, .step.sel")) el.classList.remove("sel");
      }
      return;
    }
    case "rev-open": {
      // `o` means "open the thing, HERE" — the fleet keymap's own sense, and the reason `o` is not
      // "open on GitHub" (that is the `g h` chord). What it opens is the row: the reading, the
      // verdicts and the composer are all in the expansion. A stack head has no row of its own, so
      // it opens the step the stack would start at, which is what the selection means there.
      const st = revSel ? revStacks.get(revSel) : null;
      const target = st ? (revStackNext(st) || st.steps[0]) : revKeyPr(revSel);
      if (target) toggleRevRow(rk(target));
      return;
    }
    case "rev-aside": {
      const pr = revKeyPr(revSel);
      if (!pr) return;   // a stack head is set aside step by step, from where the evidence is
      const key = rk(pr);
      archivePr(pr.repo_id, pr.number, true);
      // The receipt strip lives in the expanded row; on a collapsed one the row greys (`held`)
      // and the toast names the key that takes it back — once, because the eye may be mid-list.
      if (!revOpen.has(key)) toast(`#${pr.number} set aside — u undoes it`);
      const at = revNav.indexOf(key);
      if (at >= 0 && at + 1 < revNav.length) revKeySelect(revNav[at + 1]);   // selection advances
      return;
    }
    case "rev-undo": {
      const waiting = k => k && (revPending.get(k) || {}).state === "waiting";
      const key = [revLastActKey, revSel].find(waiting) || [...revPending.keys()].find(waiting);
      if (!key) return;
      revUndo(key);
      revKeySelect(key);   // selection returns to the row whose act was taken back (§6)
      return;
    }
    case "rev-search": {
      const box = revpane && revpane.querySelector && revpane.querySelector(".revsearch");
      if (box && box.focus) box.focus();
      return;
    }
    case "rev-repo-menu": {
      const picker = revpane && revpane.querySelector && revpane.querySelector(".revrepo");
      if (!picker) return;
      if (picker.focus) picker.focus();
      try { if (picker.showPicker) picker.showPicker(); } catch {}   // needs a user gesture; a keydown is one
      return;
    }
    case "rev-github": {
      const pr = revKeyPr(revSel);
      // **`window.open` navigates, so it asks the same question the anchors ask** (SKEIN-602).
      //
      // Every anchor the page builds goes through `link`, which asks `safeHref` whether the scheme
      // is one this cockpit will follow. This keypress opens the same API-supplied `pr.url` without
      // drawing an anchor at all — no `href` for that fix to have covered — and a guard that holds
      // on the drawn link but not on the shortcut to the same destination is a guard with a
      // keyboard-shaped hole in it.
      //
      // Refused means nothing opens, which is `link`'s own answer rather than a new one: a URL this
      // page will not follow is not followed. The row is already saying so — its "open on GitHub ↗"
      // is sitting there as plain text, because the same judgement refused the same string when the
      // row was drawn — so the reader is not being silently ignored, and a notice here would be the
      // one place in the page that announces a refusal out loud.
      const safe = pr && pr.url ? safeHref(pr.url, document.baseURI) : null;
      if (safe && typeof window !== "undefined" && window.open) window.open(safe, "_blank", "noopener");
      return;
    }
    case "rev-reread": {
      // The keyboard half of the row's read control. A stack head has no reading of its own — the
      // steps do — so it reads the step the stack would start at, which is the row the selection
      // means when it sits on a stack.
      const st = revSel ? revStacks.get(revSel) : null;
      const pr = st ? (revStackNext(st) || st.steps[0]) : revKeyPr(revSel);
      if (!pr) return;
      revFetchSummary(pr.repo_id, pr.number, "force");
      // The keypress is invisible otherwise — the selected row may be off screen — so the toast
      // says WHICH pull request is being read and nothing about what it costs: see `revReadAgain`
      // on why "never counts against the day" left every control it was on.
      toast(`reading #${pr.number} again`);
      return;
    }
    case "rev-approve":
      // Deliberate refusal, out loud (§6). A verdict is a press on the row's own control, where the
      // reading it is a verdict about is printed directly above it — never a keystroke on a list.
      // The key EXISTS so the answer can be this sentence instead of silence.
      toast("approve is a chip on the row — ↵ opens it");
      return;
    // `c`, `r`, `]` and `[` addressed hunks and files in the reading view, which is gone. They stay
    // in the REVIEW table because being in that table is what shadows them from the fleet map — a
    // key that reached the fleet map from this pane would move a selection behind it — and they
    // answer for the same reason `rev-approve` does directly above: the key exists so the answer
    // can be a sentence instead of silence. Naming the chips by their exact labels is what makes
    // the sentence actionable; `revVerdictHtml` is where those labels are written.
    case "rev-comment":
      toast("comment… is a chip on the row — ↵ opens it");
      return;
    case "rev-request":
      toast("request changes… is a chip on the row — ↵ opens it");
      return;
    // These two have no chip to name: nothing in the pane is file-shaped any more. So they say
    // where the diff went, and name the key that goes there (`g h`, `rev-github`).
    case "rev-next-file":
    case "rev-previous-file":
      toast("skein does not show the diff — g h opens the change on GitHub");
      return;
  }
  const nth = /^rev-repo-([1-9])$/.exec(action);
  if (nth) {
    // g1…g9: the nth repo in the picker's own order, which is the `repos` list's order.
    const r = repos[+nth[1] - 1];
    if (!r) return;
    revSel = null;   // a repo jump starts at its first row, not wherever the old key resurfaces
    openReview(r.id);
    if (revNav.length) revKeySelect(revNav[0]);
    return;
  }
}

// ---- stacks: dependent pull requests are one row, reviewed bottom-up (SKEIN-147/160) ----
//
// Detection is `base_ref ∈ {head_ref}` over the queue the pane already holds — no network, no
// model. Per repo, because a chain never crosses repos, over the MERGED list, because the row
// must sit among rows that do.
// The branch every pull request in this repo is ultimately for. The queue reports it per repo
// (`prq::Queue::trunk`), and it is what makes a stack knowable rather than guessable — see
// `revChains`. Empty when skein could not read it, which the caller must handle rather than treat
// as "no trunk".
function revTrunkOf(repoId) {
  for (const q of (revQueue && revQueue.queues) || []) {
    if (q.repo_id === repoId) return q.trunk || "";
  }
  return "";
}

// The stacks in a queue, as a TREE flattened depth-first — parents before children, so "review from
// the bottom" is still true reading downwards along any path.
//
// **The trunk is severed because it IS the trunk** (SKEIN-288). This used to link a base to its
// child only where that base had exactly ONE open child, using "several children" as a proxy for
// "this is a trunk". The proxy was written for a real bug — PR #625 is `develop → master`, so
// `develop` became "a head", every develop-rooted stack dissolved and an arbitrary one won — but it
// cannot tell a trunk from a FORK. On the owner's own queue, `fix/readiness-abstention-kinds` has
// two open children (#586 and #671), so their 21-step stack was cut in half and the far half
// renumbered from step 1: reported as "PR 586 is 4th on the list while it shows up as 1st". Three
// more rows (#671, #672, #711) were stranded loose by the same rule.
//
// Now the trunk is read from the queue and excluded by NAME, and a fork is what it is: a branch.
// When the trunk is not known the old proxy is kept as the fallback, deliberately — without it
// skein genuinely cannot tell a trunk from a fork, and dissolving every stack is the worse of the
// two failures.
function revChains(prs) {
  const out = [];
  const byRepo = new Map();
  for (const p of prs) {
    if (!byRepo.has(p.repo_id)) byRepo.set(p.repo_id, []);
    byRepo.get(p.repo_id).push(p);
  }
  for (const [repo, list] of byRepo) {
    const trunk = revTrunkOf(repo);
    const byHead = new Map(list.map(p => [p.head_ref, p]));
    const kids = new Map();
    for (const p of list) {
      // The trunk is not a seam, however many pull requests sit on it.
      if (trunk && p.base_ref === trunk) continue;
      if (!byHead.has(p.base_ref)) continue;
      kids.set(p.base_ref, (kids.get(p.base_ref) || []).concat([p]));
    }
    // Without a trunk to name, fall back to what this did before: a base with more than one child
    // is assumed to be a trunk. It shatters forks — that is the bug above — but it is the failure
    // that leaves the stacks standing rather than the one that dissolves them all.
    if (!trunk) for (const [base, arr] of [...kids]) if (arr.length > 1) kids.delete(base);
    const isChild = new Set();
    for (const arr of kids.values()) for (const p of arr) isChild.add(rk(p));
    // A root has at least one step above it and is nobody's child.
    const roots = list.filter(p => kids.has(p.head_ref) && !isChild.has(rk(p)));
    for (const r of roots) {
      const steps = [];
      const depth = new Map();
      const branches = new Set();   // steps that START a branch — a second-or-later child
      const forks = new Set();      // steps the tree divides at
      const seen = new Set();
      const walk = (p, at) => {
        if (seen.has(rk(p))) return;   // a loop in branch names must not hang the pane
        seen.add(rk(p));
        steps.push(p);
        depth.set(rk(p), at);
        const mine = kids.get(p.head_ref) || [];
        if (mine.length > 1) forks.add(rk(p));
        mine.forEach((kid, i) => { if (i) branches.add(rk(kid)); walk(kid, at + 1); });
      };
      walk(r, 1);
      if (steps.length > 1) {
        out.push({
          repo_id: repo, steps, depth, branches, forks,
          // **Is step 1 really step 1?** Only when the root sits on the trunk. #650's own base is
          // `parser-results-file-hash`, which is in nobody's queue — merged, closed, or past the end of
          // a truncated search — so the stack is deeper than anything skein can see and a "1" would
          // be the same lie the fork was. `rooted: false` makes every number read "at least".
          rooted: !!trunk && r.base_ref === trunk,
          name: revStackName(steps), misnamed: revMisnamed(steps),
        });
      }
    }
  }
  return out;
}

// The chain's name: the longest branch-name prefix its members share, cut at a separator.
function revStackName(steps) {
  let prefix = steps[0].head_ref || "";
  for (const s of steps) {
    const h = s.head_ref || "";
    let i = 0;
    while (i < prefix.length && prefix[i] === h[i]) i++;
    prefix = prefix.slice(0, i);
  }
  const cut = Math.max(prefix.lastIndexOf("/"), prefix.lastIndexOf("-"));
  return (cut > 0 ? prefix.slice(0, cut) : prefix).replace(/[\/-]+$/, "") || steps[0].repo_id;
}

// The steps whose own titles number them out of order. The reason reverse order is dangerous is
// that the TITLES carry a competing order that looks authoritative — so the view contradicts them
// out loud rather than silently re-sorting.
function revMisnamed(steps) {
  const num = s => {
    const m = /\bslice\s+(\d+)/i.exec(s.title || "") || /-(\d\d)[a-z]?-/.exec(s.head_ref || "");
    return m ? +m[1] : null;
  };
  const pad = n => String(n).padStart(2, "0");
  const out = new Map();
  let prev = null;
  for (const s of steps) {
    const n = num(s);
    if (n != null && prev != null && n < prev) out.set(s.number, `named ${pad(n)}, sits after ${pad(prev)}`);
    if (n != null) prev = n;
  }
  return out;
}

// A stack's own key in the queue's namespace — its head PR's rk with a marker, so the keyboard's
// selection can name a stack row the same way it names a loose one and neither can collide.
function revStackKey(st) { return "stack:" + st.repo_id + "#" + st.steps[0].number; }
// The first step still awaiting you — the only place a review of this stack can start. Asked of
// `moveOf`, so a stack of your OWN pull requests with a thread open on one of them enters the
// your-move list at that step rather than sitting silently below it (SKEIN-302).
function revStackNext(st) { return st.steps.find(p => moveOf(p) === "yours") || null; }
// A stack sits in the group of its next actionable step, not of its tip.
function revStackLane(st) { return revStackNext(st) ? "yours" : "theirs"; }

let revStackOpenKey = null;   // which stack is expanded — at most one, ever
let revStackStep = null;      // rk of the step whose full row is open inside it

function toggleRevStack(key) {
  revStackOpenKey = revStackOpenKey === key ? null : key;
  // Exclusive, and not as a preference: fifteen steps is most of a viewport, and a second open
  // anything makes the queue unnavigable. Opening a stack closes every row; see toggleRevRow for
  // the other direction.
  revOpen = new Set();
  revStackStep = null;
  if (revStackOpenKey) { revSel = key; revSelAt = Math.max(0, revNav.indexOf(key)); }   // opening is selecting
  renderReviewNow();
  // §6 focus rule 4, same as a row: fifteen steps of expansion land at ~25%, not below the fold.
  if (revStackOpenKey && revpane && revpane.querySelector) {
    revKeyShow(revpane.querySelector(`.revrow${revRkQuery(key)}`));
  }
}
function toggleStackStep(key) {
  revStackStep = revStackStep === key ? null : key;
  // A step's body is `revBody`, the same expansion a loose row draws — so it needs the same prose,
  // which the queue payload no longer carries (SKEIN-287).
  if (revStackStep === key) {
    revLoadReading(key.slice(0, key.lastIndexOf("#")), Number(key.slice(key.lastIndexOf("#") + 1)));
  }
  renderReviewNow();
}

// A step's number is its DEPTH in the stack, and it says "at least" when the bottom is out of sight.
//
// It used to be the step's position in whatever fragment the page had assembled, which is what the
// owner caught: "PR 586 is 4th on the list while it shows up as 1st" (SKEIN-288). Two things had to
// be true for the number to stop lying — the fragment had to become the whole stack (`revChains`),
// and the number had to stop counting positions. A stack whose lowest steps are merged, closed or
// past the end of a truncated search is deeper than the queue can show, and `3+` is what skein
// actually knows: at least three.
function revStepNo(st, p) {
  const d = st.depth.get(rk(p)) || (st.steps.indexOf(p) + 1);
  return st.rooted ? `${d}` : `${d}+`;
}
// One line, deliberately: `lift.mjs`'s `grab` ends a `const` at the first newline outside brackets,
// so a backslash continuation here is a truncated declaration in every suite that lifts it.
const REV_UNROOTED_WHY = "the pull requests below this stack are not in your queue — merged, closed, or past the end of the search — so this is at least that deep, not exactly";

// ── READING A WHOLE STACK (SKEIN-337) ─────────────────────────────────────────────────────────
//
// A stack is one change split across pull requests — the owner runs an 18-step one — and reading it
// meant pressing the row's control eighteen times and waiting ~35 seconds after each. The per-step
// control stays the default; this is a second, explicitly labelled one, which is what the owner
// chose over replacing it: "read step 2" and "read all 18" are different sizes of decision and the
// button should say which you are taking.

// What a read has actually COST here, most recent last. The estimate on the button is derived from
// this and from nothing else: a hardcoded "~35s" would be a claim about a model, a fleet and a
// network that changes without telling anyone, and it would go on being printed after it stopped
// being true. Eight is enough to be a median and short enough to follow a real change in speed.
let revReadMs = [];
function revNoteReadMs(ms) {
  if (!(ms > 0)) return;
  revReadMs.push(ms);
  if (revReadMs.length > 8) revReadMs.shift();
}

// The median rather than the mean: one read that hit a retry or a cold cache should not move the
// estimate for the other seventeen.
function revReadTypicalMs() {
  if (!revReadMs.length) return 0;
  const sorted = [...revReadMs].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
}

// **No measurement, no estimate.** An empty string, not a guess — a page that has read nothing yet
// has nothing to base a number on, and a number with nothing behind it is worse than no number,
// because it will be believed.
function revStackEstimate(n) {
  const typical = revReadTypicalMs();
  if (!typical || n < 1) return "";
  const ms = Math.ceil(n / REV_SUM_PARALLEL) * typical;
  return ms >= 90_000 ? ` · ~${Math.round(ms / 60_000)}m` : ` · ~${Math.round(ms / 1000)}s`;
}

// **Removed: the "no review — read again" affordance** (SKEIN-660; it was SKEIN-371's).
//
// What stood here was `revNoReviewCameBack` — true when skein held a reading of THIS commit, no
// review of it, and a review HAD been bought here and had not come back — and with it go the three
// surfaces it decided: the "noreview" answer `revWantsRead` used to give, the row control labelled
// "no review — read again" carrying the reason in its tooltip, and the stack shortfall
// "N read but with no review" in `revStackRunHtml`. `revStepNeedsReading` counted it too, so such a
// step was re-read by "read all"; it no longer is.
//
// **It went because the wire has no field for it.** Its sole discriminator was `critique_because`,
// and `Known` is a `Summary` and a `stale` flag (`src/review/summary.rs`) — neither names it, and
// no server has sent it since the drafted review moved to GitHub (SKEIN-612). The branch could not
// answer true on any real payload, so what was deleted was an affordance nothing could reach.
//
// **What that costs, decided with the cost in front of the owner.** A stack can no longer tell a
// reviewed step from an unreviewed one. Read is read: nothing on a payload separates "a review came
// back" from "a review was bought and failed" from "nobody ever asked for one". SKEIN-371 was a
// real complaint — ten steps of a twenty-step stack counted as read with no review, and no retry
// offered on any of them — and that failure mode did not stop existing; the page simply lost the
// ability to say so. Saying it again is the SERVER's move first: a field on `Summary` recording
// that a reading was bought and its review did not come back. Adding a client-side guess instead
// would be this branch again, and it would be wrong the same way.

// Is this step worth spending a model call on? Unread, or read against a commit that has moved, or
// a reading that failed to be REACHED — and never one already in flight, which is somebody else's
// purchase of the same thing. A reading whose REVIEW did not come back was a fourth answer here
// until SKEIN-660; the note above says why it is not one any more.
function revStepNeedsReading(p) {
  const key = rk(p);
  if (revInFlight.has(key)) return false;
  const s = revSums.get(key);
  if (!s || s === "…") return true;
  return !!s.transient || s.depth === "unread" || s.head_sha !== p.head_sha;
}

// **A run per stack** (SKEIN-353). This was one global — *"one run at a time, and only the stack
// that is open; two of them racing is not a state worth supporting"* — and that decision was wrong
// twice over. The owner runs several stacks, and the code did not refuse the second press: it took
// it, overwrote the global, and dropped whatever the first run still had to read. Reported as the
// smaller half of the damage — "the ladder stack was running read all and when I clicked the read
// all for other stack, I can't see the read all progress in ladder stack" — because a run whose
// queue has been orphaned looks exactly like a run that finished quietly.
//
// Both halves came from the same line. `revStackRunHtml` draws a run only for the stack it belongs
// to, so the first stack fell back to its "read all N" button; `revStackPump` read the global, so
// nothing advanced the first run's `todo` ever again. What was already right, and is what makes
// this fix small: the `.finally` closes over its own `r`, so the reads in flight when the second
// press landed still finished and still booked against the run that started them.
let revStackRuns = new Map();   // stack key -> its run

function revStackReadAll(key) {
  const st = revStacks.get(key);
  if (!st) return;
  // A run already going on THIS stack is left alone. The press cannot be seen while one is running
  // — the row draws progress where the button was — so arriving here means a stale render, and
  // replacing the run would be the exact silent abandonment this is fixing, one stack in.
  const going = revStackRuns.get(key);
  if (going && !going.stopped && (going.todo.length || going.running.size)) return;
  const todo = st.steps.filter(revStepNeedsReading).map(p => ({
    repo: p.repo_id, number: p.number, no: revStepNo(st, p), key: rk(p),
  }));
  if (!todo.length) { toast("every step of this stack already has a reading of its current commit"); return; }
  revStackRuns.set(key, {
    key, todo, running: new Map(), done: 0, total: todo.length,
    started_ms: Date.now(), stopped: false,
  });
  revStackPump();
  renderReviewNow();
}

// **Stopping does not undo.** Steps already read stay read — they were paid for and the readings are
// on disk — so this only stops STARTING more. The ones in flight finish, because cancelling them
// would spend the money and throw the answer away.
function revStackStop(key) {
  const r = revStackRuns.get(key);
  if (!r) return;
  r.stopped = true;
  // The other runs keep their slots and carry on — stopping one stack is not stopping reading.
  // Their pump has to be nudged, because the width this one just gave back is shared.
  revStackPump();
  renderReviewNow();
}

// Several at once, which is what the owner chose over one-at-a-time for the wall clock. The width is
// `REV_SUM_PARALLEL` and deliberately the same number as the pane's own pump: the reason for that
// throttle is not a UI nicety, it is that these reads share a rate limit with the boxes doing the
// real work, and a stack read is exactly the burst that would take the window away from them.
// **A press is not held to the read-ahead width, and `REV_SUM_PARALLEL` is not this function's
// business** (SKEIN-353). That width of three governs skein's OWN initiative — `revPumpSummaries`
// — and nothing a person pressed. The reads not ending is its own defect (SKEIN-350); the shared
// width of three is what turned three stuck reads into two stalled queues.
//
// So a run reads at `REV_ASKED_PARALLEL` — see there for why that number exists at all and why it
// is not `REV_SUM_PARALLEL` — and runs are INDEPENDENT of each other. They do not share a pipe: a
// second stack the reader pressed does not queue behind the first, because both are things he
// asked for and neither is skein deciding to spend his day.
function revStackPump() {
  for (const r of revStackRuns.values()) {
    while (!r.stopped && r.running.size < REV_ASKED_PARALLEL && r.todo.length) {
      const step = r.todo.shift();
      r.running.set(step.key, step);
      revFetchSummary(step.repo, step.number, "force").finally(() => {
        // `r`, not a lookup: a run the reader has since stopped, or a stack that has left the
        // queue, must still finish its own bookkeeping against itself rather than crediting a step
        // to whatever is running now.
        r.running.delete(step.key);
        r.done++;
        revStackPump();
        renderReviewNow();
      });
    }
  }
}

// What the stack row says while a run is going, and after it ends.
//
// It stays on screen when the run finishes rather than vanishing: the last thing a reader wants to
// know about eighteen model calls is that they happened and how many landed, and a progress line
// that disappears at 18 of 18 answers that question by removing it.
function revStackRunHtml(st) {
  const key = revStackKey(st);
  const unread = st.steps.filter(revStepNeedsReading).length;
  // A shortfall count stood here — "N read but with no review", SKEIN-371's number — and went with
  // the branch that decided it (SKEIN-660). This line can say how many steps have no reading; it
  // cannot say how many were read and came back without a review, because no payload says so.
  const r = revStackRuns.get(key) || null;
  if (!r) {
    if (!unread) return `<div class="stackread dim">Every step has a reading of its current commit.</div>`;
    const all = unread === st.steps.length;
    return `<div class="stackread">
      <button type="button" class="revchip" onclick="event.stopPropagation(); revStackReadAll(${esc(JSON.stringify(key))})">${
        all ? `read all ${unread}` : `read the ${unread} not yet read`}${revStackEstimate(unread)}</button>
      <span class="dim">${unread} model call${unread === 1 ? "" : "s"}, ${
        Math.min(unread, REV_ASKED_PARALLEL)} at a time</span>
    </div>`;
  }
  const live = [...r.running.values()];
  // Over when nothing is running and there is nothing left to start — and a STOPPED run has
  // nothing left to start whatever is still in its queue, which is the case that read as "still
  // going" for ever: `stopped` never drains `todo`, so a run ended by the reader would otherwise
  // sit on "⟳ reading stack…" with nothing in flight and nothing coming.
  const over = !live.length && (!r.todo.length || r.stopped);
  const steps = live.map(x => x.no).sort((a, b) => a - b);
  return `<div class="stackread">
    <span class="revflight">${over ? "✓" : "⟳"} ${
      over ? (r.stopped ? "stopped" : "read the stack")
           : `reading stack…`} ${r.done} of ${r.total} done
      <span class="revflight-secs" data-started="${r.started_ms}">${revElapsed(Date.now() - r.started_ms)}</span></span>
    ${live.length ? `<span class="dim">${live.length} running · step${steps.length === 1 ? "" : "s"} ${steps.join(", ")}</span>` : ""}
    ${r.stopped && live.length ? `<span class="dim">stopping — ${live.length} still finishing</span>` : ""}
    ${over && r.stopped && r.todo.length ? `<span class="dim">${r.todo.length} left unread — nothing was undone, the steps already read stay read</span>` : ""}
    ${over || r.stopped ? "" : `<button type="button" class="revchip" onclick="event.stopPropagation(); revStackStop(${esc(JSON.stringify(key))})">stop</button>`}
  </div>`;
}

// **What a stack's run says on its COLLAPSED row** (SKEIN-370). Empty when it has none.
//
// SKEIN-353 made the runs independent — two stacks read at once, each with its own queue, counter
// and stop control — and left the progress exactly where it had always been: `revStackRunHtml`,
// reached only from `revStackSteps`, which draws only for the stack that is OPEN. Opening is
// exclusive (`toggleRevStack` sets `revOpen = new Set([key])` and clears `revStackOpenKey`,
// docs/review-ux.md §2.5), so the owner's sentence was half answered — "the ladder stack was
// running read all and when I clicked the read all for other stack, I can't see the read all
// progress in ladder stack". The run survives now; it was simply off screen while he looked at the
// other one, and the collapsed row said "you are at step 2 of 18 · review from the bottom" whether
// or not eighteen model calls were in flight for it.
//
// **It leads the gist cell, and it is short.** That column is rationed (§4) and truncates from the
// right, so the news has to be the part that survives. What it displaces is "review from the
// bottom" — standing advice about where to start, not news — while "you are at step N of M" stays,
// because a reader still has to know where they are.
//
// **It stays after the run ends**, the same rule the expanded line already has: the last thing
// somebody wants to know about eighteen model calls is that they happened and how many landed, and
// a progress line that vanishes at 18 of 18 answers that question by removing it.
//
// A stack with no run renders exactly what it rendered before — the empty string is the whole of
// that promise, and `tests/ui/stackread.mjs` holds it.
function revStackRunGist(st) {
  const r = revStackRuns.get(revStackKey(st));
  if (!r) return "";
  // Over when nothing is running and there is nothing left to start — and a STOPPED run has
  // nothing left to start whatever is still in its queue. Same test as `revStackRunHtml`, because
  // two surfaces disagreeing about whether a run is going is worse than either being wrong.
  const over = !r.running.size && (!r.todo.length || r.stopped);
  return `<span class="revflight">${over ? "✓" : "⟳"} ${
    over ? (r.stopped ? "stopped at" : "read") : "reading"} ${r.done} of ${r.total}</span> · `;
}

function revStackRow(st) {
  const key = revStackKey(st);
  const open = revStackOpenKey === key;
  const next = revStackNext(st);
  // Drawn once and asked twice: the marker's presence is also what takes "review from the bottom"
  // off the line, and computing it in two places is how the two come to disagree.
  const running = revStackRunGist(st);
  const line = `<div class="revline" onclick="toggleRevStack(${esc(JSON.stringify(key))})">
      <span class="mv ${next ? "yours" : "done"}" title="${next ? "your move" : "every step is decided"}"></span>
      <span class="stackmark" title="${esc(st.repo_id)}">stack</span>
      <span class="tcell"><span class="revtitle">${esc(st.name)} — ${st.steps.length} pull requests, one change${
        st.forks.size ? `, branching` : ""}</span></span>
      <span class="gist"${st.rooted ? "" : ` title="${esc(REV_UNROOTED_WHY)}"`}>${running}${next
        ? `you are at step ${revStepNo(st, next)} of ${st.steps.length}${
            st.rooted ? "" : " skein can see"}${running ? "" : " · review from the bottom"}`
        : `every step is decided`}</span>
      ${revRail(next || st.steps[0])}
    </div>`;
  return `<div class="revrow stack${open ? " open" : ""}${revSel === key ? " sel" : ""}${
    revFlash === key ? " flash" : ""}" data-rk="${esc(key)}">${line}${open ? revStackSteps(st) : ""}</div>`;
}

// Whose move a STEP is — the row's own question, plus the one only a stack can ask. A step that
// needs you but sits on a base you have not reviewed cannot be reviewed yet, and `.mv.blocked`
// (docs/review-ux.md §4) and `REV_MOVE_WORDS.blocked` were both written for exactly that and had
// no caller: the one thing that makes a stack dangerous — reviewing step 7 before step 3 — was the
// one state the mark could not take.
//
// The base is found by BRANCH NAME, never by position in the list. A stack is a tree (SKEIN-288),
// and on a fork the step above you in the depth-first order is the other branch's tip rather than
// the thing your diff is expressed against — so "is the row before me undecided" would block a
// second branch whose own base was decided days ago.
function revStepMove(st, p) {
  const m = revMove(p);
  if (m !== "yours") return m;
  const base = st.steps.find(q => q.head_ref === p.base_ref);
  return base && moveOf(base) === "yours" ? "blocked" : m;
}

function revStackSteps(st) {
  const next = revStackNext(st);
  const rows = st.steps.map((p, i) => {
    const decided = moveOf(p) === "theirs";
    const cls = p === next ? " next" : decided ? " done" : "";
    const note = st.misnamed.get(p.number);
    // A FORK is not a break, and it is not silent either (SKEIN-288). Two pull requests on one base
    // is an ordinary thing to do in a live stack; the old rule shattered the stack there rather than
    // saying so. The steps are laid out depth-first — every path still reads bottom-up — and the
    // step that starts a second branch says which step it left from, so a reader can see the tree in
    // a list without the list pretending to be a line.
    const from = st.branches.has(rk(p)) ? (st.depth.get(rk(p)) || 1) - 1 : 0;
    // **The row's own cells, on a step** (SKEIN-352). `revGist` and `revRail` rather than anything
    // written here: a step showing a summary in its own dialect is two renderings of one reading,
    // and the second is the one that drifts. It is also what makes the stack read's "4 of 16 done"
    // checkable — the four say what they found, the one in flight carries its counter, and the
    // eleven left say `not read`.
    const mv = revStepMove(st, p);
    return `<div class="step${cls}${st.branches.has(rk(p)) ? " branch" : ""}${
      revSel === rk(p) ? " sel" : ""}" data-rk="${esc(rk(p))}" onclick="toggleStackStep(${esc(JSON.stringify(rk(p)))})">
        <span class="sn"${st.rooted ? "" : ` title="${esc(REV_UNROOTED_WHY)}"`}>step ${revStepNo(st, p)}</span>
        <span class="node"><span class="mv ${mv}" title="${esc(REV_MOVE_WORDS[mv] || "")}"></span></span>
        <span class="revnum" title="${esc(p.repo_id || "")}">#${p.number}</span>
        <span class="tcell"><span class="st">${esc(p.title || "")}</span>${from
          ? `<span class="revtag" title="two pull requests are based on step ${from}, so the stack branches there — each branch is still reviewed bottom-up">branches from step ${from}${st.rooted ? "" : "+"}</span>`
          : ""}${note ? `<span class="misnamed">⟨${esc(note)}⟩</span>` : ""}${
          // **The retry, on the step itself** (SKEIN-371). A step whose review did not come back
          // has to be buyable from where it is read, not by leaving the stack — and the same
          // control covers the two states it always covered, which a step row never drew at all.
          // `revReadAgain`, not a second control written here: one rule for when a read is offered.
          revReadAgain(p, revStackStep === rk(p))}</span>
        ${revGist(revSums.get(rk(p)), rk(p), revStackStep === rk(p))}
        ${revRail(p)}
      </div>${revStackStep === rk(p) ? revBody(p) : ""}`;
  }).join("");
  return `<div class="steps">
      ${revStackRunHtml(st)}
      <div class="stackhint">bottom-up: each diff is expressed against the one below it. The order here is the branch graph (base_ref), not the numbers in the titles.${
        st.rooted ? "" : " The steps below this stack are not in your queue, so the numbers say \u201cat least\u201d."}</div>
      ${rows}</div>`;
}

function toggleRevRow(key) {
  if (revOpen.has(key)) {
    // A LANDED receipt has been seen; folding the row retires it. Waiting and failed stay — one
    // still owes GitHub the post when its window lapses, the other still owes YOU an answer, and
    // the row keeps its `revtag.refused` either way (SKEIN-385).
    const p = revPending.get(key);
    if (p && p.state === "posted") revPending.delete(key);
    revOpen.delete(key);
    renderReviewNow();
    return;
  }
  // Looking at it IS the acknowledgement (SKEIN-333). Cleared here rather than on a timer, so the
  // mark survives however long the reader is away and goes the moment they arrive.
  revUpdated.delete(key);
  // Exclusive (docs/review-ux.md §2.5): opening a row closes every other row and any open stack.
  revOpen = new Set([key]);
  revStackOpenKey = null;
  revStackStep = null;
  // Opening IS selecting — the keyboard continues from the row the mouse chose, not from where
  // the selection last was.
  revSel = key;
  revSelAt = Math.max(0, revNav.indexOf(key));
  renderReviewNow();
  // §6 focus rule 4: the expansion lands at ~25% of the viewport, actions visible.
  if (revpane && revpane.querySelector) revKeyShow(revpane.querySelector(`.revrow${revRkQuery(key)}`));
  // The prose the row shape left behind — off disk, no model call.
  const [repo, number] = [key.slice(0, key.lastIndexOf("#")), Number(key.slice(key.lastIndexOf("#") + 1))];
  if (((revQueue && revQueue.prs) || []).some(p => rk(p) === key)) revLoadReading(repo, number);
}

// Set aside until the head moves (SKEIN-144) — the other instrument beside the archive: an
// archive holds until a human undoes it, a snooze holds until the AUTHOR acts. Empty sha is the
// by-hand return; the ordinary ending is the author's next push un-matching the stored sha.
function revSnooze(id, n, sha) {
  fetch(`/api/repos/${encodeURIComponent(id)}/review/${n}/snooze`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ head_sha: sha || "" }),
  }).then(r => r.json()).then(d => {
    if (!d.ok) { toast(d.error || "that did not go through"); return; }
    toast(sha ? `#${n} set aside — back at the author's next push` : `#${n} is back`);
    loadReview(true);
  }).catch(() => toast("that did not go through"));
}

// One decision, not twenty-six: every red row still in your lane, set aside at the head it shows.
// Composed here from rows the page is already holding, so the sweep cannot disagree with the
// screen that was clicked — each returns on its own when its author pushes.
function revSnoozeRed() {
  const rows = ((revQueue && revQueue.prs) || []).filter(p =>
    (!revRepoFilter || p.repo_id === revRepoFilter)
    && p.checks === "failing" && p.lane === "needs-you" && p.head_sha);
  if (!rows.length) { toast("nothing red is waiting on you"); return; }
  Promise.all(rows.map(p =>
    fetch(`/api/repos/${encodeURIComponent(p.repo_id)}/review/${p.number}/snooze`, {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ head_sha: p.head_sha }),
    }).then(r => r.json()).catch(() => ({ ok: false }))
  )).then(results => {
    const ok = results.filter(r => r && r.ok).length;
    toast(`${ok} of ${rows.length} set aside — each returns when its author pushes`);
    loadReview(true);
  });
}

// Set aside rides SKEIN-162's receipt/undo shape too — §7.1 calls it "the cheapest gesture and
// therefore the one most often mis-aimed after a reflow": the button becomes the receipt, undo
// inside the window means nothing was ever posted, and the row greys in place rather than the
// pane rebuilding. Bring-back stays immediate: restoring a row is its own undo.
function archivePr(id, n, on) {
  const key = id + "#" + n;
  const send = () => fetch(`/api/repos/${encodeURIComponent(id)}/review/${n}/archive`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ on })
  }).then(r => r.json()).catch(e => ({ ok: false, error: String(e.message || e).split("\n")[0] }));
  if (!on) {
    send().then(d => {
      if (!d.ok) { toast("could not update: " + (d.error || "unknown")); return; }
      loadReview(true);
    });
    return;
  }
  const pr = ((revQueue && revQueue.prs) || []).find(x => rk(x) === key) || {};
  revHold(key, {
    repo: id, number: n, kind: "archive", verdict: false, mineStore: "",
    url: pr.url || `https://github.com/${id}/pull/${n}`, send,
    state: "waiting", left: Math.round(REV_UNDO_MS / 1000), timer: null, tick: null, error: "", said: "",
  });
}

