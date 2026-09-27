// ---------- what the fleet costs: Settings → Usage (SKEIN-835) ----------
//
// Three rules came with this pane, and each one is visible below.
//
//   * **Nothing is read on page load.** The fetch happens when the pane is first opened, and only
//     once per page; the board's poll never touches it. A fleet reading walks every box's own
//     transcripts, and paying for that on every reload — to answer a question nobody has asked —
//     is how a cockpit becomes slow to open.
//   * **Refresh on demand**, which is the only thing that asks for a re-read.
//   * **At most hourly otherwise.** That ceiling belongs to whatever serves `/api/usage`; a page
//     cannot enforce it and this one does not pretend to. What the page owes is honesty about the
//     age of what it is showing — `read_at` and `fresh`, and the sentence `usageRead` builds from
//     the pair of them.
//
// `/api/usage` is served by `api_usage` in `src/bin/skein-server/health.rs` — grep the router for the
// path; the line number this gave had drifted eight lines. A route the cockpit asks for and the
// server does not serve is caught by `tests/ui/usage.mjs` against a running server, and by
// `cockpit_routes::the_cockpit_never_asks_for_a_route_this_server_does_not_serve` in the source.
//
// A failed fetch is still a supported state rather than a broken one, because a route existing is
// not a route answering: the pane says skein could not ask, and shows no figure at all — never a
// zero, which is the one wrong answer a person would believe.
let usageReading = null;   // the last payload, or null if this page has never had one
let usageAsked = false;    // whether the pane has been opened at all
let usageBusy = false;
let usageProblem = "";
function drawUsage() {
  const el = document.getElementById("set-usage");
  if (!el) return;
  // The clock is an argument rather than something the markup reads for itself, which is what lets
  // "read 47 minutes ago" be asserted at a chosen instant in cockpit/test/usage.test.mjs.
  el.innerHTML = usageHtml(usageReading, Date.now(), esc, usageProblem);
  const btn = document.getElementById("ug-refresh");
  // Bound here rather than once at load: the button is part of the markup `usageHtml` returns and
  // is a new node after every redraw, so a handler bound to the old one is bound to nothing.
  if (btn) {
    btn.disabled = usageBusy;
    if (usageBusy) btn.textContent = "reading…";
    btn.addEventListener("click", () => loadUsage(true));
  }
}
async function loadUsage(refresh) {
  if (usageBusy) return;
  usageBusy = true; usageProblem = ""; drawUsage();
  try {
    const r = await fetch(refresh ? "/api/usage?refresh=1" : "/api/usage");
    if (!r.ok) throw new Error(`skein answered ${r.status}`);
    usageReading = await r.json();
  } catch (e) {
    // The reading already held is KEPT and still drawn; only the failure is added above it.
    // Dropping it would turn a failed refresh into an empty pane, and an empty pane is a worse
    // answer than an old one that says how old it is.
    usageProblem = `skein could not read the fleet's usage — ${e.message || e}.`
      + (usageReading ? " The figures below are the last reading it took." : "");
  }
  usageBusy = false;
  drawUsage();
}
// Opening the pane. The first open fetches; every open after it redraws what is already held, so
// the staleness line ages as the dialog is used instead of freezing at the moment it was drawn.
function showUsage() {
  if (usageAsked) { drawUsage(); return; }
  usageAsked = true;
  loadUsage(false);
}

