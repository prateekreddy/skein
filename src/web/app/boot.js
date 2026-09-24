// ---------- global keys ----------
document.addEventListener("keydown", e => {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") { e.preventDefault(); pal.classList.contains("open") ? closePalette() : openPalette(); return; }
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "n") { e.preventDefault(); openNewBox(); return; }
  const away = document.getElementById("away");
  if (away?.classList.contains("open")) { if (e.key === "Escape") away.classList.remove("open"); return; }
  if (mbx.classList.contains("open")) { if (e.key === "Escape") closeMailbox(); return; }
  if (newbox.classList.contains("open")) return;
  if (pal.classList.contains("open")) return;
  // Settings owns its own keys (esc closes, ⏎ saves, arrows walk the section rail); a key that leaks
  // to the fleet keymap from an open dialog moves the selection behind it.
  if (settingsModal().classList.contains("open")) { if (e.key === "Escape") closeSettings(); return; }
  // When focus is in a live terminal or any text field, let it own every key — including Esc, which
  // the agent (claude/vim/less/fzf) needs. **The decision is `shortcutFor`, tested in node**: what
  // is DOM-shaped is knowing where the focus is, and that is what is passed in.
  const inTerm = view.mode === "term" && activeSession()?.host.contains(document.activeElement);
  const inField = /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName) || e.target.isContentEditable;
  // Which key means what is `shortcutFor`, one table, tested in node. This switch does the thing;
  // it does not decide it, and it no longer re-reads `e.key` — a keymap in two places is one place
  // a key can be forgotten.
  //
  // The review pane is its own surface (SKEIN-151): with it open, shortcutFor consults the REVIEW
  // table and the fleet map is reached for nothing — a review key that fell through would move a
  // selection BEHIND the pane, and ↵ would navigate out of review with a composer open. The chord
  // is one letter of page memory (`g` was pressed); what the second key MEANS stays in the table.
  const pending = revChord;
  revChord = "";
  const action = view.mode === "review"
    ? shortcutFor(e, { inTerm, inField, pane: "review", pending })
    : shortcutFor(e, { inTerm, inField });
  if (!action) return;
  if (action === "rev-chord") { e.preventDefault(); revChord = "g"; return; }
  if (action.startsWith("rev-")) { e.preventDefault(); revKey(action); return; }
  switch (action) {
    // Esc returns focus to the fleet for keyboard nav — it never closes/kills a session (that was a
    // footgun: one keypress destroyed the agent session). Close explicitly via the tab ✕.
    case "deselect": e.preventDefault(); document.activeElement?.blur?.(); break;
    case "next": e.preventDefault(); move(1); break;
    case "previous": e.preventDefault(); move(-1); break;
    case "open": if (sel) { e.preventDefault(); openTerminal(sel); } break;
    case "diff": if (sel) { e.preventDefault(); openDiff(sel); } break;
    case "keys": e.preventDefault(); openSettings("keys"); break;
    // `/` is the one binding a fleet product cannot do without past a dozen boxes, and it was the
    // last thing on the previous audit's still-unfixed list.
    case "filter": e.preventDefault(); openFilter(); break;
    case "next-needs-you": e.preventDefault(); nextNeedsYou(); break;
    case "load": e.preventDefault(); showLoad(); break;
    case "previous-session": e.preventDefault(); cycleSession(-1); break;
    case "next-session": e.preventDefault(); cycleSession(1); break;
  }
});
function cycleSession(d) {
  const ks = orderedSids(); if (!ks.length) return;
  const i = ks.indexOf(activeSid());
  const ns = sessions.get(ks[(i+d+ks.length)%ks.length]);
  showBox(ns.box, "term", ns.kind);
}
// Select the nth open tab (1-based; n beyond the end selects the last, so ⌥9 is always "the last one").
function selectTab(n) {
  const ks = orderedSids(); if (!ks.length) return;
  const s = sessions.get(ks[Math.min(n, ks.length) - 1]);
  showBox(s.box, "term", s.kind);
}
// Tab keys that must work *while you are typing in the agent*, so they can't live in the fleet's
// keymap (which yields every key to a focused terminal). ⌥ chords only: the browser reserves ⌘1-9 and
// ⌃Tab, and the agents' own composers use ⌥←/⌥→ for word movement — brackets are free in all of them.
// Matched on e.code, since ⌥[ on macOS reports e.key as "“".
document.addEventListener("keydown", e => {
  if (!e.altKey || e.metaKey || e.ctrlKey) return;
  const digit = /^Digit([1-9])$/.exec(e.code);
  const step = e.code === "BracketRight" ? 1 : e.code === "BracketLeft" ? -1 : 0;
  if (!digit && !step) return;
  if (!sessions.size) return;
  e.preventDefault(); e.stopPropagation();
  if (digit) { selectTab(+digit[1]); return; }
  if (e.shiftKey) {                                  // ⌥⇧[ / ⌥⇧] — move the tab, don't switch to it
    const id = activeSid(); const ids = orderedSids();
    if (id && ids.includes(id)) moveTab(id, ids.indexOf(id) + step);
    return;
  }
  cycleSession(step);
}, true);

// ---------- attachments: paste · drag-and-drop · attach button ----------
// The agent runs inside the sandbox and can't see your clipboard or your disk, so anything you want
// it to look at — screenshot, PDF, spreadsheet, video, a whole folder of samples — is uploaded into
// the box (the server streams it to /tmp/skein-drop-<batch>/ via sbx exec) and its in-box path is
// pasted into the active terminal for the agent to open. Text paste is untouched: it flows through to
// xterm as before. One drop = one batch dir, so a folder keeps its structure and can be referenced by
// the directory alone.
const MAX_ATTACH = 200;            // per drop — a stray Downloads folder shouldn't upload for an hour
let attachSeq = 0;
// A handover that has not been delivered waits by SID, not on the session object. `reconnectSession`
// deletes that object and builds a new one, so a paste parked on `s.pending` by a terminal that had
// dropped went into the bin with it — silently, five times over, each one reported as "attached →".
const attachWaiting = new Map();   // sid -> { bytes, text }, given to the socket the moment one opens

// Hand `sid`'s waiting attachment to its socket, and only then say it was attached. The report
// follows the delivery rather than the attempt: "attached →" claimed after a `send()` nobody read is
// what let five uploads be reported as delivered while the agent's conversation stayed empty.
// `tail` is whatever the caller has to add about the journey — the wait, when there was one worth
// reporting (SKEIN-269). It rides the delivered sentence rather than a line of its own, because a
// reader who waited eight minutes should be told so BY the sentence that says it finally arrived.
// Empty from `ws.onopen`, which knows nothing about how long the upload took.
function flushAttach(sid, lead = "", tail = "") {
  const waiting = attachWaiting.get(sid);
  if (!waiting) return false;
  const s = sessions.get(sid);
  if (!s || s.ws.readyState !== 1) return false;
  s.ws.send(waiting.bytes);
  attachWaiting.delete(sid);
  toast(lead + "attached → " + waiting.text + tail);
  return true;
}
// Append, never overwrite: two drops onto a still-connecting terminal must both survive to onopen.
function concatBytes(a, b) {
  if (!a) return b;
  const out = new Uint8Array(a.length + b.length); out.set(a); out.set(b, a.length); return out;
}

// Turn a DataTransferItem list into [{rel, file}], expanding dropped directories (webkitGetAsEntry is
// the only way a browser exposes a folder's contents; `rel` keeps each file's path inside it).
async function collectEntries(items) {
  const out = [];
  const entries = [];
  for (const it of items) {
    if (it.kind !== "file") continue;
    // Read both now: a clipboard/drag item is neutered the moment this event's handler returns, so
    // nothing after the first `await` below can come back for it.
    const entry = it.webkitGetAsEntry ? it.webkitGetAsEntry() : null;
    const file = it.getAsFile();
    // Only a *folder* needs the entry API. For a plain file getAsFile() is the read that works
    // everywhere: some browsers hand back an entry for a pasted or copied file whose .file() then
    // fails, and preferring the entry is what made a pasted screenshot attach nothing at all.
    if (file && !(entry && entry.isDirectory)) out.push({ rel: file.name || (entry && entry.name) || "paste", file });
    else if (entry) entries.push(entry);
  }
  const walk = async (entry, prefix) => {
    if (out.length >= MAX_ATTACH) return;
    const rel = prefix ? prefix + "/" + entry.name : entry.name;
    if (entry.isFile) {
      // Timed: .file() only builds the File object, so it is quick when it answers at all — but a
      // callback that never fires would hang the whole attach silently.
      const file = await new Promise(res => {
        const t = setTimeout(() => res(null), 5000);
        const done = f => { clearTimeout(t); res(f); };
        entry.file(done, () => done(null));
      });
      if (file) out.push({ rel, file });
      return;
    }
    const reader = entry.createReader();
    // readEntries returns at most ~100 per call — keep reading until it comes back empty.
    for (;;) {
      const chunk = await new Promise(res => reader.readEntries(res, () => res([])));
      if (!chunk.length) break;
      for (const child of chunk) await walk(child, rel);
      if (out.length >= MAX_ATTACH) break;
    }
  };
  for (const entry of entries) await walk(entry, "");
  return out;
}

// Show `box`'s terminal and return its session id. Called *before* the uploads so the socket is
// connecting while bytes move, and so a drop made on the diff tab, on a fleet row, or with no session
// open still ends up in the agent's prompt instead of being refused.
function ensureTerminal(box) {
  const sid = (view.box === box && view.mode === "term")
    ? activeSid()
    : (openTerminal(box), sidOf(box, "agent"));
  // A socket that has closed is not a terminal anything can be handed to, and the fast path above
  // never asked: a drop onto the box you were ALREADY looking at parked its paste on a dead session,
  // where nothing would ever flush it. `showBox` reconnects a dead tab when you click it; this is
  // the same rule for the path that skips `showBox`.
  const s = sessions.get(sid);
  if (s && (s.dead || s.ws.readyState > 1)) reconnectSession(boxOf(sid), kindOf(sid));
  return sid;
}

