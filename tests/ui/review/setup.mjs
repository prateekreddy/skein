// The review pane's browser suite, its shared setup: the fixture (a GitHub stub, a mirror, a
// stand-in `claude`), the harness helpers every part waits and presses with, and the run itself —
// one server and one page, opened on the cockpit. `tests/ui/review.mjs` is the suite; this is the
// module every part of it imports.
//
// Evaluating this module IS the setup. Its top level builds the fixture, starts the server and
// opens the page, so the first `import` of it does that once and every later one shares the result.
// `page`, `mustSee`, `find` and `settle` are assigned below after the page exists, and an export is
// a live binding, so a part reads the page this module opened.

import { chromium } from "playwright";
import { fixtureRoot, freshFixture } from "../lift.mjs";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { openDoor } from "../lift.mjs";
import { erring, finding, ledger, seeing, settler } from "../harness/browser.mjs";
import { stub } from "../harness/github.mjs";
import { startServer } from "../harness/server.mjs";
import { fileURLToPath } from "node:url";

// ---------- refuse to run as anything but `review.mjs` ----------
/** **A part is not a suite, and run as one it can only fail and then never end.**
 *
 *  The parts under `tests/ui/review/` inherit the queue as the one before them left it, and only
 *  `tests/ui/review.mjs` ends a run: it reports, closes the browser and calls `process.exit`. So
 *  `node review/row.mjs` fails every check that needed the pane `queue.mjs` opens — "no gist cells
 *  at all", `revQueue` null — and then, with a live server and a live chromium keeping its event
 *  loop open and nothing left to close them, waits for ever. It was run that way on 2026-09-25,
 *  and it hung for nine hours before somebody stopped it.
 *
 *  So this module, which every part imports, refuses before it builds anything: a fixture, a server
 *  or a browser started here would be the very thing that keeps the process alive. It asks what the
 *  entry script is, rather than trusting a flag the entry sets, because the failure is precisely an
 *  entry that sets nothing. */
// Both through `realpath`: node resolves `import.meta.url` through symlinks and leaves `argv[1]` as
// it was typed, so a checkout reached through a link would otherwise be refused its own suite.
const real = p => { try { return fs.realpathSync(p); } catch { return path.resolve(p); } };
const entry = process.argv[1] ? real(process.argv[1]) : "";
const suite = real(path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "review.mjs"));
if (entry !== suite) {
  console.error(
    `${path.relative(process.cwd(), entry) || entry} is a part of the review suite, not a suite: it ` +
    `needs the queue the parts before it leave, and nothing but review.mjs ends the run.\n` +
    `Run the whole suite instead: node ${path.relative(process.cwd(), suite)}`);
  process.exit(2);
}


// ---------- fixture ----------
/** The link a third party wrote (SKEIN-602), as one constant so the fixture and the checks that
 *  read it back cannot disagree about what was sent.
 *
 *  It is a WORKING payload, not a placeholder: `void(…)` so the URL evaluates to `undefined` and
 *  the browser therefore does not replace the document with the result, and a sentinel on `window`
 *  so "did this run" is a question the page can be asked rather than inferred. The checks below
 *  click it both ways round — through the page's guard, and through `esc` alone — and a payload
 *  that could not run either way would make both answers meaningless. */
