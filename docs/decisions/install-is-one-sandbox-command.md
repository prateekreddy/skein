# The install is one file handed to `sbx`

**Decision.** Installing skein is: download one file, hand it to `sbx`, done. No clone on the host,
no Rust on the host, and no skein binary or service on the host. The sandbox builds skein from
source on its first start and keeps its own clone and toolchain, because upgrading is its job. The
warden is optional, not removed: see [warden-or-prompt.md](warden-or-prompt.md).

**Date.** 2026-08-25.

**Decided by.** The project owner.

**Why.** "Move skein into the sandbox" was twice read as the smaller question — moving the server
process inside, which was already done — and a whole host-side migration path was built against
that reading and reverted (`CONTRIBUTING.md`, "Before you change anything", rule 1). The owner's
meaning was the whole install. A first build that takes minutes is an accepted cost: what runs is
what was published. Running a warden means trusting skein with an executable on your host, and not
everyone will, so both routes to a privileged act are first-class.

**Rules out.**

- Any host-side build, service or migration step in the install or the repair path.
- A configuration switch between "warden" and "no warden".
- A prompt that omits what happens if the person declines. When skein needs the person to run
  something, it says the command, why, and what declining costs.

**Enforced at.**

- `README.md:100-104` — the install as it is published: one file handed to `sbx`, the sandbox
  builds with a toolchain of its own, and `bootstrap.sh` is the whole install.
- `README.md:131-134` — re-running the same lines is also the repair, because there is no skein on
  the host to repair with.
- `src/warden_client.rs:1014` — the three-part prompt.

What nothing enforces: the first command is written by hand in the README, because nothing can
prompt for it before skein exists. A change to the install that forgets to update those lines is
caught by no gate today.
