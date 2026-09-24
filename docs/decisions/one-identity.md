# What skein does on GitHub it does as you

**Decision.** There is no distinction between the person and skein. A review, a verdict, a merge, a
label or a deleted branch that skein performs is performed on the person's own GitHub login and
appears under their name. skein does not act as a second actor "on behalf of" anyone. If a separate
identity is ever wanted, for a team install, it goes behind a setting that is off by default; no
such setting exists today.

**Date.** 2026-09-05, while the credentials design was being written.

**Decided by.** The project owner, who rejected a proposal that review sessions post under a GitHub
App identity with "on behalf of" text.

**Why.** skein is the person's hand: a review it posts is their review. An approval that is not the
person's is also worth nothing on a protected branch, so the review path stays on the person's side
of that line (`src/prq/mod.rs:13-16`). What needed fixing in that design was where a credential is
put and how long it lives, not whose it is.

**Rules out.**

- Designing an automated act as a second actor, or a second, quieter credential for automation
  (`src/prq/credentials.rs:147-150`).
- Answering a credential problem by changing identity instead of placement and lifetime.

This is about acts on pull requests, which run on the host. It is the deliberate opposite of what a
box gets: a box's git access is a scoped installation token precisely so that a box cannot act as the
person (`src/prq/mod.rs:14-15`, and `src/gitgate/mod.rs`).

**Enforced at.**

- `src/prq/credentials.rs:151` — `host_token`, the one credential the host's GitHub calls run on.
  Its refusal says in as many words that a GitHub App cannot stand in, because an installation
  token is not a person.
- `src/prq/write.rs:1-4` — every act on a pull request runs on the host's own login.
- `src/bin/skein-server/review.rs:819-820` — drafting produces text for the person and reaches
  GitHub not at all; posting acts under their name, and the two are separate calls.
