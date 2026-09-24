// ---------- model ----------
const esc = s => String(s).replace(/[&<>"']/g, c => ({ "&":"&amp;","<":"&lt;",">":"&gt;","\"":"&quot;","'":"&#39;" }[c]));
// **An href needs more than `esc`, and this is the helper that gives it more** (SKEIN-602).
//
// `esc` encodes `& < > " '` and nothing else, which is exactly right for TEXT and not enough for a
// URL: `javascript:alert(1)` contains none of those characters and survives it byte for byte. Every
// URL this page renders arrives from the GitHub API — a check's `detailsUrl`/`targetUrl` is written
// by whoever configured the check, and a PR's own URL by whoever the API answered about — so
// "it comes from GitHub" is not the same as "we wrote it".
//
// `safeHref` is the judgement and already existed for markdown links; this is the same judgement at
// the eight places that build an anchor by hand. **The link degrades to its own text** rather than
// to a dead `href`: a link that silently does nothing reads as the cockpit being broken, and the
// sentence somebody wrote is still worth showing.
//
// The NORMALISED string is what goes in the attribute, never the one that came in — `safeHref`
// judges the string the browser's URL parser will get, and echoing the original would judge one
// string while the browser parsed another. That is the shape of both bypasses this guard has had.
const link = (raw, inner, attrs = 'target="_blank" rel="noopener"') => {
  const safe = safeHref(raw, document.baseURI);
  return safe === null ? inner : `<a ${attrs} href="${esc(safe)}">${inner}</a>`;
};

// A STRING ARGUMENT inside an `onclick="…"` is `esc(JSON.stringify(x))`, never a bare
// JSON.stringify and never `'${esc(x)}'`.
//
// Bare JSON.stringify breaks the markup: its quotes are the attribute's own delimiter, so the
// handler ends at the opening one and the remainder is parsed as further attributes — a click that
// throws SyntaxError and a button that looks fine. Three of them shipped that way. Escaped, the
// parser hands the decoded `"…"` back to the JS, which is what the double quotes were for.
//
// Quoting it by hand — `f('${esc(x)}')` — breaks something quieter and worse, and the `'` in `esc`
// above does NOT save it. An attribute's value is entity-decoded *before* the JS parser reads it,
// so `&#39;` arrives at the parser as `'` and closes the string: measured in Chromium, a button
// built as `onclick="f('${esc(x)}')"` with `x` = `x'+(window.PWNED=1)+'y` runs the payload, and the
// same button built with `esc(JSON.stringify(x))` receives the text unharmed. Fifty-two arguments
// on fifty lines were written the first way. `writeModule` took a `moduledocs::Module::path`
// — a CODEOWNERS pattern or a top-level directory name of the repository under review, which passes
// no `valid_name` on the way here (`src/moduledocs.rs`) — so landing a directory on a base branch
// was enough to reach one. `esc` encodes `'` for the single-quoted attributes elsewhere on the page.
// coerce to integers — these land in innerHTML, so never interpolate raw box-reported values
const diffBadge = d => d && (d.files||d.ins||d.del)
  ? `<span class="pos">+${d.ins|0}</span> <span class="neg">−${d.del|0}</span>${d.files?` · ${d.files|0}f`:""}` : "";

let boxes = [], order = [], sel = null, prevState = {};
// When each row was last *observed*, by name. The server sends `age_secs` — the age at the moment
// of the observation — and this is the other half of the sum: how long ago that was. Set when a row
// arrives, so a row that has not changed keeps the moment it was last seen rather than being
// treated as fresh on every re-render.
const receivedAt = new Map();
// Re-read the two numbers already on the row and rewrite one string. No traffic, no layout beyond
// the text, and it runs whether or not the stream is saying anything — which is the case it exists
// for.
function tickAges() {
  for (const el of document.querySelectorAll(".age")) {
    const raw = el.dataset.secs;
    const secs = raw === "" || raw === undefined ? null : Number(raw);
    const seen = Number(el.dataset.seen || Date.now());
    el.textContent = ageNow(secs, Date.now() - seen);
  }
}
setInterval(tickAges, 1000);
let runtimes = [];
const runtimeInfo = id => runtimes.find(runtime => runtime.id === id) || runtimes[0] || { id:id || "", label:id || "Agent" };
const runtimeLabel = id => runtimeInfo(id).label;
const nextRuntime = id => {
  if (runtimes.length < 2) return null;
  const index = Math.max(0, runtimes.findIndex(runtime => runtime.id === id));
  return runtimes[(index + 1) % runtimes.length];
};
const stateOf = name => boxes.find(b => b.name === name)?.state || "unknown";
const agentOf = name => boxes.find(b => b.name === name)?.agent
  || repos.find(r => r.id === boxes.find(b => b.name === name)?.repo)?.agent
  || settings.default_agent || runtimes[0]?.id || "";
// Demo mode (?demo): seed realistic data and short-circuit the network so the cockpit can be opened
// and design-reviewed with no server / no sbx. Purely a preview switch — no effect on the real app.
const DEMO = new URLSearchParams(location.search).has("demo");

// Every API call carries the fleet's token, so a page opened WITHOUT one gets a 401 from all of them
// — and left alone that is a board stuck on "reconnecting…" for ever. Measured on a real first run:
// the visitor sees an empty fleet, four 401s in a console they will never open, and nothing anywhere
// saying a token exists. That is exactly the state a teammate lands in when they are handed the bare
// URL or bookmark `127.0.0.1:7878`, so it is the first impression the whole cockpit gets to make.
//
// Caught once here rather than at each of the ~80 call sites: a refusal is a property of the page's
// credentials, not of whichever request happened to notice first, and every caller's own error path
// stays exactly as it was.
const nativeFetch = window.fetch.bind(window);
window.fetch = (...args) => nativeFetch(...args).then(res => {
  if (res.status === 401) sayUnauthorised();
  return res;
});
// Deliberately a wall, not a toast: nothing on the board is real without a token, and a dismissible
// note over a fleet that reads as empty is worse than no note — it says "something is wrong" while
// the page keeps insisting you have no boxes.
let saidUnauthorised = false;
function sayUnauthorised() {
  if (saidUnauthorised) return;
  saidUnauthorised = true;
  const el = document.createElement("div");
  el.id = "noauth";
  el.innerHTML = `<div class="noauth-card">`
    + `<div class="noauth-t">This cockpit needs the fleet's token</div>`
    + `<div class="noauth-b">Not a login — one shared secret that keeps the fleet off the network`
    + ` it is reachable on. The page loads without it; nothing behind it does.</div>`
    + `<div class="noauth-h">Open the URL <b>skein-server printed on startup</b> — it carries the`
    + ` token, and your browser keeps it in a cookie afterwards, so this is a one-time step:</div>`
    + `<pre class="noauth-c">skein-server → http://127.0.0.1:7878/?t=&lt;token&gt;</pre>`
    + `<div class="noauth-h">Lost that line? On the host it is in the file itself:</div>`
    + `<pre class="noauth-c">open "http://127.0.0.1:7878/?t=$(cat ~/.skein/api-token)"</pre>`
    + `<div class="noauth-b">Sharing a fleet with someone else means sharing that token — it is the`
    + ` whole of the access. <a href="?demo">Or look around in demo mode</a>, which needs none.</div>`
    + `</div>`;
  document.body.append(el);
}