const settingsModal = () => document.getElementById("settings");
// Settings → Shortcuts, and `?`. Rendered from `keySheet()` in cockpit/src/keys.mjs — the tables
// that bind the keys — so what the sheet claims and what the app binds are one list (SKEIN-1187).
// There used to be a second table here, and it had drifted from the first while saying it could not.
const MAC = /Mac|iP(hone|ad|od)/.test(navigator.platform || navigator.userAgent || "");
const glyph = k => platformKeys(k, MAC);
function renderKeys() {
  document.getElementById("set-keys").innerHTML = keySheet(MAC).map(g =>
    `<div class="set-kgrp"><b>${esc(g.sec)}</b>` + g.items.map(([keys, what]) =>
      `<div class="set-krow"><span class="kk">${keys.map(k => k === "…" ? "…" : `<kbd>${esc(glyph(k))}</kbd>`).join("")}</span><span class="kd">${esc(glyph(what))}</span></div>`
    ).join("") + `</div>`).join("");
}
// Every other place the page names a modifier: the header buttons' hints, the board's footer and
// the mailbox's placeholder are written with the Mac's ⌘ in `shell.html`, marked `data-keys`, and
// put through the same `glyph` once at load — so a Linux keyboard is told Ctrl+K on the button as
// well as in the sheet.
function platformHints(root = document) {
  for (const el of root.querySelectorAll("[data-keys]")) {
    for (const attr of ["title", "aria-label", "placeholder"]) {
      if (el.hasAttribute(attr)) el.setAttribute(attr, glyph(el.getAttribute(attr)));
    }
    const walk = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
    for (let n = walk.nextNode(); n; n = walk.nextNode()) n.nodeValue = glyph(n.nodeValue);
  }
}
const SET_PANES = {
  repos:    "the repos skein can launch boxes into",
  github:   "which GitHub credential a box gets, and how far it reaches",
  fleet:    "the one VM every box shares — its memory, cores and disk",
  usage:    "what the fleet has cost, from each box's own transcripts",
  boxes:    "defaults a new box inherits",
  tracking: "the shared backlog boxes claim work from",
  update:   "which skein this is, and whether a newer one exists",
  diag:     "what skein can see of its own environment",
  keys:     "every shortcut the cockpit binds",
};
function setPane(pane) {
  if (!SET_PANES[pane]) pane = "repos";
  for (const el of settingsModal().querySelectorAll(".set-navi")) el.classList.toggle("on", el.dataset.pane === pane);
  for (const el of settingsModal().querySelectorAll(".set-pane")) el.classList.toggle("on", el.dataset.pane === pane);
  const nav = settingsModal().querySelector(`.set-navi[data-pane="${pane}"]`);
  document.getElementById("set-htitle").textContent = nav ? nav.textContent.replace(/\d+$/, "").trim() : pane;
  document.getElementById("set-hsub").textContent = SET_PANES[pane];
  // The blanket "`$SKEIN_*` env vars override these" used to run here on every other pane, true or
  // not — most of Boxes has no override at all, and no other pane has ever had one. Dropped
  // (SKEIN-1141, owner's decision 2026-09-24); a field that really has one carries its own
  // `.set-envnote` beside it instead, filled only while the override is in force (`renderHeld`).
  const hint = document.getElementById("set-hint");
  if (hint) hint.innerHTML = pane === "repos"
    ? "each repo saves as you leave a field — these live in <code>repos.json</code>, not in this form"
    : pane === "tracking"
      ? "each connection saves as you leave a field — a credential never rides along with the config form"
      : "";
  if (pane === "diag") renderDiagnostics();
  if (pane === "usage") showUsage();
  if (pane === "update") loadUpdate();
  settingsModal().querySelector(".set-scroll").scrollTop = 0;
  // Shortcuts is a reference, not a form; repos and connections save themselves. A Save button that
  // covers none of what's on screen is worse than no Save button.
  settingsModal().querySelector(".set-foot").classList.toggle("readonly", ["keys", "repos", "tracking", "diag", "update", "usage"].includes(pane));
}
// Unsaved-changes indicator: the footer says so, so Save is never a guess about whether it's needed.
function markDirty(on) { document.getElementById("set-dirty").classList.toggle("on", on); }
function openSettings(pane = "repos") {
  // The host's own numbers, beside the ones being typed. Fetched with the rest so the pane never
  // renders a size against a blank.
  loadFleetPlan().then(() => {
    renderFleetPlanNotes();
    const el = document.getElementById("set-hostcap");
    if (!el) return;
    const host = fleetPlan?.host;
    el.textContent = host
      ? `${host.cpus} CPUs · ${gbOf(host.memory_mb)} memory · ${gbOf(host.disk_free_mb)} free on ${host.disk_path}`
      : "could not be measured";
  });
  // Synchronously, from the report the board poll already has. The fetch below re-applies it, but
  // it lands after the dialog is open — and "after the dialog is open" is precisely the window in
  // which a rebuild button that had not yet been hidden would be clickable.
  Promise.all([loadRuntimes(), loadSettings(), loadRepos(), loadSync()]).then(() => {
    // `!== false` would read an absent field as chosen, which is exactly the default this stopped
    // being. Unset means nobody picked the account token, so the switch is off.
    document.getElementById("set-seedgh").checked = settings.seed_gh_secret === true;
    // A fleet actually running on the account token must not have that folded out of sight — same
    // rule as the stored-token fold: hide a second path, never a path in use.
    const ghalt = document.getElementById("set-ghalt");
    if (ghalt && settings.seed_gh_secret === true) ghalt.open = true;
    document.getElementById("set-confirmdestroy").checked = settings.confirm_destroy !== false;
    document.getElementById("set-agent").value = settings.default_agent || "";
    document.getElementById("set-fleetmem").value = settings.fleet_memory || "";
    document.getElementById("set-fleetcpus").value = settings.fleet_cpus || "";
    document.getElementById("set-fleetdisk").value = settings.fleet_disk || "";
    document.getElementById("set-onedisk").checked = !!settings.fleet_one_disk;
    document.getElementById("set-gitscope").checked = !!settings.scope_git_to_repo;
    document.getElementById("set-ghappid").value = settings.github_app_id || "";
    document.getElementById("set-review-identity").value = settings.review_identity === "app" ? "app" : "me";
    renderReviewIdentityNote();
    document.getElementById("set-boxdisk").value = settings.box_disk_max || "";
    // `?? ""` rather than `|| ""`: 0 is a real setting here — the owner switching the age offer
    // off — and `||` would show them a blank box that reads back as the default.
    document.getElementById("set-staledays").value = settings.stale_build_days ?? "";
    document.getElementById("set-gitname").value = settings.git_name || "";
    document.getElementById("set-gitemail").value = settings.git_email || "";
    document.getElementById("set-boxmax").value = settings.box_memory_max || "";
    document.getElementById("set-boxhigh").value = settings.box_memory_high || "";
    document.getElementById("set-ai").checked = !!settings.ai_enrichment;
    // `!== false` rather than `!!`: this one defaults ON, so a settings payload from a server that
    // predates the field must read as on, not as off.
    document.getElementById("set-prai").checked = settings.review_summaries !== false;
    document.getElementById("set-review-model").value = settings.review_model || "";
    // Defaults ON, so `!== false` for the reason `set-prai` gives above.
    document.getElementById("set-boxplugin").checked = settings.box_plugin !== false;
    renderHeld();
    // The toggle says what you asked for; this says what would actually happen. "on, but `claude`
    // is not on PATH" is the state a checkbox alone can never show.
    fetch("/api/health").then(r => r.json()).then(h => {
      // Before the `h.ai` guard below, and not folded into it: which deployment this is has nothing
      // to do with whether the AI check answered, and a report missing one must not take the other
      // down with it — that is how the rebuild button would stay visible in the fleet.
      const note = document.getElementById("set-ainote");
      if (!note || !h.ai) return;
      note.textContent = h.ai.detail;
      note.className = "set-note" + (/^on —/.test(h.ai.detail) ? " ready" : /^on,/.test(h.ai.detail) ? " gap" : "");
    }).catch(() => {});
    // Credentials come from their own endpoint, never from the settings payload — a token has no
    // business in a document the page reads back. This also paints the repo cards' token rows, so
    // it runs before the render below rather than racing it.
    loadGitCreds();
    renderSync(); renderRepoList(); renderKeys(); markDirty(false); setPane(pane);
    settingsModal().classList.add("open");
    // Focus the dialog itself so esc/⏎ reach its handler; focusing the nav button instead would
    // paint a focus ring that then contradicts the active section once you switch panes.
    settingsModal().querySelector(".set-shell")?.focus();
  });
}
// "held off by `$SKEIN_X`" beside a control, only while that variable is holding it — the server
// says which (`settings.held`). A control with nothing holding it carries no note at all.
function renderHeld() {
  for (const el of settingsModal().querySelectorAll(".set-envnote[data-held]")) {
    el.innerHTML = heldNote(el.dataset.held, settings.held);
    el.hidden = !el.innerHTML;
  }
}
// Work tracking. `syncStatus` is host state (which connections exist, and is each one usable?),
// never a token itself — there is no route that reads one back, by design.
let syncStatus = { connections: [], ready: false };
const tracking = new Set();
function loadSync() {
  return fetch("/api/sync").then(r => r.json()).then(s => { if (s) syncStatus = s; }).catch(() => {});
}
// The connection a box could actually be wired to, or null. Per box, not per host: with more than
// one backlog, "some connection somewhere is ready" says nothing about *this* box, and offering a
// button that can only fail is worse than not offering it. `repo` is what the row already carries.
// Wire one box up: the host mints it its own gateway token and registers the MCP server inside it.
// A click, never a tick — it spends a network round trip and creates a real credential.
function trackWork(name) {
  if (!name || tracking.has(name)) return;
  tracking.add(name);
  fetch(`/api/boxes/${encodeURIComponent(name)}/sync`, { method:"POST" })
    .then(async r => { if (!r.ok) throw new Error((await r.text()) || r.statusText); return r.json(); })
    .then(d => toast(d.note || `${name} is tracking work`))
    .catch(e => toast(`couldn't wire ${name}: ${e.message}`))
    .finally(() => { tracking.delete(name); });
}
// Re-apply the work-tracking documents to a box that already has them. `replace` (shift-click) is
// for boxes wired up before skein recorded what it installed, where stale and box-edited cannot be
// told apart — even then the host refuses documents it can see were edited, so the promise that
// skein never overwrites your changes holds either way.
const refreshingDocs = new Set();
function refreshDocs(name, replace) {
  if (!name || refreshingDocs.has(name)) return;
  refreshingDocs.add(name); renderDockbar();
  fetch(`/api/boxes/${encodeURIComponent(name)}/sync/refresh${replace ? "?replace=1" : ""}`, { method:"POST" })
    .then(async r => { if (!r.ok) throw new Error((await r.text()) || r.statusText); return r.json(); })
    .then(d => toast(`${name}: ${d.note || "done"}`))
    .catch(e => toast(`couldn't update ${name}: ${e.message}`))
    .finally(() => { refreshingDocs.delete(name); renderDockbar(); });
}
// One card per connection. A gateway URL and the token that mints at it are one thing — a token is
// only valid at the gateway that issued it — so they live together, and a repo picks a connection
// rather than restating half of one. Cards save on change, like repo cards: the dialog's Save
// covers the config object, and a credential is not part of that object.
function renderSync() {
  const el = document.getElementById("set-conns");
  const note = document.getElementById("set-syncnote");
  const pill = document.getElementById("set-syncn");
  const conns = syncStatus.connections || [];
  if (pill) pill.textContent = syncStatus.ready ? "on" : "";
  const half = conns.filter(c => !c.ready);
  note.className = "set-note" + (syncStatus.ready && !half.length ? " ready" : conns.length ? " gap" : "");
  note.textContent = !conns.length
    ? "No connections yet. A connection is one backlog — the sync gateway your boxes claim work through (Plane behind an atomic claim, so two boxes never take the same item) plus the Plane token that mints each box its own. Add one, then pick it per repo under Repositories."
    : syncStatus.ready && !half.length
      ? "Ready. Each box is wired up on its own — open one and press Track work. It gets its own token, which is what lets two boxes hold different items; Plane's activity log still shows your name, because the gateway writes as whoever's token minted it. Destroying a box revokes its token."
      : `${half.map(c => c.label).join(", ")} ${half.length === 1 ? "is" : "are"} half-configured — a connection needs both a gateway URL and a token before it can mint a box anything.`;
  if (!conns.length) { el.innerHTML = `<div class="none">No work-tracking connections yet.</div>`; return; }
  el.innerHTML = conns.map(c => {
    const used = c.repos.length
      ? c.repos.map(r => `<span class="rtag own">${esc(r)}</span>`).join("")
      : `<span>no repos use it yet</span>`;
    return `<div class="ccard" data-conn="${esc(c.id)}">`
      + `<div class="chead"><input class="clabel" data-field="label" value="${esc(c.label)}" placeholder="name this backlog" autocomplete="off" spellcheck="false" />`
      + `<span class="rsaved" data-saved="conn-${esc(c.id)}">saved</span>`
      + `<span class="cstate${c.ready ? " ready" : ""}">${c.ready ? "ready" : c.token_set ? "no gateway" : "no token"}</span></div>`
      + `<label class="set-field"><span class="set-title">Gateway URL</span>`
      + `<span class="desc">the <code>sync</code> gateway boxes on this connection claim work through</span>`
      + `<input data-field="gateway_url" value="${esc(c.gateway_url)}" placeholder="https://plane.example.com" autocomplete="off" spellcheck="false" /></label>`
      + `<label class="set-field"><span class="set-title">Plane personal token</span>`
      + `<span class="desc">used <b>once per box</b>, here on the host, to mint that box its own tracker token — a box never receives this one, because a Plane token can bypass the claim. Plane → avatar → Settings → Personal access tokens.</span>`
      + `<div class="path-row"><input data-field="token" type="password" placeholder="${c.token_set ? "•••••••••• stored on this host" : "plane_api_…"}" autocomplete="off" spellcheck="false" />`
      + (c.token_set ? `<button type="button" class="kbtn" data-forget="${esc(c.id)}" title="Delete the stored token from this host">Forget</button>` : "")
      + `</div></label>`
      + `<div class="cuse">${used}<span class="spacer"></span>`
      + `<button type="button" class="kbtn ghost danger" data-dropconn="${esc(c.id)}" title="Forget this connection and its token">Remove</button></div>`
      + `</div>`;
  }).join("");
  el.querySelectorAll(".ccard input").forEach(i => i.addEventListener("change", () => saveConnection(i.closest("[data-conn]"))));
  el.querySelectorAll("[data-forget]").forEach(b => b.addEventListener("click", () => forgetConnectionToken(b.dataset.forget)));
  el.querySelectorAll("[data-dropconn]").forEach(b => b.addEventListener("click", () => removeConnection(b.dataset.dropconn)));
}
// The whole card in one request, so a URL and its token can never be half-saved against each other.
// A blank token field means "unchanged" — Forget is the deliberate act, and it is a separate one.
function saveConnection(card) {
  const id = card.dataset.conn || null;
  const val = f => card.querySelector(`[data-field="${f}"]`).value.trim();
  const body = { id, label: val("label"), gateway_url: val("gateway_url") };
  const token = val("token");
  if (token) body.token = token;
  return fetch("/api/sync/connections", {
    method:"POST", headers:{"Content-Type":"application/json"}, body: JSON.stringify(body),
  })
    .then(async r => { if (!r.ok) throw new Error((await r.text()) || r.statusText); return r.json(); })
    .then(s => {
      syncStatus = s;
      // Confirm at the card, not only in a toast: the toast is gone before you've read the next
      // label. Re-render after, so the flag survives the repaint.
      renderSync(); renderRepoList();
      const flag = document.querySelector(`[data-saved="${CSS.escape("conn-" + (id || (s.connections.at(-1) || {}).id || ""))}"]`);
      if (flag) { flag.classList.add("on"); setTimeout(() => flag.classList.remove("on"), 1600); }
    })
    .catch(e => toast(e.message));
}
function forgetConnectionToken(id) {
  fetch(`/api/sync/connections/${encodeURIComponent(id)}/token`, { method:"DELETE" })
    .then(async r => { if (!r.ok) throw new Error((await r.text()) || r.statusText); return r.json(); })
    .then(s => { syncStatus = s; renderSync(); toast(`forgot the token for ${id}`); })
    .catch(e => toast(e.message));
}
function removeConnection(id) {
  const c = (syncStatus.connections || []).find(c => c.id === id);
  if (c && c.repos.length && !confirm(`${id} is used by ${c.repos.join(", ")}. Remove it anyway?`)) return;
  fetch(`/api/sync/connections/${encodeURIComponent(id)}`, { method:"DELETE" })
    .then(async r => { if (!r.ok) throw new Error((await r.text()) || r.statusText); return r.json(); })
    .then(s => { syncStatus = s; renderSync(); renderRepoList(); })
    .catch(e => toast(e.message));
}
// A new card is a card, not a prompt() — it starts blank in the list and saves like any other once
// it has a URL, so adding one and editing one are the same two fields in the same place.
function addConnection() {
  const el = document.getElementById("set-conns");
  if (el.querySelector('[data-conn=""]')) { el.querySelector('[data-conn=""] [data-field="gateway_url"]').focus(); return; }
  if (!(syncStatus.connections || []).length) el.innerHTML = "";
  el.insertAdjacentHTML("beforeend",
    `<div class="ccard" data-conn="">`
    + `<div class="chead"><input class="clabel" data-field="label" placeholder="name this backlog" autocomplete="off" spellcheck="false" />`
    + `<span class="cstate">new</span></div>`
    + `<label class="set-field"><span class="set-title">Gateway URL</span>`
    + `<span class="desc">the <code>sync</code> gateway boxes on this connection claim work through</span>`
    + `<input data-field="gateway_url" placeholder="https://plane.example.com" autocomplete="off" spellcheck="false" /></label>`
    + `<label class="set-field"><span class="set-title">Plane personal token</span>`
    + `<span class="desc">minted per box on the host — a box never receives this one</span>`
    + `<div class="path-row"><input data-field="token" type="password" placeholder="plane_api_…" autocomplete="off" spellcheck="false" /></div></label>`
    + `<div class="cuse"><span class="spacer"></span><button type="button" class="kbtn ghost" data-cancelconn>Cancel</button>`
    + `<button type="button" class="primary" data-saveconn>Add</button></div></div>`);
  const card = el.querySelector('[data-conn=""]');
  card.querySelector("[data-cancelconn]").addEventListener("click", () => renderSync());
  card.querySelector("[data-saveconn]").addEventListener("click", () => saveConnection(card));
  card.querySelector('[data-field="gateway_url"]').focus();
}
const openRepos = new Set();   // which cards are expanded (survives the re-render after a save)
// A repo's own settings live in repos.json, not in the config form — so they save when you leave the
// field, and the dialog's Cancel has nothing to do with them. Saying so beats a Save button that
// only covers half of what's on screen.
function repoField(r, key, title, desc, placeholder) {
  return `<label class="set-field"><span class="set-title">${esc(title)}<span class="rsaved" data-saved="${esc(r.id)}-${key}">saved</span></span>`
    + `<span class="desc">${desc}</span>`
    + `<input data-id="${esc(r.id)}" data-key="${key}" value="${esc(r[key] || "")}" placeholder="${esc(placeholder)}" autocomplete="off" spellcheck="false" /></label>`;
}
// A picker, not a text field: the thing a repo needs is a whole connection — a gateway *and* the
// token that mints at it — and only Work tracking knows which pairs exist. Typing a URL here could
// only ever name half of one.
// **The composition nobody chose in one place** — docs/pr-review.md §13. Auto-review with the
// ceiling at `approve`, plus a merge train that claims this repo's pull requests, is skein
// approving its own work and merging it with nobody in it. That is reachable on purpose; what it
// came with is that it must not be reachable silently.
//
// Rendered from the two halves the PAGE can see — this repo's own switches — and it names
// `skein doctor` for the half it cannot: whether any workflow in the fleet actually merges is a
// fleet-level fact, and `prwork::the_loop_this_repo_has_built` is the authority on it. A page that
// guessed at that would either cry wolf or stay quiet, and both are worse than pointing at the
// thing that knows.
//
// It appears at the moment the decision is made — under the ceiling select, when "approve" is what
// is chosen — rather than in a banner somewhere a person is not looking.
function repoLoopWarning(r) {
  if (!r.auto_review || r.auto_review_ceiling !== "approve") return "";
  return `<div class="set-field"><span class="desc warnline">`
    + `Approvals are unattended here. Where a merge train also claims this repo's pull requests, `
    + `<b>an approval skein posts is what merges it</b> — nobody is in that loop. `
    + `Run <code>skein doctor</code> to see whether one does.`
    + `</span></div>`;
}
function repoSelect(r, key, title, desc, options) {
  // A boolean field arrives as true/false and its <option> values are strings; without this the
  // select falls back to its first option and silently misreports the stored setting.
  const cur = typeof r[key] === "boolean" ? String(r[key]) : (r[key] || "");
  return `<label class="set-field"><span class="set-title">${esc(title)}<span class="rsaved" data-saved="${esc(r.id)}-${key}">saved</span></span>`
    + `<span class="desc">${desc}</span>`
    + `<select data-id="${esc(r.id)}" data-key="${key}">`
    + options.map(([value, label]) => `<option value="${esc(value)}"${cur === value ? " selected" : ""}>${esc(label)}</option>`).join("")
    + `</select></label>`;
}
// `owner/name` from whatever a repo's source is spelled as, or "" when that spelling isn't a GitHub
// remote.
//
// For a repo skein already manages, use its `slug` — the host resolved it, and can consult the
// clone's `origin` where this cannot. This is for the one moment there is no clone to ask: a source
// still being typed into the add dialog.
//
// Mirror of `gitgate::slug_from_url` — keep them in sync. Faithful rather than approximate, because
// the two disagreeing is worse than either being wrong alone: this decides which field the page
// offers, and the host decides which token it mints, so a divergence is a form that accepts a token
// for a repository the host will never issue one for.
//
// A filesystem path is not a remote, and `repos::add_repo` refuses one — but a path can still
// be TYPED here, and `/Users/…/code/thing` parses to `Users/…`, a repository that does not exist.
const nameable = slug => {
  const i = slug.indexOf("/");
  if (i < 0) return false;
  const ok = s => !!s && !s.startsWith("-") && !s.includes("..") && /^[A-Za-z0-9._-]+$/.test(s);
  return ok(slug.slice(0, i)) && ok(slug.slice(i + 1));
};
// Two segments exactly, never a prefix of a longer path: taking the first two would turn any deep
// path into a plausible-looking repository.
const slugFromPath = path => {
  const p = path.trim().replace(/^\/+/, "");
  const i = p.indexOf("/");
  if (i < 0) return "";
  const slug = p.slice(0, i) + "/" + p.slice(i + 1).replace(/\.git$/, "");
  return nameable(slug) ? slug : "";
};
function repoSlug(source) {
  const url = (source || "").trim();
  if (!url || /^[/.~]/.test(url)) return "";
  const afterScheme = url.includes("://") ? url.slice(url.indexOf("://") + 3) : url;
  const at = afterScheme.lastIndexOf("@");
  const rest = at >= 0 ? afterScheme.slice(at + 1) : afterScheme;
  let host, path;
  const colon = rest.indexOf(":");
  if (colon >= 0) {
    host = rest.slice(0, colon);
    const p = rest.slice(colon + 1);
    // `host:port/path` only when everything before the first slash is digits — the one ambiguity
    // in the grammar, since `git@host:owner/name` and `host:22/owner/name` are the same shape.
    const head = p.split("/")[0];
    path = (/^\d+$/.test(head) && head !== "" && p.includes("/")) ? p.slice(p.indexOf("/") + 1) : p;
  } else if (rest.includes("/")) {
    host = rest.slice(0, rest.indexOf("/"));
    path = rest.slice(rest.indexOf("/") + 1);
  } else {
    return nameable(rest) ? rest : "";   // a bare `owner/name` has no host at all
  }
  if (host && host.toLowerCase() !== "github.com") {
    // `owner/name` reaches here as host=`owner`, path=`name`. A dot is the tell: GitHub owners
    // cannot contain one, and hostnames of interest always do.
    const whole = `${host}/${path}`;
    return (!host.includes(".") && nameable(whole)) ? whole : "";
  }
  return slugFromPath(path);
}
// The host a source names, when it names one that is not GitHub — "" for GitHub, for a path, and
// for anything `repoSlug` can read a GitHub slug out of (SKEIN-812). The host half of `repoSlug`'s
// own parse, so the two cannot disagree about where the host ends.
function nonGitHubHost(source) {
  const url = (source || "").trim();
  if (!url || /^[/.~]/.test(url) || repoSlug(url)) return "";
  const afterScheme = url.includes("://") ? url.slice(url.indexOf("://") + 3) : url;
  const at = afterScheme.lastIndexOf("@");
  const host = (at >= 0 ? afterScheme.slice(at + 1) : afterScheme).split(/[:/]/)[0];
  return host && host.toLowerCase() !== "github.com" ? host : "";
}
// A repo's write token, on the repo's own card — because "which token does *this* repo push with"
// is a question about the repo, and answering it three panes away is how it went unfound.
function repoTokenRow(r) {
  // `r.slug` comes from the host, which resolves it from `source` — or, for an entry written when
  // a path could still be registered, from that mirror's `origin` (`gitgate::repo_slug`). Parsing
  // `source` here instead was wrong for every such repo: a path says nothing about its remote.
  const slug = r.slug || "";
  if (!slug) {
    // **Two different repos reach this row, and each gets its own sentence** (SKEIN-812, the
    // owner's wording of 2026-09-23). A legacy entry registered from a path has a next step, and it
    // is re-adding by URL: `repos::add_repo` replaces the entry with the same id in place and leaves
    // the mirror and every box alone. A repo on another host has none — the write token exists to
    // let a box push to GitHub, and this repo is not there. That second sentence is the one place a
    // row legitimately stops at naming the problem; `docs/recovery-survey.md` §7 records why.
    const src = (r.source || "").trim();
    const host = nonGitHubHost(src);
    const why = /^[/.~]/.test(src)
      ? "this repo was registered from a path, and skein can no longer read a remote from one. Add it again by its GitHub URL and this becomes a token field."
      : host
        ? `${esc(host)} is not GitHub, and skein only holds GitHub push credentials, so this repo's boxes can commit but skein gives them nothing to push with. Nothing about your repo needs changing; this is a limit of skein.`
        : "no GitHub remote — nothing to scope, and nowhere for a box to push";
    return `<label class="set-field"><span class="set-title">Write token</span>`
      + `<span class="desc" data-notoken>${why}</span></label>`;
  }
  const c = credFor(slug);
  const state = c && !c.problem && c.has_token
    ? `<span class="sq-state approved">token stored</span><span class="cwhy">boxes push with it, and your queue, reviews and merges here run on it; storing another replaces it</span>`
      + `<button type="button" class="kbtn ghost" onclick="removeGitCred(${esc(JSON.stringify(c.id))})">Forget</button>`
    : `<span class="sq-state">${c && c.problem ? "refused" : "from the App"}</span><span class="cwhy">${
        c && c.problem ? esc(c.problem)
        : "no token of its own — the GitHub App mints one per push if it is installed here. Paste a token below only if it is not"}</span>`;
  return `<label class="set-field"><span class="set-title">Write token<span class="rsaved" data-saved="${esc(r.id)}-token">saved</span></span>`
    + `<span class="desc">a fine-grained PAT covering <b>${esc(slug)}</b> and nothing else. This repo's boxes push with it, and skein reads your review queue, posts your reviews and merges here with it, ahead of <code>$GH_TOKEN</code> and <code>gh</code>. Stored 0600 on the host and never served back</span>`
    + `<div class="set-cred">${state}</div>`
    + `<div class="path-row"><input type="password" data-repotoken="${esc(slug)}" data-id="${esc(r.id)}" placeholder="github_pat_… (write to ${esc(slug)})" autocomplete="off" />`
    + `<button type="button" class="kbtn" data-storetoken="${esc(slug)}" data-id="${esc(r.id)}">Store</button></div></label>`;
}
function renderRepoList() {
  const el = document.getElementById("set-repos");
  const n = document.getElementById("set-repon");
  if (n) n.textContent = repos.length || "";
  if (!repos.length) { el.innerHTML = `<div class="none">No repositories yet — add one below to launch boxes into it.</div>`; return; }
  const conns = syncStatus.connections || [];
  el.innerHTML = repos.map(r => {
    const conn = conns.find(c => c.id === r.sync_connection);
    // Tags say what this repo would actually do, and whether that came from the repo or the default.
    const tags = [
      [r.plane_project ? "Plane" : "", true, "tracked in a Plane project"],
      [conn ? conn.label : "", true, conn ? (conn.ready ? conn.gateway_url : `${conn.gateway_url || "no gateway"} — not usable yet`) : ""],
    ].filter(([label]) => label)
     .map(([label, own, tip]) => `<span class="rtag${own ? " own" : ""}" title="${esc(tip)}">${esc(label)}</span>`).join("");
    return `<div class="rcard${openRepos.has(r.id) ? " open" : ""}" data-card="${esc(r.id)}">`
      + `<button type="button" class="rhead" aria-expanded="${openRepos.has(r.id)}"><span class="rid">${esc(r.id)}</span>`
      + `<span class="rsrc" title="${esc(r.source)}">${esc(r.source)}</span>${tags}<span class="rchev">›</span></button>`
      + `<div class="rbody">`
      // Per repo (the owner, 2026-09-27): it was one fleet-wide field, honoured only by the repos
      // whose remote happened to have that branch.
      + repoField(r, "base_branch", "Branch a new box starts from",
          "blank starts from the remote's own default branch. A branch the remote does not have is ignored, and the box starts from the default",
          "(the remote's default)")
      + repoField(r, "plane_project", "Plane project", "paste the project URL or its uuid — a box's tracker token binds to it, so its work lands on that board", "https://plane…/projects/<uuid>/issues")
      + repoSelect(r, "sync_connection", "Work tracking",
          conns.length
            ? `which backlog this repo's boxes claim work from — set up under <b>Work tracking</b>${conn && !conn.ready ? `. <b>${esc(conn.label)}</b> isn't usable yet: it needs both a gateway URL and a token` : ""}`
            : "no connections configured yet — add one under <b>Work tracking</b> and it'll appear here",
          [["", "Not tracked"], ...conns.map(c => [c.id, c.ready ? c.label : `${c.label} (not usable yet)`])])
      // The per-repo half of "may skein read pull requests" (docs/pr-review.md §10, layer 1), on the
      // card beside the queue it reads from. It had been reachable only from a chip in the review
      // pane. Posted to its own route (`revSetReadingFor`), which is the one place it is written.
      + repoSelect(r, "read_prs", "Read ahead",
          "let skein read this repo's pull requests on its own: the ones waiting on your review and the ones you opened, one unit of the day's budget each. Off, it reads one only when you ask. Needs <b>Read pull requests</b>, the master switch in Settings → Boxes",
          [["false", "Off"], ["true", "On"]])
      + repoSelect(r, "review_queue", "Review queue",
          "list this repo's pull requests, and count the ones waiting on you. Turn it off for a repo whose PRs are none of your business — a fork, a scratch clone — and skein stops asking GitHub about it entirely",
          [["true", "On"], ["false", "Off"]])
      // The reviewer engine's two switches, in the order they resolve: may it act here at all, and
      // then how far. Separate rows because they answer different questions and one is a
      // permission — folding them into a single four-value select would make "off" and "on but
      // posting nothing" the same control, and they are not the same decision.
      + repoSelect(r, "auto_review", "Automatic review",
          "let skein review this repo's pull requests on its own — read them, and post what the ceiling below allows. Off everywhere by default; nothing turns it on by itself",
          [["false", "Off"], ["true", "On"]])
      + repoSelect(r, "auto_review_ceiling", "Unattended up to",
          "how far automatic review may go without you. Anything past the ceiling is still read, and waits. <b>Comment</b> is where a repo starts: findings unattended, verdicts waiting",
          [["none", "Nothing — read only"], ["comment", "Findings"], ["changes", "Findings and refusals"], ["approve", "Everything, including approvals"]])
      + repoLoopWarning(r)
      + repoTokenRow(r)
      + `<div class="rfoot"><button type="button" class="kbtn ghost danger" data-remove="${esc(r.id)}" title="skein forgets this repo; the clone and store stay on disk">Unregister</button></div>`
      + `</div></div>`;
  }).join("");
  el.querySelectorAll(".rhead").forEach(h => h.addEventListener("click", () => {
    const id = h.closest("[data-card]").dataset.card;
    openRepos.has(id) ? openRepos.delete(id) : openRepos.add(id);
    h.closest(".rcard").classList.toggle("open");
    h.setAttribute("aria-expanded", openRepos.has(id));
  }));
  el.querySelectorAll("[data-remove]").forEach(b => b.addEventListener("click", () => removeRepo(b.dataset.remove)));
  // The token input is excluded by name, not by luck: it carries a `data-id` like its neighbours but
  // no `data-key`, so leaving it in would POST `{undefined: "<the token>"}` to the repo settings
  // route — a token in the wrong store, on blur, silently.
  el.querySelectorAll(".rbody input:not([data-repotoken]), .rbody select")
    .forEach(i => i.addEventListener("change", () => saveRepoField(i)));
  el.querySelectorAll("[data-storetoken]").forEach(b => b.addEventListener("click", () => {
    const slug = b.dataset.storetoken;
    const input = el.querySelector(`[data-repotoken="${CSS.escape(slug)}"]`);
    const token = (input.value || "").trim();
    if (!token) { toast("paste a token first"); input.focus(); return; }
    // The card is re-rendered by loadGitCreds, so the field clears itself — but clear it here too,
    // because a failed store must not leave the token sitting in a visible input either.
    input.value = "";
    storeGitCred(slug, token).then(() => {
      const flag = document.querySelector(`[data-saved="${CSS.escape(b.dataset.id + "-token")}"]`);
      if (flag) { flag.classList.add("on"); setTimeout(() => flag.classList.remove("on"), 1600); }
    }).catch(e => toast(String(e.message || "could not store that token")));
  }));
}
function saveRepoField(input) {
  const { id, key } = input.dataset;
  // Booleans on the wire. Sending the string "false" would deserialize-fail and the field would
  // report saved while nothing changed — the worst shape a settings bug can take. A LIST rather
  // than one name because the second such field arrived (`auto_review`) and a comparison that
  // named one would have made the new one fail exactly that way, silently.
  const BOOL_KEYS = ["review_queue", "auto_review", "read_prs"];
  const raw = input.value.trim();
  const value = BOOL_KEYS.includes(key) ? raw === "true" : raw;
  const saved = () => {
    // Confirm at the field, not only in a toast: the toast is gone before you've read the next label.
    const flag = document.querySelector(`[data-saved="${CSS.escape(id + "-" + key)}"]`);
    if (flag) { flag.classList.add("on"); setTimeout(() => flag.classList.remove("on"), 1600); }
  };
  // Consent to read has its own route and its own writer, shared with the review pane's chip.
  if (key === "read_prs") {
    revSetReadingFor(id, value).then(ok => ok ? loadRepos().then(() => { renderRepoList(); saved(); }) : input.focus());
    return;
  }
  fetch(`/api/repos/${encodeURIComponent(id)}/settings`, {
    method:"POST", headers:{"Content-Type":"application/json"}, body: JSON.stringify({ [key]: value }),
  })
    .then(async r => { if (!r.ok) throw new Error((await r.text()) || r.statusText); return r.json(); })
    .then(() => {
      // Confirm at the field, not only in a toast: the toast is gone before you've read the next label.
      const flag = document.querySelector(`[data-saved="${CSS.escape(id + "-" + key)}"]`);
      if (flag) { flag.classList.add("on"); setTimeout(() => flag.classList.remove("on"), 1600); }
      // Both, not just repos: a connection card lists the repos that depend on it, so picking one
      // here changes what the other pane is entitled to say about a Remove.
      Promise.all([loadRepos(), loadSync()]).then(() => { renderRepoList(); renderSync(); });
    })
    .catch(e => { toast(`${id}: ${e.message}`); input.focus(); });
}
// Sync a repo's working clone to its remote (git pull --ff-only). `x` is the clicked control; it
// carries the repo id in data-id and shows a brief busy state. Used by each repo's sidebar header.
function pullRepo(x) {
  const id = x.dataset.id;
  if (x.classList.contains("busy")) return;
  x.classList.add("busy"); x.textContent = "…";
  fetch(`/api/repos/${encodeURIComponent(id)}/pull`, { method:"POST" })
    .then(async r => { if (!r.ok) throw new Error((await r.text()) || r.statusText); return r.json(); })
    .then(d => toast(`${id}: ${(d.summary || "pulled").split("\n")[0]}`))
    .catch(e => toast(`pull failed: ${e.message}`))
    .finally(() => { x.classList.remove("busy"); x.textContent = "⟳"; });
}
function removeRepo(id) {
  if (!confirm(`Unregister repo "${id}"? Its clone and store stay on disk; only skein's registration is removed.`)) return;
  fetch(`/api/repos/${encodeURIComponent(id)}`, { method:"DELETE" })
    .then(async r => { if (!r.ok) throw new Error((await r.text()) || r.statusText); return r.json(); })
    .then(() => { toast(`removed ${id}`); loadRepos().then(renderRepoList); })
    .catch(e => toast(`remove failed: ${e.message}`));
}
function closeSettings() { settingsModal().classList.remove("open"); }
// The settings payload, built in one place so an action that must save first (applyBoxLimits) can
// never drift from what Save itself sends — a half-saved settings object is the kind of bug that
// only shows up as a setting that mysteriously does not stick.
// Spread the loaded settings first, so a field this screen does not render is sent back unchanged
// rather than omitted. The server merges too — belt and braces, because the cost of getting this
// wrong is skein forgetting its fleet sandbox exists.
function settingsPayload() {
  return {
    ...settings,
    seed_gh_secret: document.getElementById("set-seedgh").checked,
    confirm_destroy: document.getElementById("set-confirmdestroy").checked,
    default_agent: document.getElementById("set-agent").value,
    fleet_memory: document.getElementById("set-fleetmem").value.trim(),
    fleet_cpus: document.getElementById("set-fleetcpus").value.trim(),
    fleet_disk: document.getElementById("set-fleetdisk").value.trim(),
    fleet_one_disk: document.getElementById("set-onedisk").checked,
    scope_git_to_repo: document.getElementById("set-gitscope").checked,
    github_app_id: document.getElementById("set-ghappid").value.trim(),
    review_identity: document.getElementById("set-review-identity").value,
    box_disk_max: document.getElementById("set-boxdisk").value.trim(),
    // Blank means the same as 0, which the field says: skein says nothing about age at all. The
    // two provable sweeps beside it are unaffected either way.
    stale_build_days: Math.max(0, parseInt(document.getElementById("set-staledays").value, 10) || 0),
    git_name: document.getElementById("set-gitname").value.trim(),
    git_email: document.getElementById("set-gitemail").value.trim(),
    box_memory_max: document.getElementById("set-boxmax").value.trim(),
    box_memory_high: document.getElementById("set-boxhigh").value.trim(),
    ai_enrichment: document.getElementById("set-ai").checked,
    review_summaries: document.getElementById("set-prai").checked,
    review_model: document.getElementById("set-review-model").value.trim(),
    box_plugin: document.getElementById("set-boxplugin").checked,
  };
}
// Work-tracking connections are deliberately NOT in the payload: they carry a credential, and it
// round-trips through the browser on every save. Their cards save themselves.
function saveSettingsQuiet() {
  return fetch("/api/settings", { method:"POST", headers:{"Content-Type":"application/json"},
                                  body: JSON.stringify(settingsPayload()) })
    .then(r => { if (!r.ok) throw new Error(r.statusText); return r.json(); })
    .then(s => { settings = s; return s; });
}
// The box plugin's own after-Save line, approved with its switch (SKEIN-1057): said only when the
// value changed, because it is about what the change does to running boxes.
const BOX_PLUGIN_SAVED = "Saved. Boxes pick this up at their next session; the ones running now keep what they started with.";
function saveSettings() {
  const pluginWas = settings.box_plugin !== false;
  saveSettingsQuiet()
    .then(() => loadSync())
    .then(() => {
      renderDockbar(); closeSettings();
      toast((settings.box_plugin !== false) !== pluginWas ? BOX_PLUGIN_SAVED : "settings saved");
    })
    .catch(e => toast(`couldn't save: ${e.message}`));
}
// A cgroup limit is live: no snapshot, no restart, no box interrupted mid-turn. That is the whole
// difference between this and the fleet's own memory and CPUs, which are fixed at sandbox creation
// and so cannot be applied from here at all (see the note above `set-resize-infleet`). Save first —
// the server applies what is stored, not what is typed.
function applyBoxLimits() {
  const btn = document.getElementById("set-applylimits");
  btn.disabled = true; btn.textContent = "applying…";
  saveSettingsQuiet()
    .then(() => fetch("/api/fleet/limits", { method:"POST" }))
    .then(r => r.ok ? r.json() : r.text().then(t => { throw new Error(t); }))
    .then(res => {
      const failed = res.failed || [];
      toast(failed.length
        ? `ceilings applied; ${failed.join(", ")} has no cgroup and was left as it is`
        : "ceilings applied to every running box");
    })
    .catch(e => toast(`couldn't apply: ${e.message}`))
    .finally(() => { btn.disabled = false; btn.textContent = "Apply to running boxes"; });
}

