# Cockpit smoke test

`cargo test` proves the API is right. This proves the **page** is right — which is not the same
thing, and once wasn't: the Files tab shipped with every directory row rendered and then hidden by
an unrelated CSS rule (`body.docked .dir` matched the file browser's `.fent.dir`). The API returned
those folders perfectly; you just couldn't click them, and nothing inside a folder was reachable.
Unit tests, `tests/server.rs` and clippy were all green. Only a browser could see it.

So this test asserts what is **visible**, never what merely exists in the DOM — that's what
`mustSee()` is for. If you add a check here, make it a click a person would make.

## Setup (once)

```sh
cd tests/ui && npm run setup     # npm install + playwright's chromium (~150MB, cached in ~/.cache)
```

## Run

```sh
node tests/ui/smoke.mjs          # from the repo root; builds skein-server itself
```

It launches the real binary against a throwaway workspace in `$TMPDIR` (a README, a `docs/` folder
with a doc inside, a symlink pointing in, a symlink pointing out) on a free port, with `$SKEIN_HOME`
redirected — it never touches your real store, registry or boxes. Exit code is 0 or 1; on failure it
prints a screenshot path and keeps the fixture for inspection.

Not wired into `cargo test` on purpose: it needs node and a browser, which the Rust toolchain can't
assume. Run it before shipping anything that touches `src/web/index.html`.
