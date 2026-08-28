# Shared working files in Skein boxes

Use `$HOME/shared` (`/home/agent/shared`) for durable project working material that must survive a
box restart or be available live in another Claude/Codex box for this repo. Examples: reference
documents, sample corpora, research captures, and working notes.

The canonical directory is `shared-home/` in this repo's mounted Skein store. It is read-write and
project-scoped. Changes appear in every box immediately; concurrent writes have normal filesystem
last-writer-wins behavior, so coordinate edits to the same file.

Real `$HOME` is intentionally private to each box. Do not put credentials, `.ssh`, agent runtime
state (`.claude`/`.codex`), caches, toolchains, repositories, build outputs, sockets, or locks in
`shared`. If `$HOME/shared` is missing or is not a symlink, report it instead of assuming data is
being persisted.

Host-side import from an old box is deliberately explicit:

```sh
skein shared import <source-box>                         # read-only inventory
skein shared import <source-box> --include <name> --apply
```

Imports never overwrite existing destinations. Unsafe content is excluded; unreadable files are
skipped and recorded in the import receipt.