// How long an upload may go unanswered before the page stops showing "uploading…" and says so.
// Well under the patience of the person watching it: the owner's five attempts (SKEIN-269) were
// spread over seven minutes because nothing on screen ever changed after the first toast, and the
// first thing a reader does with a surface that has stopped moving is press it again.
const ATTACH_SLOW_MS = 4000;

// One file, up the wire, with a clock on it — and a running word while it is still going.
//
// `fetch` has neither progress nor a deadline, so `await`ing one makes a stalled upload and a fast
// one the same picture: one toast, then nothing, for however long it takes. That is the whole of
// what the reader was given.
//
// It returns TWO numbers, and the pair is what names the cause rather than guessing at it:
//   * `waited` — this call's own wall clock, from just before `fetch` to the parsed reply. It
//     includes any time the request spent queued in the BROWSER, before it was ever sent.
//   * `d.ms.total` — the server's own clock, which starts when its handler does.
// A large `waited` with a small `total` is time spent before the request reached skein; a `total`
// that fills the `waited` is the box holding it. SKEIN-269 could not tell those apart because
// neither side was measured.
async function uploadOne(box, batch, rel, file, say) {
  const began = Date.now();
  // Says it is STILL going, repeatedly, rather than once: a single "this is slow" ages into the
  // same silence it was written to break.
  const watch = setInterval(() => say(
    `still uploading ${rel} — ${Math.round((Date.now() - began) / 1000)}s with no answer from ${box} yet; attaching it again will not make this one faster`
  ), ATTACH_SLOW_MS);
  try {
    const r = await fetch(`/api/boxes/${encodeURIComponent(box)}/upload`, {
      method: "POST",
      headers: { "Content-Type": file.type || "application/octet-stream",
                 "X-Skein-Drop": batch, "X-Skein-Name": encodeURIComponent(rel) },
      body: file });
    const d = await r.json();
    d.waited = Date.now() - began;
    return d;
  } finally { clearInterval(watch); }
}

// Where a slow attach's time actually went, as a clause to hang off the toast. Empty when it was
// quick, because a number nobody was waiting on is noise on a line that has something to say.
//
// The comparison, not either number alone: the server's clock cannot see the browser's queue and
// the browser's cannot see inside the box, so only the gap between them says which end held it.
// `served < 0` means the server sent no clock at all — an older skein — and the honest thing then
// is to report the wait and claim nothing about where it went.
function attachDelayWord(waited, served) {
  if (!(waited >= ATTACH_SLOW_MS)) return "";
  const s = n => (n / 1000).toFixed(1) + "s";
  if (!(served >= 0)) return ` — ${s(waited)}, and this skein does not say where it went`;
  return served * 2 < waited
    ? ` — ${s(waited)}, of which the box took ${s(served)}: the rest went before the request left this browser`
    : ` — ${s(waited)}, and ${s(served)} of it was the box`;
}

// Upload every file of one drop, then paste the resulting in-box path(s) into the box's terminal.
async function attachFiles(box, files, dropped) {
  if (!files.length) { toast("nothing to attach"); return; }
  if (dropped > files.length)                       // never let a cap look like full coverage
    toast(`attaching the first ${files.length} of ${dropped} files`);
  const sid = ensureTerminal(box);
  const batch = Date.now().toString(36) + "-" + (attachSeq++).toString(36);
  const many = files.length > 1;
  toast(many ? `uploading ${files.length} files…` : "uploading " + files[0].rel + "…");
  const paths = [];
  let failed = 0, dir = null, waited = 0, served = 0;
  for (let i = 0; i < files.length; i++) {
    const { rel, file } = files[i];
    if (many && i && i % 10 === 0) toast(`uploading ${i}/${files.length}…`);
    try {
      const d = await uploadOne(box, batch, rel, file, toast);
      // Summed over the drop, both of them: one slow file in twenty is still the drop being slow,
      // and the pair has to stay a pair or the comparison below compares two different uploads.
      waited += d.waited || 0;
      served += (d.ms && d.ms.total) >= 0 ? d.ms.total : -Infinity;
      if (d.ok) { paths.push(d.path); dir = dir || d.path.split("/").slice(0, 3).join("/"); }
      // A refusal names its own delay too. "attach failed: the box stopped taking the file" after
      // eight minutes and the same words after eight milliseconds are different bugs.
      else { failed++; if (failed === 1) toast("attach failed: " + (d.error||"").split("\n")[0] + attachDelayWord(d.waited || 0, (d.ms && d.ms.total) >= 0 ? d.ms.total : -1)); }
    } catch { failed++; }
  }
  if (!paths.length) { toast("attach failed"); return; }
  // Hung off whatever the attach ends up saying, so the sentence that reports the handover is also
  // the one that reports the wait — a reader who waited eight minutes should not have to go looking
  // for why on a different line.
  const slow = attachDelayWord(waited, served);
  // Collapse each attached *folder* to its directory — one path beats three hundred, and the agent can
  // list it itself. Derived from the paths the server returned (it sanitises names), not from the local
  // names, so what we hand over is exactly what exists in the box.
  const dirs = new Set(), loose = [];
  for (const p of paths) {
    const rest = dir && p.startsWith(dir + "/") ? p.slice(dir.length + 1) : "";
    if (rest.includes("/")) dirs.add(dir + "/" + rest.split("/")[0]); else loose.push(p);
  }
  // Claude Code only auto-detects an image path when it arrives as a *paste*, so send it via bracketed
  // paste rather than typing — that's what turns it into an inline [Image #N], and it's equally what
  // Codex reads as a file reference.
  // The separating space goes *after* the closing marker, as a keystroke: inside the paste it becomes
  // part of the pasted text and a provider matching "the paste is a path" stops recognising it.
  const text = [...dirs, ...loose].join(" ");
  const bytes = new TextEncoder().encode("\x1b[200~" + text + "\x1b[201~ ");
  const lead = failed ? `${paths.length} attached, ${failed} failed — ` : "";
  const s = sessions.get(sid);
  // Every sentence below ends with where the file IS. When the handover does not happen, that path is
  // the whole remedy — the reader tells the agent to read it — and its absence is what turned one
  // failed attach into five attempts and a hunt through the box's /tmp.
  if (!s) { copyText(text); toast(lead + "no terminal for " + box + " — copied; it is in the box at " + text + slow); return; }
  // Queued by sid first and delivered second, so the payload is somewhere a reconnect can still find
  // it rather than in a local that goes out of scope with the function.
  const had = attachWaiting.get(sid);
  attachWaiting.set(sid, { bytes: had ? concatBytes(had.bytes, bytes) : bytes,
                           text: had ? had.text + " " + text : text });
  if (flushAttach(sid, lead, slow)) return;   // it says "attached → …" itself, once that is true
  toast(lead + (s.ws.readyState === 0
    ? "queued for " + box + "'s terminal — it is in the box at " + text
    : box + "'s terminal is not connected — it is in the box at " + text) + slow);
}

document.addEventListener("paste", e => {
  if (view.mode !== "term" || !view.box) return;
  const items = e.clipboardData && e.clipboardData.items; if (!items) return;
  const fileItems = [...items].filter(it => it.kind === "file");
  if (!fileItems.length) return;              // plain text → let xterm handle the paste
  e.preventDefault(); e.stopPropagation();    // don't let xterm paste binary as text
  const box = view.box;
  // Same list by another road, taken here and now while the event is still live: if the item reads
  // come back empty (they are the fussier API), a pasted file is still attached rather than dropped.
  const direct = [...(e.clipboardData.files || [])].map(f => ({ rel: f.name || "paste", file: f }));
  collectEntries(fileItems).then(files => {
    const use = files.length ? files : direct;
    attachFiles(box, use, use.length);
  });
}, true);

// Drag-and-drop: the same path as paste, and the only way to hand over a folder. Handled at the
// document, not just the dock — a drop that lands slightly off (the fleet rail, the gutter, the diff
// tab) must still attach rather than silently do nothing, and it must never be left to the browser,
// which treats a file drop as navigation and would replace the cockpit with the file.
(function () {
  let depth = 0, hot = null;
  const isFiles = e => [...(e.dataTransfer?.types || [])].includes("Files");
  // A fleet row targets *that* box (drop straight onto the box you mean); anywhere else targets the
  // box the dock is already showing.
  const target = e => e.target?.closest?.("#fleet [data-name]")?.dataset.name || view.box || null;
  const mark = el => {
    if (hot === el) return;
    hot?.classList.remove("dropping");
    hot = el; el?.classList.add("dropping");
  };
  const clear = () => { depth = 0; mark(null); };
  document.addEventListener("dragenter", e => {
    if (!isFiles(e)) return;
    e.preventDefault(); depth++;
    const row = e.target?.closest?.("#fleet [data-name]");
    mark(row || (target(e) ? document.getElementById("dock") : null));
  });
  document.addEventListener("dragover", e => {
    if (!isFiles(e)) return;
    e.preventDefault(); e.dataTransfer.dropEffect = target(e) ? "copy" : "none";
  });
  document.addEventListener("dragleave", e => { if (isFiles(e) && depth > 0 && --depth <= 0) clear(); });
  document.addEventListener("drop", e => {
    if (!isFiles(e)) return;
    e.preventDefault(); clear();
    const box = target(e);
    if (!box) { toast("open a box first — a drop attaches to the box you're looking at"); return; }
    collectEntries([...(e.dataTransfer.items || [])]).then(files => attachFiles(box, files, files.length));
  });
})();

// Attach button (dockbar) — a file picker for touch/remote use where dragging isn't possible.
// `webkitdirectory` is how a browser lets you pick a whole folder; shift-click chooses that mode.
function pickAttachment(folder) {
  const box = view.box; if (!box) return;
  const inp = document.createElement("input");
  inp.type = "file"; inp.multiple = true;
  if (folder) { inp.webkitdirectory = true; inp.directory = true; }
  inp.onchange = () => {
    const picked = [...inp.files];
    // webkitRelativePath is "<folder>/…" for a folder pick, so the box mirrors the same tree.
    const files = picked.slice(0, MAX_ATTACH).map(f => ({ rel: f.webkitRelativePath || f.name, file: f }));
    attachFiles(box, files, picked.length);
  };
  inp.click();
}

