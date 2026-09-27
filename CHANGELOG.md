# Changelog

Notable changes to skein, in the shape Keep a Changelog describes: grouped under Added, Changed,
Fixed, Removed and Security, newest first.

**Nothing has been released yet.** `Cargo.toml:9` says `version = "0.1.0"`, and the repository
carries no release tag — `git tag` lists `archive/in-fleet-final` and `archive/rescue-2026-08-29`,
which mark archived branches, not versions. So there is no version history to write, and this file
records how skein got here: what the tree held on `2026-09-07`, under **Unreleased**, frozen at that
date, and the path to it, under **Development history**. What skein does today is the README's.

Every entry below traces to at least one commit, though most were cited by hashes this history no
longer has. Nothing here was written from memory.

**Every count below names the commit it was taken at**, so it reproduces for good rather than
until the next push. The first version of this block did not, and by the time anyone re-ran it
every figure in it was wrong.

```sh
git log --format='%s' a8cbac3 | wc -l                            # 858 commits
git log --reverse --format='%ad' --date=short | head -1          # 2026-06-28
git log -1 --format='%ad' --date=short a8cbac3                   # 2026-09-07
```

How many commits of each type the history holds is counted once, under "Commit messages" in
[`CONTRIBUTING.md`](CONTRIBUTING.md), against a commit of its own, and is not repeated here.

No commit in this history is marked `!` and no body carries a `BREAKING CHANGE`
footer (`git log --format='%B' a8cbac3 | grep -c '^BREAKING CHANGE'` → 0), so nothing below is a
declared break — the removals are recorded as removals because they removed a capability, not
because a version boundary was crossed.

The anchor in that last command is not decoration. Unanchored it returns 1, and the one match is
*this paragraph*, quoted inside the commit body that added it: a falsifier that can be satisfied by
the sentence it is checking is not a falsifier. The footer form conventional commits defines is at
the start of a line, so anchoring it is both stricter and self-immune.

---

## [Unreleased]

The state of the tree at `2026-09-07`, frozen there: a record, not a description of today.

### Added

**Running a fleet of agent boxes**

- One shared sandbox hosts every box, each in its own `bwrap` namespace, instead of a microVM per
  box. The pool's memory, CPUs and disk are one set of numbers you choose at creation, with a
  per-box ceiling on top so one runaway box cannot take the fleet down.
- Installing is one downloaded file and one `sbx` command, with nothing built or run on your own
  machine. The sandbox carries a kit that puts the cockpit back at every start, so a restart no
  longer comes back with the install intact and nothing serving.
- A resize carries every box's work and its conversation across, rather than rebuilding it, and
  refuses rather than silently discarding what Docker is holding.
- `skein doctor` is a preflight for the failures that otherwise happen quietly.
- Settings → Update, because skein could not previously tell you it was behind.

**The web cockpit**

- A live fleet board over SSE with an embedded per-box terminal — click a box, talk to that agent in
  the browser (`5370a435`, `9520f864`), with a persistent multi-terminal dock, tabs you can reorder
  and drive from the keyboard, and a full mobile terminal with an on-screen key bar.
- The board says which box needs you and what it is asking, lets you answer it by holding a key, and
  says which transport is carrying its calls.
- A box's own CPU, memory and disk on hover, useful before the sandbox answers rather than after.
- A first run that tells you the three things you need to know, and a fleet-sizing dialog that shows
  what your host has beside what skein proposes to take.
- Your open tabs survive a reload — including the case that used to lose them for good, where a
  reload landing during a slow `sbx ls` read an empty snapshot as "those boxes are gone" and
  persisted it over the saved list.

**Reviewing pull requests inside skein**

- One review queue across every repository, where a repository is a filter and not a mode, triaged
  into lanes that say whose move it is rather than whether you have already acted, worked from the
  keyboard.
- A stack of dependent pull requests is one row, opened in the order it must be reviewed — and it
  contradicts the titles when the author numbered them wrong.
