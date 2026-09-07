# Changelog

Notable changes to skein, in the shape Keep a Changelog describes: grouped under Added, Changed,
Fixed, Removed and Security, newest first.

**Nothing has been released yet.** `Cargo.toml:9` says `version = "0.1.0"`, and the repository
carries no release tag — `git tag` lists `archive/in-fleet-final` and `archive/rescue-2026-08-29`,
which mark archived branches, not versions. So there is no version history to write, and this file
is instead: what skein does today, under **Unreleased**, and how it got there, under **Development
history**.

Every entry below traces to at least one commit. Nothing here was written from memory.

**Every count below names the commit it was taken at**, so it reproduces for good rather than
until the next push. The first version of this block did not, and by the time anyone re-ran it
every figure in it was wrong.

```sh
git log --format='%s' 02ad7cfb | wc -l                          # 878 commits
git log --reverse --format='%ad' --date=short | head -1          # 2026-06-28
git log -1 --format='%ad' --date=short 02ad7cfb                  # 2026-09-07
git log --format='%s' 02ad7cfb | grep -oE '^[a-z]+' | sort | uniq -c | sort -rn
```

`fix` 370, `feat` 274, `docs` 117, `refactor` 45, `test` 44, `perf` 9, `chore` 7, `build` 4, `ci` 2,
`style` 2, and one each of `wip` and `tools`, which are the two that are not conventional types at
all. No commit in this history is marked `!` and no body carries a `BREAKING CHANGE` footer
(`git log --format='%B' 02ad7cfb | grep -c '^BREAKING CHANGE'` → 0), so nothing below is a declared
break — the removals are recorded as removals because they removed a capability, not because a
version boundary was crossed.

The anchor in that last command is not decoration. Unanchored it returns 1, and the one match is
*this paragraph*, quoted inside the commit body that added it: a falsifier that can be satisfied by
the sentence it is checking is not a falsifier. The footer form conventional commits defines is at
the start of a line, so anchoring it is both stricter and self-immune.

---

## [Unreleased]

The state of the tree at `2026-09-07`.

### Added

**Running a fleet of agent boxes**

- One shared sandbox hosts every box, each in its own `bwrap` namespace, instead of a microVM per
  box (`de06b0f6`, `48efd31e`). The pool's memory, CPUs and disk are one set of numbers you choose
  at creation (`993ac6bc`, `72527eef`, `313a6788`), with a per-box ceiling on top so one runaway
  box cannot take the fleet down (`5fabf365`, `3787fbd1`).
- Installing is one downloaded file and one `sbx` command, with nothing built or run on your own
  machine (`bf9f89f9`). The sandbox carries a kit that puts the cockpit back at every start, so a
  restart no longer comes back with the install intact and nothing serving (`f3ea1fa0`, `ae4d8a34`).
- A resize carries every box's work and its conversation across, rather than rebuilding it
  (`9aec8281`, `9e4e9ec0`), and refuses rather than silently discarding what Docker is holding
  (`7fbb4483`).
- `skein doctor` is a preflight for the failures that otherwise happen quietly (`d960d672`).
- Settings → Update, because skein could not previously tell you it was behind (`222615a6`).

**The web cockpit**

- A live fleet board over SSE with an embedded per-box terminal — click a box, talk to that agent
  in the browser (`5370a435`, `9520f864`), with a persistent multi-terminal dock (`864379e0`), tabs
  you can reorder and drive from the keyboard (`dccd9417`), and a full mobile terminal with an
  on-screen key bar (`4f3aa151`).
- The board says which box needs you and what it is asking (`4701418d`), lets you answer it by
  holding a key (`93eeb643`), and says which transport is carrying its calls (`0cf694d8`).
- A box's own CPU, memory and disk on hover (`303e6d84`), useful before the sandbox answers rather
  than after.
- A first run that tells you the three things you need to know (`090169cd`), and a fleet-sizing
  dialog that shows what your host has beside what skein proposes to take (`3cb12b4d`).
- Your open tabs survive a reload (`b096e881`) — including the case that used to lose them for
  good, where a reload landing during a slow `sbx ls` read an empty snapshot as "those boxes are
  gone" and persisted it over the saved list.

**Reviewing pull requests inside skein**

- One review queue across every repository, where a repository is a filter and not a mode
  (`42f06eab`), triaged into lanes that say whose move it is rather than whether you have already
  acted (`25fe1c3a`), worked from the keyboard (`8e56875f`).
- A stack of dependent pull requests is one row, opened in the order it must be reviewed
  (`2348d65c`) — and it contradicts the titles when the author numbered them wrong.
- skein reads a pull request once it has settled and the reading survives the commits after it
  (`c5b2cd79`); the change is readable in the pane, and a verdict exists only beside it
  (`e0058f25`).
- skein drafts a review, you vet each comment, then it posts (`45adbd37`). A verdict is a receipt
  where you pressed it, with eight seconds to take it back (`830c0326`).