// ---------- fleet render (keyed reconcile) ----------
// Hybrid layout: a pinned "needs you" section across ALL repos (the attention inbox), then a
// collapsible section per repo (its boxes, sorted by attention). Sections are dynamic (depend on
// which repos exist), so they're reconciled by id alongside the keyed rows.
const fleet = document.getElementById("fleet");
const rowEls = new Map(), sectionEls = new Map();
// A fleet is sold as many boxes and the board was tuned for about seven. Two things follow at
// twenty: you cannot find one by eye, and the pinned zone stops pinning — measured at 20 boxes it
// held 11 of them, ~1100px, so you scrolled *through* the pin.
let boardFilter = "";
const PIN_CAP = 5;
let pinExpanded = false;
// Name, branch, repo and the headline — the four things you would actually be looking for. Not
// state: `/error` should find a box called error, and the group headers already sort by state.
// `foreign:` — the one filter term that changes WHICH boxes are eligible rather than narrowing them.
//
// A sandbox skein did not place is on the board only because `sbx ls` is authoritative for what
// exists and cannot say which sandboxes are skein's. Left visible, a machine with a couple of
// unrelated `sbx` boxes looked like a fleet full of broken ones: no branch, no signals, no attach.
// So they are out of the way by default and typing `foreign:` shows exactly them — nothing is
// hidden without a way back to it, and the way back costs no permanent chrome.
// **Fetched, not pushed.** These rows used to ride on every 2s tick, which cost one `sbx ls` per
// tick per open tab — the single most expensive thing the board did, for the one part of it that is
// not about skein's own boxes. `sbx ls` is the right instrument for "what sandboxes are on this
// machine" and the wrong one for "which boxes exist", so it became a question somebody asks.
//
// `null` means never asked, `[]` means asked and there is nothing, and `foreignError` means asked
// and could not be told. All three render differently, because "no other sandboxes" and "sbx did not
// answer" are different answers and collapsing them is how a board lies quietly.
let foreignRows = null, foreignError = "", foreignAsking = false;
async function askForForeign() {
  if (foreignAsking) return;
  foreignAsking = true;
  try {
    const r = await fetch("/api/machine/sandboxes");
    const body = await r.json().catch(() => null);
    if (r.ok && Array.isArray(body)) { foreignRows = body.map(asRow); foreignError = ""; }
    else { foreignRows = []; foreignError = (body && body.error) || `sbx could not be asked (${r.status})`; }
  } catch (e) {
    foreignRows = [];
    foreignError = String(e);
  }
  foreignAsking = false;
  render(boxes);
}
// A sandbox drawn in a board built for boxes. The adapter is HERE and not on the server, which is
// the whole of parity.md §7's point: the server says what a sandbox is — a name, a run state, and
// whether it is a skein fleet — and does not hand back a box with an empty branch and no signals,
// which is what made these rows read as a fleet full of broken ones.
//
// Ours is excluded: it is not "something else on this machine", it IS this machine's fleet, and its
// boxes are already the board.
const asRow = s => ({
  name: s.name,
  foreign: true,
  // Never skein's job: a sandbox skein did not place cannot have a purpose skein recorded, and
  // leaving this absent would let `managed:` narrow on `undefined` instead of on an answer.
  managed: false,
  state: s.running === true ? "live" : s.running === false ? "stale" : "unknown",
  headline: s.ours ? "this fleet" : s.skein_fleet ? "another skein fleet" : "not skein's",
  branch: "", repo: "", dir: "", tier: 5,
});
// Set the board filter from a link, keeping the input and the state in step — typing it by hand and
// clicking "show them" have to mean the same thing, and the input is what a person edits next.
function setFilter(q) {
  // Entering foreign mode re-asks: the answer is a snapshot of the machine, and one taken when the
  // tab was opened an hour ago is not what somebody typing `foreign:` now is asking about.
  if (wantsForeign((q || "").toLowerCase()) && !wantsForeign((boardFilter || "").toLowerCase())) {
    foreignRows = null;
  }
  boardFilter = q;
  const el = document.getElementById("ffilter");
  if (el) el.value = q;
  // Opened, not just set: a filter arriving from a link with the box still collapsed would narrow the
  // board with nothing on screen to say why.
  openFilter();
  render(boxes);
}
const NEEDS_TIER = 1;   // tier <= 1 (needs-input, waiting) → the pinned "needs you" section
// Collapse state per section id, persisted so it survives reloads.
let collapsed = new Set();
try { collapsed = new Set(JSON.parse(localStorage.getItem("skein.collapsed") || "[]")); } catch {}
const isCollapsed = id => collapsed.has(id);
function toggleCollapse(id) {
  collapsed.has(id) ? collapsed.delete(id) : collapsed.add(id);
  try { localStorage.setItem("skein.collapsed", JSON.stringify([...collapsed])); } catch {}
  render(boxes);
}
// Create/fetch a section shell (header with caret + label + count, optional per-repo ＋, rows box).
function ensureSection(id, repoId) {
  let s = sectionEls.get(id);
  if (!s) {
    const section = document.createElement("div"); section.className = "group"; section.dataset.sid = id;
    const head = document.createElement("div"); head.className = "ghead";
    head.innerHTML = `<span class="gcaret"></span><span class="glabel"></span><span class="gc"></span><span class="gpull" title="Sync to remote (git pull --ff-only)">⟳</span><span class="gadd" title="New box in this repo">＋</span>`;
    const rows = document.createElement("div"); rows.className = "grows";
    section.append(head, rows); fleet.append(section);
    const add = head.querySelector(".gadd"), pull = head.querySelector(".gpull");
    // clicking the header toggles collapse — except on the per-repo action controls.
    head.addEventListener("click", e => { if (!e.target.closest(".gadd, .gpull")) toggleCollapse(id); });
    s = { section, head, label: head.querySelector(".glabel"), count: head.querySelector(".gc"),
          caret: head.querySelector(".gcaret"), rows, add, pull };
    sectionEls.set(id, s);
  }
  if (repoId) {
    s.add.style.display = ""; s.add.onclick = e => { e.stopPropagation(); openNewBoxFor(repoId); };
    s.pull.style.display = ""; s.pull.dataset.id = repoId;
    s.pull.onclick = e => { e.stopPropagation(); pullRepo(s.pull); };
  } else {
    s.add.style.display = "none"; s.pull.style.display = "none";
  }
  return s;
}
function makeRow(name) {
  const el = document.createElement("div"); el.dataset.name = name;
  el.innerHTML = `<span class="dot"></span><div class="main"><div class="name"></div>
    <div class="sub"><span class="repotag"></span><span class="vmtag"></span><span class="mgtag"></span><span class="scopetag"></span><span class="dchip"></span><span class="branch"></span><span class="dstat"></span><span class="dir"></span><span class="hookwarn"></span><span class="covwarn"></span><span class="capwarn"></span></div>
    <div class="headline"><span class="pchip"></span><span class="htext"></span></div>
    <div class="taskline"><span class="ttext"></span></div></div>
    <span class="pill"></span><span class="age"></span><span class="chev">›</span>`;
  el.addEventListener("click", e => {
    // the "proceed?" chip is the action itself — one click continues the box, no terminal trip
    if (e.target.classList.contains("pchip") && e.target.classList.contains("proceed")) { resumeBox(name); return; }
    if (e.target.classList.contains("hookwarn") && e.target.dataset.restart === "1") { restartAgent(name); return; }
    if (e.target.classList.contains("dchip")) { askDiskLimit(name); return; }
    // the check chip is a link to what the check actually said — a red tick you can't read is noise
    openTerminal(name);
  });
  // mousemove, not mouseenter: rows shift under a stationary cursor every tick, and mouseenter on
  // the row that arrived under it yanked the j/k keyboard selection to wherever the mouse rested
  el.addEventListener("mousemove", () => { select(name, true); armResourcePop(name, el); });
  el.addEventListener("mouseleave", hideResourcePop);
  return el;
}
// One-click continue for a single box paused on a trivial "proceed?" — the per-box sibling of the
// header's batch ▸ Continue N (same endpoint the batch flow uses, same never-silent philosophy:
// it's always your click).
function resumeBox(name) {
  toast(`continuing ${name}…`);
  fetch(`/api/boxes/${encodeURIComponent(name)}/resume`, { method:"POST" }).then(r => r.json()).then(d => {
    if (d.ok) { toast(`continued ${name}`); refreshRow(name); }
    else toast("continue failed: " + (d.error || "").split("\n")[0]);
  }).catch(() => toast("continue failed"));
}
function restartAgent(name) {
  const runtime = activeSession()?.box === name ? activeSession().runtime : agentOf(name);
  toast(`restarting ${runtimeLabel(runtime)} in ${name}…`);
  fetch(`/api/boxes/${encodeURIComponent(name)}/restart-agent`, {
    method:"POST", headers:{"Content-Type":"application/json"}, body:JSON.stringify({ runtime })
  }).then(r => r.json()).then(result => {
    if (!result.ok) throw new Error(result.error || "restart failed");
    const id = sidOf(name, "agent"), session = sessions.get(id);
    if (session) { session.dead = true; reconnectSession(name, "agent"); }
    else openTerminal(name, runtime);
    toast(`restarted ${runtimeLabel(runtime)} in ${name}`);
  }).catch(error => toast(`restart failed: ${error.message}`));
}
function updateRow(el, b, showRepo) {
  const g = groupOf(b.state);
  const wired = boxHasSession(b.name);
  el.className = `row s-${g}` + (b.name === sel ? " sel" : "") + (wired ? " wired" : "")
    + (b.name === view.box ? " active" : "") + (boxAnyStreaming(b.name) ? " streaming" : "");
  el.querySelector(".name").textContent = b.name;
  // repo tag: shown only in the cross-repo "needs you" section (in a repo section the header already
  // names the repo, so it'd be noise). Caller passes showRepo.
  const rt = el.querySelector(".repotag");
  const tag = (showRepo && b.repo) ? b.repo : "";
  rt.textContent = tag;
  rt.style.display = tag ? "" : "none";
  // The foreign tag. These rows are hidden unless `foreign:` is filtered for, so this labels them
  // once they are deliberately on screen — and says why nothing here works, since a sandbox skein did
  // not build has no checkout, store or tmux contract for the cockpit to attach through.
  const vt = el.querySelector(".vmtag");
  vt.textContent = b.foreign ? "not skein's" : "";
  vt.style.display = b.foreign ? "" : "none";
  vt.title = b.foreign
    ? "skein did not create this sandbox, so it cannot attach to it, read its work, or manage it. Reach it directly with `sbx exec -it " + b.name + " bash -l`, or register its repo with `skein add` and create the box from here."
    : "";
  // The managed tag. Unlike the foreign one these rows are on the board by default, so this is not
  // "you asked to see it, here is why it is odd" — it is the row saying who asked for it, which is
  // the one thing that distinguishes it from the box beside it that a person made.
  const mg = el.querySelector(".mgtag");
  mg.textContent = b.managed ? "skein's" : "";
  mg.style.display = b.managed ? "" : "none";
  mg.title = b.managed
    ? "skein started this box itself to do a job of its own. It is an ordinary box otherwise — you can open it, read it and stop it like any other."
    : "";
  // Scope tag, and it badges the EXCEPTION for the same reason `legacy` does. `scoped` is absent
  // entirely when the fleet cannot scope anything, so a host with no GitHub App stays unlabelled
  // rather than having every row call itself unscoped.
  const st = el.querySelector(".scopetag");
  const open = b.scoped === false;
  st.textContent = open ? "open" : "";
  st.style.display = open ? "" : "none";
  st.title = open
    ? "this box holds the fleet-wide GitHub credential, not one scoped to its own repo — box settings → github access"
    : "";
  diskChip(el.querySelector(".dchip"), b);
  el.querySelector(".branch").textContent = b.branch || "—";
  el.querySelector(".dir").textContent = b.dir || "";
  // The state pill, plus a `◐` when only half the signal produced it (hook edges, no screen). One
  // glyph rather than a sentence: on a fleet of older boxes this is true of every row, and it is a
  // caveat on how the state was derived, not a problem with the box.
  const pill = el.querySelector(".pill"),
        half = screenHalf(b);
  const pillHtml = esc(labelOf(b.state)) + (half ? ` <i class="hdot"></i>` : "");
  if (pill.dataset.html !== pillHtml) { pill.dataset.html = pillHtml; pill.innerHTML = pillHtml; }
  pill.title = half ? `${half[0]} — ${half[1]}\n\n${half[2]}` : "";
  // Aged from the observation rather than from the string the server formatted. The stream no
  // longer re-sends a box that only got older — a box's age moves every second whether or not
  // anything happened to it — so a server-formatted string would freeze between real changes and a
  // fleet quiet for ten minutes would say "2m ago" for ever. `.dataset` carries the two numbers the
  // timer needs, so ageing a row costs no traffic and no re-render of anything else.
  const age = el.querySelector(".age");
  age.dataset.secs = b.age_secs === null || b.age_secs === undefined ? "" : String(b.age_secs);
  age.dataset.seen = String(receivedAt.get(b.name) || Date.now());
  age.textContent = ageNow(b.age_secs, Date.now() - (receivedAt.get(b.name) || Date.now()));
  el.querySelector(".chev").textContent = wired ? "▣" : "›";
  el.querySelector(".dstat").innerHTML = diffBadge(b.diff);
  // hook-health: a Running box whose probes are not reporting normally — every other signal on this
  // row is then a lie of omission, so say so instead of impersonating a quiet box. Drawn through
  // `hookHalf`, so a value this page has not learned says so rather than rendering nothing.
  const hw = el.querySelector(".hookwarn");
  const hh = hookHalf(b);
  hw.textContent = hh ? hh[0] : "";
  hw.style.display = hh ? "" : "none";
  hw.dataset.restart = hh && hh[2] ? "1" : "0";
  hw.title = hh ? hh[1] : "";
  // Which isolation this box actually has. It is invisible from every other angle — an uncovered
  // box looks exactly like a covered one on this row and from inside itself — and it can only
  // change by restarting, because the cover is built at box start and the box keeps the namespace
  // it was born with. No click handler on purpose: a restart costs the agent's unfinished work.
  const cw = el.querySelector(".covwarn");
  const older = b.cover === "older";
  cw.textContent = older ? "⚠ older isolation" : "";
  cw.style.display = older ? "" : "none";
  cw.title = older
    ? "this box started before the isolation skein installs now, and keeps the namespace it was "
      + "born with until it is restarted — `skein restart " + b.name + "` rebuilds it with the "
      + "current covers, leaving the checkout and branch alone"
    : "";
  // What bounds this box's memory, which was invisible until the launcher started reporting it:
  // the ceiling lives in a cgroup inside the sandbox and the record of it in a file the host cannot
  // read. An uncapped box looks exactly like a capped one, and it is the one whose runaway build
  // takes the sandbox with it.
  const pw = el.querySelector(".capwarn");
  const uncapped = b.ceiling && !b.ceiling.startsWith("capped");
  pw.textContent = uncapped ? "⚠ no ceiling" : "";
  pw.style.display = uncapped ? "" : "none";
  pw.title = uncapped
    ? "nothing bounds this box's memory (" + b.ceiling + "), so a runaway build in it reaches the "
      + "whole sandbox rather than being killed inside the box"
    : "";
  // inbox headline + pause chip: the ask when a box needs you. When nothing's owed, the headline
  // falls back to the box's current task — so a working tab still shows what it's doing (peripheral
  // awareness). The task also gets its own sub-line when there's a distinct ask above it, so a real
  // question shows both "what it asked" and "what it was doing".
  // The chip prefers `blocked_kind` — what the box's screen actually shows — over the pause the
  // fork-detector guessed from the headline text. Each kind asks something different of you:
  // approve a tool, answer a question, trust a folder, or go fix auth/quota.
  const p = b.pause, chip = KCHIP[b.blocked_kind] || PCHIP[p];
  // error/ended carry their reason in the headline (the backend put it there); show it even though
  // they have no pause chip, so the row reads e.g. "API error: rate limit".
  const outcome = b.state === "error" || b.state === "ended";
  const pauseHead = ((p && p !== "none") || outcome) ? b.headline : "";
  const task = b.task || "";
  const hl = pauseHead || task;
  el.classList.toggle("has-head", !!hl);
  el.classList.toggle("live-head", !pauseHead && !!task && hl === task);
  el.querySelector(".htext").textContent = hl || "";
  const showTask = task && task !== hl;
  el.classList.toggle("has-task", !!showTask);
  el.querySelector(".ttext").textContent = showTask ? task : "";
  const pc = el.querySelector(".pchip");
  // A screen-observed block is worth showing even with no headline: a trust prompt or an expired
  // login has no "last message" at all, and those are exactly the boxes that used to look fine.
  const showChip = !!(chip && (pauseHead || b.blocked_kind));
  pc.className = "pchip" + (showChip ? " " + (b.blocked_kind ? "k-" + b.blocked_kind : p) : "");
  pc.textContent = showChip ? chip : "";
  pc.style.display = showChip ? "" : "none";
  pc.title = showChip ? (KTIP[b.blocked_kind] || (p === "proceed" ? "click to continue this box" : "")) : "";
  // dedupe: the actionable chip (decision / proceed?) already names what's owed, so drop the
  // right-side state word on those rows — no printing "decision" twice.
  el.querySelector(".pill").style.display = showChip ? "none" : "";
}
// pause → row-chip label (the fork-detector's verdict; statement/none get no chip)
const PCHIP = { "needs-input":"decision", fork:"asks", proceed:"proceed?" };
// blocked_kind → chip label + tooltip. Observed from the box's screen rather than inferred, so these
// win over PCHIP above; `trust` and `auth` are states no hook can report at all.
const KCHIP = { permission:"decision", question:"asks", trust:"trust?", auth:"sign in" };
const KTIP = {
  permission: "a tool is waiting for your approval",
  question:   "it asked you something and is waiting",
  trust:      "it can't start until you trust this folder",
  auth:       "signed out or out of quota — run `skein login claude` on the host; every box inherits it",
};
// Full-board re-render, fired on each 2s SSE "boxes" tick.
function render(snapshot) {
  boxes = snapshot.slice().sort((a,b) => (a.tier ?? 9) - (b.tier ?? 9) || a.name.localeCompare(b.name));
  // `boxes` is every sandbox `sbx ls` knows; `mine` is the ones skein placed. Everything a person
  // reads as "the fleet" — the counts, the empty state, the ▸ Continue batch — is about `mine`.
  const mine = boxes.filter(b => !b.foreign);
  const foreignN = boxes.length - mine.length;
  // Counted, not subtracted out. A managed box is one of `mine` — skein's own box, spending skein's
  // model calls — so it belongs in the fleet total and in "need you"; the segment only says how many
  // of that total skein started for itself, which is the question the count cannot otherwise answer.
  const managedN = mine.filter(b => b.managed).length;
  const counts = {};
  for (const b of mine) (counts[groupOf(b.state)] ??= []).push(b);
  // "need you" includes error: a box that broke is the thing you most need to know about
  const need = (counts.error?.length||0) + (counts.attn?.length||0) + (counts.waiting?.length||0), working = counts.working?.length||0, done = counts.done?.length||0;
  document.getElementById("counts").innerHTML =
    `<span class="seg"><span class="n">${mine.length}</span> boxes</span>`
    + (boardFilter.trim() ? `<span class="seg filt">filtered</span>` : "")
    + (managedN ? `<span class="seg"><span class="n">${managedN}</span> skein's</span>` : "")
    + (need ? `<span class="seg"><span class="pip" style="background:var(--attn)"></span><span class="n">${need}</span> need you</span>` : "")
    + (working ? `<span class="seg"><span class="pip" style="background:var(--working)"></span><span class="n">${working}</span> working</span>` : "")
    + (done ? `<span class="seg"><span class="pip" style="background:var(--done)"></span><span class="n">${done}</span> done</span>` : "");

  // batch-resolve button: count the boxes paused on a trivial "proceed?" (the fork-detector's verdict)
  const proceedN = mine.filter(b => b.pause === "proceed").length;
  const ca = document.getElementById("contall");
  ca.style.display = proceedN ? "" : "none";
  ca.textContent = `▸ Continue ${proceedN}`;
  ca.title = `Resume ${proceedN} box${proceedN === 1 ? "" : "es"} paused on a trivial "proceed?" (one gesture, not silent)`;

  fleet.querySelector(".empty")?.remove();
  // No boxes of skein's ⇒ the first run, even on a machine with other sandboxes running. Those used
  // to fill the board instead, so the one screen that says what to do next never appeared.
  if (!mine.length && !wantsForeign(boardFilter.trim().toLowerCase())) {
    for (const [, s] of sectionEls) s.section.style.display = "none";
    const e = document.createElement("div"); e.className = "empty";
    e.innerHTML = firstRunHtml(foreignN);
    fleet.append(e); order = [];
    renderTabs(); notify([]);   // clear stale title/favicon/tabs when the fleet drains
    return;
  }

  // Build the desired sections: pinned "needs you" (tier ≤ 1, across repos), then one per repo
  // (its remaining boxes), then "other" for boxes matching no managed repo. Empty managed repos are
  // still shown so you can launch into them.
  // The filter narrows what is DRAWN and nothing else: counts, the tab title, the voice and the
  // away digest all still speak for the whole fleet. A filter that also silenced the announcements
  // would be a way to hide a box that needs you and never hear about it.
  const q = boardFilter.trim().toLowerCase();
  // Asked the moment somebody wants them, and only then.
  if (wantsForeign((boardFilter || "").trim().toLowerCase()) && foreignRows === null) askForForeign();
  // `foreignRows` is an argument rather than something the function reaches for: that is what makes
  // it pure, and what makes it testable in node.
  const shown = boardRows(boxes, boardFilter, foreignRows);
  const needyAll = shown.filter(b => (b.tier ?? 9) <= NEEDS_TIER);
  const rest  = shown.filter(b => (b.tier ?? 9) >  NEEDS_TIER);
  // A pin holding more than a handful has stopped being a pin. The rest are one click away, and the
  // header still counts all of them, so nothing is hidden — only deferred.
  const pinHidden = pinExpanded ? 0 : Math.max(0, needyAll.length - PIN_CAP);
  const needy = pinHidden ? needyAll.slice(0, PIN_CAP) : needyAll;
  // Grouped apart from the manual boxes, which is what the owner asked for and is the whole of the
  // feature on this side: a board that mixes them makes a person read every row to find out which
  // ones are theirs to answer. NOT hidden — see `BoxView::managed` — so this is a section, not a
  // reveal. The "needs you" pin still wins over it: a managed box asking a question is a question.
  const managedRest = rest.filter(b => b.managed);
  const byRepo = new Map();
  for (const b of rest) { if (b.managed) continue; const k = b.repo || "other"; (byRepo.get(k) || byRepo.set(k, []).get(k)).push(b); }
  const repoIds = new Set([...repos.map(r => r.id), ...byRepo.keys()].filter(k => k !== "other"));
  const desired = [];
  if (needy.length) desired.push({ id:"needs", label:"⚠ needs you", repoId:null, showRepo:true, boxes:needy });
  // Above the repo sections, because "who started this" sorts before "which repo is it in": these
  // rows are not somebody's work in progress and reading them as such is the mistake to prevent.
  if (managedRest.length)
    desired.push({ id:"managed", label:"skein's own", repoId:null, showRepo:true, boxes:managedRest });
  for (const id of [...repoIds].sort((a,b) => a.localeCompare(b)))
    desired.push({ id:`repo:${id}`, label:id, repoId:id, showRepo:false, boxes:byRepo.get(id) || [] });
  if (byRepo.has("other"))
    desired.push({
      id: "other",
      // "unassigned" would be wrong for a sandbox that was never skein's to assign.
      label: wantsForeign(q) ? "not skein's" : repos.length ? "unassigned" : "fleet",
      repoId: null, showRepo: true, boxes: byRepo.get("other"),
    });

  order = []; const seen = new Set();
  const want = new Set(desired.map(d => d.id));
  for (const [id, s] of sectionEls) if (!want.has(id)) { s.section.remove(); sectionEls.delete(id); }
  for (const d of desired) {
    const s = ensureSection(d.id, d.repoId);
    fleet.appendChild(s.section);                 // (re)order to match `desired`
    const col = isCollapsed(d.id);
    s.label.textContent = d.label;
    s.caret.textContent = col ? "▸" : "▾";
    s.count.textContent = d.boxes.length || "";
    s.rows.style.display = col ? "none" : "";
    s.section.style.display = "";
    // "+N more" for the pinned zone, kept as the section's last child so it cannot be mistaken for
    // a box and cannot fall out of order with them.
    let more = s.rows.querySelector(".pinmore");
    if (d.id === "needs" && pinHidden) {
      if (!more) {
        more = document.createElement("button");
        more.type = "button";
        more.className = "pinmore";
        more.onclick = () => { pinExpanded = true; render(boxes); };
        s.rows.append(more);
      }
      more.textContent = `+${pinHidden} more need you`;
    } else if (more) more.remove();
    let pos = 0;
    for (const b of d.boxes) {
      let el = rowEls.get(b.name); if (!el) { el = makeRow(b.name); rowEls.set(b.name, el); }
      el.dataset.showRepo = d.showRepo ? "1" : "";   // remembered so per-row refreshes keep the repo tag
      updateRow(el, b, d.showRepo);
      // Only (re)insert when out of position: appendChild on an already-in-place node still re-inserts
      // it, which restarts CSS animations (the attention ring never completed a pulse), clears text
      // selection, and defeats scroll anchoring — every 2s, board-wide.
      if (s.rows.children[pos] !== el) s.rows.insertBefore(el, s.rows.children[pos] || null);
      pos++;
      if (!col) order.push(b.name);
      seen.add(b.name);
    }
  }
  for (const [name, el] of rowEls) if (!seen.has(name)) { el.remove(); rowEls.delete(name); }
  // Pruned against the FLEET, not against what is drawn. `seen` now means "on screen", and a box
  // hidden by the filter is still very much alive — keying this on `seen` would throw away its
  // unsent review comments the moment you typed a letter that did not match its name.
  const known = new Set(boxes.map(b => b.name));
  for (const k of [...comments.keys()]) if (!known.has(k)) comments.delete(k);
  for (const k in prevState) if (!known.has(k)) delete prevState[k];
  if (sel && !known.has(sel)) sel = null;
  renderTabs();          // tab status follows registry status
  notify(boxes);
  refreshDock();         // the dockbar tracks the viewed box's live state, not just view changes
  // Reopen the tabs from before a reload — and keep trying on later snapshots, because the first
  // one can arrive before the fleet does.
  if (!restored) restoreSessions(); else openPending();
}
function refreshRow(name) { const el = rowEls.get(name), b = boxes.find(x => x.name === name); if (el && b) updateRow(el, b, el.dataset.showRepo === "1"); }

