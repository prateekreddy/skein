# The shared login moves on presence and is reported on liveness

**Decision.** skein's shared login asks two different questions and never lets one stand in for the
other. **Propagation** — whether a credential is carried from one copy to another — asks only
whether a login is present. **Reporting** — what the cockpit, the banner and `skein doctor` say —
asks whether it still works. Expiry may guard a propagation (a live copy is never overwritten by a
dead one); it never decides one.

**Date.** 2026-08-26.

**Decided by.** Settled by the incident below; the rule was already written in the code, twice.

**Why.** A dead token still seeds boxes, because a later heal can replace it, and nothing can replace
a void (`src/fleet/login.rs:144-148`). The move function was changed for a day to decide with the
reporting judgement. With the fleet's credential expired, both copies read as expired, the match
fell through to neither side, and the last leg of the login chain moved nothing — in exactly the
state where somebody is trying to log back in. It was reported from use within a day.

The tests missed it for a reason worth more than the fix: every one asserted an outcome for a pair of
inputs, and none asserted the invariant. A change that swapped the question was invisible to them.
**When a doc comment states an invariant, the test asserts the invariant, not the cases.**

A related rule from the same round: a refusal remembered against a credential is judged by the
credential's token, not by the file's modification time or bytes, because an OAuth client that is
failing to refresh rewrites its own file on every attempt.

**Rules out.**

- Reading `login_state` or `RuntimeLogin` on any path that seeds or heals.
- A test of the move that only checks outcomes for chosen pairs.

**Enforced at.**

- `src/fleet/login.rs:125` — `carries_login`, presence only, and never expiry.
- `src/fleet/login.rs:205-206` — `RuntimeLogin` is reporting only: nothing that seeds or heals reads
  it.
- `src/fleet/fleetlogin.rs:433` — the move, and the incident, where it is decided.
- `src/fleet/fleetlogin.rs:876` — `an_expired_credential_propagates_exactly_as_far_as_a_live_one`,
  which runs the same moves live and expired, so identical expectations are the assertion.