// ---------- saving every box's work, which is its own act (SKEIN-680) ----------
//
// The archive is never a side effect. This posts only when the button is pressed, and it is the
// same `fleet::save_boxes` that `skein save` runs — one act, two front doors, no second
// implementation to drift.
//
// **The report is rendered per box and left on screen.** A toast is wrong here twice over: a save
// takes minutes and the answer arrives after attention has moved, and the answer is a list of
// paths, one per box, that somebody needs to be able to read and copy. `renderSaved` builds nodes
// with `textContent` rather than markup — the paths and the error text come from the server, and a
// path is the last string to interpolate into HTML.
function saveBoxes() {
  const btn = document.getElementById("set-save-go");
  const out = document.getElementById("set-save-out");
  btn.disabled = true; btn.textContent = "saving…";
  // Said before the wait rather than after it, because the wait is minutes long and a silent
  // disabled button is indistinguishable from one that did nothing.
  out.textContent = "copying every box out — nothing is stopped while this runs.";
  fetch("/api/fleet/save", { method:"POST" })
    .then(r => r.ok ? r.json() : r.text().then(t => { throw new Error(t); }))
    .then(res => renderSaved(out, res.boxes || []))
    // A refusal is the whole act declining before it wrote anything — no census, no room on the
    // host — so it is reported as such rather than as a partial save.
    .catch(e => { out.textContent = `nothing was copied out: ${e.message}`; })
    .finally(() => { btn.disabled = false; btn.textContent = "Save every box's work"; });
}
function renderSaved(out, boxes) {
  out.textContent = "";
  const head = document.createElement("p");
  head.textContent = savedSummary(boxes);
  out.appendChild(head);
  for (const one of boxes) {
    const row = document.createElement("p");
    const name = document.createElement("b");
    name.textContent = one.name;
    row.appendChild(name);
    if (one.error) {
      row.appendChild(document.createTextNode(` — not saved: ${one.error}`));
      out.appendChild(row);
      continue;
    }
    // Where it went, which is what was asked for, and then how to bring it back — the second is
    // `fleet::restore_script`'s own text, so what is shown here and what a restore runs are one
    // string.
    row.appendChild(document.createTextNode(" "));
    const where = document.createElement("code");
    where.textContent = one.archive;
    row.appendChild(where);
    const back = document.createElement("div");
    back.className = "hint";
    back.appendChild(document.createTextNode("put it back, inside the fleet sandbox: "));
    const how = document.createElement("code");
    how.textContent = one.restore;
    back.appendChild(how);
    row.appendChild(back);
    out.appendChild(row);
  }
}

