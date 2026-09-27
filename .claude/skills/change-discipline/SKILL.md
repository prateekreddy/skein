---
name: change-discipline
description: The checks that must happen BEFORE writing code in this repo — trace the call path, check what was already decided, and prove the test can fail. Invoke before any non-trivial change, and before concluding that code is dead, that a test passes for the right reason, or that a design question is open.
---

# Think it through, then change it

Every rule here was bought with a real failure in this repo. None of it is generic caution; the
incident is named so the rule can be argued with rather than obeyed.

The failures share one shape: **acting on a belief that a thirty-second check would have
corrected.** Not sloppiness in the edit — a wrong premise, confidently implemented.

---

## The seven rules

They live in `CONTRIBUTING.md`, under **"Before you change anything"** — one copy, inside the gates,
readable by anyone who clones this repository, not only by an agent that happens to have this skill
loaded. Each one names the incident that bought it; that reasoning is the part worth your time, and
it is not repeated here, because two copies of it is how this file went stale — it listed six rules
for as long as CONTRIBUTING had seven.

`CONTRIBUTING.md` is the source of truth. Where it and this file disagree, believe it and fix this
one. The lines below are an index into it, not a summary of it:

1. Find out what was already decided — before building, and before filing.
2. Trace the whole path before you call anything dead.
3. A test you cannot make fail is worse than no test — and the sabotage that proves it goes quiet
   three ways: a stale snapshot, a snapshot in `/tmp`, an md5 that moved for the wrong reason.
4. Never send a field you did not mean to change.
5. Commit by explicit path, and do not stage early — a pathspec commits the working tree, even on
   `--amend`; and never rewrite a commit a record has cited.
6. Structural cuts: snapshot, match at the symbol's own indent, check the delta — and snapshot
   before any gate tool's `--update`; its table says which ones rewrite what a person wrote.
7. A heredoc eats the `\` that holds a Rust sentence together.

---

## The gates

**`tools/gates.sh` is the one list** — `tools/gates.sh --list` prints it, and `CONTRIBUTING.md`'s
"The gates" says what each one enforces. This file used to carry its own copy of the commands, and
when it was rewritten that copy ran eleven of the nineteen the script ran at the time, so it names
the script instead. Run `tools/gates.sh --list` for the list as it stands rather than trusting a
count written here.

What the script cannot tell you, because it is about your machine rather than the repository. The
commands assume `cargo` and `node` are on your `PATH`; if your toolchain lives somewhere unusual,
export that before anything else, or a gate reports `cargo: command not found` as if it were a
finding.

```sh
# In a worktree, build inside the worktree — a build directory outside every checkout has no owner
# and no lifetime: nothing removes it when the worktree goes.
export CARGO_TARGET_DIR="$PWD/.target"
# A unix socket path cannot exceed 108 bytes, and an agent worktree path is longer than that
# before the browser fixture appends anything.
export SKEIN_UI_FIXTURE_ROOT=/var/tmp/skein-uifix
```

After every browser run, not once at the end — `tests/ui/README.md` says why it reads the names it
looks for rather than carrying a list:

```sh
node tests/ui/harness/leaks.mjs      # exit 0, and it prints the names it looked for
```

## Reporting

Say what you actually verified and what you did not. A correction that changes nothing for the
reader is noise; a correction that changes what they would do next is the whole job. When a check
contradicts something you already told them, lead with that.