// ---------- resizable fleet sidebar ----------
// Drag the gutter to set --fleet-w (the docked fleet width); persisted across reloads. Keeps the
// active terminal fitted while dragging. (Groundwork for a future right-side terminal pane too.)
(function () {
  const gutter = document.getElementById("gutter");
  const saved = localStorage.getItem("skein.fleetW");
  if (saved) document.documentElement.style.setProperty("--fleet-w", saved);
  gutter.addEventListener("pointerdown", e => {
    e.preventDefault(); gutter.classList.add("drag");
    try { gutter.setPointerCapture(e.pointerId); } catch {}
    let w = saved;
    const move = ev => {
      w = Math.max(220, Math.min(window.innerWidth - 320, ev.clientX)) + "px";
      document.documentElement.style.setProperty("--fleet-w", w);
      activeSession()?.fit.fit();
    };
    const up = () => {
      gutter.classList.remove("drag");
      document.removeEventListener("pointermove", move);
      document.removeEventListener("pointerup", up);
      if (w) { try { localStorage.setItem("skein.fleetW", w); } catch {} }
      activeSession()?.fit.fit(); sendResize(activeSid());
    };
    document.addEventListener("pointermove", move);
    document.addEventListener("pointerup", up);
  });
})();

// ---------- live stream ----------
// Staleness banner: without it, a dead server left the board showing confident, fresh-looking data
// (ages keep ticking client-side) with only the tiny live pip as a tell. Age the last snapshot
// visibly so a stale board is never mistaken for a calm one.
//
// Thirty seconds, not eight. Eight was chosen against a fleet that answered promptly, and a busy one
// does not: a box is free to take every core here — that is the shared sandbox working as intended —
// and the poll behind this banner queues behind it. So the banner spent its time announcing a board
// that was about to update anyway, which is how a warning becomes something you look past. It is
// worth reading only when it means the server is actually gone.
// Thirty seconds is a threshold on the `alive` heartbeat the producer sends every ten (see
// `stream::ALIVE_EVERY`), so it now measures what it always claimed to: time since the SERVER last
// spoke. It used to measure time since the DATA changed, and the stream deliberately says nothing
// when nothing moves — so a calm fleet raised this banner every single time, for up to the ten
// minutes until the re-sync floor. A warning that fires on the ordinary case is one nobody reads.
const STALE_AFTER_S = 30;
let lastTickAt = Date.now();
setInterval(() => {
  const age = Math.round((Date.now() - lastTickAt) / 1000);
  let ban = document.getElementById("staleban");
  if (age >= STALE_AFTER_S) {
    if (!ban) {
      ban = document.createElement("div"); ban.id = "staleban";
      ban.style.cssText = "position:fixed;top:0;left:50%;transform:translateX(-50%);z-index:60;"
        + "background:var(--attn);color:#08090b;font:600 12px/1 var(--sans,sans-serif);"
        + "padding:6px 14px;border-radius:0 0 8px 8px;pointer-events:none";
      document.body.append(ban);
    }
    // Which of the two it is, because they want different things from the reader. An open stream
    // that has gone quiet is a producer that wedged — nothing to wait for. A closed one is the
    // browser already retrying, and waiting is exactly right.
    ban.textContent = es && es.readyState === EventSource.OPEN
      ? `board is ${age}s stale — the server stopped answering`
      : `board is ${age}s stale — reconnecting…`;
  } else ban?.remove();
}, 2000);
// Draw the board from the server, now, rather than waiting for the stream to notice.
//
// The stream is the board's normal supply and this is the exception: after an action the person
// took and is watching for the result of. The fleet rebuild (since removed) and `saveBoxSettings`
// both called `refresh` on their success path and nothing ever declared it, so both threw
// `ReferenceError` inside a `.then` whose `.catch` reports `e.message` — a rebuild that worked said
// "resize failed: refresh is not defined" and a saved setting said "settings: refresh is not
// defined". They wanted this, so this is what they get, once, rather than two spellings of it.
//
// Waiting for the stream instead would have been defensible for the fleet rebuild — `ceiling` and
// `disk_limit_mb` are on the row, so the producer's diff carries them within a tick — but not for
// the settings dialog, whose tracking connection and git identity are not on the row at all. One
// re-read answers both and is what the `behind` handler already does for the same reason.
function refresh() {
  return fetch("/api/boxes").then(r => r.json()).then(render).catch(() => {});
}
let es = null;
function connect() {
  if (es) { try { es.close(); } catch {} }   // don't stack streams on manual reconnect
  es = new EventSource("/api/events");
  // **Transitions, not snapshots.** One producer feeds every open tab now, and after the opening
  // snapshot it sends only what moved — so a quiet fleet sends nothing at all, which is what lets
  // "nothing needs you" be a state rather than an absence.
  //
  // `gone` is its own list rather than an absence, because "not in this update" and "no longer
  // there" are different facts and conflating them would mean re-sending everything to express one.
  const applyTick = tick => {
    lastTickAt = Date.now();
    if (tick.event === "snapshot") {
      const now = Date.now();
      for (const b of (tick.boxes || [])) receivedAt.set(b.name, now);
      render(tick.boxes || []);
      for (const b of (tick.boxes || [])) retryWaiting("wait-box", b.name);
      return;
    }
    const by = new Map(boxes.map(b => [b.name, b]));
    for (const name of (tick.gone || [])) { by.delete(name); receivedAt.delete(name); }
    // Only the rows that ARRIVED get a new moment. A row the update did not mention keeps the one
    // it had, which is what makes its age keep advancing rather than resetting on every tick.
    for (const b of (tick.boxes || [])) { by.set(b.name, b); receivedAt.set(b.name, Date.now()); }
    render([...by.values()]);
    // **The board is the recovery channel for a pane refused because its box was not there**
    // (SKEIN-702). That pane's own socket is gone — that is what the refusal did — and the box
    // arriving or changing state is a transition this stream already carries, so no second mechanism
    // is needed and none is added. A refusal that repeats simply re-arms the wait from the new
    // socket's close, which costs one refused upgrade and is why this does not have to guess which
    // states count as "running".
    for (const b of (tick.boxes || [])) retryWaiting("wait-box", b.name);
  };
  es.addEventListener("snapshot", e => { try { applyTick(JSON.parse(e.data)); } catch {} });
  es.addEventListener("changed", e => { try { applyTick(JSON.parse(e.data)); } catch {} });
  // **A terminal slot came free, so every pane that was refused one takes it back** (SKEIN-702). The
  // server publishes this from the release itself, so there is nothing to poll and no window in
  // which the slot is announced but not yet returned. `lastTickAt` is deliberately untouched: this
  // says nothing about whether the fleet producer is turning, and feeding the staleness banner from
  // it would let a busy terminal hide a dead board — the same reasoning as `reading` below.
  es.addEventListener("pty-freed", () => retryWaiting("wait-pty"));
  // "Still here", and nothing else — no render, because nothing about the fleet changed. This is the
  // whole reason the staleness banner can mean "the server is gone" rather than "nothing happened".
  es.addEventListener("alive", () => { lastTickAt = Date.now(); });
  // **A reading, on the connection the page already holds** (SKEIN-366). Not a fleet tick and not
  // counted as one: `lastTickAt` is deliberately untouched, because a reading says nothing about
  // whether the fleet producer is still turning, and letting it feed the staleness banner would
  // make a busy review pane hide a dead board.
  //
  // This is the whole point of the change. A reading is a model call taking tens of seconds; on its
  // own request it held one of the browser's six connections for that long, and ten at once
  // (`REV_ASKED_PARALLEL`) starved everything else the cockpit does. Here, N readings cost nothing
  // beyond this one stream.
  es.addEventListener("reading", e => { try { revReadArrived(JSON.parse(e.data)); } catch {} });
  // Told rather than silently skipped: this tab stopped reading long enough to fall behind the
  // producer, so what it holds may be wrong in ways a delta cannot repair. Ask for the whole thing.
  es.addEventListener("behind", e => {
    console.warn(`skein: this board fell ${e.data} ticks behind; re-syncing`);
    refresh();
  });
  es.onopen = () => { document.getElementById("live").classList.add("on"); document.getElementById("livetext").textContent = "live"; };
  es.onerror = () => { document.getElementById("live").classList.remove("on"); document.getElementById("livetext").textContent = "reconnecting…"; };
}

