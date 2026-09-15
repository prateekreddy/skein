// Granting write access hands a box a real credential for someone else's repository, so what the
// button sends has to be exactly what its owner chose — and it has to say so only once.
//
// Five things are worth testing and none needs a browser:
//
//   1. **What the grant carries.** The default expires; "keep indefinitely" is the deliberate act.
//      The two controls must not be able to disagree — a ticked keep with `12` still in the hours
//      box has to mean one thing, not two. A denial must never carry a duration that could be read
//      as an approval by a later change to the endpoint.
//   2. **How often it announces.** A request sits pending until a person answers it, which is the
//      exact shape that produced endless re-announcing once already.
//   3. **What an unconfigured fleet says.** With no GitHub App, nothing is scoped and no box has
//      lost anything — an empty panel must say which of those it is showing.
//   4. **Which repository the page thinks a source names.** `repoSlug` decides which field the
//      settings screen offers; `gitgate::slug_from_url` decides which token the host mints. Two
//      implementations of one rule, so the divergence is the thing under test — a form that accepts
//      a token for a repository the host will never issue one for is the failure.
//   5. **That a repo survives its own setup.** The add dialog now applies tracker settings and a
//      token after the clone. The clone is the expensive, irreversible part: a settings call that
//      fails must be reported, never allowed to unmake the repository.
//
//   node tests/ui/gitgate.mjs
import { grab, harness, page } from "./lift.mjs";

const source = [
  // `gitqShown` is lifted with the function that owns it. It is module state — what each pending
  // request looked like when it was last seen — and `decideGitq` closes over it, so lifting the
  // function alone gave `ReferenceError: gitqShown is not defined` and this suite stopped running
  // entirely (SKEIN-113). Same reason `voice.mjs` lifts `awaySince`.
  "gitqShown", "gitqAnnounced", "gitqCreds", "gitqPrimed", "decideGitq", "pollGitq", "paintGitqBadge", "gitqCard",
  "gitqGrantRow", "revokeGitq", "gitCredRow", "editGitCred", "credId", "credFor", "storeGitCred",
  "addGitCred", "removeGitCred", "renderGitState", "nameable", "slugFromPath", "repoSlug",
  "repoTokenRow", "arApplySettings",
].map(grab).join("\n");