- skein reads a pull request once it has settled and the reading survives the commits after it; the
  change is readable in the pane, and a verdict exists only beside it.
- skein drafts a review, you vet each comment, then it posts. A verdict is a receipt where you
  pressed it, with eight seconds to take it back.
- Every reading runs in the pull request's own box, and the reviewer stands in the change rather
  than reading only the diff.
- A merge train: serial per repository, trunk-based only, a skip moves ahead, watched and paused
  from the workflows pane.
- A model budget bounds what skein reads unasked, and a round skein was not asked for has to earn
  itself first.

**Credentials, and one of them doing every job it can**

- skein reads GitHub over the API itself; the `gh` dependency is gone, so there is no CLI to
  install, nothing to authenticate, and no system keyring being asked for a password every three
  minutes.
- A stored token covers exactly one repository, enforced where it is used, with a GitHub App, a
  per-repo token, or the account token as the third choice.
- A push to a repository you cannot write files the request instead of dead-ending, and a person
  approves it in the cockpit.
- One `skein login` authenticates once where every box seeds from and reaches the boxes already
  running.

**The primitives, and the gates that make them law**

- `Answer<T>`, the provenance primitive four features had hand-rolled; `Place`, so a box is an
  identity and where it runs is a lookup; the four Sources, with a checker that makes the law a law;
  Acts that each say what they make wrong; and an Operation that says who may perform it, where
  "nobody" is an answer it can give.
- Prose that names a function the code does not have fails the build.
- A host warden with five endpoints, of which the two that only report cannot be built with a doer;
  a person approves at the host on a surface no box can reach; and a flood can neither buy an
  approval nor block one.

**Work tracking, transcripts and shared state**

- One backlog the fleet claims from, with the token never leaving the host, a connection that is a
  gateway and its token, and a box choosing its tracker when it is created.
- The conversation is read from the record rather than scraped off the screen and lives on the host
  rather than in the sandbox.
- Boxes talk to each other directly, with no skein code in the path, and a standing debt must settle
  before the board speaks it.
- A box can ask the fleet for a system package, and a person answers.

### Security

- A box's own state comes back read-only, and it can still push.
- The tree its user works in is not in the sandbox at all; a box sees its own repo and nothing the
  sandbox mounts for anyone else.
- A box could read the fleet agent's token, and be root in the sandbox — closed.
- `/run` is covered where it costs nothing, and stated where it does not, after a box was found able
  to reach the sandbox's Docker socket.
- The fleet's credential flows down; a box's never flows up over it.
- A token directory that is a link is refused rather than followed.
- The volume a fleet serves from is covered from every box, and the warden's secret lives on the
  volume it is covered by while the record stays off it.
- **Shell injection through a value a box chooses.** A path that climbs out is refused wherever it
  would be read, and a request id that is not a plain name is refused before it is used. Found by an
  independent audit, 2026-08-26.
- **Cross-site scripting through a reviewed repository's directory name.** Forty-six click handlers
  were built as `onclick="f('${esc(x)}')"`; an attribute value is entity-decoded before the JS
  parser reads it, so the escaped quote arrived as a quote and closed the string. Fixed, with a
  build gate that fails if the shape comes back.
- One writer for every credential, and a type that will not talk.
- One covered directory, a socket instead of a port, and a `PATH` no box can write.
- The cockpit's port is published when the sandbox is created, closing the interval a box could
  squat it in.

### Removed

- **The per-box VM.** Every box now shares one sandbox; the board says which boxes still own a whole
  VM so they can be migrated.
- **The `gh` CLI dependency**.
- **Local-path repositories.** A repo is a remote, and the local-path half is gone; a box clones
  from the repo's mirror rather than from somebody's checkout.
- **The box-level PR tools**, replaced by the repository queue.
- **Verify** — the pane that said whose work stands up (2026-07-27) — removed a week later, and a
  box is told who it commits as instead.
- **The collision radar**, replaced by measuring against the remote base inside the box.
- **The reading view**, whose job the row now does (with the orphaned keys reported rather than
  silently dropped).