- Every reading runs in the pull request's own box (`35bdc58f`, `f58042b1`), and the reviewer stands
  in the change rather than reading only the diff (`e1250b1c`).
- A merge train: serial per repository, trunk-based only, a skip moves ahead (`9ff50312`), watched
  and paused from the workflows pane (`e380d232`).
- A model budget bounds what skein reads unasked (`3375b8f6`, `3fbd4754`), and a round skein was
  not asked for has to earn itself first (`8ccbf03e`).

**Credentials, and one of them doing every job it can**

- skein reads GitHub over the API itself; the `gh` dependency is gone (`1009545c`), so there is no
  CLI to install, nothing to authenticate, and no system keyring being asked for a password every
  three minutes.
- A stored token covers exactly one repository, enforced where it is used (`8e131b46`), with a
  GitHub App, a per-repo token, or the account token as the third choice (`c60471f2`).
- A push to a repository you cannot write files the request instead of dead-ending (`98db1f9e`), and
  a person approves it in the cockpit (`368b1c4d`).
- One `skein login` authenticates once where every box seeds from (`8772dea5`) and reaches the boxes
  already running (`ec018a4c`).

**The primitives, and the gates that make them law**

- `Answer<T>`, the provenance primitive four features had hand-rolled (`f1f30008`); `Place`, so a
  box is an identity and where it runs is a lookup (`a670cf97`); the four Sources, with a checker
  that makes the law a law (`71c501d0`); Acts that each say what they make wrong (`6699825b`); and
  an Operation that says who may perform it, where "nobody" is an answer it can give (`0e7e061a`).
- Prose that names a function the code does not have fails the build (`bb75527d`).
- A host warden with five endpoints, of which the two that only report cannot be built with a doer
  (`1960493a`); a person approves at the host on a surface no box can reach (`98269378`); and a
  flood can neither buy an approval nor block one (`7e582d24`).

**Work tracking, transcripts and shared state**

- One backlog the fleet claims from, with the token never leaving the host (`f9f0ac47`), a
  connection that is a gateway and its token (`67c4f8a5`), and a box choosing its tracker when it is
  created (`c0e42015`).
- The conversation is read from the record rather than scraped off the screen (`5c8c3695`) and lives
  on the host rather than in the sandbox (`72c99026`).
- Boxes talk to each other directly, with no skein code in the path (`f9cf1294`), and a standing
  debt must settle before the board speaks it (`58332f16`).
- A box can ask the fleet for a system package, and a person answers (`8911e8fc`).

### Security

- A box's own state comes back read-only, and it can still push (`1c6bdbb5`).
- The tree its user works in is not in the sandbox at all (`36562fc2`); a box sees its own repo and
  nothing the sandbox mounts for anyone else (`4b6f3ae8`).
- A box could read the fleet agent's token, and be root in the sandbox — closed (`f9837c4e`).
- `/run` is covered where it costs nothing, and stated where it does not (`8890f7d2`), after a box
  was found able to reach the sandbox's Docker socket.
- The fleet's credential flows down; a box's never flows up over it (`5a54a883`).
- A token directory that is a link is refused rather than followed (`ca3245cf`).
- The volume a fleet serves from is covered from every box (`1d1b1a95`), and the warden's secret
  lives on the volume it is covered by while the record stays off it (`c342ed03`, `0963526f`).
- **Shell injection through a value a box chooses.** A path that climbs out is refused wherever it
  would be read, and a request id that is not a plain name is refused before it is used
  (`527cef31`). Found by an independent audit, 2026-08-26.
- **Cross-site scripting through a reviewed repository's directory name.** Forty-six click handlers
  were built as `onclick="f('${esc(x)}')"`; an attribute value is entity-decoded before the JS
  parser reads it, so the escaped quote arrived as a quote and closed the string. Fixed, with a
  build gate that fails if the shape comes back (`ce05abcd`).
- One writer for every credential, and a type that will not talk (`c9fa90d9`).
- One covered directory, a socket instead of a port, and a `PATH` no box can write (`187d42f5`).
- The cockpit's port is published when the sandbox is created, closing the interval a box could
  squat it in (`7f3b9d21`).

### Removed

- **The per-box VM.** Every box now shares one sandbox (`de06b0f6`); the board says which boxes
  still own a whole VM so they can be migrated (`c026d145`, `69953db3`).
- **The `gh` CLI dependency** (`1009545c`).
- **Local-path repositories.** A repo is a remote, and the local-path half is gone (`ab620adc`); a
  box clones from the repo's mirror rather than from somebody's checkout (`3dac3a9d`).
- **The box-level PR tools**, replaced by the repository queue (`d0eb4581`).
- **Verify** — the pane that said whose work stands up (`b5998c83`, 2026-07-27) — removed a week
  later, and a box is told who it commits as instead (`b9d0a352`).
- **The collision radar**, replaced by measuring against the remote base inside the box
  (`3958a718`).
- **The reading view**, whose job the row now does (`d5d0e956`, with the orphaned keys reported
  rather than silently dropped in `6ac450c7`).
