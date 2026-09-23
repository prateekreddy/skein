// ---------- ship: PR · checks · stop · destroy (host-side via gh/sbx) ----------
// A box's own lifecycle buttons. PR state used to live here too — a link, its checks and a Merge —
// back when a PR was a property of the box that opened it. Reviewing happens in the review pane
// now, where the repo owns the list and a PR nobody in the fleet authored is still visible, so a
// second PR vocabulary on this toolbar would only be a second place to look and a second place to
// get it wrong.
function boxRunHtml() {
  return `<button class="kbtn" id="drestart" title="restart the active agent process and resume its native session inside tmux">Restart</button>`
       + `<button class="kbtn" id="dstop" title="stop — halt this box to free its compute; resume later via attach">Stop</button>`;
}
function stopBox(name) {
  if (!confirm(`Stop ${name}? This halts the box to free its compute. Nothing is lost — resume it later via attach.\n\nOther boxes keep running.`)) return;
  toast(`stopping ${name}…`);
  fetch(`/api/boxes/${encodeURIComponent(name)}/stop`, { method:"POST" }).then(r => r.json()).then(d => {
    if (d.ok) { closeBoxSessions(name); toast(`stopped ${name}`); refreshRow(name); }
    else toast("stop failed: " + (d.error||"").split("\n")[0]);
  }).catch(() => toast("stop failed"));
}
function destroyBox(name) {
  if (settings.confirm_destroy !== false
      && !confirm(`Destroy ${name}?\n\nThis removes THIS BOX and reclaims its disk. Other boxes are untouched. In clone mode any commits in it that were never pushed or fetched are LOST.\n\nThis cannot be undone.`)) return;
  toast(`destroying ${name}…`);
  fetch(`/api/boxes/${encodeURIComponent(name)}/destroy`, { method:"POST" }).then(r => r.json()).then(d => {
    if (d.ok) { closeBoxSessions(name);
      if (view.box === name) { view = {box:null,mode:"term",kind:"agent"}; applyView(); } toast(`destroyed ${name}`); }
    else toast("destroy failed: " + (d.error||"").split("\n")[0]);
  }).catch(() => toast("destroy failed"));
}
function batchResume() {
  const ps = boxes.filter(b => b.pause === "proceed");
  if (!ps.length) return;
  const list = ps.map(b => `  • ${b.name} — ${b.headline || "proceed?"}`).join("\n");
  if (!confirm(`Continue ${ps.length} box${ps.length === 1 ? "" : "es"} paused on a trivial "proceed?":\n\n${list}\n\n`
      + `Each resumes its agent and runs in the shared rate-limit window. They'll pause again if they hit real doubt.`)) return;
  fetch("/api/resume-batch", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ names: ps.map(b => b.name) })
  }).then(r => r.json()).then(d => {
    if (!d.ok) { toast("continue failed"); return; }
    const n = (d.resumed || []).length, h = (d.held || []).length;
    // held = boxes the AI safety gate flagged as a real decision (only when SKEIN_AI=on) — route to one
    toast(`continuing ${n}${h ? ` · held ${h} for your eyes` : ""}`);
    if (h && d.held[0]) select(d.held[0]);
  }).catch(() => toast("continue failed"));
}

// An error nobody can see is an error nobody can report.
//
// The owner's report for SKEIN-268 was "any small error anywhere in the review page just blanks the
// entire page and gives the error" — a description, not a sentence, because the sentence was in a
// devtools console that was not open. Both handlers below land in the cockpit's own toast, so the
// next report arrives with the message attached.
//
// De-duplicated on purpose: one fault fires on every render and every poll, and forty toasts about
// one bug is the reason people learn to ignore toasts. Once, then not again for a minute.
//
// Deliberately NOT a place that re-renders. This runs because something threw; asking the surface
// that just failed to repaint itself is one throw away from a loop.
let lastPageError = "", lastPageErrorAt = 0;
function reportPageError(why) {
  const line = String(why == null ? "an error with no message" : why).split("\n")[0].slice(0, 200);
  const now = Date.now();
  if (line === lastPageError && now - lastPageErrorAt < 60000) return;
  lastPageError = line;
  lastPageErrorAt = now;
  toast(`something went wrong in the page: ${line}`);
}
if (typeof window !== "undefined" && window.addEventListener) {
  window.addEventListener("error", ev => reportPageError((ev && ev.message) || ev));
  // A rejected promise never reaches `error`, and the review pane is mostly promises: every fetch
  // that lands, every draft that answers. Left alone it is the quietest failure the page has.
  window.addEventListener("unhandledrejection", ev => {
    const r = ev && ev.reason;
    reportPageError((r && r.message) || r || "a promise rejected with no reason");
  });
}