// The filter is live: a fleet you are scanning is a fleet you are scanning now, and a search you
// have to submit is one more key between you and the box you are looking for.
const filterEl = document.getElementById("ffilter");
const fsearchEl = document.getElementById("fsearch");
// Open ⇒ focused. There is no state where the box is expanded and typing goes somewhere else, which
// is the whole failure mode of a search that expands on click: it looks ready and swallows nothing.
function openFilter() {
  fsearchEl?.classList.add("on");
  document.getElementById("fsbtn")?.setAttribute("aria-expanded", "true");
  filterEl?.removeAttribute("tabindex");     // reachable by Tab only once it is open
  filterEl?.focus();
  filterEl?.select();
}
// Closing clears, because a filter you cannot see is a board that has silently lost rows. The one
// exception is `held` below: a filter that is *set* keeps the box open instead of being hidden.
function closeFilter() {
  fsearchEl?.classList.remove("on");
  document.getElementById("fsbtn")?.setAttribute("aria-expanded", "false");
  filterEl?.setAttribute("tabindex", "-1");
  if (filterEl && filterEl.value) { filterEl.value = ""; boardFilter = ""; render(boxes); }
  filterEl?.blur();
  syncFilterChrome();
}
function toggleFilter() { fsearchEl?.classList.contains("on") ? closeFilter() : openFilter(); }
// Keeps "there is a filter set" visible while the box is unfocused. Called after every change to the
// filter's value rather than only on blur, so the state cannot drift from what is being filtered.
function syncFilterChrome() {
  fsearchEl?.classList.toggle("held", !!boardFilter.trim());
}
filterEl?.addEventListener("input", () => {
  boardFilter = filterEl.value; pinExpanded = false; syncFilterChrome(); render(boxes);
});
filterEl?.addEventListener("keydown", e => {
  e.stopPropagation();                       // the board's own j/k must not fire while typing here
  if (e.key === "Escape") closeFilter();
});
// Blur closes it only when it is empty: clicking onto the board with a filter set must not throw the
// filter away, and leaving an empty box open would keep the header wide for nothing.
filterEl?.addEventListener("blur", () => {
  if (!filterEl.value.trim()) closeFilter();
});