const HOSTILE_CHECK_URL = "javascript:void(window.__followedHostileCheck = 1)";
// Five PRs, each one a state the pane has to get right (lanes say WHOSE MOVE it is, SKEIN-139):
//   #1 unreviewed              → your move
//   #2 approved on the head    → their move (you decided; it moves without you)
//   #3 approved, then moved on → your move (the case that returns work to you)
//   #4 authored by you         → their move, and the "mine" filter
//   #5 a draft                 → not ready (counted and stated, folded, never hidden)
/// A GitHub the size of what skein asks for: `/user`, `/user/teams`, one GraphQL search, a diff and
/// a file list. Answers from the fixture files written beside it.
///
/// It exists because the client changed, not because the test wanted rewriting: skein used to shell
/// out to `gh`, so the stub was a shell script on `$PATH`. It reads the API now, so the stub is an
/// API — and the test gained something in the move, because what it asserts is the request that
/// actually goes out.
async function createGitHub(root) {
  // **The live head of a pull request, which is not the one the queue photographed.** A merge is
  // checked against GitHub itself — `prwork::merge_by_hand` reads `prq::base_and_head` before it
  // sends anything — so a push landing while somebody reads is this map changing and nothing else.
  // Absent means the fixture's own `headRefOid`, so every PR starts where its search answer says.
  const heads = {};
  const headOf = n => heads[n] || `sha${n}`;
  // **Whether a refresh saw everything there was to see** — `Queue.whole` in `src/prq/types.rs`, the one
  // fact that lets anything read a pull request's ABSENCE as evidence about it.
  //
  // Two inputs, and this stub owns both. `queue_within` starts at
  // `let mut answered = !teams_unknown;` (`src/prq/refresh.rs`) and ANDs in each membership
  // search's `found.whole` (`answered &= found.whole;`, same file). The searches below answer far
  // fewer than `SEARCH_PAGE` nodes and say nothing about paging, so `one_request` reads every one
  // of them as whole (`whole: match more`, `src/prq/search.rs`) — which leaves the teams lookup as
  // the only thing here that can make a queue partial, and it used to do it unconditionally:
  // `/user/teams` answered 403 to every request, `viewer` reads a refusal as "GitHub would not
  // say" rather than "you are in no teams" (`teams_unknown`, `src/prq/refresh.rs`), and so
  // `whole` was false on every queue this file has ever driven. Any
  // page behaviour keyed on it fired in all of them, which is not a test of anything.
  //
  // So the teams are ANSWERED by default, and the two seams below put each half of an incomplete
  // refresh back on purpose: `refuseTeams` makes it partial, `emptyQueue` empties it.
  let teamsRefused = false;
  let emptied = false;
  const search = q =>
    emptied                       ? []
    : /review-requested:/.test(q) ? JSON.parse(fs.readFileSync(path.join(root, "search-review-requested.json"), "utf8"))
    : /author:/.test(q)           ? JSON.parse(fs.readFileSync(path.join(root, "search-author.json"), "utf8"))
    : [];
  const DIFFS = {
    3: "diff --git a/src/parser.rs b/src/parser.rs\n--- a/src/parser.rs\n+++ b/src/parser.rs\n@@\n-const TIMEOUT: u64 = 30;\n+const TIMEOUT: u64 = 5;\n",
    // Per PR, because the scanner reads the real diff: one diff for every number would put a moved
    // constant inside the "bug fix" too, and escalating that would be correct.
    other: "diff --git a/src/parser.rs b/src/parser.rs\n--- a/src/parser.rs\n+++ b/src/parser.rs\n@@\n-    let head = input.chars().next().unwrap();\n+    let Some(head) = input.chars().next() else { return Ok(()) };\n",
  };
  const github = await stub(({ url, body, req, send }) => {
    if (url === "/user") return send(200, { login: "me" });
    // The lookup that decides whether this fixture's queue is whole — see the seams above.
    // Answered with the team #4's roster names, so the list is one skein could really have
    // matched a `team-review-requested:` search against; refused with what a token without
    // `read:org` actually gets.
    if (url === "/user/teams") {
      return teamsRefused
        ? send(403, { message: "Requires read:org" })
        : send(200, [{ slug: "core", organization: { login: "acme" } }]);
    }
    if (url === "/graphql") {
      // The one mutation this page sends (SKEIN-305). Answered in GitHub's own shape — the
      // thread's id and its new `isResolved` — because `prq::set_thread_resolved` reads the
      // answer back and reports an `isResolved` that contradicts what was asked as a write that
      // did not take. A stub that answered `{}` would pass either way.
      if (/resolveReviewThread/.test(body)) {
        const on = !/unresolveReviewThread/.test(body);
        const id = (JSON.parse(body || "{}").variables || {}).id || "";
        return send(200, { data: { [on ? "resolveReviewThread" : "unresolveReviewThread"]:
          { thread: { id, isResolved: on } } } });
      }
      // One request carries every membership search of a refresh now, aliased q0…qN (SKEIN-209),
      // and each alias answers under its own name — a fixture that still answered the single
      // `search` field left every query reading as "GitHub returned no answer for this search".
      const vars = JSON.parse(body || "{}").variables || {};
      const data = {};
      for (const [name, value] of Object.entries(vars)) {
        if (/^q\d+$/.test(name)) data[name] = { nodes: search(String(value)) };
      }
      return send(200, { data });
    }
    // Acting on a PR: submitting a review, and merging. Both answer the way GitHub does — a JSON
    // object — because the client reads `message` out of it for what to show.
    const reviews = url.match(/^\/repos\/[^/]+\/[^/]+\/pulls\/(\d+)\/reviews$/);
    if (reviews) return send(200, { id: 1, state: "COMMENTED" });
    const merge = url.match(/^\/repos\/[^/]+\/[^/]+\/pulls\/(\d+)\/merge$/);
    if (merge) return send(200, { merged: true, message: "Pull Request successfully merged" });
    const files = url.match(/^\/repos\/[^/]+\/[^/]+\/pulls\/(\d+)\/files$/);
    if (files) return send(200, [{ filename: "src/parser.rs" }, { filename: "web/app.js" }]);
    // The repository itself. `prq::trunk_of` reads `default_branch` and a merge into a base it
    // cannot check is refused before any request, so a merge cannot be driven end to end without
    // this; `github::canonical_repo` reads `full_name`, and answering the name skein already
    // holds is what "not renamed" looks like on the wire.
    if (/^\/repos\/[^/]+\/[^/]+$/.test(url)) return send(200, { full_name: "acme/thing", default_branch: "main" });
    // One pull request, as JSON — the base branch and the live head a merge is checked against.
    // The DIFF is served from the very same path, and the only thing that tells them apart is the
    // Accept header skein sends: `prq::pr_diff_text` asks for `application/vnd.github.diff`,
    // every JSON read asks for `application/vnd.github+json`.
    const diff = url.match(/^\/repos\/[^/]+\/[^/]+\/pulls\/(\d+)$/);
    if (diff && !/diff/.test(req.headers.accept || "")) {
      return send(200, { number: Number(diff[1]), base: { ref: "main" }, head: { sha: headOf(diff[1]) } });
    }
    if (diff) return send(200, DIFFS[diff[1]] || DIFFS.other, "text/plain");
    return false;
  });
  return {
    ...github,
    // `moveTo` is a push landing on somebody else's branch: from here on GitHub answers with a
    // different head, and nothing tells the reader's page about it.
    moveTo: (number, sha) => { heads[number] = sha; },
    // The two halves of an incomplete refresh, in `moveTo`'s register: something about GitHub
    // changes, and the NEXT refresh reads it. Neither reaches the page on its own — the queue is
    // behind a 60s micro-cache (`prq::queue`, `src/prq/refresh.rs`), so a suite that flips one asks
    // again past it with `refreshQueue()` below.
    refuseTeams: on => { teamsRefused = !!on; },
    emptyQueue: on => { emptied = !!on; },
  };
}

