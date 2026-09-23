# Security

skein's product **is** an isolation boundary. It runs coding agents — programs that execute
arbitrary code on a person's behalf — inside sandboxes, and almost everything it does is an answer
to "what may this box reach?" So a report that the boundary does not hold where it claims to is not
an edge case here. It is a report about the thing itself, and it is welcome.

Please read [Scope](#what-counts-as-a-vulnerability) before reporting: this is a tool for running
untrusted code on purpose, so the line between a vulnerability and the product working as designed
is a real line and it is not in the usual place.

## Reporting a vulnerability

**Do not open a public issue, and do not open a pull request that fixes it.** A patch is a
disclosure — the diff says where the hole was and how to reach it, to everyone, at once.

Use GitHub's private vulnerability reporting: the **Security** tab of this repository →
**Report a vulnerability**. It opens a private advisory that only the maintainer can see, it needs
no key exchange and no mailbox, and it becomes the record and eventually the published advisory.

> **[OWNER DECISION — placeholder, do not publish as-is]**
>
> Three things in this file need the maintainer's answer before this repository goes public:
>
> 1. **Private vulnerability reporting must be switched on** for the repository (Settings →
>    Advanced Security → Private vulnerability reporting). If it is off, the "Report a
>    vulnerability" button does not exist and this whole section sends people nowhere.
> 2. **A fallback contact**, or a deliberate decision to have none. GitHub advisories require the
>    reporter to have a GitHub account; some researchers will not. If there is to be an address
>    here, the maintainer chooses it — no address has been invented for this file.
> 3. **The response times below are placeholders** and commit nobody to anything until the
>    maintainer replaces or removes them.

### What to send

The more of this you have, the faster it gets fixed — but send it before you have all of it rather
than sitting on it:

* what the boundary was that you crossed, in one sentence;
* the smallest reproduction you have, and which side you ran it from — inside a box, from the
  browser, from the host, or from a repository skein was asked to read;
* the commit you tested (`git rev-parse HEAD`), and whether the fleet was the in-fleet deployment
  or a host-side one;
* what you actually got: a file you should not have read, a token, a command that ran, a request
  that was approved without anybody approving it.

### What to expect

> **[PLACEHOLDER — awaiting the maintainer's decision. These numbers are a proposal, not a
> promise.]**
>
> * an acknowledgement within **3 working days**;
> * an assessment — in scope or not, and how severe — within **10 working days**;
> * a fix, or a written plan with dates, within **90 days** of the assessment;
> * credit in the advisory under whatever name you choose, unless you ask us not to.

skein is maintained by one person. That is the reason to say what you can expect rather than let
you guess, and the reason not to promise a 24-hour turnaround that would not be kept.

Please give the fix a chance to ship before publishing. There is no bug bounty.

## What counts as a vulnerability

What a box can and cannot reach today is one table in
[`docs/threat-model.md`](docs/threat-model.md), each row with the test or line of code behind it
and the known gaps listed under their tracker items. Read it first.

The design is argued in [`docs/architecture.md`](docs/architecture.md) — §9 is the trust model,
§7 is the privilege split, and §8 is the host warden. The short version is that **a box is
untrusted by construction.** It runs somebody's coding agent, that agent runs arbitrary code, and
every mechanism named below exists on the assumption that it will eventually be hostile.

### In scope

Anything that lets a box, a page, or a repository skein reads cross a line skein claims to hold:

* **A box reaching another box's files, working tree, credentials or conversation.** Each box gets
  a `bwrap` namespace inside the shared sandbox; `tests/isolation_bwrap.rs` runs a real namespace
  and reads the paths back rather than inspecting arguments.
* **A box escaping its namespace**, or becoming root in the shared sandbox by any path other than
  the Docker socket described under [Not in scope](#not-in-scope) below.
* **A box reaching the Docker socket when the design does not mean it to, or using it to cross the
  *sandbox* boundary rather than the box one.** Today the design excludes no box from the socket:
  an ordinary box's `/run` cover never touches `/run/docker.sock` at all (`src/box-session.sh:1736`),
  and the workshop box reaches it too, by skipping the whole isolation block instead
  (`src/box-session.sh:1485`, `SKEIN_BOX_PRIVILEGED=1`). A future box type the design means to
  exclude from the socket, reaching it anyway, is in scope. So is reaching the socket, or the
  daemon behind it, other than through an ordinary box's own crossing — from the cockpit, the
  warden, or the host — and so is using the root container the socket grants to reach past the
  sandbox itself rather than just past the box.
* **A box driving the cockpit's API.** The API is authenticated by a secret at `~/.skein/api-token`,
  0600, and the boundary is that a box has `~/.skein/repos` and `~/.skein/boxes` bind-mounted into
  it and *not* `~/.skein` itself. Reaching an authenticated route from inside a box without that
  file is in scope; so is anything that lets the file out. The reasoning is in `src/apiauth.rs`,
  including the measurement that made it necessary.
* **A box widening its own git scope.** A stored token is meant to cover exactly one repository,
  and a write to another is meant to become a request a person approves. A box that grants itself
  that approval, or that reaches a repository without one, is a vulnerability.
* **Getting the warden to perform a privileged operation nobody approved.** The warden runs on the
  host precisely because skein cannot be trusted to authorise its own privileged acts; its approval
  surface is its own and no box can reach it.
* **A credential going somewhere it was never meant to** — onto `argv`, into a URL, into a log,
  into a file a box can write, or *upward* from a box to the fleet. Credentials are designed to
  flow down and never up.
* **Injection into anything skein builds as text.** A box chooses its own name, its own request
  ids and its own branch names, and those reach shell scripts and JavaScript.
* **Cross-site scripting in the cockpit, or anything that exfiltrates its cookie.** The cookie is
  `HttpOnly` and `SameSite=Strict`, and the page carries a CSP for the exfiltration half; a way
  around any of the three is in scope.
* **Reaching the cockpit from off-origin.** It binds loopback and guards the `Origin` header;
  bypassing that guard is in scope.
* **Content in a repository skein reads turning into execution.** skein reads pull requests, diffs
  and directory listings from repositories it did not write. A file name, a branch name or a PR
  title that runs code is a vulnerability, and this class has produced a real one already.

Two examples of what a good report looks like, both real and both fixed:

* **August 2026 — shell injection through a box-chosen request id.** A box files a write request by
  calling a script with an id of its own choosing, and the id reached a shell as text. Fixed in
  `8656aa1`: a path that climbs out is refused where it would be read, and an id that is not a
  plain name is refused before it is used.
* **September 2026 — XSS through a reviewed repository's directory name.** The page built click
  handlers as `onclick="f('${esc(x)}')"`. An attribute value is entity-decoded *before* the JS
  parser sees it, so the `&#39;` that `esc` produces arrives as a quote and closes the string.
  Forty-six sites were written that way, and one of them interpolated a top-level directory name
  from the repository being reviewed — so landing a directory on a base branch was enough to reach
  it. Fixed in `7f25f92`, and `cockpit/test/page.test.mjs` now fails the build if the shape comes
  back.

Note what both have in common: the attacker-controlled value was something ordinary — an id, a
directory name — that arrived from a party the design already treats as untrusted, and became text
in a language with a parser.

### Not in scope

Not because they do not matter, but because they are the product rather than a defect in it:

* **An agent inside a box doing something destructive with access it was deliberately given.** That
  is what a box is for. skein's job is to bound what "given" covers, not to second-guess the agent
  inside the bound.
* **An ordinary box reaching the sandbox's Docker socket, or becoming root in the sandbox through
  it.** `/run/docker.sock` is left reachable on purpose: `grep -n 'docker.sock' src/box-session.sh`
  finds only the comment that says so (`src/box-session.sh:1736`), because skein points the
  sandbox's dockerd at the workload cgroup so that containers a box starts are accounted for
  (architecture §9.5 R11, `docs/architecture.md:1722`). That is a complete escape from the box: a
  root container in the sandbox, with any bind mount it asks for, reaches every other box's files,
  the fleet root and the volume (`docs/architecture.md:1724`). A report that an ordinary box can do
  this is describing the design, not a defect in it — see [In scope](#in-scope) above for what is
  still a real report about that same socket.
* **Anything that needs host root, physical access, or an already-compromised host.** skein trusts
  the machine it is installed on; the whole design is about not trusting what runs *inside* it.
* **`SKEIN_NO_API_AUTH=1` *outside* the fleet.** It is a documented off-switch for an owner who has
  some other boundary, and it is an environment variable rather than a setting precisely so that it
  cannot be reached through the API it disarms. **Inside the fleet it is refused** (SKEIN-962): the
  cockpit the doorway starts answers every request with the reason and serves nothing else, because
  there the switch's own justification is false — every box shares one network namespace with that
  port, so the token is the only boundary there is. A report that the switch disarms an in-fleet
  cockpit *is* in scope; a report that it disarms one an owner ran somewhere else is this bullet.
* **Resource use inside a box's declared allowance.** One sandbox is a shared pool of memory, CPU
  and disk, sized by its owner at creation, with a per-box ceiling on top. A box using what it was
  given is not an attack — a box exceeding a ceiling that was set for it *is*, and that is in
  scope.
* **The sandbox proxy answering GitHub as the account.** Every box's HTTP goes through a proxy
  skein does not run and cannot configure from inside the sandbox — the lever is an `sbx secret`
  command on the host, against the owner's own keychain — and while that proxy is injecting, a
  request from a box carrying no credential, or carrying a deliberately invalid one, is answered as
  the whole account. skein's answer is not a claim that this cannot happen. It is to **measure it
  and put it on the cockpit's banner in red** (`proxy_injection` in `src/health/reach.rs`, SKEIN-548,
  SKEIN-927), because a boundary the substrate can reopen without touching a line of this code is
  one somebody has to be told about. The row in [`docs/threat-model.md`](docs/threat-model.md)
  carries the dates it has flipped and the command that answers it today. So a report that a box
  reached GitHub as the account through the proxy is describing a condition this project already
  states — **but a report that it did so while the check said otherwise is very much in scope**,
  and so is anything that makes the check answer "nothing added" without having asked.
* **A box reaching an arbitrary host on the internet.** skein sets no egress policy, and that is a
  decision rather than an omission: boxes install from npm, PyPI, crates.io, GitHub and the model
  APIs, and an allowlist that misses one fails in a way that reads as a broken build rather than as
  a policy. A box reaches whatever the host's `sbx` policy allows, which the threat-model page
  records as measured rather than assumed (SKEIN-926). Narrowing it is the host owner's to do,
  with `sbx policy`.
* **Vulnerabilities in `sbx`, Docker, `bwrap` or a vendored dependency.** Report those upstream.
  Do tell us as well if skein's use of them makes the impact worse or the fix different.
* **Missing hardening with no demonstrated path to impact** — a header that could be stricter, an
  algorithm that could be newer. Show what it lets somebody do.

If you are unsure which side of the line something falls on, report it. Sorting that out is our
job, not yours.

## What we will do

Fixes land with a test that fails without them, because this project does not accept a fix on the
argument that it looks right — see [`CONTRIBUTING.md`](CONTRIBUTING.md) on proving a test can fail.
Where a fix changes what a box may reach, the design documents change with it, and the advisory
says so.