- **The in-sandbox agent**, deleted, with the two jobs it had that were never transport moved to
  `skein-server` (`f7ef8702`).

---

## Development history

Counts per month reproduce with
`git log --format='%ad' --date=format:'%Y-%m' 02ad7cfb | sort | uniq -c`,
and each is taken at that commit for the reason the block at the top of this file gives.

### 2026-09 — 89 commits: going public, and the last of the host

The work of making the repository readable by a stranger, and the last few things that still
assumed skein ran on the host.

- Creating a fleet stops being a side effect of starting a box and becomes something in-fleet skein
  asks the warden to perform (`53269c38`).
- Moving the volume becomes an Operation skein reports and never performs (`717162d9`).
- The fleet-wide GitHub secret stops being skein's to write, and the machine-global store goes with
  it (`78495f34`).
- The link guard decodes what a browser decodes, and handler arguments stop being hand-quoted
  (`ce05abcd`) — the XSS above.
- One writer for every credential, and a type that will not talk (`c9fa90d9`).
- Four minutes of every box creation, spent on nothing, recovered (`9d27ca15`); a box clones the
  branch it needs and can still fetch the rest (`8fd37aa4`).
- ~~CI installs chromium, so the six browser suites that prove the page finally run
  (`acfc9682`).~~ **Not on this branch.** `acfc9682` is the only one of this file's commits that
  `git merge-base --is-ancestor <sha> master` rejects; it is on the unmerged `ci-tier` branch, and
  `grep -niE 'chromium|playwright|npm' .github/workflows/ci.yml` finds nothing. So the six browser
  suites are still silently skipped on every green CI run, which is the state this entry claimed to
  have ended.

### 2026-08 — 645 commits: the shared sandbox, the review queue, and the warden

By far the largest month, and three efforts at once.

- **The fleet becomes one sandbox** (`de06b0f6` onward, 2026-08-03): namespace addressing verified
  in a box (`48efd31e`), the store resolving at one path on both sides of the mount (`926c0e64`),
  resize carrying work and conversation across (`9aec8281`), and a login reused by every box
  (`fec8aadb`).
- **The isolation boundary is closed, repeatedly and with evidence** — the Security section above is
  almost all this month. The cover is proved by running `bwrap` rather than by reading its arguments
  (`5424885c`), which is the change that made the rest checkable.
- **The review queue is built** (2026-08-15 to 2026-08-31): a repository's PR queue (`13055de1`),
  one queue across every repository (`42f06eab`), stacks as rows (`2348d65c`), a drafted review you
  vet and post (`45adbd37`), reviews that run in the pull request's own box (`35bdc58f`), and the
  merge train (`9ff50312`).
- **The warden arrives** (2026-08-21): the endpoints (`1960493a`), an approval surface on the host
  that no box can reach (`98269378`), flood resistance (`7e582d24`), and create and destroy asked of
  it from host skein (`5d99e9ba`).
- **The primitives are named and enforced**: Sources with a checker (`71c501d0`), signals that
  declare their cost (`d10c1cc0`) and name the Source that produced them (`a48834a5`), Acts that say
  what they make wrong (`6699825b`).
- **skein learns where it is running** — host or inside the fleet (`f4651948`) — and every host-only
  call is answered rather than left to fail (`99f7f4a4`).
- **The board gets faster**: one producer feeds every board and sends only what moved (`7847b072`),
  `sbx ls` is asked when somebody wants it rather than thirty times a minute (`4e4fdfe9`), liveness
  reads the anchor it recorded (`152751e6`).

### 2026-07 — 72 commits: other runtimes, and reading the box's own screen

- Codex as a second runtime, with cross-agent handoff (`3886c9ef`), work migrated into replacement
  boxes (`d47c84de`), and a shared usage statusline for both (`757fb6af`).
- Turn state read from the box's own screen, so a decision clears when you make it (`5c1f9e43`) —
  and from Codex's screen too, which says "Action Required" out loud (`c357bd48`). A box running on
  half the signal says so (`8ac19e7b`).
- A project-scoped shared home (`2a41464f`) with an explicit, dry-run-first import (`bb8fbc60`).
- Attach any file or folder to a box, not just pasted images (`e4144c04`).
- One backlog the fleet claims from (`f9f0ac47`).

### 2026-06 — 72 commits: v0, in four days

skein's first commit is `2026-06-28`, and the fleet view, the cockpit, the embedded terminal, the
diff viewer and the mailbox all landed inside the first week.

- The fleet status view over `sbx` and the shared store (`7a76b911`).
- The web cockpit, and the lib/CLI split (`5370a435`).
- The embedded per-box terminal, WebSocket to PTY (`9520f864`).
- The diff viewer, with inline comments sent back to the agent (`305314b2`, `5d6bd77b`).
- The mailbox and broadcast, and launching a box from the UI (`9a84c227`).
- Generalised to any repository, with skein-owned repos, its own kit and settings (`a74e2450`).
- skein owns the turn-state probe: hooks installed into the store, status read host-side
  (`96fdfa7d`).
