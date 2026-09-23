//! Which repositories a box may push to, and how it asks for one it may not.
//!
//! Every box in this fleet used to hold the same GitHub credential: a user token with `repo`,
//! `admin:public_key`, `gist` and `read:org`, reaching **460 repositories** read and write, plus a
//! forwarded ssh-agent socket signing for anything that key could reach. Ten boxes, one identity.
//! An agent that misread a remote could push to any of them, and `admin:public_key` let one add a
//! key to the account — access that outlives the sandbox and appears nowhere in skein.
//!
//! What replaces it is two credentials with different shapes:
//!
//! * **read** — a read-only credential (`contents: read` + `metadata: read`): a per-owner App
//!   installation token, or the optional cross-repo read PAT. It is delivered as a FILE the host
//!   places under the box's `git-tokens/` — `read/<owner>`, falling back to `read/_any` — which the
//!   credential helper hands over for a repo with no write token (`git-credential-skein.sh`, and
//!   `refresh_tokens`/`token_file` here place them). It cannot write anywhere, which is the point.
//!   It is deliberately NOT delivered as `GH_TOKEN`: a scoped box `unset`s `GH_TOKEN` in
//!   `box-session.sh` so that nothing in the environment carries a credential, and `GH_TOKEN` holds
//!   only this box's own-repo WRITE token when one exists, for `gh pr create` against that one repo.
//! * **write** — a GitHub App installation token scoped to **one repository**, valid an hour,
//!   minted by the host and dropped into the box's own host-mounted state directory. Or, for
//!   someone who would rather not install an App across their account, a fine-grained PAT they
//!   minted themselves — stored per repository, and **only** per repository.
//!
//! So no write-capable credential sits in a box's environment at all, and the App's private key
//! never leaves the host.
//!
//! **Why a stored token may cover exactly one repo.** Because the credential helper cannot contain
//! anything. It runs *inside* the box, as the same uid as the agent, so whatever it can read the
//! agent can read — the token file, its environment, its argv. It picks which credential to hand
//! over; it cannot stop anyone taking the other one. A token covering three repositories is
//! therefore write access to three repositories for every box that receives it, however carefully
//! the helper offers it for one. One repo per token means the credential a box holds is already
//! exactly as narrow as its rights, so nothing has to be trusted to stay in its lane. (The other
//! way to make a broad credential safe is for it never to enter the box — a host-side git proxy —
//! which is a different design and not this one.)
//!
//! **Why the host pushes tokens rather than the box asking for one.** A credential helper has to
//! answer inside a single `git` invocation, which wants a synchronous channel — and there isn't a
//! reliable one from a box to the cockpit (measured: `host.docker.internal:7878` answers 500 through
//! the gateway). But the box's state directory is *already* a host mount, because that is where its
//! conversation lives. The host refreshes a token file there before the hour is out; the helper only
//! ever reads a file. No new transport, and nothing to be down.
//!
//! **What this gate is, precisely.** Unlike [`crate::substrate`], the boundary here is real: GitHub
//! enforces it server-side, so a box acting with a token scoped to one repository cannot touch
//! another with it.
//!
//! **And what bounds the box, not just the token — SKEIN-548, now closed for the git path.**
//! Measured from inside a live box on 2026-09-06/07/11 and again 2026-09-15: the sandbox routes HTTP
//! through a proxy that TERMINATES TLS for the GitHub hosts, so a request carrying no Authorization
//! header — or a deliberately invalid one, or this box's own per-repo token — came back
//! authenticated as the account, while the same request sent direct is refused. `git` inherited it,
//! which made the token a placeholder rather than the credential anything authenticated with. The
//! close is not in this module: the launcher (`src/box-session.sh`) puts the
//! GitHub hosts in `NO_PROXY` for a scoped box, so git and gh reach GitHub DIRECT and present the
//! token this module places — GitHub then enforces it, and a box's REACH is bounded by its token
//! after all. What is still not bounded by this module is a process that deliberately re-routes
//! through the proxy; the substrate's deny-by-default egress (SKEIN-926) is what answers that. A
//! `fleet`-scoped box keeps the proxy and the account-wide token on purpose — the honest opt-out.
//!
//! It is also no longer only a fleet→GitHub wall. A box's token file used to be readable by every
//! other box — same uid, and every box's state directory in view — so scoping one box was worth
//! whatever the *least* careful box in the fleet did. [`crate::fleet::session_script`] now gives each
//! box a mount namespace in which no other box's directory exists, which is what makes a per-box
//! token mean per-box. Two exceptions, both deliberate and both named: the workshop box opts out
//! (see [`crate::fleet::box_is_privileged`]), and the credential helper still cannot contain
//! anything *within* a box — hence one repository per token, below.
//!
//! **How this module reaches GitHub: through [`crate::github`], and through nothing else.** It used
//! to have its own curl wrapper — a `--config` document for the JWT, a spawn, a JSON parse — which
//! was the second copy of a client that already existed, and the poorer copy of it. Three things it
//! did not have, each one bought by a live failure over there:
//!
//! * **The HTTP status.** It never asked for one, so "is this an error" was guessed from the body:
//!   a `message` and neither a `token` nor an `id`. GitHub's refusals routinely carry an `id`, and
//!   one of those came back as *success* — which [`check_token`] then read as a live token that has
//!   lost its push rights, an answer that discards a working credential.
//! * **The request body off argv.** It spent one as `-d <body>`, readable by every process on the
//!   host, in the same call whose credential the `--config` document beside it existed to hide.
//! * **The rate-limit hold, and a deadline skein can describe.** `--max-time 20` ends a call; it
//!   cannot tell anyone whether GitHub said nothing or was still talking, and it does not stop the
//!   next doomed request from being sent.
//!
//! So an App-token mint now waits behind the same hold as the review queue, is cut off by the same
//! deadline with the same sentence, and reads GitHub's status rather than sniffing its body.
//!
//! ## Layout
//!
//! One file per question, and this one is wiring: every name that resolved as `crate::gitgate::X`
//! before the split still does, through the `pub use` of each file below, at the visibility it
//! had. Tests sit in a `mod tests` beside the code they test; helpers two files share are in
//! `testkit.rs`.

use crate::secret::Secret;
use crate::util::sh_quote;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::Duration;

mod credentials;
mod decide;
mod grants;
mod mint;
mod request;
mod scope;
#[cfg(test)]
mod testkit;

pub use credentials::*;
pub use decide::*;
pub use grants::*;
pub use mint::*;
pub use request::*;
pub use scope::*;