/** Where skein looks for this repo's mirror — `repos::mirror_path`, kept in one place. */
const mirrorAt = home => path.join(home, "repos", "acme", "mirror");

/** Put a mirror of `work` where `ensure_mirror` will find one, so it clones nothing. */
function makeMirror(root, home) {
  const mirror = mirrorAt(home);
  fs.mkdirSync(path.dirname(mirror), { recursive: true });
  const out = spawnSync("git", ["clone", "-q", "--mirror", path.join(root, "work"), mirror], { encoding: "utf8" });
  // Loudly, and at the point of failure. A mirror that quietly did not get made is a repo skein
  // reports as unreadable, thirty checks later, in language about the review pane.
  if (out.status !== 0) throw new Error(`the fixture could not make acme's mirror: ${out.stderr || out.stdout}`);
}

/** Bring the mirror up to date with `work` — the same fetch `repos::fetch_mirror` runs. */
function refreshMirror(home) {
  const out = spawnSync("git", ["-C", mirrorAt(home), "fetch", "--prune", "origin",
    "+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"], { encoding: "utf8" });
  if (out.status !== 0) throw new Error(`the fixture could not refresh acme's mirror: ${out.stderr || out.stdout}`);
}

async function makeFixture() {
  // Kept on failure (below) and, until SKEIN-590, never removed afterwards: 48 directories and
  // 60 MB on this box. `freshFixture` stamps the pid on it and removes the ones whose maker has
  // exited, which is the only sweep that is safe while several worktrees run this at once.
  const root = freshFixture(fixtureRoot(), "skein-review-ui");
  const bin = path.join(root, "bin");
  const home = path.join(root, "home");
  fs.mkdirSync(bin, { recursive: true });
  fs.mkdirSync(home, { recursive: true });
  fs.writeFileSync(path.join(root, "sandboxes.json"), JSON.stringify({}));
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({}));
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  // A repo whose source IS a GitHub URL — the only kind that has a queue.
  fs.writeFileSync(path.join(home, "repos.json"), JSON.stringify([
    // `source_tree` beside `source` used to be what kept `ensure_mirror` off the network: the
    // comment here said so, and it stopped being true. Local-path repos were removed, and with them
    // `clone_mirror`'s preference for a checkout — "there is no checkout to prefer any more, and
    // `source` is a URL by construction" (src/repos/mirror.rs). So every run since has tried to
    // clone `https://github.com/acme/thing.git` for real, failed, and taken eight checks down with it:
    // the brief, the evidence block, the ownership line and every module note, each blaming the UI
    // for a repo skein simply could not read.
    //
    // `makeMirror` below is the replacement, and it works the other way round — the mirror is put
    // where skein looks for it BEFORE skein looks, so `ensure_mirror` finds one and clones nothing.
    // The field is left here because it is still what a pre-removal `repos.json` on a real machine
    // holds, and parsing one of those is worth not breaking.
    // `source_tree`, and NOT `work` beside it: `work` is a serde ALIAS for the same field, so both
    // together is a duplicate key and the whole file fails to parse — which reads downstream as
    // "no repo with id acme" rather than as a bad fixture.
    // `read_prs: true` — read-ahead switched ON, which is what a person who wants their queue read
    // has pressed. It is off by default and it is the SCOPE, not a preference: with it off, skein
    // reads nothing here on its own, neither the background pass nor the pane's pump
    // (`review::unasked_scope`, SKEIN-242), and every check below that reads a summary the pump
    // fetched would be asserting on an empty pane.
    { id: "acme", source: "https://github.com/acme/thing.git",
      source_tree: path.join(root, "work"), read_prs: true,
      store: path.join(root, "store"), agent: "claude", plane_project: "", sync_connection: "" },
  ]));

  // GraphQL's shape, because that is what skein reads now: connections rather than bare arrays, and
  // the check rollup hanging off the last commit. Same four states, one layer deeper.
  const pr = (number, title, author, extra = {}) => ({
    number, title, author: { login: author },
    url: `https://github.com/acme/thing/pull/${number}`,
    headRefName: `feat-${number}`, headRefOid: `sha${number}`, baseRefName: "main",
    isDraft: false, updatedAt: `2026-08-0${number}T00:00:00Z`,
    latestReviews: { nodes: [] },
    commits: { nodes: [{ commit: { statusCheckRollup: null } }] },
    ...extra,
  });
  const reviewed = (state, oid) =>
    ({ latestReviews: { nodes: [{ author: { login: "me" }, state, commit: { oid } }] } });
  const checks = nodes => ({ commits: { nodes: [{ commit: { statusCheckRollup: { contexts: { nodes } } } }] } });

  fs.writeFileSync(path.join(root, "search-review-requested.json"), JSON.stringify([
    pr(1, "fix a null deref in the parser", "dana", checks([{ status: "COMPLETED", conclusion: "SUCCESS" }])),
    pr(2, "rename the retry flag", "dana", reviewed("APPROVED", "sha2")),
    // Approved, and GitHub has ASKED AGAIN. The re-request is what puts this row back in your
    // lane (SKEIN-354): `decided` reads GitHub's two answers — is your verdict standing, and is
    // skein still being asked — and the commit the review was left against is no longer one of
    // them. Without the re-request this row is theirs, it leaves both the your-move lane and
    // `worth_reading`'s, and every check below that drives it has nothing to drive.
    pr(3, "change the default timeout", "erin", {
      ...reviewed("APPROVED", "older"),
      reviewRequests: { totalCount: 1, nodes: [{ requestedReviewer: { login: "me" } }] },
    }),
    // A draft. Skein must leave it alone until somebody marks it ready or asks for it by hand — a
    // draft is the author saying it is not finished, and spending a model call on the fleet's own
    // rate limit to describe something nobody has proposed yet is the clearest case of work that
    // was not asked for.
    pr(5, "wip: still moving things around", "dana", { isDraft: true }),
  ]));
  // The two you opened, and they are the two halves of SKEIN-303's rule.
  //
  // #4 is RED and nothing else: no unresolved thread, no conflict, nobody asking for changes. It
  // must NOT be your move. The owner, verbatim: "CI pass isn't your responsibility, that is of
  // whoever merges." It also carries the roster #6 does not — a person and a team — which is what
  // an author chasing an approval actually wants to read (SKEIN-306).
  //
  // #6 is also red, and it IS your move, because a review thread is open on it. Two pull requests
  // you opened, identical in their checks and opposite in the list, so a change that starts reading
  // `checks` here cannot pass by accident.
  fs.writeFileSync(path.join(root, "search-author.json"), JSON.stringify([
    pr(4, "my own change to the store layout", "me", {
      reviewDecision: "REVIEW_REQUIRED", mergeable: "MERGEABLE", mergeStateStatus: "CLEAN",
      reviewRequests: { totalCount: 2, nodes: [
        { requestedReviewer: { login: "dana" } },
        { requestedReviewer: { slug: "core", organization: { login: "acme" } } },
      ] },
      // **Two failing checks, and the second one's link is hostile** (SKEIN-602).
      //
      // A check's link is not GitHub's. `detailsUrl` on a check run and `targetUrl` on a commit
      // status are supplied by whoever created it — `POST /repos/{o}/{r}/check-runs` takes
      // `details_url` verbatim — so anything holding a token on the repository writes the string
      // this row draws, and nothing between the GraphQL answer and the attribute constrains its
      // scheme (`prq::checks::failing_contexts` takes it as it comes).
      //
      // One `https` and one `javascript:` on the SAME row, because a single hostile check would let
      // "the guard works" and "the row draws no check links at all" pass for each other.
      ...checks([
        { status: "COMPLETED", conclusion: "FAILURE", name: "build (nightly)",
          detailsUrl: "https://ci.example/1" },
        { status: "COMPLETED", conclusion: "FAILURE", name: "deploy (staging)",
          detailsUrl: HOSTILE_CHECK_URL },
      ]),
    }),
    pr(6, "the tenant seam I am waiting on", "me", {
      mergeable: "MERGEABLE", mergeStateStatus: "CLEAN",
      reviewThreads: { totalCount: 2, nodes: [
        { id: "PRRT_open", isResolved: false, isOutdated: false, comments: { nodes: [
          { author: { login: "dana" }, createdAt: "2026-08-06T09:00:00Z",
            url: "https://github.com/acme/thing/pull/6#discussion_r1" }] } },
        { id: "PRRT_done", isResolved: true, isOutdated: false, comments: { nodes: [
          { author: { login: "sam" }, createdAt: "2026-08-06T10:00:00Z",
            url: "https://github.com/acme/thing/pull/6#discussion_r2" }] } },
      ] },
      comments: { totalCount: 1, nodes: [
        { author: { login: "dana" }, body: "Can we ship this before Friday?",
          createdAt: "2026-08-06T11:00:00Z",
          url: "https://github.com/acme/thing/pull/6#issuecomment-9" },
      ] },
      ...checks([{ status: "COMPLETED", conclusion: "FAILURE", name: "build (nightly)",
                   detailsUrl: "https://ci.example/2" }]),
    }),
  ]));

  // A working clone with a CODEOWNERS, so stage 0 (ownership) runs for real rather than being
  // skipped by an absent file — the path that decides how deep a summary goes, and the same file
  // the module list is derived from.
  fs.mkdirSync(path.join(root, "work", ".github"), { recursive: true });
  fs.writeFileSync(path.join(root, "work", ".github", "CODEOWNERS"), "src/ @me\nweb/ @someone-else\n");
  // Real directories, so the module list is not empty: a CODEOWNERS pattern that names no directory
  // is deliberately not a module.
  fs.mkdirSync(path.join(root, "work", "src"), { recursive: true });
  fs.mkdirSync(path.join(root, "work", "web"), { recursive: true });
  fs.writeFileSync(path.join(root, "work", "src", "parser.rs"), "const TIMEOUT: u64 = 5;\n");
  fs.writeFileSync(path.join(root, "work", "web", "app.js"), "export const app = 1;\n");
  // **Committed, not just written.** The tree is read with `git show HEAD:<path>` off a mirror, so a
  // file that exists on disk and not in a commit is a file skein cannot see — which is exactly right
  // (a mirror can never supply a gitignored file) and exactly what this fixture used to get wrong.
  // Its own comment admitted "the fixture's clone is not a git repo" while asserting things only a
  // git repo can answer.
  const wgit = (...a) => spawnSync("git", ["-C", path.join(root, "work"), ...a], { stdio: "ignore" });
  wgit("init", "-q", "-b", "main");
  wgit("add", "-A");
  wgit("-c", "user.email=a@b", "-c", "user.name=a", "commit", "-qm", "the tree the mirror carries");

  // **The mirror, made here rather than fetched.**
  //
  // `repos::Tree` reads this repo through `git show HEAD:<path>` against `<home>/repos/acme/mirror`,
  // and `ensure_mirror` makes one by cloning `repo.source` — a GitHub URL this test has no network
  // for, and which does not exist in any case. `mirror_is_made` is the seam: it asks only whether
  // the directory holds a `HEAD` file and an `objects/` directory, so a mirror already sitting there
  // is adopted and no clone is attempted.
  //
  // Cloned from `work`, which makes `work` the mirror's `origin` — so `fetch_mirror`, and
  // `refreshMirror` below, pick up later commits from the checkout on disk. The `source` in
  // `repos.json` stays the GitHub URL because that is where the SLUG comes from
  // (`gitgate::repo_slug` reads `repo.source`), and the slug is what the whole queue is addressed
  // by. The two are allowed to disagree here precisely because only one of them is ever fetched.
  makeMirror(root, home);

  // A GitHub that answers from fixture files, on a real socket. This replaces a fake `gh` binary on
  // `$PATH`: skein reads the API directly now, so the seam that tells the truth is the wire.
  //
  // It answers WHOLLY — every membership search, and `/user/teams` — because a queue that is
  // complete is the state nearly every check below is about. The refusal a login without
  // `read:org` gets is still here, as `github.refuseTeams(true)`, driven by the handful of checks
  // that are about the gap itself.
  const github = await createGitHub(root);

  // sbx stand-in: an empty fleet is fine — review does not depend on any box being alive, which is
  // itself part of what this file proves.
  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, `#!/bin/sh\ncase "$1" in ls) echo '[]'; exit 0 ;; esac\nexit 0\n`);
  fs.chmodSync(sbx, 0o755);

  // A `claude` that answers all THREE prompts skein sends, told apart the way the prompts differ:
  // the merged one (the reading whose review the session POSTS to GitHub itself) is the only one
  // that says "the REVIEW half", stage 1 asks for the strict format without it, and stage 2 is the
  // prose brief. A change to any of those contracts shows up here as a summary that stops
  // arriving. It used to key on the literal `REVIEW:`, which was in the merged prompt's answer
  // format until the review stopped coming back to skein at all.
  const claude = path.join(bin, "claude");
  fs.writeFileSync(claude, `#!/bin/sh
# **The prompt arrives on STDIN** (SKEIN-684), and nothing on argv is it. It used to be the last
# argument — never the fourth, since a reading is a conversation (SKEIN-393) and the command line
# carries --session-id/--resume between the model and the prompt — and then skein stopped putting
# it there at all, because a real merged prompt runs to 305,366 bytes against a MAX_ARG_STRLEN of
# 131,072 on ordinary hardware, and argv is world-readable in /proc/<pid>/cmdline.
#
# **On the path this fixture pins, and on no other one** (SKEIN-706). Setting SKEIN_CLAUDE_BIN is
# how this stub is used at all, and ai::claude_in_turn reads that same variable as "run exactly
# this, and therefore run it HERE" — so this fixture forecloses the in-box destination before it is
# tried, and can say nothing about it. A reading that HAS a review box still puts the whole script,
# heredoc'd prompt included, on argv as one element, so both numbers above are still live there.
#
# The refusal below is the load-bearing half, and the comment this replaces is why. It predicted
# its own failure in as many words: read from the wrong place, this fixture "fails by falling
# through to the brief, so the summary simply stops arriving and four assertions blame the UI."
# That is exactly what happened — 8 of 97 checks failed and every one of them named the page.
# So an empty prompt is now an ERROR that says where the prompt went missing, and never an answer.
p=$(cat)
if [ -z "$p" ]; then
  echo "fixture claude: nothing on stdin. skein sends the prompt there (SKEIN-684); if it has" >&2
  echo "moved back to argv, this fixture answers the wrong thing rather than nothing." >&2
  exit 3
fi
brief='## What it does\\n\\nShortens how long a request waits before giving up.\\n\\n## What changes in how it works\\n\\nCallers that relied on the old 30s ceiling now fail after 5s.\\n'
case "$p" in
  # The second turn. Answered "nothing new" — what the sweep prompt itself calls the expected
  # outcome — and matched FIRST, because it also carries "Answer in EXACTLY this format".
  *"account for what it actually covered"*)
    printf 'OVERALL: nothing new\\n' ;;
  *"the REVIEW half"*)
    case "$p" in
      *"default timeout"*)
        printf 'KIND: feature\\nLINE: the request timeout default drops from 30s to 5s.\\nEXPAND: yes\\nFLAGS: default, behaviour\\nDETAIL:\\n'
        printf "$brief" ;;
      *)
        printf 'KIND: fix\\nLINE: stops the parser crashing on empty input.\\nEXPAND: no\\nFLAGS: none\\nDETAIL:\\nnone\\n' ;;
    esac ;;
  *"Answer in EXACTLY this format"*)
    case "$p" in
      *"default timeout"*)
        printf 'KIND: feature\\nLINE: the request timeout default drops from 30s to 5s.\\nEXPAND: yes\\nFLAGS: default, behaviour\\n' ;;
      *)
        printf 'KIND: fix\\nLINE: stops the parser crashing on empty input.\\nEXPAND: no\\nFLAGS: none\\n' ;;
    esac ;;
  *)
    printf "$brief" ;;
esac
exit 0
`);
  fs.chmodSync(claude, 0o755);
  return { root, bin, home, github, sbx, claude };
}