// ---------- selection / keyboard ----------
function select(name, soft) {
  if (sel === name) return;
  if (sel) rowEls.get(sel)?.classList.remove("sel");
  sel = name; const el = rowEls.get(name);
  if (el) { el.classList.add("sel"); if (!soft) el.scrollIntoView?.({ block:"nearest" }); }
}
function move(d) { if (!order.length) return; let i = sel ? order.indexOf(sel) : -1; i = (i+d+order.length)%order.length; select(order[i]); }
function nextNeedsYou() {
  const need = owedIn(boxes).map(b => b.name);
  if (!need.length) return; const i = sel ? need.indexOf(sel) : -1; select(need[(i+1)%need.length]);
}

// ---------- dock: persistent terminals + diff viewer ----------
// A box can have two terminals: the "agent" session (claude) and a "shell" session (sbx exec bash).
// Sessions are keyed by a composite id = box (+ a suffix for the shell), while view.box stays the
// registry box name so ship/diff/state lookups are unaffected; view.kind picks which terminal.
const SHELL_SFX = "\u0000shell";   // NUL can't appear in a valid box name → collision-free delimiter
const sessions = new Map();   // sid -> { term, fit, ws, host, box, kind, sid, streaming, timer, dead }
let view = { box: null, mode: "term", kind: "agent" };   // what the dock currently shows
const sidOf = (box, kind) => kind === "shell" ? box + SHELL_SFX : box;
const boxOf = (id) => id.endsWith(SHELL_SFX) ? id.slice(0, -SHELL_SFX.length) : id;
const kindOf = (id) => id.endsWith(SHELL_SFX) ? "shell" : "agent";
const activeSid = () => view.box ? sidOf(view.box, view.kind) : null;
const activeSession = () => { const id = activeSid(); return id ? sessions.get(id) : null; };
const boxHasSession = (box) => sessions.has(box) || sessions.has(box + SHELL_SFX);
const boxAnyStreaming = (box) => ["agent","shell"].some(k => sessions.get(sidOf(box,k))?.streaming);
function closeBoxSessions(box) { for (const k of ["agent","shell"]) { const id = sidOf(box,k); if (sessions.has(id)) closeSession(id); } }
const termarea = document.getElementById("termarea"), diffpane = document.getElementById("diffpane"),
      sesspane = document.getElementById("sesspane"), filespane = document.getElementById("filespane"),
      txpane = document.getElementById("txpane"), revpane = document.getElementById("revpane"),
      tabsEl = document.getElementById("tabs"), dockbar = document.getElementById("dockbar");