// The single highlighted call-to-action for the loop: continue/reply → review → PR → merge → destroy.
let toastT;
/// `url` makes the notice a link AND doubles its life — 8s rather than 3.5s. Until SKEIN-417 gave
/// it one in `revFire`, this parameter had no caller anywhere in the page: `grep -nE 'toast\([^)]*,'`
/// found only its own definition and two messages that happen to contain a comma.
///
/// `rel="noopener"` because this arm is now reachable: every browser skein supports implies it for
/// `target="_blank"`, and the receipt's own "open on GitHub ↗" writes it out anyway — one of them
/// stating the rule and the other relying on the default would be an invitation to copy the wrong
/// one.
function toast(msg, url) {
  let t = document.getElementById("toast");
  if (!t) { t = document.createElement("div"); t.id = "toast"; document.body.append(t); }
  t.innerHTML = url ? link(url, esc(msg)) : esc(msg);
  t.classList.add("show"); clearTimeout(toastT); toastT = setTimeout(() => t.classList.remove("show"), url ? 8000 : 3500);
}
// Keyed reconcile (like the fleet rows): tabs are long-lived elements mutated in place, not
// rebuilt — rebuilding every 2s tick dropped hover state on the ✕ and swallowed clicks whose
// mousedown straddled a tick (the mouseup landed on a detached node).
const tabEls = new Map();   // sid -> tab element
// Tab order is the user's, not the Map's: `sessions` is keyed by sid and its insertion order says
// "when you opened it", which is not the order you want to read a fleet in. Drag a tab, or ⌥⇧[ / ⌥⇧],
// to move it. Self-healing rather than bookkept — unknown sids are appended and closed ones fall out,
// so nothing can desync this from `sessions`, and persistView writes tabs in this order so a reload
// comes back the way you arranged it.
const tabOrder = [];
function orderedSids() {
  const live = new Set(sessions.keys());
  const out = tabOrder.filter(id => live.has(id));
  for (const id of sessions.keys()) if (!out.includes(id)) out.push(id);
  tabOrder.length = 0; tabOrder.push(...out);
  return out;
}
function orderedSessions() { return orderedSids().map(id => sessions.get(id)); }
// Move `id` to visual index `to` (clamped). Returns whether anything actually moved.
function moveTab(id, to) {
  const ids = orderedSids(), from = ids.indexOf(id);
  if (from < 0) return false;
  to = Math.max(0, Math.min(ids.length - 1, to));
  if (to === from) return false;
  ids.splice(to, 0, ...ids.splice(from, 1));
  tabOrder.length = 0; tabOrder.push(...ids);
  renderTabs(); persistView();
  return true;
}
// Drag state. The move is applied on `drop` and previewed with an insertion caret until then —
// reordering live on dragover looks nicer but cannot be undone: the browser sends no key events
// during a drag, so an Esc cancel arrives only as a `dragend`, indistinguishable from a release over
// a gap in the tab bar. Committing on drop makes Esc a true cancel and costs nothing.
let dragSid = null, dropAt = -1;       // sid being dragged + the insertion slot the caret marks
// The caret is an inline style, not a class: renderTabs rewrites className on every fleet tick and
// would wipe it mid-drag.
function markInsert(slot) {
  const els = [...tabsEl.querySelectorAll(".tab")];
  els.forEach(el => { el.style.boxShadow = ""; });
  dropAt = slot;
  if (slot < 0 || !els.length) return;
  const edge = "inset 2px 0 0 var(--accent)", el = els[Math.min(slot, els.length - 1)];
  el.style.boxShadow = slot >= els.length ? edge.replace("2px 0", "-2px 0") : edge;
}
function renderTabs() {
  const aid = activeSid();
  for (const [id, el] of tabEls) if (!sessions.has(id)) { el.remove(); tabEls.delete(id); }
  let pos = 0;
  for (const id of orderedSids()) {
    const s = sessions.get(id);
    let t = tabEls.get(id);
    if (!t) {
      t = document.createElement("div");
      t.dataset.id = id;
      t.addEventListener("click", e => {
        const cs = sessions.get(id); if (!cs) return;
        if (e.target.classList.contains("tx")) closeSession(id); else showBox(cs.box, "term", cs.kind);
      });
      // Drag to reorder — dragover/drop live on the tab strip (see below) so releasing in the gap
      // after the last tab still lands. A custom dataTransfer type keeps this drag out of the
      // file-attachment handler, which only acts on types containing "Files"; Firefox also refuses
      // to start a drag with no data set at all.
      t.draggable = true;
      t.addEventListener("dragstart", e => {
        dragSid = id; t.classList.add("dragging");
        e.dataTransfer.effectAllowed = "move";
        try { e.dataTransfer.setData("application/x-skein-tab", id); } catch {}
      });
      t.addEventListener("dragend", () => { t.classList.remove("dragging"); dragSid = null; markInsert(-1); });
      tabEls.set(id, t);
    }
    const g = groupOf(stateOf(s.box));
    const cls = `tab t-${g}` + (id === aid && view.mode === "term" ? " on" : "") + (s.streaming ? " streaming" : "") + (s.dead ? " dead" : "") + (s.kind === "shell" ? " shell" : "");
    if (t.className !== cls) t.className = cls;
    const what = s.kind === "shell" ? `${s.box} — shell` : `${s.box} — ${s.runtime || "agent"}`;
    // Half-signal caveat, so it's visible per tab rather than only in the tab you happen to open.
    const row = boxes.find(x => x.name === s.box) || {};
    const half = s.kind === "shell" ? null : screenHalf(row);
    // A pane skein closed on purpose is not "disconnected", and two of the three are not waiting to
    // be clicked at all (SKEIN-702) — a tab that asks for a click while the pane below it says it
    // reconnects on its own is the two halves of one screen disagreeing.
    const deadTitle = RECOVERY[s.waitFor]
      ? `${what} — ${RECOVERY[s.waitFor][0]}${RECOVERY[s.waitFor][2] ? " · click to reconnect" : ""}`
      : `${what} — disconnected · click to reconnect`;
    const title = (s.dead ? deadTitle : what)
      + (half ? `\n\n${half[0]} — ${half[1]}` : "")
      + `\ndrag to reorder · ⌥1–⌥9 to switch · ? for all shortcuts`;
    if (t.title !== title) t.title = title;
    const lead = s.dead ? `<span class="trc" title="reconnect">↻</span>` : `<span class="td"></span>`;
    const label = esc(s.box) + (s.kind === "shell"
      ? ` <span class="tk">sh</span>`
      : ` <span class="tk">${esc((s.runtime || "agent").slice(0,2))}</span>`)
      + (half ? ` <i class="hdot thalf"></i>` : "");
    const html = `${lead}<span class="tl">${label}</span><span class="tx" title="close">✕</span>`;
    if (t.dataset.html !== html) { t.dataset.html = html; t.innerHTML = html; }
    if (tabsEl.children[pos] !== t) tabsEl.insertBefore(t, tabsEl.children[pos] || null);
    pos++;
  }
  let da = tabsEl.querySelector(".dock-actions");
  if (sessions.size) {
    if (!da) {
      da = document.createElement("div"); da.className = "dock-actions";
      da.innerHTML = `<span class="da" title="close all sessions">close all</span>`;
      da.querySelector(".da").addEventListener("click", () => [...sessions.keys()].forEach(closeSession));
    }
    if (tabsEl.lastElementChild !== da) tabsEl.appendChild(da);
  } else da?.remove();
}
// Which insertion slot an x coordinate means: the first tab whose midpoint it hasn't passed, else
// "after the last one".
function slotFor(x) {
  const els = [...tabsEl.querySelectorAll(".tab")];
  for (let i = 0; i < els.length; i++) {
    const r = els[i].getBoundingClientRect();
    if (x < r.left + r.width / 2) return i;
  }
  return els.length;
}
// The reorder gestures live on the strip, not the individual tabs, so the gap past the last tab and
// the "close all" corner are valid places to release. Capture phase, and no-ops unless a tab drag is
// in flight — the file-attachment handler on the same events only acts on drags carrying Files.
document.addEventListener("dragover", e => {
  if (!dragSid || !tabsEl || !e.target?.closest?.("#tabs")) return;
  e.preventDefault(); e.dataTransfer.dropEffect = "move";
  markInsert(slotFor(e.clientX));
}, true);
document.addEventListener("drop", e => {
  if (!dragSid || !tabsEl || !e.target?.closest?.("#tabs")) return;
  e.preventDefault();
  const ids = orderedSids(), from = ids.indexOf(dragSid), slot = dropAt;
  markInsert(-1);
  // The slot counts positions in the list *including* the dragged tab, so a rightward move loses one
  // index when that tab vacates its own slot.
  if (slot >= 0 && from >= 0) moveTab(dragSid, slot > from ? slot - 1 : slot);
}, true);
function renderTabFlag(id) {
  tabEls.get(id)?.classList.toggle("streaming", !!sessions.get(id)?.streaming);
}
function refreshRowAll() { for (const name of rowEls.keys()) refreshRow(name); }