// ---------- the two sentences `/api/fleet/plan` carries for the fleet pane ----------
//
// `why` is why sbx could not be asked about the sandbox — in-fleet that is the correct answer
// rather than a fault, and unsaid it leaves the pane quietly claiming nothing about a fleet it
// cannot see. `lifecycle_refusal` is what the rebuild route would refuse with, rendered verbatim so
// the note and the refusal cannot drift.
//
// The plan used to carry a third, `exists`, and it went with the create-fleet dialog that was its
// only reader (SKEIN-627): from inside the sandbox the answer is `Some(true)` about this fleet and
// unknowable about any other, which is a field with one value and no question behind it.
function renderFleetPlanNotes() {
  const p = fleetPlan || {};
  const why = document.getElementById("set-fleetwhy");
  if (why) { why.textContent = p.why || ""; why.style.display = p.why ? "" : "none"; }
  const line = document.getElementById("set-resize-line");
  if (line) {
    line.textContent = p.lifecycle_refusal || "";
    line.style.display = p.lifecycle_refusal ? "" : "none";
  }
}
// Load the repos skein manages so the new-box dialog can prefix `<repo>-<branch>` and the palette
// can offer "Add a repo". Refreshed when the dialog/palette opens.
function loadRepos() {
  return fetch("/api/repos").then(r => r.json()).then(rs => {
    repos = rs || [];
    fillRepoSelect();
    // The review pane's repo picker is built from this list, and on a reload the pane can render
    // before the list arrives — without this it keeps the single-repo spelling forever.
    if (view.mode === "review") renderReview();
    render(boxes);   // empty managed repos get their own section so you can launch into them
  }).catch(() => {});
}
function fillRepoSelect() {
  // Shown whenever there is one, not only when there is a choice. Hiding a single-option select is
  // ordinarily right and is wrong here: the box is NAMED after the repo, and the moment someone
  // most needs to see which repo they are launching into is the first box after adding one — the
  // moment the control was hidden. "Where is the repo?" was the first question a new user asked.
  const show = repos.length > 0;
  nbRepo.style.display = show ? "" : "none";
  nbRepoLbl.style.display = show ? "" : "none";
  const cur = nbRepo.value;
  nbRepo.innerHTML = repos.map(r => `<option value="${esc(r.id)}">${esc(r.id)}</option>`).join("");
  if (cur && repos.some(r => r.id === cur)) nbRepo.value = cur;
  // The fleet's default, the same for every repo: a repo no longer carries a runtime of its own.
  const wanted = settings.default_agent;
  if (runtimes.some(runtime => runtime.id === wanted)) nbAgent.value = wanted;
}
// The repo id to prefix a new box with: the dialog selection, else the only/first repo, else a
// neutral "box" (no repos registered yet — a degenerate case the launch can't really satisfy).
function currentRepoId() {
  if (nbRepo.value) return nbRepo.value;
  if (repos.length) return repos[0].id;
  return "box";
}
// `loadFleetPlan` is deliberately NOT in this list any more. It was here for one reason — so
// `launchBox` had an `exists` on the plan to branch on before offering to create the fleet — and
// that branch is gone (SKEIN-627). Nothing else in this dialog reads the plan, and fetching it on
// every open bought a round trip nobody looked at.
function openNewBox() { newbox.classList.add("open"); nbBranch.value = ""; clearIdentityFields(); Promise.all([loadRuntimes(),loadSettings(),loadRepos(),loadSync(),loadGitScopeEffective()]).then(() => { fillRepoSelect(); fillSyncSelect(); fillGitScopeSelect(); gateLaunch(); }); nbBranch.focus(); }