// The fleet's API token. Written by the fixture rather than read back after startup: the server
// mints one on first use, and a test that raced that would be flaky for a reason having nothing to
// do with what it is testing. The auth path itself is still exercised end to end — the browser gets
// its cookie from `?t=`, exactly as a person does, and every direct fetch carries the bearer.
const API_TOKEN = "t".repeat(64);
const apiToken = () => API_TOKEN;
const authHeader = () => ({ Authorization: `Bearer ${API_TOKEN}` });

// ---------- harness ----------
const { check, results, report } = ledger();
// Bound to the page below, once it exists — all three ask a question of it.
let page, mustSee, find, settle;
/** How long any wait in this file gives the pane before it calls the pane broken — and the ONLY
 *  number in any of them (SKEIN-801, SKEIN-802).
 *
 *  Every fixed `settle` the waits below replace was a duration somebody guessed on an idle box —
 *  200, 300, 400, 600 — standing between an act and an assertion about what the act drew. A guess
 *  that is enough here and not on a machine running five lanes under two dozen spinners is not a
 *  wait at all: it is a verdict decided by how busy the box is, which is how a real regression in
 *  this tier came to arrive looking exactly like noise. Waiting for the thing ITSELF answers on the
 *  frame it happens — sooner than any of those beats even on a quiet box — so the only number left
 *  is where "slow" becomes "broken", and that is `page.setDefaultTimeout`'s, which this file had
 *  already chosen. No site gets a number of its own. */