// The last health report, for anything that needs to say what is wrong rather than only that
// something is. Module-level because `firstRunHtml` reads it during a board render.
let lastHealth = null;
function loadHealth() {
  if (DEMO) return;
  // **Nothing drawn before the banner can cancel it** (SKEIN-1012, the owner's call: "banner can't
  // be cancelled"). Every step below that is not the banner is a call into drawing code about
  // something else, and they all used to share this handler's one `.catch(() => {})` — so a throw
  // in any of them skipped the banner, silently, on a fleet whose report already said `ok: false`.
  // That is SKEIN-1003's failure reached by a second route: nothing on screen and nothing sending
  // anybody to the diagnostics pane. It was live, not argued: `loginban.mjs` defined no
  // `renderModelChoices` for as long as it existed, and never once reached the banner block.
  // So each step gets its own guard, its throw goes to the console under its own name, and the
  // banner is drawn from the report whatever happened to the others.
  const step = (name, draw, ...args) => {
    try { draw(...args); } catch (e) { console.error(`skein: ${name} threw during the health poll; the health banner is drawn regardless`, e); }
  };
  fetch("/api/health").then(r => r.json()).then(health => {
    const hadNoReport = !lastHealth;
    lastHealth = health;
    // On the poll, not only when the settings dialog opens: this is what makes `lastHealth` a
    // report the dialog can trust synchronously, and it costs nothing on a host.
    // The first-run checklist ticks off this report, so it has to redraw when the report lands —
    // otherwise the steps sit unticked until the next box event, which on an empty fleet never comes.
    if (hadNoReport || !boxes.length) step("render", render, boxes || []);
    step("renderDiagnostics", renderDiagnostics);
    // Before the `health.ok` early return: a fleet can be healthy by every environment check and
    // still hold a dead credential, and this banner rides the same poll rather than earning one.
    step("renderLoginBanner", renderLoginBanner, health.expired_logins || []);
    // A newer agent CLI used to be a bar here too. It is Settings -> Update now, deliberately: a
    // dead credential has STOPPED work and an old CLI has not, so only one of them is worth taking
    // the top of a board somebody is trying to read. The check is unchanged; where it is drawn is
    // not.
    // **The review-model choices, from the CLI rather than from a list in here** (SKEIN-451). A
    // `datalist` and not a `select` on purpose: it is a dropdown of what `claude --help` says it
    // takes AND still a text box, so an exact build name stays typeable when the list is short,
    // wrong, or — because skein could not ask — empty. One control, and the free-text setting the
    // owner already had is not taken away to give him the picker he asked for.
    step("renderModelChoices", renderModelChoices, health.models || []);
    let banner = document.getElementById("healthban");
    if (health.ok) { banner?.remove(); return; }
    if (!banner) {
      // A row above the app, never a pill over it: floating at the top covered the dock's tab bar,
      // which blocked exactly the work the warning was interrupting. As body's first flex child it
      // pushes the app down instead of painting over it, and disappears without leaving a hole.
      banner = document.createElement("button"); banner.id = "healthban";
      banner.onclick = () => openSettings("diag"); document.body.prepend(banner);
    }
    // **Every check the report carries, and the list is not a selection any more** (SKEIN-1003).
    // It used to hold twelve of the fourteen, hand-written, and the two it was short of were the
    // two nobody had decided about: "gh" — which is "curl is installed" and "GitHub can be reached
    // at all" (SKEIN-548, SKEIN-926) — and "ai". Whether a check turns the banner red is decided
    // in ONE place, `OnBanner` in `src/health/report.rs`, and the page is not that place: a key
    // missing from here cannot suppress a banner, it can only produce `ok: false` with nothing in
    // the row to read, which is the one failure this list has ever had. So it carries all of them,
    // and `every_check_the_report_carries_is_named_on_the_page` holds it level with the report's
    // own fields — a new check is on this list or that test fails by name.
    //
    // Carrying a check the verdict does not count is deliberate and costs nothing: a check the
    // Rust side marks `NotCounted` gets named here when the banner is already up for some other
    // reason, and stays unable to raise one by itself.
    //
    // **And it cannot take the headline** (SKEIN-1013, the owner's call: "a check that raised
    // it"). The row's one sentence — the only part a person can read on a phone and copy — used
    // to be `failed[0]` in this list's order, and `ai` sits ahead of seven counted checks, so a
    // fleet with a full disk and an uncounted `ai` fault would have headlined `ai` and put the
    // disk in the tooltip. The headline is now the first failure the report says is `counted`;
    // the page does not decide which those are, because `OnBanner` does and the report carries
    // its answer. Uncounted failures are still in the row's count of the rest, and the tooltip.
    const CHECKED = ["registry","sbx","git","gh","probes","mailbox","ai","memory","disk","gitgate","token_expiry","proxy_injection","warden","cover"];
    const failed = CHECKED.filter(key => health[key]?.level === "unsatisfied");
    // Reported, never counted as a fault: these are the ones skein could not answer.
    const unsure = CHECKED.filter(key => health[key]?.level === "unknown");
    const stale = health.stale_boxes?.length || 0, dark = health.dark_boxes?.length || 0;
    // The banner used to read `environment: registry, sbx` — two nouns, no verb, no consequence,
    // with every actual detail hidden in a `title` tooltip that cannot be copied, read on a phone,
    // or survive a scroll. The first counted failure's own sentence says what is wrong and usually
    // what to do; the rest stay in the tooltip until there is a diagnostics pane to put them in.
    const counted = new Set(health.counted || []);
    const lead = failed.find(key => counted.has(key));
    const first = lead ? (health[lead]?.detail || "").split(".")[0] : "";
    // Everything failed that is not the headline — so when no counted check has failed and the
    // banner is up for stale sessions, an uncounted failure is still counted here, not dropped.
    const rest = failed.length - (lead ? 1 : 0);
    const extra = rest ? ` (+${rest} more)` : "";
    banner.textContent = lead
      ? `${lead}: ${first}${extra}`
      : `environment: probe updates${dark ? ` · ${dark} dark` : ""}${stale ? ` · ${stale} stale` : ""}${extra}`;
    banner.title = [
      ...failed.map(key => `${key}: ${health[key]?.detail || "unhealthy"}${health[key]?.fix ? ` → ${health[key].fix}` : ""}`),
      ...unsure.map(key => `${key}: could not be checked — ${health[key]?.detail || "no reason given"}`),
      ...(dark ? [`no signals: ${health.dark_boxes.join(", ")}`] : []),
      ...(stale ? [`stale sessions: ${health.stale_boxes.join(", ")}`] : [])
    ].join("\n");
    // Said rather than swallowed (SKEIN-1012): what reaches here now is the fetch, the parse, or
    // the banner block itself — and a server that is down already has the staleness banner saying
    // so, so a warning per poll is the cost of never again losing a throw without a trace.
  }).catch(e => console.warn("skein: the health poll failed", e));
}

// ---------- the fleet's login, repaired from the page (SKEIN-212) ----------
// The owner's ask, verbatim: "there should be a popup that this happened and ask me to login.
// There should be an easy way to do it. I just click something, a session opens I login there and
// then it closes and everything else starts working."
//
// The "popup" is a ROW (the #healthban/#trainban rule: a banner pushes the app down, never covers
// it), and the "something to click" opens a one-shot terminal onto GET /api/login/:runtime/terminal
// — a WebSocket the server bridges to a PTY running the interactive login. The terminal lives in
// the modal stack (the #mbx pattern), NOT the dock: dock sessions belong to boxes, persist into
// localStorage, and reconnect when their socket drops — and a login socket dropping means the flow
// is OVER, which is the exact opposite contract.