// A box belongs to a repo. With none registered there is nothing to clone, nothing to name it
// after, and nothing for the agent to open — `currentRepoId` falls back to the literal string
// "box", which its own comment calls a case the launch cannot satisfy. It used to launch anyway
// and produce `box-my-feature`: a dock tab with a full toolbar over a black terminal, while the
// board beside it said "0 boxes" and "the skein is empty". Three contradictory claims on one
// screen, one click from first run. Refuse, and point at the thing that fixes it.
// The first screen of a product nobody has used before.
//
// It said `the skein is empty — add a repository then launch a box`, once, in the middle of 900px
// of nothing. It named neither of the two things that actually stop a first run — sbx missing, and
// `skein login` never run — and it only rendered when there were zero repos AND zero boxes, so it
// vanished the moment you added a repo, exactly when "now launch a box" was the thing to say.
//
// The steps that can be checked are checked. A tick is worth more than a sentence here: it turns
// "did that work?" — the question every one of these steps leaves you with — into something the
// page answers before you ask.
function firstRunHtml(foreignN = 0) {
  const h = lastHealth || {};
  // The runtime a sign-in is for: the one a new box would start with.
  const runtime = settings.default_agent || runtimes[0]?.id || "claude";
  const steps = [
    // `level`, not `ok`: a check carries no `ok` field, so this step could never tick and the list
    // could never read "ready" (SKEIN-1184 found it on the way past).
    { done: h.sbx?.level === "satisfied",
      label: "sbx is installed and answering",
      hint: `it runs the fleet — <a href="https://docs.docker.com/ai/sandboxes/" target="_blank" rel="noreferrer">install it</a>, and make sure Docker is running` },
    // Opens the same login terminal as the expired-login banner and the board's `sign in` chip
    // (SKEIN-1183): one fix, from the cockpit, and nothing to run anywhere else.
    { done: (h.logins || []).length > 0,
      label: (h.logins || []).length ? `an agent is signed in (${h.logins.join(", ")})` : "sign an agent in",
      hint: `<a href="#" class="fr-login" onclick="openLoginTerminal(${esc(JSON.stringify(runtime))});return false">sign in to ${esc(runtime)}</a> — once for the whole fleet: every box inherits it, and without it each one comes up at a sign-in prompt and does nothing` },
    { done: repos.length > 0,
      label: repos.length ? `${repos.length} ${repos.length === 1 ? "repository" : "repositories"} registered` : "add a repository",
      hint: `<a href="#" onclick="addRepoPrompt();return false">add one</a> — a box is a branch of a repo, so there is nothing to launch without it` },
    // Asked, because it is no longer answered behind your back. The account token used to be seeded
    // by default, so a box could always push and this step would have been noise; making the broadest
    // credential a deliberate choice means an unconfigured fleet cannot push at all, and a first run
    // that discovers that from a failed push an hour later is the outcome this step exists to prevent.
    { done: !!h.git_credential,
      label: h.git_credential ? `boxes can push (${h.git_credential})` : "choose how boxes push",
      hint: `<a href="#" onclick="openSettings('github');return false">pick a credential</a> — a GitHub App (one key, a token per repo), a per-repo token, or this account's <code>gh</code> token. Until then boxes read public repos and cannot push` },
  ];
  // **The warden is information, not a step** (SKEIN-1184, `docs/decisions/warden-or-prompt.md`).
  // It used to gate "ready" on the reasoning that a first Launch created the fleet through it; a
  // Launch creates no fleet (SKEIN-627), and without a warden skein shows the person the command
  // instead. So it is said — the health check's own sentence, which names where it looked and what
  // a warden adds — and counted in nothing.
  const warden = h.warden ? `
    <li class="fr-step fr-info">
      <span class="fr-tick">·</span>
      <span class="fr-txt"><b>host warden — optional</b><span class="fr-hint">${esc(h.warden.detail || "")}</span></span>
    </li>` : "";
  const rows = steps.map(s => `
    <li class="fr-step ${s.done ? "done" : ""}">
      <span class="fr-tick">${s.done ? "✓" : "○"}</span>
      <span class="fr-txt"><b>${s.label}</b>${s.done ? "" : `<span class="fr-hint">${s.hint}</span>`}</span>
    </li>`).join("") + warden;
  const ready = steps.every(s => s.done);
  // Counted, not spelled out. It said "three things" while listing four the moment credentials became
  // a step — the kind of wrong that makes a first run feel unmaintained before it has done anything.
  const left = steps.filter(s => !s.done).length;
  const head = ready
    ? "ready — launch your first box"
    : `${["", "one thing", "two things", "three things", "four things", "five things"][left] || `${left} things`} and you are running`;
  return `<div class="fr">
    <div class="fr-head">${head}</div>
    <ol class="fr-steps">${rows}</ol>
    ${ready ? `<button type="button" class="kbtn primary" onclick="openNewBox()">Launch a box</button>` : ""}
    <div class="fr-foot">not sure what this looks like with a fleet? <a href="?demo">see the demo</a>${
      foreignN ? ` · this machine has ${foreignN} sandbox${foreignN === 1 ? "" : "es"} skein did not create — <a href="#" onclick="setFilter('foreign:');return false">show them</a>` : ""
    }</div>
  </div>`;
}

function gateLaunch() {
  const go = document.getElementById("nb-go");
  const hint = document.querySelector("#newbox .nb-foot .hint");
  const none = !repos.length;
  go.disabled = none;
  if (!hint) return;
  if (none) {
    hint.innerHTML = `add a repository first — a box is a branch of one`;
    return;
  }
  // The real name, not a placeholder shaped like one. `<repo>-<branch>` in angle brackets is the
  // documentation of a naming rule; what someone about to click Launch wants is the string that is
  // going to exist, because that string is also the box's binding to its repo — get it wrong and
  // the launcher refuses a box the dialog itself named.
  const branch = (nbBranch.value || "").trim();
  const name = `${currentRepoId()}-${branch ? slug(branch) : "…"}`;
  hint.innerHTML = `creates <code id="nb-cmd">${esc(name)}</code> → opens in the dock`;
}