const REDRAW_MS = 4000;
/** Wait for something the pane is supposed to come to hold, and fail in this suite's own words.
 *
 *  `page.waitForFunction` alone reports `Timeout 4000ms exceeded`, which is the one sentence that
 *  cannot tell a pane that is wrong from a box that is slow — and telling those two apart is the
 *  whole of SKEIN-801. So every wait carries the message its fixed beat used to throw, and `why` is
 *  read AFTER the timeout, so the failure quotes the state that was really there. */
const until = async (fn, arg, why) => {
  try { await page.waitForFunction(fn, arg, { timeout: REDRAW_MS }); }
  catch { throw new Error(typeof why === "function" ? await why() : why); }
};
/** The visible rows of one group, by title — the queue as a person reads it.
 *
 * Keyed on `data-lane`, which is `moveOf`'s word (`yours` / `theirs` / `not-ready` / `archived`)
 * rather than the heading's, because the heading is prose and prose is what a design changes. */
const laneTitles = async (lane) => page.evaluate(l => {
  const el = document.querySelector(`#revpane .revlane[data-lane="${l}"]`);
  return el ? [...el.querySelectorAll(".revtitle")].map(e => e.textContent.trim()) : null;
}, lane);
/** The heading of one group, as a reader sees it. */
const laneHead = async (lane) => page.evaluate(l =>
  document.querySelector(`#revpane .revlane[data-lane="${l}"] h4`)?.textContent.replace(/\s+/g, " ").trim() ?? null,
lane);
/** Open a folded group. The two groups under "your move" are places you go looking, so they draw a
 *  count until asked (SKEIN-302) — a test that wants their rows has to ask, exactly as a reader does.
 *
 *  **This is the site SKEIN-716 was filed about, and the reason is visible in the shape.** Between
 *  finding the heading and pressing it there is a whole `page.evaluate` — `laneTitles` — and
 *  `renderReview` replaces `#revpane`'s `innerHTML` wholesale (src/web/index.html:4014), so a
 *  repaint landing in that gap detached the heading and the press died with
 *  `elementHandle.click: Element is not attached to the DOM`. Three checks went down together on a
 *  four-lane run, because two of them were reading the group this one never opened. Reproduced in
 *  place by putting `await page.evaluate(() => renderReviewNow())` in that gap: the same three, with
 *  the same three messages — and then the one call to this helper that is NOT inside a `check`
 *  taking the rest of the suite with it, which the four-lane run had not got as far as showing.
 *
 *  A locator is the query rather than the node, so the press resolves the heading against the
 *  document that exists when it presses. `find` still says "there is no ... group on screen" in the
 *  suite's own words rather than leaving a missing group to Playwright's timeout.
 *
 *  **And the heading is WAITED for, which is the whole of SKEIN-802.** `find` asks once by default
 *  — that is `page.$`'s timing, deliberately — so a group that is one repaint away from being on
 *  screen reads here as a group that does not exist. The caller before this one has just pressed a
 *  filter chip; under five lanes and twenty-four spinners the pane had not redrawn under it yet,
 *  this said "there is no not-ready group on screen to open", and the twenty checks after it ran
 *  against a queue the throw had left unfolded. The press that opens the group is waited for the
 *  same way: the rows appearing ARE the fold opening, and there is nothing else a beat could have
 *  been waiting for. */