function openTerminal(name, runtime) { showBox(name, "term", "agent", runtime || agentOf(name)); }
function takeOver(name, runtime) {
  if (!confirm(`Create a ${runtime} replacement for ${name}?\n\nSkein will snapshot commits, staged/unstaged edits, untracked files, and bounded conversation context. The source box remains intact as rollback.`)) return;
  toast(`snapshotting ${name} and creating its ${runtime} replacement…`);
  fetch(`/api/boxes/${encodeURIComponent(name)}/takeover`, {
    method:"POST", headers:{"content-type":"application/json"}, body:JSON.stringify({target:runtime})
  }).then(async r => {
    const text = await r.text();
    if (!r.ok) throw new Error(text || `HTTP ${r.status}`);
    return JSON.parse(text);
  }).then(replacement => {
    toast(`replacement ready: ${replacement.target}`);
    showBox(replacement.target, "term", "agent", runtime);
  }).catch(error => toast("takeover failed: " + String(error.message || error).split("\n")[0]));
}
function openShell(name) { showBox(name, "term", "shell"); }
function openDiff(name) { showBox(name, "diff"); }
function openSession(name) { showBox(name, "session"); }
function openFiles(name) { showBox(name, "files"); }
function openTranscript(name) { showBox(name, "tx"); }
function showBox(name, mode, kind = "agent", runtime, handoff = false) {
  document.body.classList.remove("show-fleet");   // tapping a box leaves the mobile fleet view
  select(name);
  view = { box: name, mode, kind };
  if (mode === "term") {
    const id = sidOf(name, kind);
    let s = sessions.get(id);
    const previousRuntime = s?.runtime || agentOf(name);
    const want = kind === "agent" ? (runtime || s?.runtime || agentOf(name)) : "shell";
    // One visible agent tab per box, while each provider's process survives in its own tmux session.
    // Replacing the websocket/terminal here does not kill that provider's tmux server-side session.
    if (s && kind === "agent" && s.runtime !== want) {
      clearTimeout(s.timer); clearInterval(s.statusTimer); try { s.ws.close(); } catch {} try { s.term.dispose(); } catch {}
      s.host.remove(); sessions.delete(id); s = null;
    }
    if (!s) createSession(name, kind, null, want, handoff, previousRuntime);
    else if (s.dead) reconnectSession(name, kind);   // clicking a disconnected tab reconnects it
  }
  applyView();
  if (mode === "diff") loadDiff(name);
  if (mode === "session") loadSession(name);
  if (mode === "files") loadFiles(name);
  if (mode === "tx") loadTranscript(name);
}
function applyView() {
  persistView();   // remember open tabs + active view so a page reload restores them
  // Review is repo-scoped, so it is the one view that docks without a box. Leaving `view.repo` out
  // of this test is what would make the pane render into a dock that is still collapsed.
  const docked = sessions.size > 0 || !!view.box || !!view.repo;
  document.body.classList.toggle("docked", docked);
  if (!docked) return;
  const termMode = view.mode === "term";
  document.body.classList.toggle("term-mode", termMode);   // gates the mobile key bar
  termarea.classList.toggle("on", termMode);
  diffpane.classList.toggle("on", view.mode === "diff");
  sesspane.classList.toggle("on", view.mode === "session");
  filespane.classList.toggle("on", view.mode === "files");
  txpane.classList.toggle("on", view.mode === "tx");
  revpane.classList.toggle("on", view.mode === "review");
  const aid = activeSid();
  for (const [id, s] of sessions) s.host.classList.toggle("on", termMode && id === aid);
  if (termMode && aid && sessions.has(aid)) {
    const s = sessions.get(aid);
    requestAnimationFrame(() => { s.fit.fit(); sendResize(aid); s.term.focus(); });
  }
  renderDockbar(); renderTabs(); refreshRowAll();
}
// The close code skein's own end sends (src/bin/skein-server/terminal.rs) when THERE IS NOTHING
// HERE TO RECONNECT TO, rather than the connection having gone away. Agreed with that end and with
// nothing else: 4000-4999 is the range RFC 6455 leaves to an application, and a browser's own close for a
// socket that died is 1006, never a 4xxx.
//
// **It was `CLOSE_CHILD_ENDED`, and the rename is the fix rather than tidying** (SKEIN-702). A
// command that finished was the first thing it covered; a terminal skein REFUSED to open — the PTY
// cap, a box with no placement, a PTY that would not open — is the same answer to the only question
// this page asks the code, which is whether to offer a reconnect. There is no child in a refusal and
// nothing ended, so the old name would have been quietly false the moment one carried it.
const CLOSE_NOTHING_TO_RECONNECT = 4001;
// What to do INSTEAD, read from the first word of the close reason. The code says "do not offer a
// reconnect" and cannot say what to offer in its place, and the three answers are really different:
// two of them are conditions skein can watch and come back from by itself, and the third is skein's
// own failure with nothing anywhere to wait for.
//
// `[title, what is happening, offer the button]` — and the button is the FALLBACK. A pane that is
// watching says what it is watching for and no more, because a control beside a promise to come back
// on its own is an invitation to press something that was not needed.
const RECOVERY = {
  "wait-pty": ["waiting for a terminal to close",
               "This pane reopens on its own the moment a slot comes free — nothing else to do.", false],
  "wait-box": ["waiting for the box",
               "skein is watching the board and reopens this pane on its own once the box is there.", false],
  "no-watch": ["skein could not open this terminal",
               "There is no condition to wait for, so nothing will reopen it by itself.", true],
};
// **`retry`: skein's own failure inside `pump_pty`, tried again on a bounded backoff** (SKEIN-883).
// Not a row in `RECOVERY`, because its strip is not one sentence: while the tries last it says when
// the next one is and offers no button, and after the fifth it offers the button and stops. The
// delays are the owner's (2026-09-23); the fifth ends 223s after the first failure, which the
// exhausted sentence rounds to four minutes. Each try goes through `retryWaiting`, so
// `RETRY_FLOOR_MS` holds for these as for every other automatic reconnect.
const RETRY_BACKOFF_MS = [3000, 10000, 30000, 60000, 120000];
// Per `sid`, like `retriedAt`, because a try replaces the session object and the count must outlive
// it. Cleared when the pane shows it is live (its first PTY bytes) or when somebody presses Try again.
const retryTries = new Map();
const retryTimers = new Map();
function scheduleRetry(s) {
  clearTimeout(retryTimers.get(s.sid));
  const n = retryTries.get(s.sid) || 0;
  if (n >= RETRY_BACKOFF_MS.length) { s.retryAt = 0; return; }
  const wait = RETRY_BACKOFF_MS[n];
  retryTries.set(s.sid, n + 1);
  s.retryTry = n + 1; s.retryAt = Date.now() + wait;
  const fire = () => {
    retryTimers.delete(s.sid);
    if (sessions.get(s.sid) !== s || s.waitFor !== "retry") return;   // closed, or already back
    retryWaiting("retry", s.box, s.kind);
    // Held back by the floor: try again the moment it allows, rather than never.
    if (sessions.get(s.sid) === s && s.waitFor === "retry")
      retryTimers.set(s.sid, setTimeout(fire, Math.max(50, RETRY_FLOOR_MS - (Date.now() - (retriedAt.get(s.sid) || 0)))));
  };
  retryTimers.set(s.sid, setTimeout(fire, wait));
}
function retryStrip(s) {
  if (s.retryAt) {
    const secs = Math.max(0, Math.ceil((s.retryAt - Date.now()) / 1000));
    return ["skein could not open this terminal — trying again",
            `It retries on its own: next try in ${secs}s (${s.retryTry} of ${RETRY_BACKOFF_MS.length}). Nothing in your box is wrong.`, false];
  }
  return ["skein could not open this terminal",
          `It tried ${RETRY_BACKOFF_MS.length} times over 4 minutes. Press Try again once the cause above is fixed.`, true];
}
function createSession(box, kind, launchBranch, runtime, handoff = false, fromRuntime = null) {
  runtime = kind === "agent" ? (runtime || agentOf(box)) : "shell";
  if (DEMO) return createDemoSession(box, kind, runtime);   // no server in demo — show a placeholder pane
  const id = sidOf(box, kind);
  const host = document.createElement("div"); host.className = "thost"; termarea.append(host);
  // "not connected" overlay — shown when the socket closes; clicking it (or the tab) reconnects.
  const disco = document.createElement("div"); disco.className = "disco";
  disco.innerHTML = `<div class="disco-card"><div class="disco-t">session not connected</div><div class="disco-b">click to reconnect</div></div>`;
  disco.addEventListener("click", () => reconnectSession(box, kind));
  host.append(disco);
  // The strip under the terminal for a pane skein closed on purpose — empty and undisplayed until
  // `ws.onclose` reads a reason for it. It is a sibling of the terminal rather than a layer over it
  // (see `.recovering` in the stylesheet).
  const recover = document.createElement("div"); recover.className = "recover"; host.append(recover);
  const adaptedStatus = kind === "agent" && !!runtimeInfo(runtime)?.adapted_statusline;
  const statusEl = document.createElement("div"); statusEl.className = "agent-statusline";
  statusEl.setAttribute("aria-label", "agent usage status"); host.append(statusEl);
  if (adaptedStatus) host.classList.add("adapted-status");
  // cursorBlink is OFF on purpose: a blinking cursor doesn't reset its on/off phase on keypress, so
  // a character that rendered instantly can sit next to a cursor that's mid-blink-off for up to
  // ~300ms, reading as "my typing lagged" even though nothing did. A solid cursor removes that.
  const term = new Terminal({ fontFamily:"ui-monospace,'JetBrains Mono','SF Mono',Menlo,monospace", fontSize:13,
    cursorBlink:false, customGlyphs:true, scrollback:8000,
    theme:{ background:"#08090b", foreground:"#e6edf3", cursor:"#7c7cf0" } });
  const fit = new FitAddon.FitAddon(); term.loadAddon(fit); term.open(host);
  // Keep xterm's built-in canvas renderer. Its custom glyph path draws the box/block characters
  // emitted by tmux-backed agent TUIs pixel-perfectly; WebGL's texture atlas can collapse them into
  // underscore-like bottom strokes on some browser/GPU/font combinations.
  // OSC 52 clipboard: TUIs like Claude Code run with mouse mode on, so a drag is consumed by the app
  // (neither xterm nor the browser sees a selection — that's the x=0/w=0). The app copies by emitting
  // an OSC 52 escape ("sent N chars via OSC 52"); xterm ignores OSC 52 by default, so it never reached
  // the clipboard. Honour it here so "copy on select" in the TUI just works. Payload is "Pc;<base64>"
  // (Pc = clipboard selection; "?" is a read request we decline).
  term.parser.registerOscHandler(52, data => {
    const semi = data.indexOf(";");
    const payload = semi === -1 ? data : data.slice(semi + 1);
    if (!payload || payload === "?") return true;
    try {
      const text = new TextDecoder().decode(Uint8Array.from(atob(payload), c => c.charCodeAt(0)));
      if (!text) return true;
      // The escape arrives async (over the websocket), outside any user gesture, so a direct
      // clipboard write is refused (NotAllowedError, esp. Safari). Stash it and let the Cmd+C
      // gesture commit it; also attempt a best-effort async write for browsers that permit it.
      oscClipboard = text;
      if (navigator.clipboard?.writeText) navigator.clipboard.writeText(text).catch(() => {});
    } catch {}
    return true;
  });
  const proto = location.protocol === "https:" ? "wss" : "ws";
  const params = new URLSearchParams();
  if (launchBranch) params.set("launch", launchBranch);
  if (kind === "shell") params.set("shell", "1");
  if (kind === "agent") params.set("agent", runtime);
  if (handoff) { params.set("handoff", "1"); if (fromRuntime) params.set("from", fromRuntime); }
  const qs = params.toString() ? `?${params}` : "";
  const ws = new WebSocket(`${proto}://${location.host}/api/boxes/${encodeURIComponent(box)}/terminal${qs}`);
  ws.binaryType = "arraybuffer";
  const s = { term, fit, ws, host, statusEl, recover, box, kind, runtime, sid: id,
    streaming:false, timer:0, statusTimer:0, adaptedStatus, waitFor:"" };
  sessions.set(id, s);
  const isActive = () => view.mode === "term" && activeSid() === id;
  ws.onopen = () => {
    if (isActive()) sendResize(id);
    if (s.adaptedStatus) {
      setTimeout(() => refreshAgentStatusline(s), 500);
      s.statusTimer = setInterval(() => refreshAgentStatusline(s), 30000);
    }
    if (s.pending) { ws.send(s.pending); s.pending = null; }
    flushAttach(id);   // a drop made while this terminal was down, delivered now that it is up
    if (handoff && kind === "agent") {
      // Existing provider tmux session: SessionStart may not fire, so submit a real prompt. The
      // UserPromptSubmit handoff hook injects the pending brief before the model sees this request.
      setTimeout(() => {
        if (ws.readyState !== 1) return;
        const p = "Take over this Skein box from the previous agent. Read the injected handoff and current working tree, then continue from the existing state without redoing completed work.";
        ws.send(new TextEncoder().encode("\x1b[200~" + p + "\x1b[201~\r"));
      }, 1200);
    }
  };
  ws.onmessage = e => {
    // PTY bytes arrive binary and a refusal's sentence as text, so the first binary frame is the
    // pane being live — the end of any `retry` run it was in (SKEIN-883).
    if (typeof e.data !== "string") retryTries.delete(id);
    term.write(typeof e.data === "string" ? e.data : new Uint8Array(e.data));
    markStreaming(id);
  };
  // A session that ENDED and a session that was CUT OFF both arrive here, and until the server said
  // which (`CLOSE_NOTHING_TO_RECONNECT`) they were indistinguishable — so the reconnect overlay went
  // up over both. Over a launch whose command failed that panel is 82% opaque over the shell's
  // `not found`, which is the one line explaining the empty box, and it offers to reconnect to a
  // command that has already finished (SKEIN-672). The pane stays `dead` either way — the tab still
  // says so, and a click still reopens it — only the cover comes off.
  //
  // **And a REFUSAL now arrives here too** (SKEIN-702): "too many terminals open", "box … does not
  // exist", a PTY that would not open. Those used to close with no code at all, so the browser read
  // 1006 and this covered them with the same panel. The code takes the cover off; the first word of
  // the reason says what the pane should do instead, and two of the three are conditions skein
  // watches, so the pane waits for one rather than asking to be clicked.
  ws.onclose = e => {
    s.dead = true; s.ended = e.code === CLOSE_NOTHING_TO_RECONNECT;
    s.waitFor = s.ended ? String(e.reason || "").split(" ")[0] : "";
    host.classList.add("dead"); host.classList.toggle("ended", s.ended);
    if (s.waitFor === "retry") scheduleRetry(s);
    showRecovery(s);
    renderTabs(); refreshRow(box);
  };
  term.onData(d => {
    // sticky Ctrl (armed from the mobile key bar): fold the next char to its control code
    if (ctrlArmed && d.length === 1) { d = String.fromCharCode(d.charCodeAt(0) & 0x1f); ctrlArmed = false; updateCtrlBtn(); }
    if (ws.readyState === 1) ws.send(new TextEncoder().encode(d));
  });
  term.onResize(() => { if (isActive()) sendResize(id); });
}
function statuslineHtml(line) {
  const classes = new Set(); let out = "", at = 0;
  const colour = code => { for (const c of ["sl-red","sl-yellow","sl-green"]) classes.delete(c); if (code) classes.add(code); };
  // Keep layout-significant spaces as content, not whitespace-only DOM nodes. Flexbox discards
  // bare whitespace nodes, and an older cached stylesheet can otherwise collapse the whole line.
  const append = text => { if (!text) return; const value = esc(text).replace(/ /g,"&nbsp;"); out += classes.size ? `<span class="${[...classes].join(" ")}">${value}</span>` : value; };
  const sgr = /\x1b\[([0-9;]*)m/g; let match;
  while ((match = sgr.exec(line))) {
    append(line.slice(at, match.index)); at = sgr.lastIndex;
    const codes = (match[1] || "0").split(";").map(Number);
    for (const code of codes) {
      if (code === 0) classes.clear();
      else if (code === 1) classes.add("sl-bold");
      else if (code === 2) classes.add("sl-dim");
      else if (code === 22) { classes.delete("sl-bold"); classes.delete("sl-dim"); }
      else if (code === 31) colour("sl-red");
      else if (code === 32) colour("sl-green");
      else if (code === 33) colour("sl-yellow");
      else if (code === 39) colour(null);
    }
  }
  append(line.slice(at)); return out;
}
function refreshAgentStatusline(s) {
  fetch(`/api/boxes/${encodeURIComponent(s.box)}/statusline`).then(r => r.ok ? r.json() : null).then(data => {
    const line = data?.line || "", active = !!line;
    s.statusEl.innerHTML = active ? statuslineHtml(line) : "";
    if (s.host.classList.contains("adapted-status") !== active) {
      s.host.classList.toggle("adapted-status", active);
      requestAnimationFrame(() => { s.fit.fit(); if (activeSid() === s.sid) sendResize(s.sid); });
    }
  }).catch(() => {});
}
function markStreaming(id) {
  const s = sessions.get(id); if (!s) return;
  // DOM work only on the false→true transition: this runs on EVERY websocket frame, potentially
  // hundreds/sec while an agent streams — unconditional tab/row updates here were the single
  // biggest source of typing latency during the tool's core activity. The timeout handles true→false.
  if (!s.streaming) { s.streaming = true; renderTabFlag(id); refreshRow(s.box); }
  clearTimeout(s.timer); s.timer = setTimeout(() => { s.streaming = false; renderTabFlag(id); refreshRow(s.box); }, 1100);
}
function sendResize(id) { const s = sessions.get(id); if (s && s.ws.readyState === 1) s.ws.send(JSON.stringify({ resize:{ cols:s.term.cols, rows:s.term.rows } })); }
function closeSession(id) {
  const s = sessions.get(id); if (!s) return;
  const box = s.box;
  clearTimeout(s.timer); clearInterval(s.statusTimer); // no callbacks on a disposed session
  try { s.ws.close(); } catch {} try { s.term.dispose(); } catch {}
  const wasAt = orderedSids().indexOf(id);
  s.host.remove(); sessions.delete(id);
  // An undelivered handover does not outlive the tab it was for — it would otherwise arrive out of
  // nowhere on whatever the next terminal for this box turns out to be. The path is said on the way
  // out, because it is the only thing left that still helps.
  const stranded = attachWaiting.get(id);
  if (stranded) { attachWaiting.delete(id); toast("never handed over — it is in the box at " + stranded.text); }
  if (activeSid() === id) {   // closed the active terminal → fall back to the tab that took its place
    const rest = orderedSids();
    const nextId = rest[Math.min(Math.max(wasAt, 0), rest.length - 1)];
    const ns = nextId ? sessions.get(nextId) : null;
    view = ns ? { box: ns.box, mode:"term", kind: ns.kind } : { box:null, mode:"term", kind:"agent" };
  }
  applyView(); refreshRow(box);
}
// Persist the open tabs + active view so a page reload can pick the session back up. The agent
// session lives inside the sandbox (tmux + `claude --continue`), so reattaching just continues it.
function persistView() {
  try {
    // In tab order, not open order: restoreSessions replays this list, so the arrangement survives.
    const open = orderedSessions().map(s => ({ box: s.box, kind: s.kind, runtime: s.runtime }));
    // Tabs still waiting for their box to show up are part of "what was open", and leaving them out
    // is how a delayed restore became a permanent loss: the first snapshot arrived without the
    // fleet in it, nothing was reopened, and the very next persist wrote `open: []` over the only
    // record that those tabs had ever existed.
    for (const p of pendingRestore) {
      if (!open.some(o => o.box === p.box && o.kind === p.kind)) open.push(p);
    }
    // Same for the active view: until it is restored, the saved one is still the truth about where
    // you were, and `view` is merely where the page starts.
    // `view.repo` counts as "somewhere you are": review is the one view with no box, and testing
    // only `view.box` would let a reload drop you back to whatever box was open before it.
    const active = (view.box || view.repo) ? view : (pendingView || view);
    localStorage.setItem("skein.view", JSON.stringify({
      open, box: active.box, mode: active.mode, kind: active.kind, repo: active.repo }));
  } catch {}
}
// On the first fleet snapshot, reopen whatever was open before the reload (filtered to boxes that
// still exist), and reattach — continuing the previous session rather than starting cold.
let restored = false;
// What the last session had open, still waiting for its boxes to appear. Held separately from
// `sessions` because these are not open yet, and folded back into `persistView` so waiting cannot
// destroy them.
let pendingRestore = [];
let pendingView = null;
function restoreSessions() {
  restored = true;
  let saved; try { saved = JSON.parse(localStorage.getItem("skein.view") || "null"); } catch {}
  if (!saved) return;
  pendingRestore = (saved.open || []).map(o => (
    // back-compat: the old format was a bare list of names
    typeof o === "string"
      ? { box: o, kind: "agent", runtime: null }
      : { box: o.box, kind: o.kind === "shell" ? "shell" : "agent", runtime: o.runtime || null }
  )).filter(o => o.box);
  pendingView = saved.box ? { box: saved.box, mode: saved.mode, kind: saved.kind } : null;
  // Review restores immediately rather than through `openPending`: that gate waits for a box to
  // exist in the fleet, and a repo's PR queue does not depend on any box being alive.
  if (!pendingView && saved.mode === "review" && saved.repo) openReview(saved.repo);
  openPending();
}
// Reopen what the fleet can account for, and run on EVERY snapshot rather than only the first.
//
// It used to be first-snapshot-only, which quietly assumed the first snapshot is complete. It is
// not: the server serves the last good fleet list while `sbx ls` is slow or failing, so a reload
// during a stall got an empty one, filtered every saved tab out as "that box no longer exists", and
// never looked again.
//
// An empty snapshot is therefore not evidence of anything — it is the one case that caused the bug —
// so nothing is given up on until the fleet has actually reported. A box missing from a snapshot
// that DOES list boxes is genuinely gone, and that is when its tab is dropped.
function openPending() {
  if (!pendingRestore.length && !pendingView) return;
  if (!boxes.length) return;
  const exist = new Set(boxes.map(b => b.name));
  for (const o of pendingRestore) {
    if (exist.has(o.box) && !sessions.has(sidOf(o.box, o.kind))) {
      createSession(o.box, o.kind, null, o.runtime);
    }
  }
  pendingRestore = [];
  if (pendingView && exist.has(pendingView.box)) {
    const mode = ["diff", "session", "files", "tx"].includes(pendingView.mode) ? pendingView.mode : "term";
    showBox(pendingView.box, mode, pendingView.kind === "shell" ? "shell" : "agent");
  } else {
    applyView();
  }
  pendingView = null;
}
// Reconnect a disconnected session: drop the dead one and open a fresh socket to the same box.
// Reconnecting builds a fresh xterm (rewiring a live one risks double-registering onData, i.e.
// every keystroke sent twice) — so carry the old buffer's text across by hand. Without this a
// server restart or a reload wiped what you had already read: tmux repaints only the *visible*
// pane to a re-attaching client, so nothing refills the gap. Colours are lost, the words are not.
// The durable copy is the Transcript tab; this is just so the terminal doesn't blank.
function carryOverBuffer(s) {
  try {
    const buf = s.term.buffer.active, lines = [];
    for (let i = 0; i < buf.length; i++) lines.push(buf.getLine(i)?.translateToString(true) ?? "");
    while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
    return lines.length ? lines.slice(-2000).join("\r\n") : "";
  } catch { return ""; }
}
// `bring` is whether this reconnect also brings the pane to the front, and it is false for exactly
// one caller: the automatic one (SKEIN-702). Every other call site is a person acting on this pane —
// the overlay, a tab click, a restart, a file drop — and going there is what they asked for. A pane
// that reconnects itself because a terminal slot came free was NOT asked for, and yanking the screen
// to it would be the recovery interrupting the person it recovered for. `applyView` still runs
// either way: the fresh host is hidden until it does.
function reconnectSession(box, kind, bring = true) {
  const id = sidOf(box, kind);
  const s = sessions.get(id);
  const runtime = s?.runtime;
  const carried = s ? carryOverBuffer(s) : "";
  if (s) { clearTimeout(s.timer); clearInterval(s.statusTimer); try { s.ws.close(); } catch {} try { s.term.dispose(); } catch {} s.host.remove(); sessions.delete(id); }
  createSession(box, kind, null, runtime);
  if (carried) {
    const next = sessions.get(sidOf(box, kind));
    next?.term.write(carried + "\r\n\x1b[2m── reconnected ──\x1b[0m\r\n");
  }
  if (bring) view = { box, mode: "term", kind };
  applyView();
}
// The strip under a pane skein closed on purpose: what is happening, and — only where nothing is
// being watched — the control (SKEIN-702).
//
// **The rule this enforces is that a pane cannot do both and cannot do neither.** Either it is
// watching for something specific and says what, or it is watching nothing and offers the button. A
// spinner that waits for nothing is worse than a button, and a button beside a promise to come back
// on its own is an invitation to press something that was not needed.
//
// A reason this build has never heard of falls to the button rather than to silence: an unknown
// token is precisely the case where the page cannot claim to be watching anything.
function showRecovery(s) {
  clearInterval(s.retryTick);
  const known = s.waitFor === "retry" ? retryStrip(s) : RECOVERY[s.waitFor];
  const offer = s.waitFor && s.waitFor !== "child-ended";
  s.host.classList.toggle("recovering", !!offer);
  if (!offer) { s.recover.innerHTML = ""; return; }
  const [title, body, button] = known || RECOVERY["no-watch"];
  s.recover.innerHTML =
    (button || !known ? "" : `<span class="recover-w"></span>`) +
    `<span class="recover-t">${esc(title)}</span><span class="recover-b">${esc(body)}</span>` +
    (button || !known ? `<button class="recover-go">Try again</button>` : "");
  // A press starts a fresh run of tries: it is somebody saying the cause is fixed.
  s.recover.querySelector(".recover-go")?.addEventListener("click", () => {
    retryTries.delete(s.sid); reconnectSession(s.box, s.kind);
  });
  // The countdown in the retry strip, a second at a time, and only while a try is pending.
  if (s.waitFor === "retry" && s.retryAt) s.retryTick = setInterval(() => {
    const b = s.recover.querySelector(".recover-b");
    if (sessions.get(s.sid) !== s || s.waitFor !== "retry" || !b) { clearInterval(s.retryTick); return; }
    b.textContent = retryStrip(s)[1];
  }, 1000);
  // The terminal is 46px shorter now, and xterm sizes itself from the box it was given.
  requestAnimationFrame(() => { try { s.fit.fit(); } catch {} });
}
// The floor under an automatic retry, per pane.
//
// **Because a retry is not free and its trigger is not rate-limited.** A reconnect copies the dead
// terminal's whole buffer into the fresh one so the refusal stays readable, so a pane that retried
// on every board tick for an hour would carry 1,800 copies of its own scrollback. Keyed by `sid`,
// which survives the reconnect the session object does not. Well under the two-second board tick, so
// an ordinary "the box came up" still recovers on the first event that says so.
const RETRY_FLOOR_MS = 3000;
const retriedAt = new Map();
// Every pane waiting for `what`, reconnected without being asked. The wait is cleared FIRST, so a
// refusal that repeats re-arms from the new socket's own close rather than this firing twice for one
// condition.
function retryWaiting(what, box = null, kind = null) {
  const now = Date.now();
  for (const [, s] of [...sessions]) {
    if (s.waitFor !== what) continue;
    if (box !== null && s.box !== box) continue;
    if (kind !== null && s.kind !== kind) continue;
    if (now - (retriedAt.get(s.sid) || 0) < RETRY_FLOOR_MS) continue;
    retriedAt.set(s.sid, now);
    s.waitFor = "";
    reconnectSession(s.box, s.kind, false);
  }
}
// Keep the dockbar in step with the 2s tick: it shows the *viewed* box's dot/branch/diff state,
// which used to render only on explicit view changes — so the box you were actually staring at froze
// while every fleet row updated. Rebuild only when the underlying data changed, so button hover and
// focus aren't nuked every tick.
let dockSig = "";
function refreshDock() {
  if (!view.box) { dockSig = ""; return; }
  const b = boxes.find(x => x.name === view.box);
  const sig = [view.box, view.mode, view.kind, b?.state, b?.branch, JSON.stringify(b?.diff || 0),
    (comments.get(view.box) || []).length].join("|");
  if (sig !== dockSig) { dockSig = sig; renderDockbar(); }
}
// Turn state has two halves (hook edges + the box's own screen). When the screen half isn't
// contributing the board silently falls back to edges alone and looks completely normal — which is
// how an answered decision could sit there for twenty minutes. So say which halves are live.
// `SHALF[kind] = [label, what it means, what to do]`.
const SHALF = {
  none:        ["hooks only",     "no screen observation for this box, so turn state is coming from hook events alone: a dialog you answer may linger until the next event.", "Reattach the box to start its screen observer."],
  stale:       ["screen lost",    "this box's screen observer stopped reporting (its last sample is over 35s old), so turn state has fallen back to hook events alone.", "Reattach the box to restart it."],
  unreadable:  ["screen unread",  "skein does not recognise what is on this box's screen, so turn state has fallen back to hook events alone — usually a change in the agent's TUI.", "The unrecognised sample is saved in the store as <box>.pane.json; worth reporting."],
  unsupported: ["hooks only",     "skein has no screen grammar for this runtime, so turn state uses hook events alone.", "Expected, not a fault."],
  // Different fault and different fix from `unreadable`: that is a TUI this grammar has not seen,
  // this is a skein running behind its own probe. Skipping is deliberate — reading an observation
  // whose fields may have changed meaning is quieter and worse than not reading it.
  newer:       ["probe ahead",     "this box's screen observer is writing a newer format than this skein reads, so its observations are skipped and turn state has fallen back to hook events alone.", "The box is running a probe from a newer skein — restart the box to reinstall the one this build matches, or update skein."],
  // Not `none` with extra words: there IS an observer and it IS writing, it just could not establish
  // which box it is in, so the file under this box's name holds somebody else's turn state. Refusing
  // it is the only safe reading — the recipe differs in the part that matters, so the badge does too.
  misfiled:    ["screen misfiled", "the screen observation stored under this box's name says it is a DIFFERENT box's screen, so it is refused rather than shown as this box's turn state — turn state has fallen back to hook events alone.", "The box's observer could not establish which box it is in. Reattach the box: the attach exports SKEIN_BOX, which is the only thing that knows."],
};
// The case that is not in the map at all, and the one nothing disclosed: the observer is healthy and what you are
// looking at still came from an edge, because a hook event arrived after the last screen sample. It
// is not a fault — showing the event at once is why edges lead — but the state has not been
// confirmed by anything that can see the screen, and the next sample may correct it.
const EDGE_AHEAD = ["unconfirmed", "a hook event arrived after the last screen reading, so this state is the event's and not the screen's — shown at once for latency. The next screen sample confirms or corrects it, usually within ten seconds.", "Nothing to do; it settles on its own."];
// `hook_health`'s map — `screen_health`'s sibling in Rust (`signals::hook_health`), and now its
// sibling here too. `HWARN[kind] = [badge, what it means and what to do, clickable-to-restart]`.
//
// A map with a fallback rather than a chain of `===`, and that is the SKEIN-260 fix rather than a
// third comparison: the chain rendered nothing for any value it had not been taught, and nothing is
// exactly how a healthy box looks. `screen_health` learned this once already (SKEIN-223, and the
// `shUnknown` note below), and `hook_health` was the same shape one signal along, waiting to fail
// the same way. It has now gained a value twice; the next one names itself.
const HWARN = {
  never:    ["⚠ no signals", "this box is running but its probes have never reported — inspect environment health", false],
  stale:    ["↻ update probes", "this session predates the installed probe contract — click to restart its agent session", true],
  // Not a variant of `never`: the signals ARE there, under this box's name, saying they are a
  // different box's — so they are refused. `never` is a box to reattach; this is a store to clean,
  // and the file on disk is somebody else's turn state. Deliberately not click-to-restart, because
  // a restart throws away the agent's work and would not remove the file causing this.
  misfiled: ["⚠ signals misfiled", "a hook signal stored under this box's name says it belongs to a DIFFERENT box, so it is refused rather than shown as this box's turn state. Not a missing probe: the file is in the store and is somebody else's. Remove the misfiled file under <store>/{status,sessions,tasks}/<box>.json, then reattach the box so its probes export SKEIN_BOX.", false],
};
// The lookup, with the same fallback discipline `screenHalf` has. `""` is the healthy answer and
// the only one that draws nothing.
const hookHalf = b => (!b.hook_health ? null : (HWARN[b.hook_health] || [
  "⚠ hooks: " + b.hook_health,
  'this cockpit has no explanation for the hook state "' + b.hook_health + '" that the server reported, so read every other signal on this row as unconfirmed.',
  false,
]));

// A value the server sent that this page has no entry for. `screen_health` gains values in Rust and
// this map learns them afterwards — `misfiled` shipped in 07fa02b and rendered nothing here until
// SKEIN-223 — and "nothing" is not a neutral outcome: a missing badge is precisely how a healthy
// screen looks, so an unlearned state read as confidence skein did not have. That silence is the
// thing this whole row exists to break, so an unrecognised value says so and names itself.
const shUnknown = h => [
  "screen: " + h,
  'this cockpit has no explanation for the screen state "' + h + '" that the server reported, so read the state beside it as unconfirmed by the screen.',
  "The page is probably older than the skein serving it — hard-reload. If it survives that, this build is missing an entry for a state its own server emits; worth reporting.",
];
// The one lookup all three callers share: the dock badge, the fleet row's pill, and the tab title.
// Order matters — an unknown health is still a statement about the screen, so it outranks the
// edge-ahead caveat, which is what you say when the screen half is fine and merely behind.
const screenHalf = b => (SHALF[b.screen_health]
  || (b.screen_health ? shUnknown(b.screen_health) : null)
  || (b.status_from === "edge-ahead" ? EDGE_AHEAD : null));
function screenBadge(b, isShell) {
  // A shell tab has no agent screen to read, so the caveat would be meaningless there.
  const h = !isShell && screenHalf(b);
  if (!h) return "";
  return `<span class="scrw" title="${esc(h[1] + "\n\n" + h[2])}"><i class="hdot"></i>${esc(h[0])}</span>`;
}
function renderDockbar() {
  if (!view.box) { dockbar.innerHTML = ""; return; }
  const b = boxes.find(x => x.name === view.box) || { name: view.box };
  const g = groupOf(b.state || "unknown");
  dockbar.className = "dockbar s-" + g;
  const m = view.mode, isShell = view.kind === "shell";
  const runtime = (!isShell && activeSession()?.runtime) || b.agent || agentOf(view.box);
  const activeRuntimeLabel = runtimeLabel(runtime);
  const hasReview = m === 'diff' && (comments.get(view.box) || []).length;
  dockbar.innerHTML =
      `<button class="kbtn ghost dback" id="dback" title="back to the fleet"><svg class="ic" viewBox="0 0 24 24"><polyline points="15 18 9 12 15 6"/></svg></button>`
    + `<span class="bdot"></span><span class="nm">${esc(view.box)}</span>`
    + (b.branch ? `<span class="br">${esc(b.branch)}</span>` : "")
    + `<span class="dstat">${diffBadge(b.diff)}</span>`
    + screenBadge(b, isShell)
    + `<div class="seg-toggle">
         <button data-m="term" class="${m==='term'&&!isShell?'on':''}">${esc(activeRuntimeLabel)}</button>
         <button data-m="shell" class="${m==='term'&&isShell?'on':''}">Shell</button>
         <button data-m="session" class="${m==='session'?'on':''}">Session</button>
         <button data-m="diff" class="${m==='diff'?'on':''}">Diff</button>
         <button data-m="files" class="${m==='files'?'on':''}">Files</button>
         <button data-m="tx" class="${m==='tx'?'on':''}" title="the conversation as the box's own record has it — survives reboots, restarts and reloads">Transcript</button>
       </div>`
    + `<div class="dock-right">`
    + (m==='term' ? `<button class="kbtn ghost" id="dattach" title="attach files or a folder for the agent to read (or just paste / drag them onto the terminal)"><svg class="ic" viewBox="0 0 24 24"><path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg></button>` : "")
    + (m==='diff' ? `<button class="kbtn ghost" id="dref" title="refresh diff"><svg class="ic" viewBox="0 0 24 24"><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/></svg></button>` : "")
    + `<button class="kbtn ghost" id="dbset" title="this box's own work tracking, committer and disk allowance">⚙ Box settings</button>`
    // Only when the store actually holds something newer. A button that is always there teaches you
    // to ignore it, and this one has nothing to say on most days.
    + (b.docs_update ? `<button class="kbtn ghost" id="drefreshdocs"${refreshingDocs.has(view.box) ? " disabled" : ""} title="the work-tracking rules in the store are newer than the ones this box installed — re-apply them. Only documents the box has not edited are rewritten; shift-click to also take ones installed before skein recorded what it wrote">${refreshingDocs.has(view.box) ? "applying…" : "⟳ Update rules"}</button>` : "")
    + (hasReview
        ? `<button class="kbtn primary" id="dsend" title="paste the review into the agent prompt">Send ${hasReview}</button>` : "")
    + boxRunHtml()
    + `<span class="hdiv"></span>`
    + `<button class="kbtn ghost danger" id="ddestroy" title="destroy — removes this box and reclaims its disk (unpushed commits lost)"><svg class="ic" viewBox="0 0 24 24"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg></button>`
    + `<button class="kbtn ghost" id="dclose" title="close"><svg class="ic" viewBox="0 0 24 24"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg></button>`
    + `</div>`;
  dockbar.querySelector('[data-m=term]').onclick = () => openTerminal(view.box, runtime);
  dockbar.querySelector('[data-m=shell]').onclick = () => openShell(view.box);
  dockbar.querySelector('[data-m=session]').onclick = () => openSession(view.box);
  dockbar.querySelector('[data-m=diff]').onclick = () => openDiff(view.box);
  dockbar.querySelector('[data-m=files]').onclick = () => openFiles(view.box);
  dockbar.querySelector('[data-m=tx]').onclick = () => openTranscript(view.box);
  const rf = dockbar.querySelector("#dref"); if (rf) rf.onclick = () => loadDiff(view.box);
  const bs = dockbar.querySelector("#dbset"); if (bs) bs.onclick = () => openBoxSettings(view.box);
  const rd = dockbar.querySelector("#drefreshdocs"); if (rd) rd.onclick = e => refreshDocs(view.box, e.shiftKey);
  // plain click = files, shift-click = a whole folder (the picker can only do one or the other).
  const at = dockbar.querySelector("#dattach"); if (at) at.onclick = e => pickAttachment(e.shiftKey);
  const sd = dockbar.querySelector("#dsend"); if (sd) sd.onclick = () => sendReview(view.box);
  const restart = dockbar.querySelector("#drestart"); if (restart) restart.onclick = () => restartAgent(view.box);
  const ar = dockbar.querySelector("#dstop"); if (ar) ar.onclick = () => stopBox(view.box);
  const ds = dockbar.querySelector("#ddestroy"); if (ds) ds.onclick = () => destroyBox(view.box);
  const bk = dockbar.querySelector("#dback"); if (bk) bk.onclick = () => document.body.classList.add("show-fleet");
  dockbar.querySelector("#dclose").onclick = () => { const id = activeSid(); if (id && sessions.has(id)) closeSession(id); else { view = {box:null,mode:"term",kind:"agent"}; applyView(); } };
}