// Blank on every open, because these are a per-box exception: carrying the last box's identity
// forward is how the wrong name ends up on a commit nobody checked.
function clearIdentityFields() {
  document.getElementById("nb-gitname").value = "";
  document.getElementById("nb-gitemail").value = "";
}
// The tracking choice belongs to the box, not only to its repo: two boxes on the same repo can be
// one claiming from the backlog and one doing something nobody filed. "Repo default" stays the
// first option so the common case is still one keystroke.
// Usage is measured, not enforced: the fleet's disk is one filesystem shared by every box, so this
// says who is taking the space rather than stopping them. Click to change just this box's allowance.
function diskChip(el, b) {
  const used = b.disk_mb, cap = b.disk_limit_mb;
  if (used == null || !cap || used < cap * 0.8) { el.style.display = "none"; el.textContent = ""; return; }
  el.style.display = "";
  el.className = `dchip ${used >= cap ? "over" : "near"}`;
  el.textContent = `${fmtGb(used)}/${fmtGb(cap)}`;
  el.title = `${used >= cap ? "over" : "near"} this box's ${fmtGb(cap)} share of the fleet's shared disk`
    + `\n\nnothing stops it — one filesystem serves every box, so this is a measurement, not a quota`
    + `\n\nclick to change this box's allowance`;
}
// ---------- a box's resources, on hover ----------
//
// Disk is why this exists. It is the one thing the fleet actually runs out of — the sandbox hit 100%
// mid-build while one box transiently took 25 GB — and until now the only way to see a box's share
// was ⌘K → "Load by box", which is a thing you have to already know about. The chip on the row still
// only speaks near the limit, deliberately; this is for when you simply want to look.
//
// CPU and memory come from `/api/fleet/load`, which execs into the sandbox and samples cgroups over
// half a second. Far too expensive per hover, so it is fetched at most once every 15s and reused.
// Disk needs no fetch at all: it rides on `/api/boxes`, which the board already polls.
let loadCache = { at: 0, rows: [], inflight: null };
function loadRows() {
  const fresh = Date.now() - loadCache.at < 15000;
  if (fresh) return Promise.resolve(loadCache.rows);
  if (loadCache.inflight) return loadCache.inflight;
  loadCache.inflight = fetch("/api/fleet/load")
    .then(r => (r.ok ? r.json() : []))
    .catch(() => [])
    .then(rows => { loadCache = { at: Date.now(), rows, inflight: null }; return rows; });
  return loadCache.inflight;
}

let rpopFor = "", rpopTimer = 0;
function rpopEl() {
  let el = document.getElementById("rpop");
  if (!el) { el = document.createElement("div"); el.id = "rpop"; document.body.append(el); }
  return el;
}
function hideResourcePop() {
  clearTimeout(rpopTimer); rpopTimer = 0; rpopFor = "";
  const el = document.getElementById("rpop");
  if (el) el.classList.remove("on");
}
// Armed from `mousemove` rather than `mouseenter` for the same reason the row's selection is: rows
// shift under a stationary cursor every tick, so `mouseenter` fires on whichever row arrives under
// it. The delay is what keeps a cursor crossing the board from opening ten cards on the way past.
function armResourcePop(name, row) {
  if (rpopFor === name) return;
  clearTimeout(rpopTimer);
  rpopTimer = setTimeout(() => { rpopFor = name; showResourcePop(name, row); }, 280);
}
function resourceRows(name) {
  const b = boxes.find(x => x.name === name) || {};
  const r = loadCache.rows.find(x => x.name === name);
  const used = b.disk_mb, cap = b.disk_limit_mb;
  // Disk first: it is the reason to look, and the number people are hunting when a build dies with
  // no room left.
  const diskCls = (used != null && cap) ? (used >= cap ? "over" : used >= cap * 0.8 ? "near" : "") : "";
  const disk = used == null ? "—" : cap ? `${fmtGb(used)} / ${fmtGb(cap)}` : fmtGb(used);
  return `<div class="rp-n">${esc(name)}</div>`
    + `<div class="rp-r ${diskCls}"><span>disk</span><span>${disk}</span></div>`
    + `<div class="rp-r"><span>memory</span><span>${r ? fmtGB(r.mem_anon) : "…"}</span></div>`
    // The question the headline provokes, answered in the same panel: a box whose memory looks
    // small and whose charge is large has been reading files, and saying so is cheaper than
    // leaving somebody to wonder why the two disagree. Hidden when there is nothing to explain.
    + (r && r.mem_cache > (r.mem_anon || 0)
        ? `<div class="rp-r sub" title="page cache — files this box has read or written. The kernel hands it back the moment anything else needs it, so it is not memory the box is holding."><span>· cache</span><span>${fmtGB(r.mem_cache)}</span></div>`
        : "")
    + `<div class="rp-r"><span>cpu</span><span>${r ? r.cores.toFixed(1) + " cores" : "…"}</span></div>`
    + `<div class="rp-r"><span>processes</span><span>${r ? (r.pids | 0) : "…"}</span></div>`
    + `<div class="rp-w">disk is measured, not enforced — one filesystem serves every box</div>`;
}
function showResourcePop(name, row) {
  const el = rpopEl();
  el.innerHTML = resourceRows(name);
  const box = row.getBoundingClientRect();
  el.classList.add("on");
  // Placed after it has been measured, and flipped above the row when there is no room below —
  // otherwise the last row's card is drawn off the bottom of the window, which is exactly where the
  // longest-running boxes sit.
  const h = el.getBoundingClientRect().height;
  const top = box.bottom + 8 + h > window.innerHeight ? Math.max(8, box.top - h - 8) : box.bottom + 8;
  el.style.top = `${top}px`;
  el.style.left = `${Math.min(box.left + 22, window.innerWidth - el.getBoundingClientRect().width - 12)}px`;
  // The live half arrives when the sandbox answers; the card is already up with the disk figure, so
  // nothing waits on a round trip that may be several seconds behind an `sbx exec` fallback.
  loadRows().then(() => { if (rpopFor === name) el.innerHTML = resourceRows(name); });
}

// One box's own settings. Every field is blank-means-inherit, and the placeholder is what it would
// inherit — so leaving the panel untouched is always the same as never opening it.
let boxSettingsFor = "";
function openBoxSettings(name) {
  boxSettingsFor = name;
  Promise.all([loadSync(), fetch(`/api/boxes/${encodeURIComponent(name)}/settings`).then(r => r.json())])
    .then(([, cur]) => {
      document.getElementById("bs-title").textContent = name;
      const conns = syncStatus.connections || [];
      document.getElementById("bs-sync").innerHTML = [
        `<option value="">repo default${cur.repo_connection ? ` — ${esc(cur.repo_connection)}` : " — not tracked"}</option>`,
        `<option value="none">not tracked</option>`,
        ...conns.map(c => `<option value="${esc(c.id)}">${esc(c.label)}${c.ready ? "" : " (not ready)"}</option>`),
      ].join("");
      // Stored "" means "this box claims nowhere"; *absent* means it inherits. The API can only
      // send a string, so the panel asks whether an override exists at all.
      document.getElementById("bs-sync").value =
        cur.connection ? cur.connection : (cur.has_tracking_override ? "none" : "");
      const gn = document.getElementById("bs-gitname"), ge = document.getElementById("bs-gitemail");
      gn.value = cur.own_git_name || ""; gn.placeholder = cur.effective_git_name || "(unset)";
      ge.value = cur.own_git_email || ""; ge.placeholder = cur.effective_git_email || "(unset)";
      const d = document.getElementById("bs-disk");
      d.value = cur.own_disk || "";
      d.placeholder = cur.effective_disk_mb ? fmtGb(cur.effective_disk_mb) : "unlimited";
      // Three states, and the empty one is inheritance rather than a value — same grammar as work
      // tracking above. The default option names what inheriting currently *means*, because "fleet
      // default" alone does not tell you which way this box will come up.
      const gs = document.getElementById("bs-gitscope");
      const eff = cur.effective_git_scope === "repo" ? "its own repo only" : "the whole fleet credential";
      gs.innerHTML = [
        `<option value="">fleet default — ${esc(eff)}</option>`,
        `<option value="repo">its own repo — read others, ask to write them</option>`,
        `<option value="fleet">unscoped — the fleet-wide credential</option>`,
      ].join("");
      gs.value = cur.git_scope || "";
      // A mount namespace and a credential are both fixed as the box comes up, so unlike the fields
      // above this one is not live. Saying so here is cheaper than it reading as broken.
      document.getElementById("bs-gitscope-note").textContent = cur.git_scope_available
        ? "applies at this box's next start — a running box keeps the credential it was given"
        : "nothing to scope with yet: set up a GitHub App under Settings → GitHub & keys";
      // The workshop box. Ordinary boxes get a mount namespace that hides every other box's
      // directory and an empty file over the fleet agent's token; this is the box that opts out of
      // both, because debugging skein itself means reading the fleet.
      const pv = document.getElementById("bs-priv");
      pv.value = cur.privileged ? "1" : "0";
      // The terms, where the switch is (§9.5 R9). The mount cover being off is the one a person
      // cannot discover by using the box: it is what the guards on the git-token directory and the
      // resize archive lean on, and this switch turns it off for this box.
      document.getElementById("bs-priv-note").textContent = cur.privileged
        ? "this box reads every other box's credentials and conversations, and the mount cover is off for it — keep it to the one you develop skein in"
        : "applies at this box's next start — a namespace is built when a box comes up";
      // Work tracking: the button says what pressing it would DO, which depends on whether the box
      // already holds a token. Nothing host-side records that, so the panel asked the box.
      const wire = document.getElementById("bs-wire");
      const conn = cur.effective_connection;
      wire.disabled = !conn || tracking.has(name);
      wire.textContent = tracking.has(name) ? "wiring…"
        : !conn ? "◇ Not tracked"
        : cur.wired ? `◇ Re-mint token at ${conn}` : `◇ Wire up ${conn}`;
      wire.title = !conn
        ? "this box claims work from nowhere — pick a connection above first"
        : cur.wired
          ? `this box already holds a token for ${conn}; re-minting replaces it (its predecessor was this box's own)`
          : `mint this box its own token at ${conn} and register the sync server inside it`;
      wire.onclick = () => { trackWork(name); closeBoxSettings(); };

      // Runtime: the other one, or nothing to offer when there is no other.
      const take = document.getElementById("bs-take");
      const other = nextRuntime(cur.agent || agentOf(name));
      take.style.display = other ? "" : "none";
      if (other) {
        take.textContent = `↔ Replace with ${other.label}`;
        take.title = `snapshot this box and create a ${other.label} replacement; this one stays intact as rollback`;
        take.onclick = () => { closeBoxSettings(); takeOver(name, other.id); };
      }
      document.getElementById("bs-note").textContent =
        "Blank inherits the fleet default; `none` means unlimited. Disk is measured, not enforced — one filesystem serves every box.";
      document.getElementById("boxset").classList.add("open");
    })
    .catch(e => toast(`settings: ${e.message}`));
}
function closeBoxSettings() { document.getElementById("boxset").classList.remove("open"); }
function saveBoxSettings() {
  const name = boxSettingsFor;
  const sync = document.getElementById("bs-sync").value;
  const gitName = document.getElementById("bs-gitname").value.trim();
  const gitEmail = document.getElementById("bs-gitemail").value.trim();
  const disk = document.getElementById("bs-disk").value.trim();
  const gitScope = document.getElementById("bs-gitscope").value;
  const privileged = document.getElementById("bs-priv").value === "1";
  const post = (path, body) => fetch(`/api/boxes/${encodeURIComponent(name)}/${path}`, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
  }).then(async r => { if (!r.ok) throw new Error((await r.text()) || r.statusText); });
  // Absent vs empty is the whole grammar here: cleared field ⇒ inherit again, not "set to nothing".
  Promise.all([
    post("tracking", { connection: sync === "" ? null : (sync === "none" ? "" : sync) }),
    post("identity", (gitName || gitEmail) ? { name: gitName, email: gitEmail } : {}),
    post("disk", { limit: disk || null }),
    post("git-scope", { scope: gitScope || null }),
    post("privileged", { on: privileged }),
  ])
    .then(() => { toast(`${name}: settings saved`); closeBoxSettings(); refresh(); })
    .catch(e => toast(`settings: ${e.message}`));
}
function askDiskLimit(name) { openBoxSettings(name); }
// The launch dialog's copy of the box-settings control. Empty is inheritance, and the label says
// what inheriting currently means — a bare "fleet default" would not tell you which way this box
// comes up, which is the only thing the person launching it wants to know.
function fillGitScopeSelect() {
  const el = document.getElementById("nb-gitscope");
  if (!el) return;
  // `scope_git_to_repo` is what the fleet *asks for*; whether it takes effect also depends on there
  // being a way to issue a write token at all. Reading the setting alone told a user their box
  // would be scoped when it would not — the one dialog where that claim is load-bearing, since it
  // is where the choice is made. `/api/fleet/git-grants` carries the effective answer.
  const on = !!settings.scope_git_to_repo && gitScopeEffective !== false;
  el.innerHTML = [
    `<option value="">fleet default — ${on ? "its own repo only" : "the whole fleet credential"}</option>`,
    `<option value="repo">its own repo — read others, ask to write them</option>`,
    `<option value="fleet">unscoped — the fleet-wide credential</option>`,
  ].join("");
  el.value = "";
}