const unfold = async (lane) => {
  const h = await find(`#revpane .revlane[data-lane="${lane}"] h4.revfold`, { within: REDRAW_MS });
  if (!h) throw new Error(`there is no ${lane} group on screen to open`);
  if (!(await laneTitles(lane)).length) {
    await h.click();
    await until(l => {
      const el = document.querySelector(`#revpane .revlane[data-lane="${l}"]`);
      return !!el && el.querySelectorAll(".revtitle").length > 0;
    }, lane, async () => `the ${lane} group did not open: it draws ${JSON.stringify(await laneTitles(lane))}`);
  }
};
/** Fold a group back, if it is open. The mirror of `unfold`, and idempotent for the same reason:
 *  the fold is page state that survives a re-render, so a bare click is a toggle rather than a
 *  close.
 *
 *  The fold landing is waited for rather than slept through, for [`unfold`]'s reason and one of its
 *  own: this is what the checks below INHERIT. A fold that had not taken by the time a fixed beat
 *  expired left the queue two groups deeper for everything after it, and `revNav` two rows longer
 *  — so the keyboard checks were walking a different queue run to run (SKEIN-802). */
const fold = async (lane) => {
  const h = await find(`#revpane .revlane[data-lane="${lane}"] h4.revfold`);
  if (h && (await laneTitles(lane)).length) {
    await h.click();
    await until(l => {
      const el = document.querySelector(`#revpane .revlane[data-lane="${l}"]`);
      return !el || el.querySelectorAll(".revtitle").length === 0;
    }, lane, async () => `the ${lane} group did not fold back: it still draws `
      + `${JSON.stringify(await laneTitles(lane))}, which every check below this one would inherit`);
  }
};
/** Press one of the queue's filter chips, and wait for the pane to be drawn under it.
 *
 *  `setRevFilter` assigns `revFilter` and repaints in the same call (src/web/index.html:4162), so
 *  the filter having changed IS the pane having been redrawn under it — and it is the condition
 *  every group the caller then opens depends on. What stood at both call sites was a fixed 500ms
 *  between the press and the first `unfold` (SKEIN-802). */