// ---------- session digest: "what happened here" (free — commits + journal + last message) ----------
function loadSession(name) {
  sesspane.innerHTML = `<div class="sess"><div class="note">loading…</div></div>`;
  fetch(`/api/boxes/${encodeURIComponent(name)}/session`).then(r => r.ok ? r.json() : null).then(d => {
    if (view.box !== name || view.mode !== "session") return;
    sesspane.innerHTML = d ? renderSession(d) : `<div class="sess"><div class="note">no session digest yet</div></div>`;
    sesspane.querySelector("#seediff")?.addEventListener("click", e => { e.preventDefault(); openDiff(name); });
    // lazy AI narration (step 7): only when there's no journal to read; returns null unless SKEIN_AI=on.
    // One rationed Haiku call per turn-end, server-cached — so reopening the tab won't re-spend.
    if (d && !d.journal && (d.last_message || d.blocked_on)) loadNarration(name);
  }).catch(() => { sesspane.innerHTML = `<div class="sess"><div class="note">could not load session</div></div>`; });
}
function loadNarration(name) {
  fetch(`/api/boxes/${encodeURIComponent(name)}/narrate`).then(r => r.json()).then(d => {
    if (view.box !== name || view.mode !== "session" || !d.summary) return;
    const sess = sesspane.querySelector(".sess"); if (!sess) return;
    sess.querySelector("#ai-sum")?.remove();   // rapid tab toggling can land two responses — keep one
    const blk = document.createElement("div"); blk.className = "blk"; blk.id = "ai-sum";
    blk.innerHTML = `<h4>summary <span class="prov">written by Haiku from the box's last message, not by the box</span></h4>`
      + `<div class="msg ai">${esc(d.summary)}</div>`;
    sess.prepend(blk);
  }).catch(() => {});
}
function renderSession(d) {
  let h = `<div class="sess">`;
  if (d.blocked_on) h += `<div class="blk"><h4>blocked on</h4><div class="ask">${esc(d.blocked_on)}</div></div>`;
  else if (d.last_message) h += `<div class="blk"><h4>last message</h4><div class="msg">${esc(d.last_message)}</div></div>`;
  if (d.journal) h += `<div class="blk"><h4>journal</h4><div class="jrnl">${esc(d.journal)}</div></div>`;
  if (d.commits?.length)
    h += `<div class="blk"><h4>commits (${d.commits.length})</h4><ul class="cmts">${d.commits.map(c => `<li>${esc(c)}</li>`).join("")}</ul></div>`;
  if (d.diff && (d.diff.files||d.diff.ins||d.diff.del))
    h += `<div class="blk"><h4>changes</h4><div class="meta">${diffBadge(d.diff)} — <a href="#" id="seediff">view diff ▸</a></div></div>`;
  if (!d.blocked_on && !d.last_message && !d.journal && !d.commits?.length)
    h += `<div class="note">nothing reported yet — the box writes a digest when its agent pauses (Stop hook)</div>`;
  return h + `</div>`;
}