// Whether anything can issue a write token — undefined until asked, so a slow answer never flips
// the label to the wrong one on the way past.
let gitScopeEffective;
function loadGitScopeEffective() {
  return fetch("/api/fleet/git-grants").then(r => r.json())
    .then(d => { gitScopeEffective = !!d.ready; })
    .catch(() => {});
}

function fillSyncSelect() {
  const el = document.getElementById("nb-sync");
  const repo = (repos || []).find(r => r.id === currentRepoId());
  const conns = (syncStatus.connections || []);
  const inherited = conns.find(c => c.id === (repo || {}).sync_connection);
  const opts = [
    `<option value="">repo default${inherited ? ` — ${esc(inherited.label)}` : " — not tracked"}</option>`,
    `<option value="none">not tracked</option>`,
    ...conns.map(c => `<option value="${esc(c.id)}">${esc(c.label)}${c.ready ? "" : " (not ready)"}</option>`),
  ];
  el.innerHTML = opts.join("");
}
// Open the new-box dialog with a specific repo preselected (the per-repo ＋ in the sidebar).
// Open the New box dialog already pointed at a repo, and optionally at a branch that exists.
//
// The branch argument is what the review pane uses to put hands on a PR. It deliberately goes
// through this dialog rather than launching directly: agent, work tracking, committer and git scope
// are all decisions the launcher reads *as the box comes up*, and a second launch path would be a
// second place for those to be forgotten.
function openNewBoxFor(repoId, branch) {
  newbox.classList.add("open"); nbBranch.value = branch || ""; clearIdentityFields();
  Promise.all([loadRepos(), loadSync()]).then(() => {
    if ([...nbRepo.options].some(o => o.value === repoId)) nbRepo.value = repoId;
    fillSyncSelect(); fillGitScopeSelect();
    if (typeof gateLaunch === "function") gateLaunch();
    nbBranch.focus(); nbBranch.select();
  });
}
function closeNewBox() { newbox.classList.remove("open"); }
// Prompt for a repo URL/path and register it, then refresh the repo list.
// Derive the repo id the server would pick (mirror of lib::repo_id_from_source) so we can preview it.
function deriveRepoId(src) {
  src = (src || "").trim().replace(/\/+$/, "");
  if (!src) return "";
  const last = src.split(/[/:]/).pop() || src;
  return last.replace(/\.git$/, "");
}
const arModal = () => document.getElementById("addrepo");
function arSetMsg(text, kind) {        // kind: "" | "err" | "warn" | "ok"
  const el = document.getElementById("ar-msg");
  if (!text) { el.style.display = "none"; el.textContent = ""; el.className = "ar-msg"; return; }
  el.style.display = ""; el.textContent = text; el.className = "ar-msg " + (kind || "");
}
function openAddRepo() {
  const src = document.getElementById("ar-src");
  src.value = ""; arSetMsg(""); arWarnedHost = "";
  document.getElementById("ar-token").value = "";
  document.getElementById("ar-plane").value = "";
  // Off, matching `repos::add`. A new repo is a repo nobody has said they review yet, and the
  // queue is the one setting here that spends something on its own every three minutes.
  document.getElementById("ar-review").value = "false";
  // The connection list is whatever Work tracking currently knows about — fetched here rather than
  // read from whatever the last Settings visit left behind, because this dialog opens from the
  // header too, and offering "Not tracked" as the only choice would be a lie about the fleet.
  arRenderConns();
  loadSync().then(arRenderConns);
  arUpdateDerived();
  document.getElementById("ar-go").textContent = "Add →";
  arModal().classList.add("open");
  setTimeout(() => src.focus(), 0);
}
function closeAddRepo() { arModal().classList.remove("open"); }
// Re-drawn rather than patched, so the late fetch can replace an empty list without disturbing a
// choice already made: whatever was selected is restored if it still exists.
function arRenderConns() {
  const el = document.getElementById("ar-conn");
  if (!el) return;
  const was = el.value;
  el.innerHTML = [`<option value="">Not tracked</option>`].concat((syncStatus.connections || []).map(c =>
    `<option value="${esc(c.id)}">${esc(c.ready ? c.label : c.label + " (not usable yet)")}</option>`)).join("");
  if (was && [...el.options].some(o => o.value === was)) el.value = was;
}
function arUpdateDerived() {
  const source = document.getElementById("ar-src").value;
  const id = deriveRepoId(source);
  const d = document.getElementById("ar-derived");
  d.innerHTML = id ? `box names will be <b>${esc(id)}</b>-&lt;branch&gt;` : "";
  // A registrable remote need not be a GitHub one — a GitLab or self-hosted URL parses to no slug
  // here — so the field stays usable and the host tells us which repository the token was stored
  // against once the repo exists. Disabling it here is what left such a repo unable to push.
  const slug = repoSlug(source);
  const tok = document.getElementById("ar-token");
  const hint = document.getElementById("ar-tokhint");
  const noSlug = !!source.trim() && !slug;
  tok.disabled = false;
  tok.placeholder = "github_pat_… — leave blank if a GitHub App covers it";
  hint.innerHTML = noSlug
    ? "a fine-grained PAT for this repo, which its boxes push with. Which repository that is comes from the remote skein clones — if that is no GitHub remote, the token is not stored and skein says so."
    : `a fine-grained PAT covering only <b>${slug ? esc(slug) : "this repo"}</b>, which its boxes push with. On the App path you need none.`;
}
// The source the non-GitHub warning was last shown for: pressing Add again on the same source is
// the "anyway" (SKEIN-812). Cleared when the dialog opens, so a new visit asks again.
let arWarnedHost = "";
function submitAddRepo() {
  const src = document.getElementById("ar-src").value.trim();
  if (!src) { arSetMsg("Paste the repo's git URL.", "err"); return; }
  if (DEMO) { arSetMsg("Adding repos is disabled in demo.", "warn"); return; }
  // Said BEFORE the clone rather than after it: the clone is the expensive part, and a repo that
  // cannot push and has no queue is a thing somebody may not want once they know.
  const host = nonGitHubHost(src);
  if (host && arWarnedHost !== src) {
    arWarnedHost = src;
    arSetMsg(`${host} is not GitHub: boxes of this repo can commit but not push, and it has no review queue. Add it anyway?`, "warn");
    return;
  }
  const go = document.getElementById("ar-go");
  // `add_repo` clones inline with a 300s bound, so this can sit here for minutes on a large repo.
  // A static "Adding…" for four of them is indistinguishable from a hang, and the honest reading of
  // a frozen button is that you should press it again — which is the one thing that must not happen.
  // Streaming git's own progress is the better answer and a bigger change; until then, say that it
  // is working, how long it has been, and that minutes are normal.
  go.textContent = "Adding…"; go.disabled = true; arSetMsg("");
  const began = performance.now();
  const tick = setInterval(() => {
    const secs = Math.round((performance.now() - began) / 1000);
    go.textContent = `Cloning… ${secs}s`;
    if (secs === 8) arSetMsg("cloning — a large repo can take a few minutes. Leave this open.", "warn");
    if (secs === 240) arSetMsg("still cloning — this gives up at 5 minutes.", "warn");
  }, 1000);
  const settle = () => { clearInterval(tick); go.disabled = false; };
  // No `store`: the route refuses one outright (SKEIN-535), and adopting an existing store is a
  // CLI-only affordance now — `skein add <git-url> --store <path>`.
  fetch("/api/repos", { method:"POST", headers:{"Content-Type":"application/json"}, body: JSON.stringify({ source: src }) })
    .then(async r => { if (!r.ok) throw new Error((await r.text()) || r.statusText); return r.json(); })
    .then(res => {
      settle();
      // The host resolved which GitHub repository this is, from the URL it cloned — carried on the
      // repo so the token below is stored against the same repository the box will be handed a
      // token for.
      const repo = Object.assign({}, res.repo || res, { slug: res.slug || "" });
      // Applied here rather than sent with the clone: every one of these keys on the repo's id, and
      // the server picks that id — deriving it in the page would be a second implementation of a
      // rule that already exists, wrong exactly when they disagree.
      go.textContent = "Setting up…";
      return arApplySettings(repo).then(problems => {
        loadRepos();
        const notes = [res.warning, ...problems].filter(Boolean);
        if (notes.length) {         // e.g. SSH origin — keep the modal up so the note is readable
          arSetMsg(notes.join(" · "), "warn");
          go.textContent = "Done"; go.onclick = closeAddRepo;
        } else {
          closeAddRepo(); toast(`added ${repo.id}`);
        }
      });
    })
    .catch(e => {
      settle();
      // A timeout leaves a half-made repo directory behind, and "it failed" without that is how the
      // next attempt fails too, for a reason nobody can see.
      const timedOut = /timed out|deadline|timeout/i.test(e.message || "");
      arSetMsg(
        timedOut
          ? `${e.message} — the clone was still running when skein gave up. Check ~/.skein/repos for a partial copy before retrying.`
          : `Couldn't add it: ${e.message}`,
        "err",
      );
      go.textContent = "Add →";
    });
}
// The repo-specific settings, applied to a repo that now exists. Resolves to a list of things that
// did not take — never rejects, and never unmakes the repo. The clone is the expensive, irreversible
// part and it succeeded; a tracker field that failed to save is a thing you fix on the card in ten
// seconds, and failing the whole add over one would throw away the minutes that actually cost.
function arApplySettings(repo) {
  const problems = [];
  const plane = (document.getElementById("ar-plane").value || "").trim();
  const conn = document.getElementById("ar-conn").value;
  const review = document.getElementById("ar-review").value === "true";
  const token = (document.getElementById("ar-token").value || "").trim();
  const body = {};
  if (plane) body.plane_project = plane;
  if (conn) body.sync_connection = conn;
  // Sent EXPLICITLY, both ways. It used to send only a deliberate off, riding on the server's
  // default — and that default has just moved, which is exactly the argument against leaning on
  // one: `repos::add` now registers a new repo with the queue off, while serde's default for an
  // ABSENT field stays true so that an upgrade cannot switch off a queue somebody has been using.
  // Two different questions, two different answers, and a body that omits the field cannot say
  // which of them it meant.
  body.review_queue = review;
  const jobs = [];
  if (Object.keys(body).length) {
    jobs.push(fetch(`/api/repos/${encodeURIComponent(repo.id)}/settings`, {
      method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
    }).then(async r => { if (!r.ok) problems.push(`settings not saved: ${(await r.text()) || r.statusText}`); })
      .catch(e => problems.push(`settings not saved: ${e.message}`)));
  }
  if (token) {
    const slug = repo.slug || "";
    if (!slug) problems.push("no write token stored — this repo has no GitHub remote to scope one to");
    else jobs.push(storeGitCred(slug, token).catch(e => problems.push(`token not stored: ${e.message}`)));
  }
  return Promise.all(jobs).then(() => problems);
}
// kept name for existing callers (header ＋repo, per-section ＋, settings, palette) → opens the modal
function addRepoPrompt() { openAddRepo(); }
// `slug` is NOT declared here. It comes from `/vendor/cockpit.js` (`cockpit/src/naming.mjs`), which
// is loaded above and is a classic script, so its top-level `function slug` is this scope's too.
// There was a second copy here — the same rule written as a regex — and the two collided the day the
// bundle grew one: both are top-level in the same global lexical scope, so `const` after `function`
// is `SyntaxError: Identifier 'slug' has already been declared`, which killed the WHOLE inline
// script and rendered a blank board. `tests/page_scripts.rs` now checks the page the way the browser
// parses it, because the failure is invisible to `cargo test` and to every check that reads one
// file at a time.
// ---------- what the fleet sandbox is, read for the settings pane ----------
//
// **This is no longer a sizing surface, and the dialog that was one is deleted** (SKEIN-627). The
// cockpit used to create the fleet: the first Launch found `exists === false`, asked what the
// sandbox may take of the machine, and created it at that size. Skein runs inside the sandbox now,
// so `fleet_exists` can only answer `Some(true)` about the fleet it is standing in — `exists ===
// false` stopped being a state the wire could carry, and the dialog stopped being openable.
//
// What is left of the plan is prose and numbers the settings pane renders: the host's capacity
// beside the fleet fields, why sbx could not be asked, and the `sbx` lines to run on the host in
// place of the rebuild button (`renderFleetPlanNotes`).
let fleetPlan = null;
async function loadFleetPlan() {
  // In demo mode there is no host to measure, and a capacity line about a machine that does not
  // exist is worse than a blank one.
  if (DEMO) { fleetPlan = null; return null; }
  try {
    const r = await fetch("/api/fleet/plan");
    fleetPlan = r.ok ? await r.json() : null;
  } catch { fleetPlan = null; }
  return fleetPlan;
}
const gbOf = mb => mb ? `${Math.round(mb / 1024)} GB` : "unknown";

