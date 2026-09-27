# What skein does on GitHub it does as you

**Decision.** There is no distinction between the person and skein. A review, a verdict, a merge, a
label or a deleted branch that skein performs is performed on the person's own GitHub credential
and appears under their name. skein does not act as a second actor "on behalf of" anyone. If a
separate identity is ever wanted, for a team install, it goes behind a setting that is off by
default.

That setting now exists, and it is narrow: `review_identity` (`src/config.rs:157-158`), `"me"` by
default. `"app"` makes a review session's own comment review act as the skein GitHub App, for team
installs (SKEIN-516). It changes nothing else. A workflow's verdict still posts as you under `"app"`
(the owner, 2026-09-27), because it is the approval that counts on a protected branch.

**Which of your credentials, per repository.** Decided 2026-09-27 (SKEIN-953). A GitHub call about
repository X uses, in order: the token you stored for X, then `$GH_TOKEN`/`$GITHUB_TOKEN`, then the
read token (for reads only, never to post, merge, label or delete), then the host's `gh` login, then
nothing. That covers the review queue, verdicts, merges, thread resolution, the workflow tick, the
review session under `"me"`, the version check and fleet-side git. The one call that names no
repository, "who am I", keeps a repository-less order, in which any stored repository token may
answer. Stored credentials are read again on every call, so a token replaced in Settings is used
from the next call; only the `gh` login is asked once per process.

**Date.** 2026-09-05, while the credentials design was being written. Amended 2026-09-27 with the
per-repository order and the record of `review_identity`.

**Decided by.** The project owner, who rejected a proposal that review sessions post under a GitHub
App identity with "on behalf of" text, and who later settled the per-repository order above.

**Why.** skein is the person's hand: a review it posts is their review. An approval that is not the
person's is also worth nothing on a protected branch, so the review path stays on the person's side
of that line, on every call about a pull request (`src/prq/mod.rs:13-16`). What needed fixing in
that design was where a credential is put and how long it lives, not whose it is.

The per-repository order exists because the host used to pick "the first stored token in the file"
for every repository. A fine-grained PAT outside its scope answers 401, so the version check, the
queue, verdicts and merges for every other repository failed while a token that covered them sat
one line further down (SKEIN-953). A token you stored for a repository is a deliberate "reach this
repo this way", which is why it outranks `$GH_TOKEN`.

**Rules out.**

- Designing an automated act as a second actor, or a second, quieter credential for automation
  (`src/prq/credentials.rs:54-56`).
- Answering a credential problem by changing identity instead of placement and lifetime.
- Handing a GitHub call about one repository a token filed under another.
- Using the read token for anything that writes.

This is about acts on pull requests, which run on the host. A box is narrowed differently: by
repository, not by identity. On the App path a box's git access is an installation token scoped to
its own repository; on the token path it is the one token you stored for that repository, which is
yours. Either way a box cannot act outside its own repository (`src/gitgate/mod.rs`).

**Enforced at.**

- `src/gitgate/credentials.rs:540` — `credential_for_repo`, the one per-repository resolver. The
  read token is reached only for `Need::Read`.
- `src/prq/credentials.rs:57` — `token_for`, which every repository-scoped call asks. Its refusal
  names the repository and what to add, and says when the read token was passed over.
- `src/prq/credentials.rs:94` — `host_token`, for "who am I" only. Its refusal says a GitHub App
  cannot stand in, because an installation token is not a person.
- `src/prq/write.rs:3-4` — every act on a pull request runs on your own credential for that
  repository.
- `src/bin/skein-server/review.rs:819-820` — drafting produces text for the person and reaches
  GitHub not at all; posting acts under their name, and the two are separate calls.
- Tests, each of which fails on the change it names:
  `prq::credentials::tests::each_repositorys_queue_and_merge_carry_that_repositorys_own_token`,
  `the_read_token_never_reaches_a_write_and_the_refusal_says_what_to_add`,
  `a_token_replaced_on_disk_is_used_by_the_next_call_without_a_restart`,
  `gh_token_ranks_below_a_repositorys_own_token`,
  `prwork::sweep::tests::the_tick_acts_on_each_repository_with_its_own_token_and_as_you_under_app`,
  and `prwork::perform::tests::a_workflows_verdict_posts_as_you_even_when_reviews_act_as_the_app`.