// ---------- transcript: the conversation from the box's own record ----------
// The terminal is a view of the conversation and the most fragile copy of it — it dies with the tab,
// the socket, the tmux session and the scrollback limit. This reads the JSONL the runtime itself
// writes, so a box reboot costs you the colours and nothing else. Tail-first, because the record is
// megabytes: "older" doubles the window rather than fetching the whole file.
const TX_START = 256 * 1024, TX_CAP = 8 * 1024 * 1024;
const txState = new Map();   // box -> bytes currently requested
function loadTranscript(name, bytes) {
  const want = bytes || txState.get(name) || TX_START;
  txState.set(name, want);
  if (!txpane.querySelector(".txmsg")) txpane.innerHTML = `<div class="txwrap"><div class="note">reading the record…</div></div>`;
  fetch(`/api/boxes/${encodeURIComponent(name)}/transcript?bytes=${want}`)
    .then(async r => { if (!r.ok) throw new Error((await r.text()) || r.statusText); return r.json(); })
    .then(d => { if (view.box === name && view.mode === "tx") renderTranscript(name, d); })
    .catch(e => { txpane.innerHTML = `<div class="txwrap"><div class="note">could not read the record — ${esc(reason(e))}</div></div>`; });
}
const txTime = ts => { const d = ts ? new Date(ts) : null; return d && !isNaN(d) ? d.toLocaleTimeString([], {hour:"2-digit", minute:"2-digit"}) : ""; };
function renderTranscript(name, d) {
  if (d.note) { txpane.innerHTML = `<div class="txwrap"><div class="note">${esc(d.note)}</div></div>`; return; }
  const kb = n => n < 1024*1024 ? `${Math.round(n/1024)}K` : `${(n/1048576).toFixed(1)}M`;
  const older = d.complete
    ? `<div class="txend">the beginning of this record</div>`
    : `<button class="kbtn ghost txmore">load older (${kb(d.scanned)} of ${kb(d.size)} read)</button>`;
  const body = d.messages.map(m => {
    const tools = (m.tools || []).map(t => `<div class="txtool">${esc(t)}</div>`).join("");
    const text = m.text ? `<div class="txtext">${m.role === "assistant" ? marked.parse(m.text) : esc(m.text)}</div>` : "";
    return `<div class="txmsg ${m.role}${m.sidechain ? " side" : ""}">`
      + `<div class="txwho">${m.role === "user" ? "you" : "claude"}${m.sidechain ? " · subagent" : ""}<span class="txts">${esc(txTime(m.ts))}</span></div>`
      + text + tools + `</div>`;
  }).join("");
  txpane.innerHTML = `<div class="txwrap"><div class="txhead">${esc(d.path.split("/").pop() || "record")}`
    + `<span class="txmeta">${d.messages.length} messages · ${kb(d.size)} on disk</span>`
    + `<button class="kbtn ghost txref" title="re-read the record">↻</button></div>`
    + `<div class="txolder">${older}</div>${body}</div>`;
  const wrap = txpane.querySelector(".txwrap");
  wrap.scrollTop = wrap.scrollHeight;   // newest last, like the terminal it replaces
  // a link in a recorded message must not navigate the cockpit away from itself
  txpane.querySelectorAll(".txtext a").forEach(a => { a.target = "_blank"; a.rel = "noopener"; });
  txpane.querySelector(".txmore")?.addEventListener("click", () => loadTranscript(name, Math.min(d.scanned * 4, TX_CAP)));
  txpane.querySelector(".txref")?.addEventListener("click", () => loadTranscript(name));
}