const pressFilter = async (key, label) => {
  await page.click(`#revpane .revchip:has-text('${label}')`);
  await until(k => revFilter === k, key, async () =>
    `pressing "${label}" did not put the queue under the ${key} filter — it is `
    + `${JSON.stringify(await page.evaluate(() => revFilter))}`);
};
/** Open the row whose title says `titleText`, by pressing it the way a reader does.
 *
 *  **A locator, not an `ElementHandle`, and that is the whole point.** Five checks below used to do
 *  this by hand: `page.$$(".revrow")`, then `await row.$eval(".revtitle", …)` to read each title,
 *  then `await row.click()`. Every `await` in there is a chance for the pane's pump to land and
 *  `renderReview` to replace every row node — after which the handle names a element that is no
 *  longer in the document, and the click dies with
 *
 *      elementHandle.click: Element is not attached to the DOM
 *
 *  which is three of the intermittent failures on SKEIN-567, all of them arriving and departing
 *  across runs of the same tree. An `ElementHandle` is a pointer to one node and does not retry; a
 *  locator is a *query*, re-resolved on each attempt, so a re-render mid-action is retried instead
 *  of thrown. Reproduced both ways against a page re-rendering on a 50ms interval: the handle
 *  raises the message above, the locator clicks.
 *
 *  `collapsedOnly` skips a row that is already expanded — `.revbody` is what an open row has — for
 *  the checks that need a row whose control strip has not been replaced by a receipt.
 *
 *  **Five more copies of the loop were left behind and are gone now** (SKEIN-716): SKEIN-567
 *  converted the checks whose failures it had in hand and left the rest, so the same defect stayed
 *  live at five sites for as long as none of them happened to lose the race. Four of them gained an
 *  assertion by moving here — the loop `break`s on the first title that matches and does NOTHING at
 *  all when none does, so the check that followed it was silently about whichever row happened to be
 *  open; `rows.first().click()` fails instead, naming the selector it waited for.
 *
 *  **The fifth meant the no-op**, and only converting it said so out loud: the authoring check
 *  reaches its last third with the row already expanded, `collapsedOnly` therefore matches nothing,
 *  and the loop's silence was the whole of "leave it open". Pressing it there closes the row and
 *  the check fails with `locator.click: Timeout 4000ms exceeded`, which is what this conversion did
 *  before [`openRow`] below existed to say the intention.
 */