// A row per expired runtime, because each needs its own login and each button names its own flow.
function renderLoginBanner(expired) {
  let ban = document.getElementById("loginban");
  if (!expired.length) { ban?.remove(); return; }
  if (!ban) { ban = document.createElement("div"); ban.id = "loginban"; document.body.prepend(ban); }
  // Two witnesses, two sentences. The credential file states a date, knowable with nothing running.
  // A refusal is something that HAPPENED, at a moment, and it is the only one of the two that can
  // be wrong about the present — so it says when it was found out and what was said, rather than
  // claiming a death date nothing here can know. Rendered identically, a person cannot tell a
  // credential that is dead from one that was dead a moment ago.
  ban.innerHTML = expired.map(l => `<span>${l.witness === "refusal"
      ? `the fleet's ${esc(l.runtime)} login was refused at ${esc(l.expired_at)}${l.said ? `: ${esc(l.said)}` : ""} — summaries, critiques and workflows are declining model calls`
      : `the fleet's ${esc(l.runtime)} login expired ${esc(l.expired_at)} — summaries, critiques and workflows are declining model calls`}</span>`
    + `<button type="button" class="loginfix" onclick="openLoginTerminal(${esc(JSON.stringify(l.runtime))})">log in${expired.length > 1 ? ` to ${esc(l.runtime)}` : ""}</button>`).join("");
}

// The options behind the review-model box. Rewritten only when the list actually changes: this
// rides a poll, and replacing the DOM under an open dropdown would close it while somebody is
// choosing.
let modelChoicesShown = "";
function renderModelChoices(models) {
  const list = document.getElementById("set-review-models");
  if (!list) return;
  const key = models.join("\u0000");
  if (key === modelChoicesShown) return;
  modelChoicesShown = key;
  list.innerHTML = models.map(m => `<option value="${esc(m)}"></option>`).join("");
}

// Settings -> Update. skein's own build, and the agent CLIs that used to live in the bar.
//
// **The bar is gone on purpose** (the owner's call): an update is a thing you go and do, not a
// thing that should sit across the top of a board you are trying to read. What the bar was right
// about is kept — the checking is still skein's, on its own clock, and this only draws the answer.
let updateState = null;      // the last `/api/update` reading
let updateLogAt = 0;         // bytes of the build log already shown
let updateTail = null;       // the poll while a build is running
let updateSaw = "";          // what we have shown, so a reconnect does not repeat it
let updateAgain = null;      // the re-read while the remote is still in flight
let updateRun = "";          // which run the log belongs to — Cancel hands it back (SKEIN-1037)
let updateQuiet = 0;         // seconds the log has gone unwritten, once that counts as no progress
let updateEnded = "";        // "cancelled" after a Cancel, so the pane can say what to do next
let restartSaid = null;      // what "Restart on new build" last said: { tone, text, cmd } (SKEIN-1029)

// Whether the pane is on screen. Everything below stops when it is not: this is a settings pane
// somebody opened, not a board, and a timer still running against a closed dialog is a fetch a
// minute for a page nobody is looking at.
function updatePaneOpen() {
  return !!document.querySelector('.set-pane[data-pane="update"].on');
}

async function loadUpdate(again) {
  const box = document.getElementById("set-update");
  if (!box) return;
  // The boxes still on an old agent CLI: asked once per opening, not on the re-read below, because
  // it asks every running box when its agent started.
  if (!again) loadAgentsBehind();
  if (!updateState) box.innerHTML = `<div class="set-note">reading…</div>`;
  try {
    updateState = await fetch("/api/update").then(r => r.json());
  } catch (e) {
    box.innerHTML = `<div class="set-note bad">could not read the update state: ${esc(String(e))}</div>`;
    return;
  }
  renderUpdate();
  // **The remote arrives late, and that is the design rather than a delay to wait out.** The check
  // runs on skein's own clock and behind the caller, so the FIRST read of a cold server always
  // answers "not asked yet" — and a pane that rendered that once would say `github unknown` for as
  // long as it stayed open. Found by opening it in a browser: every unit test passed, the API had
  // the sha, and the page showed nothing. So keep asking until the question is settled either way,
  // and only while somebody is looking.
  clearTimeout(updateAgain);
  const u = updateState.skein || {};
  const settled = !!u.remote || (!!u.why && u.why !== "not asked yet");
  if (!settled && updatePaneOpen()) updateAgain = setTimeout(() => loadUpdate(true), 1200);
  // A build already going when the pane opens — a reload during one, or a second tab. Pick it up
  // rather than showing a button that would refuse.
  if (updateState.running && !updateTail) tailUpdate();
}

// Which of the three revisions disagree, in a sentence rather than three fields to compare by eye.
//
// The order matters: "there is a newer skein" is what somebody opened this for, and "your binary is
// not your checkout" is a different and rarer thing that must not be phrased as the same one.
function updateVerdict(u) {
  if (!u.source) return ["", "no checkout to build from — this skein was not installed by bootstrap"];
  if (!u.remote) return ["", `could not ask GitHub: ${u.why || "no reason recorded"}`];
  if (u.behind) return ["behind", "a newer skein is on GitHub"];
  if (u.restartable) return ["warn", RESTART_WORDS.verdict];
  if (u.unbuilt) return ["warn", "the running binary is not the checkout beside it — a build that did not finish, or one from an edited tree"];
  return ["ok", "this is the newest skein"];
}

function renderUpdate() {
  const box = document.getElementById("set-update");
  if (!box || !updateState) return;
  const u = updateState.skein || {};
  const [state, said] = updateVerdict(u);
  // **Abbreviated by cutting the sha, never the string.** `git describe --always --dirty` appends a
  // marker, and a blind twelve characters turned `89cf36b-dirty` into `89cf36b-dirt` — which reads
  // as a sha fragment and quietly drops the one fact build.rs says matters as much as the revision.
  // The marker is not re-printed here because the row already says it in words.
  const rev = r => {
    if (!r) return `<span class="dim">unknown</span>`;
    const bare = r.endsWith("-dirty") ? r.slice(0, -"-dirty".length) : r;
    return `<code>${esc(bare.slice(0, 12))}</code>`;
  };
  const pill = document.getElementById("set-updn");
  // The nav pill counts what a press would change, so the number is the same claim the button is.
  const waiting = (u.behind ? 1 : 0) + (updateState.runtimes || []).length;
  if (pill) pill.textContent = waiting ? String(waiting) : "";

  box.innerHTML = `
    <div class="set-note ${esc(state)}">${esc(said)}</div>
    <table class="set-revs">
      <tr><th>running</th><td>${rev(u.running)}</td><td class="dim">the binary answering this page${u.dirty ? " — built from an edited tree" : ""}</td></tr>
      <tr><th>checkout</th><td>${rev(u.source)}</td><td class="dim">what a rebuild would compile</td></tr>
      <tr><th>github</th><td>${rev(u.remote)}</td><td class="dim">${esc(u.url || "")}${u.tracking ? ` @ ${esc(u.tracking)}` : ""}</td></tr>
      ${u.installed ? `<tr><th>installed</th><td>${rev(u.installed)}</td><td class="dim">${esc(RESTART_WORDS.installedRow)}</td></tr>` : ""}
    </table>
    ${renderRestart(u)}
    <div class="set-row">
      <button type="button" class="kbtn" id="upd-go" ${updateState.running ? "disabled" : ""}
        onclick="pressUpdateSkein()">${updateState.running ? "updating…" : "Update skein"}</button>
      <span class="dim">fetches, rebuilds in the sandbox, and restarts the cockpit — minutes, and this page will reconnect</span>
    </div>
    ${updateEnded === "cancelled" && !updateState.running ? `<div class="set-note warn" id="upd-ended">${esc(UPDATE_WORDS.cancelledNote)}</div>` : ""}
    <div id="upd-stall" class="set-note warn" hidden></div>
    <pre id="upd-log" class="set-log" ${updateState.running || updateSaw ? "" : "hidden"}>${esc(updateSaw)}</pre>
    ${renderRuntimeUpdates(updateState.runtimes || [])}
    <div id="upd-agents">${agentsBehindHtml()}</div>`;
  renderUpdateStall();
  // Redrawn from the saved text, so put the reader back at the end — the stall notice points at the
  // log's last lines, and a redraw that left the scroll at the top would be pointing at the first.
  const log = document.getElementById("upd-log");
  if (log) log.scrollTop = log.scrollHeight;
}

// Every sentence "Restart on new build" puts in front of a person, in one place, because the
// wording is the owner's to sign off (SKEIN-1029). `h` is the server's `PortHolder`
// (`src/update.rs`): the port, the doorway's pid, the answering server's pid and build.
const RESTART_WORDS = {
  verdict: "A newer build is installed than the one answering this page. Restart to run it.",
  installedRow: "the build on disk, which a restart would run",
  button: "Restart on new build",
  offer: "Restarts the cockpit behind the same port. This page reconnects by itself in a few seconds.",
  restarting: "restarting…",
  waiting: want => `Restarting on ${want}. Waiting for it to answer…`,
  nowRunning: build => `Now running ${build}.`,
  nothingNewer: "Nothing to restart: the build answering this page is already the installed one.",
  noDoorway: h => h.doorway
    ? `Not restarted: the doorway (pid ${h.doorway}) holds port ${h.port}, but this server (pid ${h.server}) is not the one behind it, so a reload would replace a different process. Stopping this one takes this page down until something starts the installed build in its place:`
    : `Not restarted: no skein doorway holds port ${h.port}, so this server (pid ${h.server}) was started some other way and a reload would not replace it. Stopping it takes this page down until something starts the installed build in its place:`,
  didNotTake: (h, want, secs) => h
    ? `The restart did not take: after ${secs} seconds this page is still served by ${h.build} (pid ${h.server}), not ${want}. ${h.doorway ? `The doorway (pid ${h.doorway}) holds port ${h.port}.` : `No skein doorway holds port ${h.port}.`} What the doorway says is here:`
    : `The restart did not take: nothing has answered this page for ${secs} seconds. The doorway should be starting ${want}; what it says is here:`,
  failed: why => `Could not restart: ${why}`,
  copy: "Copy",
  copied: "copied",
};

// How long a restart is given to answer as the new build before the pane says it did not take. A
// debug build starts in a second or two; the doorway's own restart delay is two.
const RESTART_WAIT_MS = 30000;

// The restart control and whatever it last said. Drawn only while there is something to say: a
// build to restart onto, or the outcome of the last press.
function renderRestart(u) {
  const said = restartSaid ? `<div class="set-note ${esc(restartSaid.tone || "")}" id="upd-restart-said">${esc(restartSaid.text)}${
    restartSaid.cmd ? `<pre class="set-log" id="upd-restart-cmd">${esc(restartSaid.cmd)}</pre><button type="button" class="kbtn" onclick="copyRestartCmd(this)">${esc(RESTART_WORDS.copy)}</button>` : ""}</div>` : "";
  if (!u.restartable) return said;
  return `<div class="set-row">
      <button type="button" class="kbtn primary" id="upd-restart" ${restartSaid && restartSaid.busy ? "disabled" : ""}
        onclick="pressRestartBuild(this)">${esc(restartSaid && restartSaid.busy ? RESTART_WORDS.restarting : RESTART_WORDS.button)}</button>
      <span class="dim">${esc(RESTART_WORDS.offer)}</span>
    </div>${said}`;
}

function copyRestartCmd(btn) {
  if (restartSaid && restartSaid.cmd && copyText(restartSaid.cmd)) btn.textContent = RESTART_WORDS.copied;
}

// The press: ask for the reload, then watch `/api/health`'s `build` until it is the installed one.
// A poll that fails is the swap itself — the old server has gone and the new one is not up yet —
// so it is asked again rather than reported.
async function pressRestartBuild(btn) {
  const want = (updateState && updateState.skein || {}).installed || "";
  btn.disabled = true;
  btn.textContent = RESTART_WORDS.restarting;
  let r = null;
  try {
    r = await fetch("/api/update/restart", { method: "POST" }).then(x => x.json());
  } catch (e) {
    r = { ok: false, why: "failed", error: String(e) };
  }
  if (!r.ok) {
    restartSaid = r.why === "no-doorway"
      ? { tone: "warn", text: RESTART_WORDS.noDoorway(r.holder), cmd: `kill ${r.holder.server}` }
      : r.why === "nothing-newer"
        ? { tone: "", text: RESTART_WORDS.nothingNewer }
        : { tone: "warn", text: RESTART_WORDS.failed(r.error || r.why) };
    return loadUpdate();
  }
  // `busy`, so a redraw while this waits — a poll, the pane reopened — keeps the button pressed.
  restartSaid = { tone: "", text: RESTART_WORDS.waiting(want), busy: true };
  renderUpdate();
  const began = Date.now();
  while (Date.now() - began < RESTART_WAIT_MS) {
    await new Promise(res => setTimeout(res, 1000));
    let build = "";
    try {
      build = (await fetch("/api/health", { signal: AbortSignal.timeout(5000) }).then(x => x.json())).build || "";
    } catch {}
    if (build && build === want) {
      restartSaid = { tone: "ok", text: RESTART_WORDS.nowRunning(build) };
      return loadUpdate();
    }
  }
  let holder = null;
  try { holder = await fetch("/api/update/restart", { signal: AbortSignal.timeout(5000) }).then(x => x.json()); } catch {}
  restartSaid = {
    tone: "warn",
    text: RESTART_WORDS.didNotTake(holder, want, Math.round(RESTART_WAIT_MS / 1000)),
    cmd: (holder || r.holder).look,
  };
  return loadUpdate();
}

// Every sentence the "no progress" state and its Cancel put in front of a person, in one place,
// because the wording is the owner's to sign off (SKEIN-1037) and a sentence scattered through a
// template is one nobody can review as a whole. `mins` is whole minutes, never under five.
const UPDATE_WORDS = {
  stalled: mins => `No progress: nothing has been written to the update log for ${mins} minutes.`,
  stalledWhy: "It may be waiting on a network that has stopped answering, or on one long build step. The log's last lines are below.",
  cancelCost: "Cancel stops this update where it is. If it had not yet installed the new build, the skein you are running is unchanged. Either way you can press Update skein again.",
  cancelButton: "Cancel update",
  cancelling: "cancelling…",
  cancelledToast: "update cancelled — the log shows where it stopped",
  cancelledNote: "You cancelled the update. The log below shows where it stopped; press Update skein to try again.",
  cancelFailed: why => `could not cancel the update: ${why}`,
};

// The "no progress" notice, drawn into its own element so a poll can update it without redrawing
// the pane — which would throw away the log's scroll position and a Cancel press in flight.
//
// Driven by the server's `stalled`, never by a timer here: the server reads the log's own mtime, so
// a page opened an hour into a hang shows it at once, and a page left open does not decide on its
// own clock that a run it cannot see has stopped.
function renderUpdateStall() {
  const box = document.getElementById("upd-stall");
  if (!box) return;
  if (!updateQuiet) { box.hidden = true; box.innerHTML = ""; return; }
  const mins = Math.max(5, Math.floor(updateQuiet / 60));
  const line = UPDATE_WORDS.stalled(mins);
  const said = box.querySelector(".upd-stall-line");
  // Only the minutes move while it is showing; the button is left alone so a press in flight keeps
  // its "cancelling…".
  if (said) { said.textContent = line; box.hidden = false; return; }
  // Styled inline rather than by a rule in the stylesheet above: a rule there moves every line
  // below it, and `docs/line-cites.toml` holds dozens of citations into this file.
  box.innerHTML = `<span class="upd-stall-line">${esc(line)}</span>
    <span class="dim" style="display:block;margin-top:4px">${esc(UPDATE_WORDS.stalledWhy)}</span>
    <span class="dim" style="display:block;margin-top:4px">${esc(UPDATE_WORDS.cancelCost)}</span>
    <button type="button" class="kbtn" id="upd-cancel" style="margin-top:8px" onclick="pressCancelUpdate(this)">${esc(UPDATE_WORDS.cancelButton)}</button>`;
  box.hidden = false;
}

// The press. It names the run the pane is showing, so it can only ever stop that one: a run that
// ended meanwhile, or another started from a second tab, is refused by the server and stopped by
// nobody. What happens next is read off the log like every other ending, so this only reports a
// press that did not land.
async function pressCancelUpdate(btn) {
  btn.disabled = true;
  btn.textContent = UPDATE_WORDS.cancelling;
  let r = null;
  try {
    r = await fetch("/api/update/cancel", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ run: updateRun }),
    }).then(x => x.json());
  } catch (e) {
    r = { ok: false, error: String(e) };
  }
  if (!r.ok) {
    toast(UPDATE_WORDS.cancelFailed(r.error));
    btn.disabled = false;
    btn.textContent = UPDATE_WORDS.cancelButton;
    return;
  }
  if (r.ended === "cancelled") updateEnded = "cancelled";
  // Nothing else to do here: the tail that drew this notice is still polling, and it is what ends
  // the state, on its next beat, by reading the marker Cancel just wrote. Starting a second tail
  // would read the same bytes twice into the log.
}

// The agent CLIs, in the pane instead of the bar. Both versions, because "an update is available"
// is not checkable and `1.2.3 -> 1.2.9` is.
function renderRuntimeUpdates(updates) {
  if (!updates.length) return `<div class="set-note">the fleet's agent CLIs are current</div>`;
  return `<div class="set-note behind">${updates.map(u =>
      `${esc(u.runtime)} ${esc(u.have)} &rarr; ${esc(u.latest)}`).join(" · ")}${
      updates.length > 1 ? " are" : " is"} available in the fleet's sandbox — every box shares them`
    + `<button type="button" class="kbtn" onclick="pressUpdateAgents(this)">update agents</button></div>`;
}