const scope = new Function(`
  const sent = [], notes = [];
  let alertsOn = true;
  let keep = { checked: false };
  let hours = { value: "24" };
  let panelOpen = false;
  let fields = { repo: { value: "" }, token: { value: "", placeholder: "", focus: () => {}, scrollIntoView: () => {} } };
  // review "false" mirrors what the dialog opens with — a new repo starts with its queue OFF
  // (SKEIN-270). A fixture that opened it ON would be testing a form nobody sees. No backticks in
  // here: this whole body is a template literal, and one would end it mid-fixture.
  let form = { plane: { value: "" }, conn: { value: "" }, review: { value: "false" }, token: { value: "" } };
  let state = { className: "", innerHTML: "" };
  let scoped = { checked: true };
  let payload = { requests: [], grants: [], app_ready: true, app_problem: "" };
  let failing = null;
  ${grab("esc")}
  const toast = m => notes.push("toast:" + m);
  const pushNote = (body, tag) => notes.push({ body, tag });
  const badge = { n: null, title: "" };
  const document = {
    getElementById: id => {
      if (id === "gitq") return { classList: { contains: () => panelOpen } };
      if (id.startsWith("gq-keep-")) return keep;
      if (id.startsWith("gq-h-")) return hours;
      if (id === "set-c-repo") return fields.repo;
      if (id === "set-c-token") return fields.token;
      if (id === "set-gitstate") return state;
      if (id === "set-gitscope") return scoped;
      if (id === "ar-plane") return form.plane;
      if (id === "ar-conn") return form.conn;
      if (id === "ar-review") return form.review;
      if (id === "ar-token") return form.token;
      if (id === "gitqbtn") return {
        querySelector: () => (badge.n === null ? null : { remove: () => { badge.n = null; } }),
        appendChild: el => { badge.n = el.textContent; },
        set title(v) { badge.title = v; },
        get title() { return badge.title; },
      };
      return null;
    },
    createElement: () => ({ className: "", textContent: "" }),
  };
  const fetch = (url, opts) => {
    if (opts && opts.method === "POST") {
      sent.push({ url, id: decodeURIComponent(url.split("/").pop()), body: JSON.parse(opts.body) });
      if (failing && url.includes(failing)) {
        return Promise.resolve({ ok: false, statusText: "boom", text: () => Promise.resolve("boom") });
      }
      return Promise.resolve({ ok: true });
    }
    if (opts && opts.method === "DELETE") {
      sent.push({ deleted: url });
      return Promise.resolve({ ok: true });
    }
    return Promise.resolve({ ok: true, json: () => Promise.resolve(payload) });
  };
  const loadGitCreds = () => { notes.push("reloaded"); return Promise.resolve(payload); };
  const loadRepos = () => Promise.resolve();
  ${source}
  return {
    decideGitq, pollGitq, gitqCard, gitqGrantRow, revokeGitq, gitCredRow, editGitCred,
    addGitCred, removeGitCred, renderGitState, repoSlug, repoTokenRow, arApplySettings, credFor,
    fields: () => fields,
    form: () => form,
    state: () => state,
    setCreds: c => { gitqCreds = c; },
    setScoped: v => { scoped.checked = v; },
    failOn: u => { failing = u; },
    sent: () => sent,
    notes: () => notes,
    badge: () => badge,
    setPayload: p => { payload = p; },
    setKeep: v => { keep.checked = v; },
    setHours: v => { hours.value = v; },
    reset: () => {
      sent.length = 0; notes.length = 0; failing = null;
      payload = { requests: [], grants: [], app_ready: true, app_problem: "" };
      gitqAnnounced.clear(); gitqPrimed = false; gitqShown.clear();
      keep = { checked: false }; hours = { value: "24" }; alertsOn = true;
      fields = { repo: { value: "" }, token: { value: "", placeholder: "", focus: () => {}, scrollIntoView: () => {} } };
      form = { plane: { value: "" }, conn: { value: "" }, review: { value: "false" }, token: { value: "" } };
      state = { className: "", innerHTML: "" };
      scoped = { checked: true };
      gitqCreds = [];
    },
  };
`);

const { check, done } = harness();
const T = scope();
const ask = (id, over = {}) => ({
  id, box: "web-main", repo: "acme/thing", reason: "fix the shared type",
  asked: "2026-08-13T09:00:00Z", state: "pending", decided: "", ...over,
});

// --- a decision is about what was on screen -----------------------------------------------------
// The rule `gitqShown` exists for, and it had no test: a decision carries the box and repo AS SHOWN,
// never a re-read. A request is a box's own words in a file the box can rewrite, so approving a
// re-read approves whatever it says at the moment you press the button — and the minted token is
// written into whichever box the grant names. Seen in the wild, which is why the map is there.
//
// So the suite has to render before it decides, exactly as a person does. It did not, and every
// decide below was silently refused once the map existed.
T.reset();
T.decideGitq("web-main/r1", true);
check("a request never rendered cannot be decided", T.sent().length, 0);
check("and says why, rather than failing quietly", T.notes()[0], "toast:that request is no longer on screen — reopen the panel");

// --- two boxes, one id -------------------------------------------------------------------------
// An id is `<time>-<pid>`, chosen by the box that filed it, and every box can read every other
// box's queue — so a box can file the id it watched a neighbour use. With `gitqShown` keyed by id
// alone, the card rendered LAST owned the entry (the panel renders newest first, so a copy dated
// earlier lands after the real one), and pressing Grant on the trusted box's card sent the
// copier's name: the token was minted into the copier (ISO-7).
//
// Pressed through the button's own `onclick`, read out of the rendered card, rather than by
// calling `decideGitq` with a key this suite spelled — a key spelled here would agree with any
// keying at all, which is what this has to catch.
//
// What would make this fail: keying `gitqShown` (or the button) by the id alone again.
const press = (html, label) => {
  const m = html.match(new RegExp(`onclick="decideGitq\\(([^"]*)\\)">${label}<`));
  if (!m) return "no button";
  const args = m[1].replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, "&");
  return new Function("T", `return T.decideGitq(${args})`)(T);
};
T.reset();
const trusted = T.gitqCard(ask("r1", { box: "trusted" }));
T.gitqCard(ask("r1", { box: "copier", repo: "acme/other" }));
check("the trusted card has a Grant button to press", press(trusted, "Grant write"), undefined);
check("Grant on one box's card sends that box, not another box's request with the same id",
  T.sent()[0] && T.sent()[0].body.box, "trusted");
