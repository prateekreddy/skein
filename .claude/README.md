# `.claude/` in this repository

What an agent working on skein loads from the checkout: a project skill and an empty settings file.
It is tracked, reviewed like any other change, and the same for everyone who clones.

| file | what it is |
|---|---|
| `settings.json` | `{}`. Nothing project-specific is set for every contributor. |
| `skills/change-discipline/SKILL.md` | the checks to make before changing anything here. It points at `CONTRIBUTING.md`, which is the source of truth for them. |

## Where each kind of agent memory lives

| scope | who reads it | where |
|---|---|---|
| box-private | one box | the box's own `$HOME`, including its conversation transcripts |
| one person's boxes on this project | every box that person runs for this repository | skein's store for the repository, which Claude's memory tool is pointed at; not shared with anyone else |
| the repository | anyone who clones it | tracked files: this folder, `CLAUDE.md`, `CONTRIBUTING.md`, and settled decisions in `docs/decisions/` |
| the tracker | the team | work items, not memory |
| never persisted | nobody | credentials and login state, which stay in the box's private `$HOME` |

A fact about this codebase belongs in the repository, where a test or a file can prove it wrong. Store
memory is for what is true only of one person's work here.

## What skein adds in a box

None of it is tracked, and the checkout's `.git/info/exclude` hides exactly these:

- `skein`: a link to skein's machinery in the store, which the hooks and the mailbox find the store
  through.
- `settings.local.json`: that box's own settings. skein writes its default status line and
  full-screen mode there, where neither this folder nor the box has set one.
- `settings.json.skein-old`: only in a checkout an older skein wrote an untracked `settings.json`
  into. It is moved aside once, and Claude does not load it.