// Every sentence the list of boxes still on an old agent CLI, and its per-box restart, put in front
// of a person — the owner's approved wording (SKEIN-1069), kept in one place to be read as a whole.
// `rt` is the runtime, `old` what a box started on, `now` what the last install put in place.
const AGENTS_WORDS = {
  several: (n, rt, old, now) => `${n} boxes are still running ${rt} ${old} and move to ${now} at their next session:`,
  one: (box, rt, old, now) => `${box} is still running ${rt} ${old} and moves to ${now} at its next session.`,
  none: (rt, now) => `every running box is on ${rt} ${now}. Stopped boxes start on it.`,
  unknown: (box, rt) => `skein could not read when ${box}'s agent started, so it cannot say which ${rt} it is running.`,
  waiting: "waiting for you",
  working: "working",
  workingMoves: "moves when this turn's session ends",
  button: now => `Restart on ${now}`,
  offer: (box, rt, now) => `Stops ${rt} in ${box} and reopens its conversation on ${now}. ${box} is waiting for you, so no turn is cut off, and nothing is sent to it. What its agent's terminal was running stops with it; anything started to outlive the terminal (nohup, setsid) keeps running.`,
  restarting: "restarting…",
  done: (box, rt, now) => `${box} is on ${rt} ${now}`,
  refused: box => `Not restarted: ${box} started working after this list was drawn. It moves at its next session, and the button comes back when it is waiting again.`,
  failed: (box, why) => `Could not restart ${box}: ${why}`,
  notRunning: now => `its agent is not running, so there is nothing to restart. Its next session starts on ${now}.`,
  noReading: "skein has no recent reading of its agent, so it cannot tell that it is waiting.",
};

// The states a box can only reach from `waiting` by starting a turn — so a press refused in one of
// them is truthfully "started working after this list was drawn".
const TURN_STARTED = new Set(["working", "compacting", "needs-input", "error", "done"]);
// A refusal in any other state gets a plain reason in the Failed wording (the owner, 2026-09-24):
// the agent has ended, or skein has no recent reading to tell waiting from anything else.
const AGENT_NOT_RUNNING = new Set(["ended"]);
const AGENT_NO_READING = new Set(["stale", "live", "idle", "unknown", ""]);

// What a refused press says, by the state the server read at the press. **No state word reaches the
// page** (the owner, 2026-09-24): a state in none of the sets above — one a later probe invents — is
// read as what it is to this pane, a state that is not a reading of the agent waiting.
function refusalText(box, state, now) {
  if (TURN_STARTED.has(state)) return AGENTS_WORDS.refused(box);
  if (AGENT_NOT_RUNNING.has(state)) return AGENTS_WORDS.failed(box, AGENTS_WORDS.notRunning(now));
  return AGENTS_WORDS.failed(box, AGENTS_WORDS.noReading);
}

// A row reads left to right — box, state, what happens — rather than spread across the pane the way
// a setting and its control are. Inline for the reason `renderUpdateStall` gives: a rule in the
// stylesheet moves every line below it, and `docs/line-cites.toml` cites into that file.
const AGENT_ROW = "justify-content:flex-start;gap:14px;padding:6px 0";

let agentsBehind = null;        // `/api/update-agents/boxes`'s last answer, per runtime
const agentsBusy = new Set();   // boxes whose restart press is in flight
const agentsSaid = new Map();   // box -> { text, refused } its last press said, when it was not a success
let agentsTick = null;          // the redraw that follows the board's live turn states

// A box's turn state as the BOARD has it now, falling back to what the list was drawn with. The
// board's states arrive live, so the button follows them: gone the moment a box starts working, and
// back when it is waiting again, without asking every box again.
function agentLiveState(b) {
  return (boxes.find(x => x.name === b.name) || b).state || "";
}

async function loadAgentsBehind() {
  let r = null;
  try { r = await fetch("/api/update-agents/boxes").then(x => x.json()); } catch { return; }
  if (!r || !Array.isArray(r.runtimes)) return;
  agentsBehind = r.runtimes;
  drawAgentsBehind();
  clearInterval(agentsTick);
  agentsTick = setInterval(() => {
    if (!updatePaneOpen()) { clearInterval(agentsTick); agentsTick = null; return; }
    drawAgentsBehind();
  }, 2000);
}

function drawAgentsBehind() {
  const el = document.getElementById("upd-agents");
  if (!el) return;
  const html = agentsBehindHtml();
  if (el.dataset.drawn === html) return;   // a press in flight keeps its button
  el.dataset.drawn = html;
  el.innerHTML = html;
}

// The restart control for one box: only while the board reads it `waiting`. A refusal that said it
// "comes back when it is waiting again" is dropped at the moment that becomes true.
function agentRestartHtml(b, rt, now) {
  const busy = agentsBusy.has(b.name);
  return `<button type="button" class="kbtn" data-box="${esc(b.name)}" data-rt="${esc(rt)}" data-now="${esc(now)}"
      ${busy ? "disabled" : ""} onclick="pressRestartOntoUpdate(this)">${esc(busy ? AGENTS_WORDS.restarting : AGENTS_WORDS.button(now))}</button>
    <span class="dim">${esc(AGENTS_WORDS.offer(b.name, rt, now))}</span>`;
}

function agentSaidHtml(b) {
  const said = agentsSaid.get(b.name);
  return said ? `<div class="set-note warn upd-agent-said" data-box="${esc(b.name)}">${esc(said.text)}</div>` : "";
}