check("and the repository on that card", T.sent()[0] && T.sent()[0].body.repo, "acme/thing");
check("and still addresses the request by its id", T.sent()[0] && T.sent()[0].id, "r1");

// --- what the grant carries --------------------------------------------------------------------
T.reset();
T.gitqCard(ask("r1"));
T.decideGitq("web-main/r1", true);
check("granting sends an approval", T.sent()[0].body.approve, true);
check("and expires by default", T.sent()[0].body.hours, 24);

T.reset();
T.gitqCard(ask("r1"));
T.setHours("2");
T.decideGitq("web-main/r1", true);
check("a shorter window is carried through", T.sent()[0].body.hours, 2);

// Ticked keep wins over whatever the number says, so the two controls cannot mean two things.
T.reset();
T.gitqCard(ask("r1"));
T.setHours("12");
T.setKeep(true);
T.decideGitq("web-main/r1", true);
check("keeping indefinitely sends 0, not the leftover number", T.sent()[0].body.hours, 0);

// A number nobody can honour must not become a grant that never expires — 0 means forever here.
T.reset();
T.gitqCard(ask("r1"));
T.setHours("nonsense");
T.decideGitq("web-main/r1", true);
check("an unusable duration falls back to the default", T.sent()[0].body.hours, 24);
T.reset();
T.gitqCard(ask("r1"));
T.setHours("0");
T.decideGitq("web-main/r1", true);
check("and zero typed into the box is not silently forever", T.sent()[0].body.hours, 24);

T.reset();
T.gitqCard(ask("r1"));
T.setKeep(true);
T.decideGitq("web-main/r1", false);
check("denying is a denial", T.sent()[0].body.approve, false);

// --- revoking ----------------------------------------------------------------------------------
T.reset();
T.revokeGitq("web-main", "acme/thing");
check(
  "a revoke encodes the repo's slash so it stays one path segment",
  T.sent()[0].deleted.endsWith("/web-main/acme%2Fthing"),
  true,
);

// --- announced once, however long it waits -----------------------------------------------------
T.reset();
T.setPayload({ requests: [ask("a")], grants: [], app_ready: true });
await T.pollGitq();
check("the first poll seeds rather than announcing", T.notes().filter(n => n.body).length, 0);
check("but the badge is painted straight away", T.badge().n, "1");

await T.pollGitq();
await T.pollGitq();
check("a request already seen is never re-announced", T.notes().filter(n => n.body).length, 0);

T.setPayload({ requests: [ask("a"), ask("b", { box: "api" })], grants: [], app_ready: true });
await T.pollGitq();
const said = T.notes().filter(n => n.body);
check("a new request is announced once", said.length, 1);
check("naming the box that asked", said[0].tag, "api");
check("and saying which repo it wants", said[0].body.includes("acme/thing"), true);

await T.pollGitq();
check("and then goes quiet", T.notes().filter(n => n.body).length, 1);

// --- the badge clears when the last request is answered -----------------------------------------
T.setPayload({ requests: [ask("a", { state: "granted" }), ask("b", { state: "denied" })], grants: [] });
await T.pollGitq();
check("an answered queue leaves no badge", T.badge().n, null);

// --- what the card actually shows ---------------------------------------------------------------
const card = T.gitqCard(ask("a"));
check("the card names the repository", card.includes("acme/thing"), true);
check("and the box that asked", card.includes("web-main"), true);
check("and the reason, which is the whole basis for deciding", card.includes("fix the shared type"), true);