function launchBox(branch) {
  branch = (branch||"").trim(); if (!branch) return;   // keep slashes — they belong to the branch
  // Here rather than only on the button, because the button was not the only way in: Enter in the
  // branch field called this directly, so a disabled Launch stopped nothing. With no repo the name
  // falls back to the literal "box", and `box-my-feature` belongs to no registered repo — the
  // launcher refuses it, after the dialog has closed and a dock tab has opened onto the refusal.
  // A box IS a branch of a repo; there is nothing to create without one.
  if (!repos.length) {
    toast("add a repository first — a box is a branch of one");
    openAddRepo();
    return;
  }
  // **Nothing about the fleet is asked here, and there is nothing left to ask.** This used to open
  // a create-fleet dialog when the plan's `exists` came back false — sized by the person, and only
  // then letting the launch through. That branch was written for a skein that existed before its
  // fleet did; skein runs INSIDE the sandbox now, `bootstrap.sh` makes the sandbox first, and
  // `fleet::fleet_exists` answers `Some(true)` for the configured name and `None` for every other.
  // So the false the branch waited for was a state the wire could no longer carry, and the dialog
  // could no longer open (SKEIN-627).
  const id = currentRepoId();
  const agent = nbAgent.value || settings.default_agent || runtimes[0]?.id;
  const name = `${id}-${slug(branch)}`;                 // e.g. thing-feat-auth
  const sync = document.getElementById("nb-sync").value;
  const gitName = document.getElementById("nb-gitname").value.trim();
  const gitEmail = document.getElementById("nb-gitemail").value.trim();
  const gitScope = document.getElementById("nb-gitscope").value;
  closeNewBox();
  select(name);
  view = { box: name, mode: "term", kind: "agent" };
  // Before the launch, not after: provisioning reads the choice, so a token minted against the
  // repo's default and corrected afterwards would already be the wrong one.
  const post = (path, body, what) =>
    fetch(`/api/boxes/${encodeURIComponent(name)}/${path}`, {
      method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
    }).catch(e => toast(`could not set ${what}: ${e.message}`));
  const chosen = Promise.all([
    sync === "" ? null : post("tracking", { connection: sync === "none" ? "" : sync }, "work tracking"),
    (gitName || gitEmail) ? post("identity", { name: gitName, email: gitEmail }, "the committer") : null,
    // Same reason as the two above, and more sharply: the launcher reads this as the box comes up to
    // decide which credential to place. Set afterwards it would apply at the *next* start, so the
    // box's first turn — the one most likely to push — would run under the choice not made.
    gitScope === "" ? null : post("git-scope", { scope: gitScope }, "github access"),
  ].filter(Boolean));
  chosen.then(() => { if (!sessions.has(name)) createSession(name, "agent", branch, agent); });
  applyView();
}
document.getElementById("set-go").addEventListener("click", saveSettings);
document.getElementById("set-applylimits").addEventListener("click", applyBoxLimits);
document.getElementById("set-save-go").addEventListener("click", saveBoxes);
// Fields that are not part of the settings form even though they sit in it: a credential is stored
// by its own button, through its own write-only route, and never round-trips through `config.json`.
// So Enter in one must store *that* — not save the form and drop what was typed — and typing in one
// must not raise an unsaved-changes flag that its Save button has nothing to do with.
const OWN_SAVE = "#set-c-repo, #set-c-token, #set-readpat, [data-repotoken]";
function ownSaveAction(el) {
  if (el.matches("#set-c-repo, #set-c-token")) return addGitCred;
  if (el.matches("#set-readpat")) return saveReadPat;
  if (el.matches("[data-repotoken]")) {
    const btn = document.querySelector(`[data-storetoken="${CSS.escape(el.dataset.repotoken)}"]`);
    return btn ? () => btn.click() : null;
  }
  return null;
}
settingsModal().addEventListener("keydown", e => {
  if (e.key === "Enter" && !e.target.closest(".set-navi")) {
    e.preventDefault();
    const own = e.target.matches?.(OWN_SAVE) ? ownSaveAction(e.target) : null;
    (own || saveSettings)();
  } else if (e.key === "Escape") { e.preventDefault(); closeSettings(); }
  e.stopPropagation();
});
// Section nav, and any edit flips the footer's unsaved-changes flag.
for (const b of settingsModal().querySelectorAll(".set-navi")) b.addEventListener("click", () => setPane(b.dataset.pane));
const formEdit = e => e.target.closest(".set-pane") && !e.target.matches?.(OWN_SAVE);
settingsModal().addEventListener("input", e => { if (formEdit(e)) markDirty(true); });
settingsModal().addEventListener("change", e => {
  if (formEdit(e)) markDirty(true);
  // The status line answers "is this actually on?", and the switch is half that answer — so it
  // repaints as the switch moves, rather than only after a save and reopen.
  if (e.target.id === "set-gitscope") renderGitState(gitLast);
});
// Click the backdrop (not the dialog) to dismiss — expected of a modal, and it was missing.
settingsModal().addEventListener("mousedown", e => { if (e.target === settingsModal()) closeSettings(); });
document.getElementById("nb-repo").addEventListener("change", () => { fillSyncSelect(); gateLaunch(); });
nbBranch.addEventListener("input", gateLaunch);
document.getElementById("bs-go").addEventListener("click", saveBoxSettings);
document.getElementById("boxset").addEventListener("mousedown", e => { if (e.target.id === "boxset") closeBoxSettings(); });
document.getElementById("nb-go").addEventListener("click", () => launchBox(nbBranch.value));
nbBranch.addEventListener("keydown", e => {
  if (e.key === "Enter") { e.preventDefault(); launchBox(nbBranch.value); }
  else if (e.key === "Escape") { e.preventDefault(); closeNewBox(); }
  e.stopPropagation();
});
document.getElementById("ar-go").addEventListener("click", submitAddRepo);
document.getElementById("ar-src").addEventListener("input", arUpdateDerived);
nbRepo.addEventListener("change", fillRepoSelect);
// **Nothing on this page asks the server about a typed path, and nothing can** (SKEIN-947). The
// probe's last field was the SSH key path, which named a file on the host that skein in the fleet
// cannot see, so every answer it gave was about the wrong machine. The field and `/api/path` went
// together, and Settings now says to run `ssh-add` on the host. The add-repo source field never
// probed (SKEIN-806): a repo is a remote, and "found: folder" affirmed what add_repo refuses.
document.getElementById("set-conn-add").addEventListener("click", addConnection);
arModal().addEventListener("keydown", e => {
  if (e.key === "Enter") { e.preventDefault(); submitAddRepo(); }
  else if (e.key === "Escape") { e.preventDefault(); closeAddRepo(); }
  e.stopPropagation();
});

// ---------- terminal clipboard ----------
// xterm renders its selection on a canvas (no DOM selection) and, after a mouse-drag select, focus
// sits on document.body — not xterm's hidden textarea — so a key handler bound to the terminal never
// sees Cmd+C and the native copy beeps on the empty body. We catch copy/paste at the document in the
// CAPTURE phase (runs before any focused element) and act on the *active* terminal's selection.
// Last text the active terminal offered via OSC 52 (copy-on-select under mouse mode). The async
// escape can't write the clipboard itself (no gesture), so Cmd+C commits this within its gesture.
let oscClipboard = "";
// Copy synchronously via execCommand inside the keydown gesture (works even when the Clipboard API
// rejects with "document not focused" or is absent in a non-secure context); fall back to the
// Clipboard API. Returns true if either path reports success.
function copyText(text) {
  let ok = false;
  const active = document.activeElement;
  const ta = document.createElement("textarea");
  ta.value = text; ta.style.position = "fixed"; ta.style.top = "-1000px"; ta.style.opacity = "0";
  ta.setAttribute("readonly", "");
  document.body.append(ta); ta.focus(); ta.select();
  try { ok = document.execCommand("copy"); } catch {}
  ta.remove();
  try { active && active.focus && active.focus(); } catch {}
  if (!ok && navigator.clipboard?.writeText) { navigator.clipboard.writeText(text).then(() => {}, () => {}); ok = true; }
  return ok;
}
document.addEventListener("keydown", e => {
  const s = (view.mode === "term") ? activeSession() : null;
  if (!s) return;
  const c = e.key === "c" || e.key === "C", v = e.key === "v" || e.key === "V";
  const copy  = (e.metaKey && !e.ctrlKey && c) || (e.ctrlKey && e.shiftKey && c);
  const paste = (e.metaKey && !e.ctrlKey && v) || (e.ctrlKey && e.shiftKey && v);
  if (copy) {
    // Always swallow the Cmd+C chord (never SIGINT on macOS — that's Ctrl+C, unmatched here) so the
    // browser's empty native copy can't beep. Copy-on-select already works via OSC 52; this also
    // covers the Shift-bypass path where holding Shift yields a native selection.
    e.preventDefault(); e.stopPropagation();
    // Prefer a live selection (Shift-bypass native or xterm); fall back to the text the TUI offered
    // via OSC 52 on select. copyText runs synchronously inside this gesture, so the write is allowed.
    const sel = s.term.getSelection() || (window.getSelection ? window.getSelection().toString() : "") || oscClipboard;
    if (sel) { const ok = copyText(sel); toast(ok ? `copied ${sel.length} chars` : "copy failed"); }
  } else if (paste) {
    // Don't intercept — calling clipboard.readText() triggers Safari's paste-permission button.
    // Just ensure the terminal owns focus so the browser's native paste event lands on xterm, which
    // pastes (with bracketed-paste) needing no prompt. No preventDefault: let that native paste run.
    s.term.focus();
  }
  // bare Ctrl+C (no shift, no meta) is NOT matched here → still reaches the shell as SIGINT
}, true);

