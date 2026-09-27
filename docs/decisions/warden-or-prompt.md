# A privileged host act is done by a warden that can, or the person is prompted

**Decision.** The host warden (`warden/`) and prompting the person to type a command are two live
routes to the same privileged host acts: creating and destroying the fleet, and publishing or
unpublishing a port. The rule is per act, not a mode: a running warden that has the capability
performs the act, with approval at its own terminal; otherwise skein shows the person the command.
Which route is taken is detected, never configured.

**Date.** 2026-09-05.

**Decided by.** The project owner, from interviews with people who would run skein.

**Why.** Some people will not install anything on their own machine. `sbx` is already required, so
asking them to run the commands skein wants costs them nothing more, and people who do run a warden
get those prompts done for them. The middle case is already real, not hypothetical: a warden can be
built with some doers compiled out (`docs/architecture.md:666`, §8.3), and a create-only warden
means create is done and destroy is prompted. Two global modes cannot say that, and they would put
the prompt behind an opt-in — which is how a fallback rots, because the people who need it on the
day are the ones who never turned it on (`src/warden_client.rs:1060-1067`).

**Rules out.**

- Calling the warden dead, or the prompt a stopgap. A change to one privileged act keeps both routes
  working.
- A setting or a first-run question that picks a route (`src/warden_client.rs:1069`).
- Reading the warden's advertised capability list to decide. A warden that advertises an act and
  then refuses it falls through to a prompt exactly as one that never claimed it.
- A prompt that only says what to type. Every prompt carries the command, why skein wants it, and
  what happens if the person declines; declining is an outcome, not an error.
- Prompting after an act whose result is unknown, which would offer the person a second execution.

**Enforced at.**

- `src/warden_client.rs:1091` — `perform`, the rule, which every privileged caller goes through:
  `src/fleet/create.rs:467` and `src/fleet/resize.rs:679`.
- `src/warden_client.rs:1014` — `Prompt`, whose three fields are the three parts.
- `src/warden_client.rs:1648` — the test that every act's prompt carries all three, and renders the
  cost of declining where it can be seen.
- `docs/architecture.md:615` — §8, the warden's design.