function agentsBehindHtml() {
  if (!agentsBehind) return "";
  let html = "";
  for (const r of agentsBehind) {
    const rt = r.runtime, now = r.latest;
    const groups = new Map();
    for (const b of r.behind || []) {
      if (agentLiveState(b) === "waiting" && agentsSaid.get(b.name)?.refused) agentsSaid.delete(b.name);
      if (!groups.has(b.have)) groups.set(b.have, []);
      groups.get(b.have).push(b);
    }
    for (const [old, list] of groups) {
      if (list.length === 1) {
        const b = list[0];
        html += `<div class="set-note behind upd-agents-line">${esc(AGENTS_WORDS.one(b.name, rt, old, now))}</div>`;
        if (agentLiveState(b) === "waiting") html += `<div class="set-row upd-agent" style="${AGENT_ROW}" data-box="${esc(b.name)}">${agentRestartHtml(b, rt, now)}</div>`;
        html += agentSaidHtml(b);
        continue;
      }
      html += `<div class="set-note behind upd-agents-line">${esc(AGENTS_WORDS.several(list.length, rt, old, now))}</div>`;
      for (const b of list) {
        const state = agentLiveState(b);
        const tail = state === "waiting"
          ? `<span class="dim">${esc(AGENTS_WORDS.waiting)}</span>${agentRestartHtml(b, rt, now)}`
          : state === "working"
            ? `<span class="dim">${esc(AGENTS_WORDS.working)}</span><span class="dim">${esc(AGENTS_WORDS.workingMoves)}</span>`
            : "";
        html += `<div class="set-row upd-agent" style="${AGENT_ROW}" data-box="${esc(b.name)}"><code>${esc(b.name)}</code>${tail}</div>${agentSaidHtml(b)}`;
      }
    }
    for (const name of r.unknown || []) {
      html += `<div class="set-note upd-agents-line">${esc(AGENTS_WORDS.unknown(name, rt))}</div>`;
    }
    if (!(r.behind || []).length && !(r.unknown || []).length) {
      html += `<div class="set-note ok upd-agents-line">${esc(AGENTS_WORDS.none(rt, now))}</div>`;
    }
  }
  return html;
}

// The press: ONE box, the one on the button. The server reads its turn state again and refuses it
// if it is no longer waiting; nothing is sent to the agent either way.
async function pressRestartOntoUpdate(btn) {
  const { box, rt, now } = btn.dataset;
  agentsBusy.add(box);
  agentsSaid.delete(box);
  drawAgentsBehind();
  let r = null;
  try {
    r = await fetch(`/api/update-agents/boxes/${encodeURIComponent(box)}/restart`, { method: "POST" }).then(x => x.json());
  } catch (e) {
    r = { ok: false, why: "failed", error: String(e) };
  }
  agentsBusy.delete(box);
  if (r.ok) toast(AGENTS_WORDS.done(box, rt, now));
  else {
    const refused = r.why === "not-waiting" && TURN_STARTED.has(r.state);
    // What the server just read is fresher than the list: without this the next draw finds the box
    // still `waiting` in it, takes that for "waiting again", and drops the refusal it just said.
    if (r.why === "not-waiting") for (const rt of agentsBehind || []) for (const b of rt.behind || []) if (b.name === box) b.state = r.state;
    agentsSaid.set(box, {
      refused,
      text: r.why === "not-waiting"
        ? refusalText(box, r.state || "", now)
        : AGENTS_WORDS.failed(box, r.error || r.why),
    });
  }
  drawAgentsBehind();
  loadAgentsBehind();
}

// The press. It ends by replacing the process serving this page, so nothing here waits for a
// result: it starts the run and switches to tailing the log.
async function pressUpdateSkein() {
  const btn = document.getElementById("upd-go");
  if (btn) { btn.disabled = true; btn.textContent = "starting…"; }
  updateSaw = ""; updateLogAt = 0; updateEnded = ""; updateQuiet = 0;
  const ended = document.getElementById("upd-ended");
  if (ended) ended.remove();
  try {
    const r = await fetch("/api/update/start", { method: "POST" }).then(r => r.json());
    if (!r.ok) { toast(`could not start: ${r.error}`); if (btn) { btn.disabled = false; btn.textContent = "Update skein"; } return; }
  } catch (e) {
    toast(`could not start: ${e}`);
    if (btn) { btn.disabled = false; btn.textContent = "Update skein"; }
    return;
  }
  if (btn) btn.textContent = "updating…";
  const log = document.getElementById("upd-log");
  if (log) log.hidden = false;
  tailUpdate();
}

// Read the log by offset until the marker says it ended.
//
// **A failed poll is not a failed build.** The last thing a successful update does is replace this
// server, so the request in flight at that moment dies — which is why the offset lives here and the
// log lives in a file: the next poll that succeeds carries on from where this one stopped, whether
// it is answered by the same binary or the new one.
function tailUpdate() {
  clearTimeout(updateTail);
  const step = async () => {
    let r = null;
    try {
      r = await fetch(`/api/update/log?from=${updateLogAt}`).then(x => x.json());
    } catch (e) {
      // The swap, most likely. Keep asking.
      updateTail = setTimeout(step, 1500);
      return;
    }
    if (r.text) {
      updateSaw += r.text;
      updateLogAt = r.at;
      const log = document.getElementById("upd-log");
      if (log) { log.hidden = false; log.textContent = updateSaw; log.scrollTop = log.scrollHeight; }
    } else {
      updateLogAt = r.at;
    }
    if (r.run) updateRun = r.run;
    updateQuiet = r.stalled && !r.done ? r.quiet : 0;
    renderUpdateStall();
    if (r.done) {
      updateTail = null;
      if (r.cancelled) { updateEnded = "cancelled"; toast(UPDATE_WORDS.cancelledToast); loadUpdate(); return; }
      toast(r.ok ? "skein updated — reloading" : "the update failed; the log says where");
      // Only on success, and after a beat: the new binary has to be the one that answers the
      // reload, and the doorway re-execs every two seconds.
      if (r.ok) setTimeout(() => location.reload(), 2500);
      else loadUpdate();
      return;
    }
    updateTail = setTimeout(step, 800);
  };
  step();
}

// The press. Slow on purpose — it is an npm install into the sandbox — so it says so before it
// starts rather than looking like nothing happened.
async function pressUpdateAgents(btn) {
  btn.disabled = true;
  btn.textContent = "updating…";
  try {
    const r = await fetch("/api/update-agents", { method: "POST" }).then(r => r.json());
    // What MOVED, not that it ran: the server answers `claude: 1.2.3 -> 1.2.9`, which is a fact
    // somebody can check, and "done" is not.
    toast(r.ok ? r.text : `could not update: ${r.error}`);
    // Only on success, and by re-reading rather than by hiding the offer: the pane draws what the
    // server says is installed, so an install that moved one CLI and not the other still shows the
    // one that is left. Removing the row on the press would have claimed both.
    if (r.ok) loadUpdate();
  } catch (e) {
    toast(`could not update: ${e}`);
  } finally {
    btn.disabled = false;
    btn.textContent = "update agents";
  }
}

// Its own function so a test can hold the opener to the exact URL the server routes.
function loginTerminalUrl(runtime) { return `/api/login/${encodeURIComponent(runtime)}/terminal`; }

let loginTermState = null;   // { ws, term, runtime, said } while the login overlay is open
function openLoginTerminal(runtime) {
  if (loginTermState) return;   // one login at a time — a second click while one is open is a no-op
  const shell = document.getElementById("loginterm"), host = document.getElementById("lt-host");
  shell.classList.add("open");
  host.innerHTML = "";
  // The same construction as the dock's terminals (createSession) — same font, theme and renderer
  // reasoning apply — minus everything tied to a box: no statusline, no persistence, no reconnect.
  const term = new Terminal({ fontFamily:"ui-monospace,'JetBrains Mono','SF Mono',Menlo,monospace", fontSize:13,
    cursorBlink:false, customGlyphs:true, scrollback:8000,
    theme:{ background:"#08090b", foreground:"#e6edf3", cursor:"#7c7cf0" } });
  const fit = new FitAddon.FitAddon(); term.loadAddon(fit); term.open(host);
  const proto = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WebSocket(`${proto}://${location.host}${loginTerminalUrl(runtime)}`);
  ws.binaryType = "arraybuffer";
  const st = loginTermState = { ws, term, runtime, said: "" };
  const sendSize = () => { if (ws.readyState === 1) ws.send(JSON.stringify({ resize:{ cols:term.cols, rows:term.rows } })); };
  ws.onopen = () => { requestAnimationFrame(() => { fit.fit(); sendSize(); term.focus(); }); };
  ws.onmessage = e => {
    if (typeof e.data === "string") {
      // PTY bytes come as binary frames; a text frame is the server's own voice — the coaching
      // line, then the post-login outcome. The LAST of those is what the toast surfaces, and the
      // coaching line is excluded so abandoning the flow does not toast an instruction.
      const line = e.data.replace(/^skein:\s*/, "").trim();
      if (line && !line.startsWith("type /login")) st.said = line;
      term.write(e.data);
    } else term.write(new Uint8Array(e.data));
  };
  // Socket close IS the end of the flow, success or not (the server's contract): drop the surface,
  // re-read health ONCE so the banner repaints from the answer — gone if the login is live now —
  // and surface the server's final sentence.
  ws.onclose = () => {
    if (loginTermState !== st) return;
    loginTermState = null;
    try { term.dispose(); } catch {}
    shell.classList.remove("open");
    host.innerHTML = "";
    loadHealth();
    toast(st.said || `${runtime} login closed — nothing changed`);
  };
  term.onData(d => { if (ws.readyState === 1) ws.send(new TextEncoder().encode(d)); });
  term.onResize(sendSize);
}
// The ✕. Closing the socket is how a login flow ends — cleanup lives on `onclose`, so a flow the
// server ends and a flow you abandon leave through the same door.
function closeLoginTerminal() { try { loginTermState?.ws.close(); } catch {} }

// ---------- fleet gauges ----------
// Every box in the fleet shares one VM's memory, disk and cores, and until this strip existed the
// only way to see how much of any of them was left was to ask the sandbox by hand — usually after
// something had already been OOM-killed. It is read-only and approximate on purpose: the host
// re-asks the sandbox at most every 30s (see `fleet_resources`), so these figures are a glance, not
// a measurement, and nothing on the board is decided from them.
const GIB = mib => mib >= 102400 ? Math.round(mib / 1024) : (mib / 1024).toFixed(1);
// A stacked bar of `[{w, cls}]` plus the label on its right. Segments are drawn in order and their
// widths are percentages of the whole, so the caller decides what "full" means for its gauge.
function gaugeRow(key, segs, label, hint) {
  const used = segs.reduce((a, s) => a + s.w, 0);
  const heat = used >= 92 ? " hot" : used >= 78 ? " warm" : "";
  return `<div class="ga${heat}" title="${esc(hint)}"><span class="gk">${esc(key)}</span>`
    + `<span class="gbar">${segs.map(s => `<span class="gseg ${s.cls}" style="width:${Math.min(100, s.w).toFixed(1)}%"></span>`).join("")}</span>`
    + `<span class="gv">${esc(label)}</span></div>`;
}