// A decided request offers no buttons — approving twice is not a thing that should be possible.
const decided = T.gitqCard(ask("a", { state: "granted" }));
check("a granted request has no Grant button left", decided.includes("Grant write"), false);

// --- and it escapes them, because the box wrote them ---------------------------------------------
//
// `reason` is "the whole basis for deciding" and it is free text composed by the box asking for a
// credential to somebody else's repository. So the card is a surface where the thing under judgement
// writes what the judge reads — and the judge reads it in the cockpit tab, which holds a terminal.
//
// This suite stubbed `esc` as `s => String(s)` until SKEIN-531, so the four checks above described a
// card that escaped nothing; `${grab("esc")}` gives the world the page's own.
//
// Fails on: `esc` returning its argument, or losing any of `& < > " '`.
{
  const hostile = T.gitqCard(ask("q<1>", {
    repo: `acme/<script>alert(1)</script>`,
    box: `<img src=x onerror=alert(1)>`,
    reason: `needs it for "the fix" & <b>urgency</b> — o'clock`,
  }));
  // Constructs, not the payload text: escaped, the payload is still in the card as `&lt;script&gt;`,
  // so a pattern matching the word would pass on the broken page too.
  check("a repository name cannot open a tag", /<script/i.test(hostile), false);
  check("nor can the box that asked", /<img/i.test(hostile), false);
  // Not `/<b>/`: the card writes a real `<b>` of its own around the box name, so that pattern is
  // answered by the template rather than by the payload.
  check("nor can the reason it gave", hostile.includes("<b>urgency</b>"), false);
  // The box and id reach an `id=` attribute (`gq-h-…`, `gq-keep-…`) and `decideGitq`'s argument,
  // keyed together.
  check("the id in the hours field cannot end the tag it is in",
    hostile.includes(`id="gq-h-&lt;img src=x onerror=alert(1)&gt;/q&lt;1&gt;"`), true);

  // And all of it is still readable — an absence check is satisfied by drawing nothing, and a card
  // that silently drops the reason is a worse failure than the one above, because the reason is
  // what the decision is made on. Spelled out rather than computed with `esc`: an expectation built
  // by calling the function under test moves with it and never fails. That is SKEIN-531.
  check("the reason is shown as the characters the box typed",
    hostile.includes(`needs it for &quot;the fix&quot; &amp; &lt;b&gt;urgency&lt;/b&gt; — o&#39;clock`), true);
  check("and the repository it names",
    hostile.includes(`acme/&lt;script&gt;alert(1)&lt;/script&gt;`), true);
}

// --- grants list --------------------------------------------------------------------------------
const live = T.gitqGrantRow({ box: "web-main", repo: "o/r", expires: "2026-08-14T00:00:00Z", live: true });
check("a live grant can be revoked from the list", live.includes("Revoke"), true);
const dead = T.gitqGrantRow({ box: "web-main", repo: "o/r", expires: "2026-08-12T00:00:00Z", live: false });
check("an expired one is shown", dead.includes("expired"), true);
check("but cannot be revoked twice", dead.includes("Revoke"), false);
const forever = T.gitqGrantRow({ box: "web-main", repo: "o/r", expires: "", live: true });
check("a permanent grant says so rather than showing a blank date", forever.includes("no expiry"), true);

// --- stored repository tokens -------------------------------------------------------------------
T.reset();
T.fields().repo.value = "acme/thing";
T.fields().token.value = "github_pat_11ABC";
await T.addGitCred();
check("storing a token names exactly one repository", T.sent()[0].body.repos, ["acme/thing"]);
check("and carries the token", T.sent()[0].body.token, "github_pat_11ABC");
check(
  "the id is derived from the repo, so re-storing it replaces rather than duplicates",
  T.sent()[0].body.id,
  "acme-thing",
);
check("the token field is cleared after storing", T.fields().token.value, "");

