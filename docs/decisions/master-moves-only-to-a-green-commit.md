# Master moves only to a commit CI has already passed

**Decision.** A merge is not pushed to `master` to be tested there. The merged commit is pushed to a
branch of its own, `ci/<short-sha>`. When CI has passed on that commit, `master` is fast-forwarded to
it and the `ci/` branch is deleted. The whole test suite is CI's to run: neither the coordinator nor a
lane runs `cargo test --lib --tests` across the merged tree as a step before pushing.

**Date.** 2026-10-31.

**Decided by.** The project owner, 2026-10-31, on the coordinator's proposal.

**Why.** On 2026-10-31 a coordinator ran the whole suite locally on every merged tree, pushed to
`master`, and then waited for CI. The two runs did not catch the same things.

- CI caught what the local run could not. A crossing test passed locally and failed on CI's Ubuntu
  runner, whose login profile appends `/snap/bin` to an agent's PATH. CI also runs jobs nobody runs
  locally: coverage, the no-skip gate, MSRV, and the mod check on the pinned Claude Code.
- The local run caught nothing CI would have missed. It was green each time; the only failures that
  day were CI's, one real and one an order-dependent flake.
- The local run cost 20 to 30 minutes of a CPU shared with the other boxes, per merge.
- It did not protect `master` either. The commit went to `master` before CI had seen it, so a red
  `master` happened anyway. `master` is what the owner updates skein from, so a red one blocks an
  update.

CI already runs on a push to any branch (`.github/workflows/ci.yml`, `on.push.branches: ["**"]`), so
testing the commit before `master` moves costs nothing extra.

**How.**

```sh
sha=$(git rev-parse --short=8 HEAD)
git push origin HEAD:refs/heads/ci/$sha     # CI runs on this commit
# wait for CI on $sha to pass, then:
git push origin $sha:master                 # a fast-forward; refused if master has moved
git push origin --delete ci/$sha
```

If CI fails, fix it on top, push the fix to a new `ci/` branch, and leave `master` where it was. If
`master` has moved meanwhile, the push is refused: merge again on the new `master` and run CI again.

**What stays local.** The checks only the verifier can make: the lane's focused tests, and the
coordinator's own sabotage of each lane, watched to fail and restored from a scratchpad copy. The
static gates (`tools/*-check.py`, `node cockpit/build.mjs --check`, `cargo fmt --check`) take seconds
and run before the `ci/` push. `commit-before-the-gates.md` still holds inside a lane.

**Rules out.**

- Pushing a merge straight to `master` and waiting for CI there.
- A local whole-suite run on the merged tree as a step before the push.
- A force-push to `master` to undo a red commit.

**Enforced at.** Nothing mechanical yet. A GitHub branch rule on `master` requiring CI's checks to
pass would enforce it: the fast-forward push carries a commit that already has a passing check. That
is a repository setting, and nobody has turned it on.