// ---------- files: browse + read the box's workspace without leaving skein ----------
// Two panes: directory list left, content right. Markdown renders via vendored marked (raw HTML
// escaped — repo content must never script against the unauthenticated cockpit API); images inline;
// everything else as plain text. Opening the tab at the root auto-opens README.md when present —
// the "I just want to read the docs" case, zero clicks.
const filesState = new Map();   // box -> { path, file }   (persists across tab switches)
// Markdown is rendered from four sources skein does not control — a `.md` file in a box's tree, the
// model's reading of a pull request, a model answer, and an assistant message — so `marked` needs
// two guards, not one.
//
// `html` was here already and closes TAG injection: a script tag or an `onerror` attribute written in
// markdown comes back escaped. That was verified and it works.
//
// `link` and `image` are the half that was missing, and the gap was real: marked v12 removed its
// `sanitize` option, so nothing in the library filters URL SCHEMES. Measured against this exact
// bundle with the `html` override applied — `[click](javascript:alert(1))` rendered as
// `<a href="javascript:alert(1)">`, and `data:text/html;base64,…` likewise. One click in the cockpit
// origin then drives the whole API: the session cookie is HttpOnly, so injected script cannot READ
// it, but it is SameSite=Strict and Path=/, so every same-origin fetch that script makes carries it.
//
// An allow-list of schemes rather than a deny-list of `javascript:`, for the reason `valid_name`
// gives on the Rust side: a deny-list has to anticipate every scheme a browser will ever navigate,
// and the next one is always the one nobody listed. Relative and anchor links carry no scheme at
// all and are the common case, so they pass by having nothing to check.
// Markdown is rendered from four sources skein does not control — a `.md` file in a box's tree, the
// model's reading of a pull request, a model answer, and an assistant message — so `marked` needs
// two guards, not one.
//
// `html` closes TAG injection: a script tag or an `onerror` attribute written in markdown comes back
// escaped. That was already here and it works.
//
// `safeHref` is the half that was missing. marked v12 removed its `sanitize` option, so nothing in
// the library filters URL SCHEMES, and `[click](javascript:…)` rendered as a live href. It lives in
// `cockpit/src/links.mjs` — imported here as a global from `/vendor/cockpit.js` — because it is a
// security boundary, and a boundary that cannot be imported cannot be tested except by
// reimplementing it. `cockpit/test/links.test.mjs` is that test, including the entity evasion that
// got through the first version of this fix.
//
// The href is corrected on the TOKEN, before rendering, rather than by replacing marked's `link` and
// `image` renderers. Two reasons, and the second is why the first version of this was thrown away:
// marked keeps rendering everything else itself, so `[**bold** text](…)` keeps its emphasis; and a
// replacement renderer needs `this.parser.parseInline` to render the link's own children, which is
// not bound on the renderer object this bundle builds from `use()` — it throws.
//
// A refused link keeps its TEXT and loses only its destination, so the reader still sees the
// sentence somebody wrote. `pointer-events:none` in the CSS makes it inert rather than merely
// harmless: an empty href would otherwise reload the cockpit and lose the page's state on a click.
marked.use({
  renderer: { html(t) { return esc(t.text ?? t.raw ?? ""); } },
  walkTokens(token) {
    if (token.type !== "link" && token.type !== "image") return;
    const safe = safeHref(token.href, document.baseURI);
    // The NORMALISED href goes back on the token, never the one that came in. `safeHref` judges the
    // string the browser's URL parser will get — entities decoded — and if the document kept the
    // original, the guard would have judged one string and the browser would parse another. That is
    // not a hypothetical: it is the shape of both bypasses this guard has had.
    if (safe !== null) { token.href = safe; return; }
    token.href = "";
    // Deliberately NOT echoing the refused URL back into the DOM. It is escaped and inert there,
    // but a hostile string that never reaches the document cannot be got wrong later.
    token.title = `skein did not follow this ${token.type} — its address was not http, https or mailto`;
  },
});
function fmtSize(n) { return n < 1024 ? `${n}B` : n < 1048576 ? `${(n/1024).toFixed(0)}K` : `${(n/1048576).toFixed(1)}M`; }
function fileUrl(box, path) { return `/api/boxes/${encodeURIComponent(box)}/file?path=${encodeURIComponent(path)}`; }
// the server's own words when it refuses, trimmed to one line; "unreachable" when fetch never landed
function reason(e) { const m = String((e && e.message) || e || "").trim().split("\n")[0]; return m && m !== "Failed to fetch" ? m.slice(0, 160) : "the host is not answering"; }
function loadFiles(name, path, file) {
  const st = filesState.get(name) || { path:"", file:null };
  const prev = st.path;
  if (path !== undefined) st.path = path;
  if (file !== undefined) st.file = file;
  filesState.set(name, st);
  fetch(`/api/boxes/${encodeURIComponent(name)}/files?path=${encodeURIComponent(st.path)}`)
    .then(r => r.ok ? r.json() : r.text().then(t => { throw new Error(t); }))
    .then(d => {
      if (view.box !== name || view.mode !== "files") return;
      // first visit at the root: auto-open the README if one exists
      if (st.file === null && !st.path) {
        const rd = d.entries.find(e => !e.dir && /^readme\.md$/i.test(e.name));
        if (rd) st.file = rd.name;
      }
      renderFiles(name, d, st);
      if (st.file) loadFileContent(name, st.file);
    })
    // A refusal must not strand you: stay where you were browsing and say WHY in the view pane
    // ("path escapes the workspace" — e.g. the .claude store link — and "workspace unavailable"
    // are different problems). Only a first load with nothing on screen takes the whole pane.
    .catch(e => {
      if (view.box !== name || view.mode !== "files") return;
      const want = st.path; st.path = prev; filesState.set(name, st);
      const note = `<div class="note">could not open ${esc(want || "the workspace")} — ${esc(reason(e))}</div>`;
      const fv = filespane.querySelector("#fview");
      if (fv) fv.innerHTML = note; else filespane.innerHTML = `<div class="fview">${note}</div>`;
    });
}
function renderFiles(name, d, st) {
  const crumbs = [`<a href="#" data-p="">${esc(name)}</a>`];
  let acc = "";
  for (const part of (d.path ? d.path.split("/") : [])) {
    acc = acc ? `${acc}/${part}` : part;
    crumbs.push(`<a href="#" data-p="${esc(acc)}">${esc(part)}</a>`);
  }
  const rows = d.entries.map(e => {
    const p = d.path ? `${d.path}/${e.name}` : e.name;
    const on = st.file === p ? " on" : "";
    return e.dir
      ? `<div class="fent fdir" data-d="${esc(p)}"><span class="fi">▸</span>${esc(e.name)}/</div>`
      : `<div class="fent${on}" data-f="${esc(p)}"><span class="fi">·</span>${esc(e.name)}<span class="fsz">${fmtSize(e.size)}</span></div>`;
  }).join("");
  // Which tree answered is not a detail: a clone-mode box's host clone is a different branch, and
  // a listing that doesn't say so is quietly showing you the wrong files.
  const src = d.vantage === "host"
    ? `<span class="fsrc host" title="${esc(d.note || "the box could not be asked")}">host clone</span>` : "";
  const empty = d.note && !d.entries.length ? `<div class="fnote">${esc(d.note)}</div>` : `<div class="fent">empty</div>`;
  // The badge sits in the breadcrumb where your eye already is; the banner carries the REASON, and
  // shows even when the fallback tree does have files — a silent one is how this shipped broken.
  filespane.innerHTML =
    `<div class="flist">${provenance(d)}<div class="fcrumb">${crumbs.join(" / ")}${src}</div>${rows || empty}</div>`
    + `<div class="fview" id="fview">${st.file ? `<div class="note">loading…</div>` : `<div class="note">select a file</div>`}</div>`;
  filespane.querySelector(".flist").addEventListener("click", e => {
    const c = e.target.closest("[data-p]"); if (c) { e.preventDefault(); loadFiles(name, c.dataset.p, null); return; }
    const dd = e.target.closest("[data-d]"); if (dd) { loadFiles(name, dd.dataset.d, null); return; }
    const ff = e.target.closest("[data-f]"); if (ff) openFileAt(name, ff.dataset.f);
  });
}
function openFileAt(name, path) {
  const st = filesState.get(name) || { path:"", file:null };
  st.file = path;
  // navigating to a file in another dir (relative md link): sync the list pane to its dir
  const dir = path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "";
  if (dir !== st.path) { loadFiles(name, dir, path); return; }
  filesState.set(name, st);
  for (const el of filespane.querySelectorAll(".fent[data-f]"))
    el.classList.toggle("on", el.dataset.f === path);
  loadFileContent(name, path);
}
function loadFileContent(name, path) {
  const fv = filespane.querySelector("#fview"); if (!fv) return;
  const ext = (path.split(".").pop() || "").toLowerCase();
  if (["png","jpg","jpeg","gif","webp","svg"].includes(ext)) {
    fv.innerHTML = `<img src="${esc(fileUrl(name, path))}" alt="${esc(path)}">`;
    return;
  }
  fetch(fileUrl(name, path))
    .then(r => r.text().then(t => { if (!r.ok) throw new Error(t); return { t, trunc: r.headers.get("x-truncated") === "1" }; }))
    .then(({ t, trunc }) => {
      if (view.box !== name || view.mode !== "files") return;
      const note = trunc ? `<div class="ftrunc">truncated at 2MB — showing the beginning</div>` : "";
      if (ext === "md" || ext === "markdown") {
        fv.innerHTML = note + `<div class="fmd">${marked.parse(t)}</div>`;
        // relative links navigate the file browser; absolute ones open a new tab; relative images
        // are rewritten to the file API so they render.
        const base = path.includes("/") ? path.slice(0, path.lastIndexOf("/") + 1) : "";
        const resolve = h => { const parts = (base + h).split("/").filter(s => s !== "."); const out = [];
          for (const s of parts) { if (s === "..") out.pop(); else out.push(s); } return out.join("/"); };
        for (const img of fv.querySelectorAll("img")) {
          const src = img.getAttribute("src") || "";
          if (src && !/^(https?:|data:|\/)/i.test(src)) img.src = fileUrl(name, resolve(src));
        }
        fv.querySelectorAll("a").forEach(a => {
          const href = a.getAttribute("href") || "";
          if (/^(https?:)/i.test(href)) { a.target = "_blank"; a.rel = "noopener"; }
          else if (href && !href.startsWith("#")) a.addEventListener("click", e => { e.preventDefault(); openFileAt(name, resolve(href)); });
        });
      } else {
        fv.innerHTML = note + `<pre class="fraw">${esc(t)}</pre>`;
      }
    })
    .catch(e => { fv.innerHTML = `<div class="note">could not read ${esc(path)} — ${esc(reason(e))}</div>`; });
}