// Half a form is not a credential — storing a repo with no token would report the fleet as ready
// to scope while every box comes up unable to push.
T.reset();
T.fields().repo.value = "a/b";
T.fields().token.value = "";
await T.addGitCred();
check("a repo with no token is refused before it is sent", T.sent().length, 0);
T.reset();
T.fields().repo.value = "";
T.fields().token.value = "github_pat_x";
await T.addGitCred();
check("and a token with no repo likewise", T.sent().length, 0);

// --- rotating one ------------------------------------------------------------------------------
T.reset();
T.setCreds([{ id: "acme-thing", repo: "acme/thing", label: "", has_token: true }]);
T.editGitCred("acme-thing");
check("replacing prefills the repo so a typo cannot store a second credential",
  T.fields().repo.value, "acme/thing");
check("and never prefills the old token", T.fields().token.value, "");
check("saying plainly that the old one goes",
  T.fields().token.placeholder.includes("replaced"), true);

T.fields().token.value = "github_pat_NEW";
await T.addGitCred();
check("the rotation reuses the same id", T.sent()[0].body.id, "acme-thing");
check("with the new token", T.sent()[0].body.token, "github_pat_NEW");

// --- what a credential row shows ----------------------------------------------------------------
const ready = T.gitCredRow({ id: "a-b", repo: "a/b", label: "", has_token: true, problem: "" });
check("a usable token reads as ready", ready.includes("ready"), true);
check("and offers to be replaced", ready.includes("Replace token"), true);

const incomplete = T.gitCredRow({ id: "a-b", repo: "a/b", label: "", has_token: false, problem: "" });
check("one with no token says so", incomplete.includes("incomplete"), true);
check("and offers to add one", incomplete.includes("Add token"), true);

// A hand-edited multi-repo entry is listed and refused — the host will not use it, and a row that
// silently vanished would leave someone staring at a repo whose token "is configured".
const wide = T.gitCredRow({
  id: "wide", repo: "", repos: ["a/one", "a/two"], label: "", has_token: true,
  problem: "names 2 repositories; a stored token must cover exactly one",
});
check("a refused credential is shown, not hidden", wide.includes("refused"), true);
check("and says why", wide.includes("exactly one"), true);

// --- what the pane says is actually on ------------------------------------------------------------
// The switch and the truth are different facts, and the gap between them is the whole reason this
// line exists: `scope_git_to_repo` defaults ON, so a fleet with no issuer draws a switch that is on
// while every box holds the account. A status line that echoed the switch would be worse than none.
T.reset();
T.setScoped(true);
T.renderGitState({ ready: false, app_ready: false, app_problem: "no App configured", credentials: [], account_token: true });
check("a switch that is on but cannot issue says NOT scoped", T.state().innerHTML.includes("Not scoped"), true);
check("and names what boxes hold instead", T.state().innerHTML.includes("account"), true);
check("drawn as a problem, not as success", T.state().className.includes("warn"), true);

// The other unscoped fleet, and it must not be described as the one above. All three credential paths
// are opt-in now, so "nothing configured" no longer implies "every box holds your account" — it means
// boxes cannot push at all, and a line claiming they hold a credential nobody chose would send someone
// hunting the wrong problem the first time a push fails.
T.reset();
T.setScoped(true);
T.renderGitState({ ready: false, app_ready: false, app_problem: "no App configured", credentials: [], account_token: false });
check("a fleet with nothing chosen says so", T.state().innerHTML.includes("No credential chosen"), true);
check("and that boxes cannot push", T.state().innerHTML.includes("cannot push"), true);
check("without claiming they hold the account token", T.state().innerHTML.includes("account&#39;s") || T.state().innerHTML.includes("account's"), false);
check("still drawn as a problem", T.state().className.includes("warn"), true);

T.renderGitState({ ready: true, app_ready: true, app_id: "12345", credentials: [] });
check("with an issuer and the switch on, it reads as scoped", T.state().innerHTML.includes("Scoped"), true);
check("naming what does the issuing", T.state().innerHTML.includes("12345"), true);
check("and drawn as good", T.state().className.includes("on"), true);