- **The in-sandbox agent**, deleted, with the two jobs it had that were never transport moved to
  `skein-server`.

---

## Development history

**These entries cited commits by the hashes they had before this history was rewritten for
publication; those hashes do not resolve here, so they are not cited.** Each entry keeps its text;
the few whose commit kept its hash still cite it.

Counts per month reproduce with
`git log --format='%ad' --date=format:'%Y-%m' a8cbac3 | sort | uniq -c`,
and each is taken at that commit for the reason the block at the top of this file gives.

### 2026-09 — 69 commits: going public, and the last of the host

The work of making the repository readable by a stranger, and the last few things that still
assumed skein ran on the host.

- Creating a fleet stops being a side effect of starting a box and becomes something in-fleet skein
  asks the warden to perform.
- Moving the volume becomes an Operation skein reports and never performs.
- The fleet-wide GitHub secret stops being skein's to write, and the machine-global store goes with
  it.
- The link guard decodes what a browser decodes, and handler arguments stop being hand-quoted — the
  XSS above.
- One writer for every credential, and a type that will not talk.
- Four minutes of every box creation, spent on nothing, recovered; a box clones the branch it needs
  and can still fetch the rest.
- CI installs chromium, so the six browser suites that prove the page finally run — true again,
  confirmed with `grep -niE 'chromium|playwright' .github/workflows/ci.yml`. The commit this entry
  first cited for it never merged under that hash and does not resolve here either, like the rest of
  this section; the capability landed through separate, later work.

### 2026-08 — 645 commits: the shared sandbox, the review queue, and the warden

By far the largest month, and three efforts at once.

- **The fleet becomes one sandbox** (2026-08-03): namespace addressing verified in a box, the store
  resolving at one path on both sides of the mount, resize carrying work and conversation across,
  and a login reused by every box.
- **The isolation boundary is closed, repeatedly and with evidence** — the Security section above is
  almost all this month. The cover is proved by running `bwrap` rather than by reading its
  arguments, which is the change that made the rest checkable.
- **The review queue is built** (2026-08-15 to 2026-08-31): a repository's PR queue, one queue
  across every repository, stacks as rows, a drafted review you vet and post, reviews that run in
  the pull request's own box, and the merge train.
- **The warden arrives** (2026-08-21): the endpoints, an approval surface on the host that no box
  can reach, flood resistance, and create and destroy asked of it from host skein.
- **The primitives are named and enforced**: Sources with a checker, signals that declare their cost
  and name the Source that produced them, Acts that say what they make wrong.
- **skein learns where it is running** — host or inside the fleet — and every host-only call is
  answered rather than left to fail.
- **The board gets faster**: one producer feeds every board and sends only what moved, `sbx ls` is
  asked when somebody wants it rather than thirty times a minute, liveness reads the anchor it
  recorded.

### 2026-07 — 72 commits: other runtimes, and reading the box's own screen

- Codex as a second runtime, with cross-agent handoff, work migrated into replacement boxes, and a
  shared usage statusline for both.
- Turn state read from the box's own screen, so a decision clears when you make it — and from
  Codex's screen too, which says "Action Required" out loud. A box running on half the signal says
  so.
- A project-scoped shared home with an explicit, dry-run-first import.
- Attach any file or folder to a box, not just pasted images.
- One backlog the fleet claims from.

### 2026-06 — 72 commits: v0, in four days

skein's first commit is `2026-06-28`, and the fleet view, the cockpit, the embedded terminal, the
diff viewer and the mailbox all landed inside the first week.

- The fleet status view over `sbx` and the shared store (`7a76b911`).
- The web cockpit, and the lib/CLI split (`5370a435`).
- The embedded per-box terminal, WebSocket to PTY (`9520f864`).
- The diff viewer, with inline comments sent back to the agent.
- The mailbox and broadcast, and launching a box from the UI.
- Generalised to any repository, with skein-owned repos, its own kit and settings.
- skein owns the turn-state probe: hooks installed into the store, status read host-side.