// The diff is computed inside the box now, against the remote base branch. It takes a moment
// (a git fork in a sandbox), which is why this says "asking the box" rather than "loading".
function loadDiff(name) {
  diffpane.innerHTML = `<div class="diff"><div class="note">asking the box…</div></div>`;
  fetch(`/api/boxes/${encodeURIComponent(name)}/diff`).then(r => r.json()).then(d => {
    if (view.box !== name || view.mode !== "diff") return;
    // An empty patch is an answer, and the server's note says which one — "nothing changed" and
    // "the box isn't running" look identical otherwise.
    diffpane.innerHTML = (d.patch || "").trim()
      ? diffBanner(d) + renderDiff(d.patch)
      : `<div class="diff"><div class="note">${esc(d.note || "no changes on this branch yet")}</div></div>`;
    renderComments(name);
  }).catch(() => { diffpane.innerHTML = `<div class="diff"><div class="note">could not load diff</div></div>`; });
}
// One renderer for provenance, used by every pane that shows a value skein read from somewhere.
// The server sends TWO provenance fields beside the value, and they answer different questions.
// `vantage` ("box" | "store" | "host") is which COPY of the fact this is — "box" is the only one
// that describes the box as it is right now, so the other two always say so. `reach` ("enter" |
// "socket" | "file" | "http") is HOW it was read. Both were once called `source`, which read as one
// question with three answers and was two questions with different ones.
//
// Only `vantage` is rendered: it is the one that changes what you should believe. `reach` rides in
// the tooltip, because the difference between entering a box and reading a file off the volume is
// what someone debugging a slow or stale pane needs and nobody else does.
const VANTAGE_LABEL = { store: "last turn end", host: "host clone" };
function provenance(a, extra) {
  const bits = (extra || []).slice();
  if (a && a.vantage && a.vantage !== "box") {
    // "written at the last turn end" reassures if that was a minute ago and misleads if it was
    // yesterday, so the age goes in the same breath as the claim.
    const when = a.as_of ? ` (${esc(a.as_of)})` : "";
    bits.push(esc(a.note || VANTAGE_LABEL[a.vantage] || a.vantage) + when);
  }
  if (!bits.length) return "";
  const stale = a && a.vantage && a.vantage !== "box";
  const how = a && a.reach ? ` title="reached by ${esc(a.reach)}"` : "";
  return `<div class="dbase${stale ? " stale" : ""}"${how}>${bits.join(" · ")}</div>`;
}
// A diff whose base you can't see is a number you can't act on, so the base rides along with it.
function diffBanner(d) {
  if (!d || !d.patch || !d.patch.trim()) return "";
  const base = d.base && d.base !== "HEAD"
    ? [`vs <code>${esc(d.base)}</code>`]
    : d.base === "HEAD" ? ["uncommitted changes only — no remote base branch resolved"] : [];
  return provenance(d, base);
}
function renderDiff(txt) {
  if (!txt || !txt.trim() || txt.startsWith("# no diff"))
    return `<div class="diff"><div class="note">${esc((txt||"").replace(/^#\s*/,"")) || "no changes reported yet"}</div></div>`;
  let curFile = "", newLine = 0;
  const lines = txt.split("\n").map(l => {
    let cls = "", file = "", line = "";
    if (l.startsWith("+++")) { cls = "meta"; const m = l.match(/^\+\+\+ b\/(.*)$/); if (m) curFile = m[1]; }
    else if (l.startsWith("@@")) { cls = "hunk"; const m = l.match(/\+(\d+)/); if (m) newLine = +m[1]; }
    else if (l.startsWith("---") || l.startsWith("diff ") || l.startsWith("index ")
             || l.startsWith("new file") || l.startsWith("deleted") || l.startsWith("rename") || l.startsWith("similarity")) cls = "meta";
    else if (l.startsWith("+")) { cls = "add"; file = curFile; line = newLine; newLine++; }
    else if (l.startsWith("-")) { cls = "del"; file = curFile; line = newLine; }
    else { file = curFile; line = newLine; newLine++; }   // context
    const attrs = file ? ` data-file="${esc(file)}" data-line="${line}"` : "";
    const ce = file ? " cmtable" : "";
    return `<span class="ln ${cls}${ce}"${attrs}>${esc(l) || "&nbsp;"}</span>`;
  }).join("");
  return `<div class="diff">${lines}</div>`;
}

