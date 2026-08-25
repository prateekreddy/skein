# Fixtures for the cockpit suites

Payloads captured from a real fleet, kept whole rather than reduced to the case they caught.

## `thing-queue.json`

`GET /api/repos/gadget-demo/review` on the owner's fleet, 2026-08-25 — 54 open pull requests on
`acme/thing`, trunk `develop`. Trimmed to the fields the stack code actually reads: `number`,
`head_ref`, `base_ref`, `lane`. Titles and author logins were captured too and then removed — no
assertion here reads them, and a fixture in this repository is not the place for another project's
work. What is left is the branch graph, which is the whole of the evidence.

It is the evidence for SKEIN-288: a real branch graph with a real trunk pull request (#625,
`develop → master`) AND two real forks (`fix/readiness-abstention-kinds` carries #586 and #671;
`fix/readiness-named-findings` carries #711 and #672). No hand-made pair of pull requests contains
both at once, which is why the bug survived a suite full of them.
