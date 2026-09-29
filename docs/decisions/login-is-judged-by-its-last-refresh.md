# Which copy of a rotating login is the working one

**Decision.** When copies of one account's Claude Code credential disagree, the working copy is the
one with the latest access-token expiry, `expiresAt`, among those whose refresh token has not
expired. The refresh token's own expiry, `refreshTokenExpiresAt`, decides only whether a copy is a
candidate at all. The two places that elect a copy must agree.

**Date.** 2026-08-29, measured on a live fleet.

**Decided by.** Settled by measurement.

**Why.** Claude Code mints a new refresh token on every refresh and supersedes the previous one. So a
fleet that shares one credential across agents that each refresh on their own logs every other box
out on each refresh. Nothing in the file says a copy has lost: `refreshTokenExpiresAt` is anchored to
the original login and barely moves, so a superseded copy goes on claiming its full life to the day.
The live fleet held five copies with four distinct refresh tokens, and ranking by the claim elected a
copy two boxes were already logged out of.

`expiresAt` is the one usable signal. An access token can only be obtained by successfully using
the refresh token, so a fresh `expiresAt` reports a refresh that happened, where
`refreshTokenExpiresAt` is a claim about the future (`src/fleet/login.rs:290-294`). Evidence about
the past beats a promise, the same distinction as
[propagation-is-not-reporting.md](propagation-is-not-reporting.md).

**How to check a fleet without moving a token.** Compare an 8-character prefix of the sha256 of
`claudeAiOauth.refreshToken` across the copies. More than one distinct prefix means boxes are
orphaned, whatever the banner says. Hash that one field, never the whole file: the file also holds
an `mcpOAuth` entry per MCP server, which legitimately differs from box to box, and a whole-file
hash reads a healthy fleet as orphaned.

**Rules out.**

- Ranking candidates by `refreshTokenExpiresAt`.
- Changing one election without the other.

**Enforced at.**

- `src/fleet/login.rs:282` — the host's election, in the heal script, with the two fields' jobs.
- `src/box-session.sh:1010` — `login_life`, the box's election at session start.
- `src/fleet/login.rs:1252` — `the_two_elections_agree_on_which_login_is_best`, which asserts that
  they agree rather than what they answer.

**Not yet enforced.** Reporting still judges a login by the claim: `login_state` reads
`refreshTokenExpiresAt` (`src/fleet/login.rs:162`), so a fleet of orphaned copies can still report
as signed in.