// ---------- inline review comments → back to the agent ----------
const comments = new Map();   // box -> [{file, line, text}]
diffpane.addEventListener("click", e => {
  const ln = e.target.closest(".ln.cmtable"); if (!ln || !view.box) return;
  // don't double-open a composer on the same line
  if (ln.nextElementSibling?.classList?.contains("composer")) return;
  openComposer(ln);
});
function openComposer(ln, existing) {
  closeComposers();
  const file = ln.dataset.file, line = ln.dataset.line;
  const box = document.createElement("div"); box.className = "cmt composer";
  box.innerHTML = `<textarea placeholder="Comment for the agent on ${esc(file)}:${esc(line)}…"></textarea>
    <div class="ca"><span class="ref">${esc(file)}:${esc(line)}</span>
      <button class="cancel">Cancel</button><button class="primary save">Save</button></div>`;
  ln.after(box);
  const ta = box.querySelector("textarea"); ta.value = existing || ""; ta.focus();
  box.querySelector(".cancel").onclick = () => box.remove();
  box.querySelector(".save").onclick = () => { saveComment(view.box, file, +line, ta.value.trim()); box.remove(); renderComments(view.box); };
  ta.addEventListener("keydown", ev => {
    if (ev.key === "Enter" && (ev.metaKey || ev.ctrlKey)) { ev.preventDefault(); box.querySelector(".save").click(); }
    else if (ev.key === "Escape") { ev.preventDefault(); box.remove(); }
    ev.stopPropagation();
  });
}
function closeComposers() { diffpane.querySelectorAll(".cmt.composer").forEach(c => c.remove()); }
function saveComment(box, file, line, text) {
  if (!text) return;
  const arr = comments.get(box) || []; arr.push({ file, line, text }); comments.set(box, arr);
  renderDockbar();
}
function removeComment(box, idx) {
  const arr = comments.get(box) || []; arr.splice(idx, 1); comments.set(box, arr);
  renderComments(box); renderDockbar();
}
function renderComments(box) {
  diffpane.querySelectorAll(".cmt.saved").forEach(c => c.remove());
  const arr = comments.get(box) || [];
  const lns = [...diffpane.querySelectorAll(".ln[data-file]")];
  arr.forEach((c, idx) => {
    const ln = lns.find(e => e.dataset.file === c.file && +e.dataset.line === c.line);
    const block = document.createElement("div"); block.className = "cmt saved";
    block.innerHTML = `<span class="ref">${esc(c.file)}:${c.line}</span><div class="body">${esc(c.text)}</div><span class="rm" title="remove">✕</span>`;
    block.querySelector(".rm").onclick = () => removeComment(box, idx);
    if (ln) ln.after(block); else diffpane.querySelector(".diff")?.prepend(block);
  });
}
function assembleReview(box) {
  const arr = comments.get(box) || []; if (!arr.length) return "";
  const byFile = {}; arr.forEach(c => (byFile[c.file] ??= []).push(c));
  let out = "Here is my review of your current changes. Please address each point:\n";
  for (const f in byFile) { out += `\n${f}:\n`; for (const c of byFile[f]) out += `  - line ${c.line}: ${c.text}\n`; }
  return out + "\n";
}
function sendReview(box) {
  const text = assembleReview(box); if (!text) return;
  comments.set(box, []);
  openTerminal(box);   // switch to (or create) the agent's terminal
  // bracketed paste so multi-line lands as one paste (not line-by-line submits); user presses Enter to send
  const payload = new TextEncoder().encode("\x1b[200~" + text + "\x1b[201~");
  const s = sessions.get(box);
  if (s.ws.readyState === 1) s.ws.send(payload); else s.pending = payload;
  applyView();
}
window.addEventListener("resize", () => { if (view.mode === "term") { activeSession()?.fit.fit(); sendResize(activeSid()); } });