const pressRow = async (titleText, { collapsedOnly = false } = {}) => {
  let rows = page.locator("#revpane .revrow")
    .filter({ has: page.locator(".revtitle", { hasText: titleText }) });
  if (collapsedOnly) rows = rows.filter({ hasNot: page.locator(".revbody") });
  await rows.first().click();
};
/** Make sure the row whose title says `titleText` is open, pressing it only if it is not.
 *
 *  The other half of [`pressRow`]'s `collapsedOnly`, and the distinction is which absence is a
 *  fault: `pressRow(…, { collapsedOnly: true })` wants a collapsed row and fails when the queue has
 *  none, while this wants the row OPEN and no collapsed match means it already is. The rest of the
 *  argument — a locator rather than a handle — is `pressRow`'s. */
const openRow = async (titleText) => {
  const collapsed = page.locator("#revpane .revrow")
    .filter({ has: page.locator(".revtitle", { hasText: titleText }) })
    .filter({ hasNot: page.locator(".revbody") });
  if (await collapsed.count()) await collapsed.first().click();
};
/** Re-read GitHub into the pane, past the queue's 60s micro-cache — the refresh button's own call
 *  (`loadReview(true)` → `/api/review?force=1`, src/web/index.html:3301, which reaches
 *  `prq::queue(repo, force)` and its `Duration::ZERO`, `src/prq/refresh.rs`).
 *
 *  This is how the GitHub seams (`refuseTeams`, `emptyQueue`) get to the page: changing what the
 *  stub answers changes nothing anybody can see until the queue is asked again. */
const refreshQueue = async () => {
  // Anything already in flight lands FIRST, and the stale-answer chase is stopped before the forced
  // read goes out. Both matter: `loadReview` re-asks a `fresh: false` answer at 4s, 8s, 16s…
  // (src/web/index.html:3336), and a retry landing after the forced read puts the REMEMBERED queue
  // back over it — which is not hypothetical here, because a queue that was not whole is served and
  // never remembered (`prq::queue_within`, SKEIN-447), so what is remembered is the last whole one.
  await page.waitForFunction(() => !revLoading, null, { timeout: 20000 });
  await page.evaluate(() => { clearTimeout(revStaleTimer); revStaleTimer = null; });
  // And nothing in the pane may hold the caret. §6 focus rule 2 DEFERS any render nobody asked for
  // while the pane holds a caret or an uncollapsed selection (`revRenderHeld`,
  // src/web/index.html:3600), so a queue that arrives while the search box or a composer has focus
  // lands in state and is never painted — the pane stays on the previous queue and every assertion
  // about what is drawn is about the wrong one. A reader blurs by clicking away; a suite says so.
  await page.evaluate(() => { const ae = document.activeElement; if (ae && ae.blur) ae.blur(); });
  await page.evaluate(() => loadReview(true));
  await page.waitForFunction(() => !revLoading, null, { timeout: 20000 });
  await settle(400);
};

// ---------- run ----------
const fx = await makeFixture();
const door = await openDoor();
const port = door.port;
const { srv, log } = await startServer({
  door,
  token: apiToken(),
  env: {
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_LS_CMD: `${fx.sbx} ls --json`,
    SKEIN_HOME: fx.home,
    // **Pinned, or this suite is pointed at the machine's own fleet** (SKEIN-530).
    // `util::fleet_root` — `src/util.rs`, and it has never been in `config` — refuses an unpinned
    // TEST process (SKEIN-690), which the server is whenever `cargo test` runs this suite through
    // `tests/browser_suites.rs`, since `$SKEIN_TEST` reaches it through the spawn; run by hand
    // with no marker it answers `/boxes` instead, a real fleet on any machine running skein.
    // Either way `SKEIN_HOME` is not enough on its own: it covers the store, while placement
    // records, gitgate requests and box sessions live under the fleet root.
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
    SKEIN_GITHUB_API: fx.github.url,
    // Deliberately NO SKEIN_REVIEW_AI: reading PRs is on by default, and the whole summary half of
    // this suite passing without an override is the proof of it.
    SKEIN_CLAUDE_BIN: fx.claude,
    PATH: `${fx.bin}:${process.env.PATH}`,
  },
});
const browser = await chromium.launch();
page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
mustSee = seeing(page);
find = finding(page);
settle = settler(page, 500);
// The same number the waits above use, said once: an act and a wait that disagree about when this
// pane is broken are two answers to one question (SKEIN-801).
page.setDefaultTimeout(REDRAW_MS);
// The page's own errors, with the browser's own complaints about its transport kept apart and
// reported rather than failing this run — the distinction is structural, not a list of spellings;
// see `harness/browser.mjs::erring` (SKEIN-998, SKEIN-1010). A 5xx is a different fact: the server
// answered and said no, which this suite has always counted, and still does.
const { errors: noise, sayBlips } = erring(page);
page.on("response", r => { if (r.status() >= 500) noise.push(`[${r.status()}] ${r.url()}`); });

await page.goto(`http://127.0.0.1:${port}/?t=${apiToken()}`, { waitUntil: "domcontentloaded" });
await settle(800);

export { HOSTILE_CHECK_URL, REDRAW_MS, authHeader, browser, check, find, fold, fx, laneHead, laneTitles, log, mustSee, noise, openRow, page, port, pressFilter, pressRow, refreshMirror, refreshQueue, report, results, sayBlips, settle, srv, unfold, until };