function loadResources() {
  if (DEMO) return;
  // 204 = no fleet sandbox configured, which is a normal board rather than a fault: say nothing.
  // Same for a fetch that fails — the health banner already carries that news.
  //
  // This used to be two fetches on one tick, the second asking which transport skein was calling
  // the fleet with. There is one way in now (SKEIN-521), so the question has no answer to report
  // and the row that reported it is gone with the endpoint.
  fetch("/api/fleet/resources").then(r => r.status === 200 ? r.json() : null).catch(() => null)
    .then(r => {
    const strip = document.getElementById("gauges");
    if (!r || !r.mem_total) {
      strip.innerHTML = "";
      strip.className = "gauges";
      return;
    }
    const pct = (n, d) => d > 0 ? (n * 100 / d) : 0;
    // Memory is stacked because a single "used" figure cannot answer the question you actually ask
    // when the sandbox is slow: which of its claims is holding it. Boxes and docker are drawn apart
    // for that reason and *summed* against the ceiling, because they share one — the workload is a
    // single pool taken first-come, not a slice each. "other" is everything outside both: the VM's
    // own services and the kernel, which is what the reserve keeps back.
    const other = Math.max(0, r.mem_used - r.boxes - r.docker);
    const workload = r.boxes + r.docker;
    // A gauge with no denominator is dropped rather than drawn at zero: `df` cannot always see the
    // fleet root, and "disk 0.0/0.0G" reads as a full disk to anyone glancing at it.
    const rows = [
      r.mem_total && gaugeRow("mem", [
        { w: pct(r.boxes, r.mem_total), cls: "boxes" },
        { w: pct(r.docker, r.mem_total), cls: "docker" },
        { w: pct(other, r.mem_total), cls: "other" },
      ], `${GIB(r.mem_used)}/${GIB(r.mem_total)}G`,
        [`boxes ${GIB(r.boxes)}G · docker ${GIB(r.docker)}G`,
         r.workload_max
           ? `${GIB(workload)}G of ${GIB(r.workload_max)}G allowed — they share one pool, taken first-come`
           : "no ceiling set on the workload",
         `the VM's own services and kernel ${GIB(other)}G`,
         `${GIB(r.mem_total - r.mem_used)}G still available`].join("\n")),
      r.disk_total && gaugeRow("disk", [{ w: pct(r.disk_used, r.disk_total), cls: "other" }],
        `${GIB(r.disk_used)}/${GIB(r.disk_total)}G`,
        `${GIB(r.disk_total - r.disk_used)}G free on the filesystem holding every box's checkout`),
      // Its own row, not a segment of the one above: this is a different disk with a different
      // size, so the two do not share a denominator and a stacked bar would be a lie. Drawn in the
      // docker accent so the strip reads the same way down — this row and the docker segment of the
      // memory bar are the same thing's two costs.
      r.images_total && gaugeRow("images", [{ w: pct(r.images_used, r.images_total), cls: "docker" }],
        `${GIB(r.images_used)}/${GIB(r.images_total)}G`,
        `${GIB(r.images_total - r.images_used)}G free on /var/lib/docker — images, volumes and build cache\n`
        + `a separate disk from the boxes' own, fixed when the fleet was created: filling this one does not show up above`),
      // Load, not a percentage: CPU is deliberately uncapped and fair-shared, so what matters is
      // whether boxes are queueing for cores — which is load against core count, nothing else.
      r.cpus && gaugeRow("cpu", [{ w: pct(r.load1, r.cpus), cls: "other" }],
        `${r.load1.toFixed(1)}/${r.cpus}`,
        `load average ${r.load1.toFixed(2)} (1m) · ${r.load5.toFixed(2)} (5m) across ${r.cpus} cores\n`
        + `over ${r.cpus} means boxes are waiting for a core, not that anything is wrong`),
    ];
    strip.innerHTML = rows.filter(Boolean).join("");
    strip.className = "gauges on" + (r.stale ? " stale" : "");
    if (r.stale) strip.title = "the sandbox is not answering — these are the last figures that arrived";
    else strip.title = "the fleet sandbox's own VM — refreshed every 30s";
  }).catch(() => {});
}

// ---------- demo seed (only used under ?demo) ----------
const DEMO_REPOS = [
  { id:"thing", source:"https://github.com/acme/gadget-demo.git", work:"~/.skein/repos/thing/work", store:"~/.skein/repos/thing/store/.claude", agent:"claude" },
  { id:"skein",  source:"https://github.com/acme/skein-demo.git", work:"~/.skein/repos/skein/work", store:"~/.skein/repos/skein/store/.claude", agent:"claude" },
];
const DEMO_BOXES = [
  { name:"thing-feat-auth", repo:"thing", branch:"feat/auth", state:"needs-input", tier:0, age:"2m",
    agent:"claude",
    dir:"~/.skein/repos/thing/work", diff:{files:6,ins:124,del:18}, pause:"needs-input",
    headline:"Store the session token in localStorage or an httpOnly cookie?", task:"Wiring the login flow" },
  { name:"skein-fix-login", repo:"skein", branch:"fix/login", state:"waiting", tier:1, age:"5m",
    agent:"codex",
    dir:"~/.skein/repos/skein/work", diff:{files:2,ins:41,del:6}, pause:"proceed",
    headline:"Fix looks right — want me to push and open a PR?", task:"" },
  { name:"thing-master", repo:"thing", branch:"master", state:"working", tier:3, age:"just now",
    agent:"claude",
    dir:"~/.skein/repos/thing/work", diff:{files:9,ins:312,del:44}, pause:"none",
    headline:"", task:"Refactoring the diff parser" },
  { name:"skein-ui-redesign", repo:"skein", branch:"ui/redesign", state:"working", tier:3, age:"just now",
    agent:"codex",
    dir:"~/.skein/repos/skein/work", diff:{files:4,ins:88,del:12}, pause:"none",
    headline:"", task:"Restyling the fleet rail" },
  { name:"thing-calendar", repo:"thing", branch:"feat/calendar", state:"done", tier:2, age:"11m",
    dir:"~/.skein/repos/thing/work", diff:{files:7,ins:210,del:3}, pause:"none",
    headline:"Calendar view implemented; all tests pass.", task:"" },
  { name:"thing-spike-ocr", repo:"thing", branch:"spike/ocr", state:"idle", tier:4, age:"3h",
    dir:"~/.skein/repos/thing/work", diff:{files:1,ins:12,del:0}, pause:"none", headline:"", task:"" },
  { name:"skein-old-probe", repo:"skein", branch:"chore/probe", state:"stale", tier:5, age:"2d",
    dir:"~/.skein/repos/skein/work", diff:null, pause:"none", headline:"", task:"" },
];
// A non-functional terminal pane for demo (no websocket); keeps the dock layout reviewable.
function createDemoSession(box, kind, runtime) {
  const id = sidOf(box, kind);
  const host = document.createElement("div"); host.className = "thost";
  host.innerHTML = `<div style="font-family:var(--mono);font-size:13px;color:var(--dim);padding:20px;line-height:1.7">
    <span style="color:var(--accent)">demo</span> — live terminal disabled.<br>
    this pane is <b style="color:var(--text)">${esc(box)}</b> · ${esc(runtime || kind)} session.<br>
    run <code style="color:var(--text)">skein-server</code> for real agent terminals.</div>`;
  termarea.append(host);
  const s = { term:{ focus(){}, write(){} }, fit:{ fit(){} }, ws:{ readyState:1, send(){} },
              host, box, kind, runtime:runtime || kind, sid:id, streaming:false };
  sessions.set(id, s);
  return s;
}

// ---------- boot ----------
setFavicon(0);
if (DEMO) {
  loadRuntimes();
  repos = DEMO_REPOS;
  restored = true;                                  // don't reopen real tabs from a prior session
  render(DEMO_BOXES);
  const live = document.getElementById("live"); live.classList.add("on");
  document.getElementById("livetext").textContent = "demo";
} else {
  loadRuntimes();
  fetch("/api/boxes").then(r => r.json()).then(render).catch(() => {});
  connect();
  loadSettings();
  loadRepos();
  // At boot, not only when Settings opens: ◇ Track work is decided per box from its repo's
  // connection, and a dockbar that waits for a dialog nobody opened would never offer it.
  loadSync().then(renderDockbar);
  loadHealth();
  setInterval(loadHealth, 15000);
  // Polled at half the host's own refresh interval so a change surfaces within one cycle of it; the
  // Gate behind the endpoint means the extra polls cost a map lookup, not an `sbx exec`.
  loadResources();
  setInterval(loadResources, 15000);
  // Slower than the rest, deliberately: a package request is answered by a person over minutes, not
  // by the board over seconds, and this poll execs into the sandbox to read the queue.
  pollSubq();
  setInterval(pollSubq, 20000);
  // Same reasoning, same cadence: a write request waits on a person, and reading its queue execs
  // into the sandbox. Offset from the package poll so the two do not exec on the same tick.
  pollGitq();
  setInterval(pollGitq, 23000);
  // A box's questions wait on a person too, and the read execs into the sandbox the same way.
  pollAskq();
  setInterval(pollAskq, 21000);
  // Slower again by an order of magnitude. The two above wait on a person inside the fleet; this one
  // waits on other people on GitHub, and each tick is three `gh` round trips per enabled repo. A PR
  // that arrives three minutes before you hear about it has cost you nothing.
  pollReviewCounts();
  setInterval(pollReviewCounts, REV_POLL_MS);
}