// ---------- mobile: on-screen keys + soft-keyboard sizing ----------
const keybar = document.getElementById("keybar");
const KEYS = { esc:"\x1b", tab:"\t", "c-c":"\x03", up:"\x1b[A", down:"\x1b[B", left:"\x1b[D", right:"\x1b[C" };
let ctrlArmed = false;
function updateCtrlBtn() { keybar.querySelector("[data-k=ctrl]")?.classList.toggle("armed", ctrlArmed); }
keybar.addEventListener("click", e => {
  const b = e.target.closest("button[data-k]"); if (!b) return;
  const k = b.dataset.k, s = activeSession();
  if (k === "ctrl") { ctrlArmed = !ctrlArmed; updateCtrlBtn(); s?.term.focus(); return; }
  if (!s || s.ws.readyState !== 1) return;
  let seq = KEYS[k] ?? k;
  if (ctrlArmed && seq.length === 1) { seq = String.fromCharCode(seq.charCodeAt(0) & 0x1f); ctrlArmed = false; updateCtrlBtn(); }
  s.ws.send(new TextEncoder().encode(seq)); s.term.focus();
});
if (window.visualViewport) {
  const vv = window.visualViewport;
  const onVV = () => {
    document.documentElement.style.setProperty("--vvh", vv.height + "px");
    if (view.mode === "term") { activeSession()?.fit.fit(); sendResize(activeSid()); }
  };
  vv.addEventListener("resize", onVV); vv.addEventListener("scroll", onVV); onVV();
}

// ---------- attention: title / favicon / notifications ----------
// Remembered across loads, exactly as the voice switch is. It was not, and that is most of "alerts
// don't work": the browser permission survives forever, so the bell looked settled, but `alertsOn`
// came back false on every reload and every reopened tab — and it is the flag `notify()` actually
// checks. You turned alerts on once and were never pinged again.
//
// Gated on the live permission too, so revoking it in the browser turns the switch off rather than
// leaving it lit over a channel that silently drops everything.
let alertsOn = false;
try {
  alertsOn = localStorage.getItem("skein.alerts") === "1"
    && "Notification" in window && Notification.permission === "granted";
} catch {}
function setFavicon(count) {
  const c = document.createElement("canvas"); c.width = c.height = 32; const x = c.getContext("2d");
  x.fillStyle = count ? "#f85149" : "#7c7cf0"; const r = 7;
  x.beginPath(); x.moveTo(2+r,2); x.arcTo(30,2,30,30,r); x.arcTo(30,30,2,30,r); x.arcTo(2,30,2,2,r); x.arcTo(2,2,30,2,r); x.fill();
  x.fillStyle = "#fff"; x.textAlign = "center"; x.textBaseline = "middle";
  x.font = "bold " + (count > 9 ? 15 : 19) + "px " + getComputedStyle(document.body).fontFamily;
  x.fillText(count ? String(count) : "s", 16, 17);
  let link = document.querySelector("link[rel=icon]"); if (!link) { link = document.createElement("link"); link.rel = "icon"; document.head.append(link); }
  link.href = c.toDataURL("image/png");
}
let audioCtx;
function beep() {
  try {
    audioCtx ??= new (window.AudioContext || window.webkitAudioContext)();
    const o = audioCtx.createOscillator(), g = audioCtx.createGain(); o.type = "sine"; o.frequency.value = 660;
    o.connect(g); g.connect(audioCtx.destination);
    g.gain.setValueAtTime(.0001, audioCtx.currentTime);
    g.gain.exponentialRampToValueAtTime(.12, audioCtx.currentTime + .02);
    g.gain.exponentialRampToValueAtTime(.0001, audioCtx.currentTime + .35);
    o.start(); o.stop(audioCtx.currentTime + .36);
  } catch {}
}