// Configured but switched off is its own state — "ready" would overclaim and "not scoped" would
// hide that the hard part is already done.
T.setScoped(false);
T.renderGitState({ ready: true, app_ready: true, app_id: "12345", credentials: [] });
check("configured but switched off says so", T.state().innerHTML.includes("switched off"), true);

// A credential that is stored but unusable must not be counted as an issuer in the summary.
T.setScoped(true);
T.renderGitState({ ready: true, app_ready: false, credentials: [
  { id: "a", repo: "a/b", has_token: true, problem: "" },
  { id: "c", repo: "", has_token: true, problem: "names 2 repositories" },
] });
check("only usable tokens are counted", T.state().innerHTML.includes("1 repository token"), true);

// --- which repository a source names --------------------------------------------------------------
// Every row here is a case `gitgate::slug_from_url` handles; the two must not disagree.
for (const [source, want] of [
  ["git@github.com:acme/thing.git", "acme/thing"],
  ["https://github.com/acme/thing.git", "acme/thing"],
  ["https://github.com/acme/thing", "acme/thing"],
  ["ssh://git@github.com/acme/thing", "acme/thing"],
  ["acme/thing", "acme/thing"],
  // A local path is the case that matters most: skein adopts repos in place, and without the
  // guard `/Users/me/code/thing` parses to `Users/me` — a repo that does not exist, offered a
  // token field that could never work.
  ["/Users/me/code/thing", ""],
  ["~/code/thing", ""],
  ["./thing", ""],
  // Deep paths are not repositories, and taking the first two segments is how they became ones.
  ["https://github.com/acme/thing/tree/main", ""],
  // Another forge has no App installation and no token to mint.
  ["git@gitlab.com:acme/thing.git", ""],
  ["https://git.example.com/a/b", ""],
  ["", ""],
]) check(`repoSlug(${JSON.stringify(source)})`, T.repoSlug(source), want);

// --- a repo's own token row -----------------------------------------------------------------------
T.reset();
T.setCreds([{ id: "acme-thing", repo: "acme/thing", has_token: true, problem: "" }]);
const owned = T.repoTokenRow({ id: "thing", source: "git@github.com:acme/thing.git", slug: "acme/thing" });
check("a repo with a stored token says so on its own card", owned.includes("token stored"), true);
check("and offers to forget it", owned.includes("Forget"), true);

const fromApp = T.repoTokenRow({ id: "other", source: "git@github.com:acme/other.git", slug: "acme/other" });
check("one without falls back to the App rather than reading as broken", fromApp.includes("from the App"), true);

// The regression this row was getting wrong: a repo adopted from a local path whose clone has a
// GitHub `origin`. The host resolves that and sends the slug; parsing `source` here called it "not
// GitHub" and offered no token field, on a repo whose boxes need one to push at all.
const adopted = T.repoTokenRow({ id: "skein", source: "/Users/me/code/skein", slug: "acme/skein" });
check("an adopted repo with a GitHub origin gets a token field", adopted.includes(`data-repotoken="acme/skein"`), true);
check("named for the repository the host will mint against", adopted.includes("<b>acme/skein</b>"), true);

// Only a repo with no GitHub remote anywhere has nothing to offer — and it is the absent remote that
// is worth saying, since that is what a box cannot push without.
const local = T.repoTokenRow({ id: "scratch", source: "/Users/me/code/scratch", slug: "" });
check("a repo with no remote is told so", local.includes("no GitHub remote"), true);
check("and gets no token field at all", local.includes("data-repotoken"), false);

// --- the add dialog's follow-up work ---------------------------------------------------------------
// Everything here keys on the repo's id, which the *server* picks — so it runs after the clone.
T.reset();
T.form().plane.value = "https://plane.example/projects/abc/issues";
T.form().conn.value = "backlog-1";
T.form().review.value = "false";
T.form().token.value = "github_pat_11ABC";
let problems = await T.arApplySettings({ id: "thing", source: "git@github.com:acme/thing.git", slug: "acme/thing" });
check("the whole form applied cleanly", problems, []);
const settings = T.sent().find(s => s.url.includes("/settings"));
check("the tracker fields are sent together, in one call", settings.body.plane_project, "https://plane.example/projects/abc/issues");
check("with the connection", settings.body.sync_connection, "backlog-1");
check("and a deliberate off for the review queue", settings.body.review_queue, false);
const cred = T.sent().find(s => s.url.includes("git-credentials"));
check("the token is stored against the repo it names", cred.body.repos, ["acme/thing"]);

// Turning it ON is sent too. It used to send only a deliberate off and ride on the server's
// default — and that default has since MOVED (`repos::add` registers a new repo with the queue off,
// while serde's default for an absent field stays true so an upgrade cannot switch off a queue
// somebody has been using). A body that omits the field cannot say which of those two it meant, so
// the form states its value either way (SKEIN-270).
T.reset();
T.form().review.value = "true";
await T.arApplySettings({ id: "thing", source: "git@github.com:acme/thing.git", slug: "acme/thing" });
const on = T.sent().find(x => x.url.includes("/settings"));
check("turning the queue on is sent as a real value, not left to a default", !!on, true);
check("and the value is the one that was chosen", on && on.body.review_queue, true);

// The dialog itself. The default is a choice about SPENDING — every watched repo costs a GitHub
// request per refresh, and eight of them exhausted the owner's rate limit and emptied the one queue
// they were actually reading — so the control opens off and says what turning it on costs.
const control = page.slice(page.indexOf('<select id="ar-review"'));
const chooser = control.slice(0, control.indexOf("</select>"));
check("the new-repo control offers off first", chooser.indexOf('value="false"') < chooser.indexOf('value="true"'), true);
check("and opens on it", /<option value="false" selected>/.test(chooser), true);
check("re-opening the dialog does not quietly turn it back on",
  page.includes('document.getElementById("ar-review").value = "false";'), true);
// In the option's own words, not a sentence under the control: the dialog is already as tall as
// the viewport, and a line added below it pushed the Add button off screen — onboarding.mjs caught
// exactly that, as a click that could never land.
check("and the ON choice says what it spends", /a GitHub request per refresh, per repo/.test(chooser), true);

// The clone is the expensive, irreversible part and it already succeeded. A tracker field that
// failed to save is a ten-second fix on the card; throwing the repo away over one is not.
T.reset();
T.failOn("/settings");
T.form().plane.value = "p";
T.form().token.value = "github_pat_x";
problems = await T.arApplySettings({ id: "thing", source: "git@github.com:acme/thing.git", slug: "acme/thing" });
check("a failed settings call is reported", problems.length, 1);
check("naming what did not happen", problems[0].includes("settings not saved"), true);
check("and never stops the token being stored", T.sent().some(s => s.url.includes("git-credentials")), true);

// A token typed for a repo with no GitHub identity must be refused loudly, not dropped quietly.
T.reset();
T.form().token.value = "github_pat_x";
problems = await T.arApplySettings({ id: "scratch", source: "/Users/me/code/scratch", slug: "" });
check("a token for a repo with no remote is refused, not silently dropped", problems.length, 1);
check("saying why", problems[0].includes("no GitHub remote"), true);
// It used to be "nothing is sent at all". The settings call is unconditional now — the review
// queue states its value either way (SKEIN-270) — so what this check is actually about, and always
// was, is that the CREDENTIAL does not go out.
check("and no credential is stored for a repo that cannot have one",
  T.sent().some(x => x.url.includes("git-credentials")), false);
check("nothing else goes out either", T.sent().map(x => x.url.includes("/settings")), [true]);

// The same form against a repo adopted in place: the host answered with a slug, so the token belongs
// to that repository. Refusing it here was the dialog's half of the same bug.
T.reset();
T.form().token.value = "github_pat_adopted";
problems = await T.arApplySettings({ id: "skein", source: "/Users/me/code/skein", slug: "acme/skein" });
check("a token typed for an adopted repo is stored, not refused", problems, []);
check("against the repository its origin names",
  T.sent().find(s => s.url.includes("git-credentials")).body.repos, ["acme/skein"]);

done();
