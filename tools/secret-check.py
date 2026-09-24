#!/usr/bin/env python3
"""The four secrets rules of SKEIN-516, checked against the text of the tree.

The design is in the item, agreed with the owner on 2026-09-05: **one private/ directory per trust
boundary, one writer and a Secret type, scope by class, no secret on argv or in a URL.** Three of
those are properties of the code that a reader can see, and a property a reader can see is one a
reader can stop seeing. The writer is the example that bought this gate: nine places in `src/`
wrote a 0600 credential by hand, in four different orders, and the differences were not deliberate
(`src/secret.rs`'s module doc). One writer fixes the nine; only a gate stops the tenth.

WHAT IT FAILS ON, one rule per heading in `main`:

  writer     A 0600 credential mode anywhere in shipped code outside the two writers —
             `src/secret.rs` and the warden's own `warden/src/secret.rs`, which is a deliberate
             second copy (see that file). `0o600` in Rust, and `0o600`, `chmod 600` and `umask 077`
             inside the shell and Python that Rust sends somewhere else, and in `src/**/*.sh|py`.
  exposure   Every `Secret::expose()` in shipped code — the one door out of the type — is either
             the value of a `Command::env` (the sanctioned channel, Rule 4) or a door somebody
             reviewed and wrote a reason for in `EXPOSED` below. Inside an `.arg()`/`.args()` it is
             refused outright and cannot be excused: that is a credential in `ps`.
  sink       Every `Authorization: Bearer`, `X-Skein-Token`, `export GH_TOKEN=` and
             `.env("…_TOKEN", …)` in shipped code is built from a `Secret` — the function it is in
             exposes one — or from a shell expansion (`$VAR`, `$(cat …)`) that another sink fed.
  argv       No `-t <token-shaped thing>`, in Rust or in shell: `sbx secret set -t "$PAT"` put the
             owner's PAT in the process list of the host.
  env        No `std::env::set_var` in shipped code. Environment reaches a child through
             `Command::env` on that child, and never through the whole process.
  placement  A credential file named in shipped code is under a `private/` directory. The
             exceptions are `PLACEMENT` below, each with its reason, and most of them are debt:
             `$SKEIN_HOME/private/` does not exist yet (SKEIN-516 Rule 1, home half).
  fixture    A credential in test code is visibly not one: it starts `skein-test-`. That is the
             prefix `.gitleaks.toml` allows, so a fixture can never be the reason a real scanner
             is taught to look away — and a string that looks like a live GitHub token and is not
             one is exactly what a scanner cannot tell apart.

DERIVED, NOT LISTED, AND REFUSING ON ZERO — the idiom of `tests/ui/harness/leaks.mjs` and every
other `tools/*-check.py`. Each rule prints what it read, and a rule that read nothing exits 2
rather than 0: no shipped `.rs` files, no `.expose()` at all, no credential sink, no fixture. A gate
that reports a clean tree because its reader stopped matching is worse than no gate (SKEIN-647).
And before any of that, `self_check` plants one violation per rule in a synthetic file and refuses
to run if any rule does not see its plant, or sees one in the clean twin.

THE THREE TABLES (`WRITERS`, `EXPOSED`, `PLACEMENT`) ARE ALLOW-LISTS, AND A STALE ENTRY FAILS. An
entry that no longer matches anything is a permission nobody is using, and the next thing to match
it would inherit a reason written about something else.

LIMITS — what it cannot see, so nobody has to find out by relying on it:

  * **Text, not types.** "Built from a Secret" is read as "the function exposes one". A function
    that exposes a secret for one purpose and builds a header from a `String` for another passes.
    What makes this tolerable is that the type does the rest: `Secret` is not `AsRef<OsStr>`, so
    it cannot reach `.arg()` without `.expose()`, which this reads.
  * **A shell expansion in a sink is trusted.** `Bearer $SKEIN_PLANE_TOKEN` passes because the
    `.env("SKEIN_PLANE_TOKEN", …)` that set it is itself a sink and is checked.
  * **An env name held in a constant** (`.env(FLEET_TOKEN_VAR, …)`) is not recognised as a sink.
    The exposure rule still sees its `.expose()`.
  * **Rust tests only for fixtures.** `tests/ui/*.mjs` is not read.
  * **Placement is read from literals.** A path assembled from variables is invisible to it.

    python3 tools/secret-check.py          check
    python3 tools/secret-check.py --show   every allow-listed finding, with its reason
"""

import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rustcut  # noqa: E402 — the one cutter every gate shares, self-checked at import

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Shipped Rust: both crates. Test code inside them is cut out by `rustcut`, and whole-file test
# modules (`#[cfg(test)] mod testkit;`) are recognised from their parent's declaration.
PROD_DIRS = [os.path.join(ROOT, "src"), os.path.join(ROOT, "warden", "src")]
# Test Rust that is not inside a crate: every integration binary, whole.
TEST_DIRS = [os.path.join(ROOT, "tests")]
# Shell and Python that skein ships and runs somewhere else.
SCRIPT_DIRS = [os.path.join(ROOT, "src")]

# The two writers. Anything else that creates a 0600 file is a hand-rolled writer.
THE_WRITERS = {"src/secret.rs", "warden/src/secret.rs"}

FIXTURE_PREFIX = "skein-test-"

# ---------------------------------------------------------------------------------------------
# The allow-lists. Every entry carries its reason, and an entry nothing matches fails the gate.
# ---------------------------------------------------------------------------------------------

# `path::fn` (or `path` for a script) -> (how many, why). A 0600 write that cannot go through
# `crate::secret` because it does not happen in this process.
WRITERS = {
    "src/fleet/login.rs::<const>": (
        1,
        "LOGIN_MERGE_PY runs inside the fleet sandbox and writes a login into a box's HOME there; "
        "`crate::secret` writes on the filesystem THIS process stands on. Same order as the "
        "writer: mkstemp (0600 at birth), then chmod, then rename.",
    ),
    "src/fleet/login.rs::share_login_script": (
        1,
        "the no-python3 fallback of the same propagation: `cp` creates the temp with the source's "
        "mode, which is a login's 0600, so the chmod only re-asserts it before the rename",
    ),
    "src/fleet/credentials.rs::place_credential_script": (
        1,
        "the review credential is written inside the box that uses it (SKEIN-576), over the "
        "crossing's stdin. `umask 077` rather than a chmod after, so it is never readable at the "
        "ambient umask — the writer's rule, spelled in the language the write is made in.",
    ),
    "src/fleet/fleetlogin.rs::sync_fleet_login_with": (
        2,
        "the host's kept login pushed back into the fleet sandbox's HOME, over stdin: `umask 077` "
        "for a file it creates, `chmod 600` for one that was there. Not a rename — see the comment "
        "there on the symlink a `cat` writes through.",
    ),
    "src/tracking.rs::sync_provision_box": (
        2,
        "the box's sync env file, written inside the box over stdin: `umask 077` so it is born "
        "0600, and a chmod for a file that already existed",
    ),
    "src/box-session.sh": (
        3,
        "`merge_login`'s Python (one), run by the launcher inside the sandbox — the same writer as "
        "LOGIN_MERGE_PY at box start. The other two are the default mode two Python blocks give "
        "`.claude.json` when they have to create it: an onboarding flag and a trust flag, written "
        "with the mode the file already has",
    ),
}

# `path::fn` -> why this function takes a credential's bytes out of `Secret`.
EXPOSED = {
    "src/github.rs::config": "curl's config, fed to curl over stdin so the header never reaches argv",
    "src/apiauth.rs::stored": "handed to `skein doctor` as a String to print the cockpit URL. The "
    "?t= URL is Rule 4's open question (SKEIN-516), and this door closes when that is decided",
    "src/bin/skein-server/main.rs::serve": "the cockpit URL, `?t=`, printed at start — the delivery "
    "channel Rule 4 says becomes a one-time exchange. That changes what a person opens, so it "
    "waits for the owner (SKEIN-516)",
    "src/prq/credentials.rs::host_credential": "copied into a fresh Secret per caller, because "
    "Secret has no Clone; it never becomes a String",
    "src/fleet/credentials.rs::github_export": "written into the box over the crossing's stdin and "
    "read back there from a 0600 file; never argv, never this process's environment",
    "src/ai/call.rs::tried": "the value of `Command::env` for GH_TOKEN and GITHUB_TOKEN two lines "
    "down, on the child that needs it — the Rule 4 channel, bound to a name first to trim it once",
}

# `path::fn` -> why a credential-shaped header here carries no credential.
NOT_CREDENTIALS = {
    "src/health/reach.rs::probe_proxy_injection_at": "PROBE_CREDENTIAL is a string marked as not "
    "being a credential: the probe asks whether the proxy replaces it, and a real one would "
    "answer a different question",
}

# `path` + the literal -> why a credential file sits outside `private/`.
# The reason most of these share: the volume half of Rule 1 is not built.
HOME_DEBT = (
    "Rule 1 debt (SKEIN-516): `$SKEIN_HOME/private/` does not exist yet, and moving this into it "
    "is a migration of every live fleet's volume, with `volume.rs`'s INSTANCE_SCOPED inverted to a "
    "carried allow-list"
)

# `(path, literal)` -> why a credential file is named outside `private/`.
PLACEMENT = {
    ("src/apiauth.rs", "api-token"): HOME_DEBT + " — the fleet's API token",
    ("src/tracking.rs", "tokens"): HOME_DEBT + " — sync connection tokens, `tokens/<id>`",
    ("src/tracking.rs", "plane-token"): "the legacy single Plane token: read once to carry it into "
    "`tokens/`, then removed — named in order to delete it",
    ("src/repos/fleetgit.rs", "github-read-token"): HOME_DEBT + " — the owner's read PAT",
    ("src/gitgate/credentials.rs", "github-read-token"): HOME_DEBT + " — the owner's read PAT",
    ("src/gitgate/credentials.rs", "github-pats"): HOME_DEBT + " — the owner's per-repo PATs",
    ("src/repos/fleetgit.rs", "github-pats"): HOME_DEBT + " — the owner's per-repo PATs",
    ("src/gitgate/mint.rs", "github-app.pem"): HOME_DEBT + " — the GitHub App's private key",
    ("src/warden_client.rs", "secret"): HOME_DEBT + " — the warden's pairing secret, which skein "
    "only READS from the warden's home on the volume; moving it is the warden's change too",
    ("warden/src/secret.rs", "secret"): HOME_DEBT + " — the warden's pairing secret, in the "
    "warden's own home; the warden is its only minter and mover",
    ("warden/src/secret.rs", ".skein/warden/secret"): "the warden's OLD location under the host "
    "`$HOME`, named so the warden can adopt what an older install left there",
    ("src/gitgate/mint.rs", "git-tokens"): "kept by Rule 1 on purpose: `<box state>/git-tokens/` "
    "holds the only secrets a box receives, bound read-only into that box alone",
    ("src/gitgate/decide.rs", "{}/git-tokens/{}"): "kept by Rule 1 on purpose: a box's own "
    "`git-tokens/`, the delegated installation tokens bound read-only into that box alone",
    ("src/fleet/credentials.rs", "\\\"$HOME\\\"/.cache/skein/review-{call}.token"): "the review "
    "credential, in the box's own private HOME for the life of one call (SKEIN-576): a call into a "
    "box reads it inside that box's namespace, where the fleet's `private/` is covered",
    ("src/fleet/paths.rs", "{}/.skein/review-github.token"): "the review credential's OLD location, "
    "named so `stale_sandbox_secrets` can delete what fleets from before the move still carry",
    ("src/repos/add.rs", "gh-secret-seeded"): "a marker that an sbx secret was once seeded, which "
    "holds no credential itself; read so `skein doctor` can say so",
}

# ---------------------------------------------------------------------------------------------
# What counts as what. Each is printed on every run, so a reader sees the vocabulary it used.
# ---------------------------------------------------------------------------------------------

# A 0600 credential mode, in any of the three languages it is written in here.
MODE_600 = re.compile(r"\b0o600\b|\bchmod\s+0?600\b|\bumask\s+0?077\b")

# The environment names that carry a credential to a child.
TOKEN_ENV = re.compile(r"^(?:GH_TOKEN|GITHUB_TOKEN|[A-Z][A-Z0-9_]*_TOKEN)$")

# Text that builds a credential header or export.
HEADER_SINK = re.compile(
    r"(?:Authorization:\s*Bearer\s+|X-Skein-Token:\s*|export\s+(?:GH_TOKEN|GITHUB_TOKEN)=)(?P<v>.?.?)"
)

# `-t` followed by something whose name says credential, in shell or in a Rust argv list.
ARGV_SHELL = re.compile(
    r"(?<![\w-])-t\s+\\?[\"']?\$\{?[A-Za-z_]*(?:token|TOKEN|secret|SECRET|pat|PAT|key|KEY|cred|CRED)"
)
ARGV_RUST = re.compile(r"\"-t\"\s*,\s*&?[A-Za-z_.()]*(?:token|secret|pat|key|cred)", re.I)

# A path component that names a credential file or directory.
CRED_COMPONENT = re.compile(r"(?i)^[\w.{}\\\"$-]*(?:token|secret)[\w.{}\\\"$-]*$|\.pem$|^github-pats$")
# …unless it is code: `box-token-usage.sh` is a probe that counts model tokens.
NOT_A_FILE_OF_SECRETS = re.compile(r"\.(?:sh|py|rs|mjs|js|md)$")
# A literal is a filesystem path when it is handed to one of these…
PATH_CALLS = {"join", "new", "from"}
# …or when it is a `format!` path rooted in a variable, `$HOME`, `~` or the fleet.
ROOTED = re.compile(r'^(?:\{|\$|~|\\"\$HOME|/boxes)')

# The shapes a real credential scanner keys on. A test literal that starts with one of these looks
# like a live token to gitleaks and to a person, which is the whole reason for the prefix.
CREDENTIAL_SHAPES = re.compile(
    r"^(?:gh[pousr]_|github_pat_|glpat-|xox[abprs]-|sk-|AKIA[0-9A-Z]|plane_api_)"
)
# Calls whose literal argument IS a credential, in test code.
FIXTURE_SINKS = re.compile(
    r"\b(?:Secret::new|set_credential_token|set_read_pat)\s*\(\s*(?:[^()]*?,\s*)?\"(?P<v>[^\"]*)\"\s*\)"
    r"|\b(?:set|set_var|env)\s*\(\s*\"(?P<name>[A-Z][A-Z0-9_]*)\"\s*,\s*\"(?P<w>[^\"]*)\""
)
FIXTURE_ENV = re.compile(r"(?:TOKEN|SECRET|_KEY|_PAT)$")

FN = re.compile(
    r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)",
    re.M,
)
CALLEE = re.compile(r"([A-Za-z_][A-Za-z0-9_]*!?)\s*$")


class Refused(rustcut.Blind):
    """This gate cannot answer, so it exits 2 and says why — never 0 (SKEIN-647)."""


# ---------------------------------------------------------------------------------------------
# Reading
# ---------------------------------------------------------------------------------------------


class Unit:
    """One Rust file: its text with comments blanked, and which spans of it are test code."""

    def __init__(self, rel, raw, whole_test):
        self.rel = rel
        self.text = rustcut.blanked(raw)
        self.tests = [(0, len(raw))] if whole_test else rustcut.cfg_test_spans(self.text)
        self.fns = functions(self.text)
        self.literals, self.exposures = scan(self.text)

    def is_test(self, pos):
        return any(a <= pos < b for a, b in self.tests)

    def line(self, pos):
        return self.text.count("\n", 0, pos) + 1

    def fn_at(self, pos):
        best = None
        for name, lo, hi in self.fns:
            if lo <= pos < hi and (best is None or lo > best[1]):
                best = (name, lo, hi)
        return best

    def where(self, pos):
        fn = self.fn_at(pos)
        return f"{self.rel}::{fn[0] if fn else '<const>'}"

    def fn_exposes(self, pos):
        fn = self.fn_at(pos)
        lo, hi = (fn[1], fn[2]) if fn else (0, len(self.text))
        return any(lo <= p < hi for p, _ in self.exposures)


def functions(text):
    out = []
    for m in FN.finditer(text):
        end = rustcut.match_brace(text, m.end())
        body = text.find("{", m.end())
        semi = text.find(";", m.end())
        if end < 0 or body < 0 or (0 <= semi < body):
            continue
        out.append((m.group("name"), body, end))
    return out


def scan(text):
    """Every string literal, with the calls it is an argument of; and every `.expose()`.

    One forward pass over the text with a stack of open parentheses, each tagged with the name
    written before it — so `.arg(format!("{}", t.expose()))` knows it is inside `arg` as well as
    inside `format!`, which a search backwards from the `.expose()` would get wrong the first time
    a literal between them held a parenthesis.
    """
    literals, exposures, stack = [], [], []
    i, n = 0, len(text)
    while i < n:
        past = rustcut.skip_token(text, i)
        if past is not None and past > i:
            if text[i] != "'" and not text.startswith("//", i) and not text.startswith("/*", i):
                raw = text[i:past]
                body = re.sub(r'^b?r?#*"', "", raw)
                body = re.sub(r'"#*$', "", body)
                literals.append((i, body, [name for _, name in stack]))
            i = past
            continue
        c = text[i]
        if c == "(":
            m = CALLEE.search(text, max(0, i - 64), i)
            stack.append((i, m.group(1) if m else ""))
        elif c == ")" and stack:
            stack.pop()
        elif c == "." and text.startswith(".expose()", i):
            exposures.append((i, [name for _, name in stack]))
        i += 1
    return literals, exposures


def rust_files(dirs):
    for d in dirs:
        for base, _, files in os.walk(d):
            for f in sorted(files):
                if f.endswith(".rs"):
                    yield os.path.join(base, f)


def load():
    test_only = set(rustcut.test_only_files(PROD_DIRS))
    prod = [
        Unit(os.path.relpath(p, ROOT), open(p, encoding="utf-8").read(), p in test_only)
        for p in rust_files(PROD_DIRS)
    ]
    tests = [
        Unit(os.path.relpath(p, ROOT), open(p, encoding="utf-8").read(), True)
        for p in rust_files(TEST_DIRS)
    ]
    scripts = []
    for d in SCRIPT_DIRS:
        for base, _, files in os.walk(d):
            for f in sorted(files):
                if f.endswith((".sh", ".py")):
                    p = os.path.join(base, f)
                    scripts.append((os.path.relpath(p, ROOT), open(p, encoding="utf-8").read()))
    return prod, tests, scripts


# ---------------------------------------------------------------------------------------------
# The rules. Each returns (findings, excused, derived) — a finding is (where, what).
# ---------------------------------------------------------------------------------------------


def rule_writer(prod, scripts, allowed):
    hits = {}
    for u in prod:
        if u.rel in THE_WRITERS:
            continue
        for m in MODE_600.finditer(u.text):
            if not u.is_test(m.start()):
                key = u.where(m.start())
                hits.setdefault(key, []).append(f"{u.rel}:{u.line(m.start())} ({key})")
    for rel, raw in scripts:
        for ln, line in enumerate(raw.splitlines(), 1):
            if line.lstrip().startswith("#"):
                continue
            if MODE_600.search(line):
                hits.setdefault(rel, []).append(f"{rel}:{ln}")
    return tabled(hits, allowed, "a 0600 credential written by hand — go through crate::secret")


def rule_exposure(prod, allowed):
    hits, findings, via_env, seen = {}, [], 0, 0
    for u in prod:
        if u.rel in THE_WRITERS:
            continue
        for pos, calls in u.exposures:
            if u.is_test(pos):
                continue
            seen += 1
            at = f"{u.rel}:{u.line(pos)}"
            if "arg" in calls or "args" in calls:
                findings.append((at, "a Secret's bytes on argv — every process on the host can read "
                                     "them in `ps`; pass it on stdin or through Command::env"))
            elif calls and calls[-1] == "env":
                via_env += 1
            else:
                hits.setdefault(u.where(pos), []).append(f"{at} ({u.where(pos)})")
    more, excused, _ = tabled(hits, {k: (len(hits.get(k, [])) or 1, v) for k, v in allowed.items()},
                              "an unreviewed `.expose()` — say in EXPOSED why the bytes leave "
                              "the type here", exact=False)
    return findings + more, excused, (seen, via_env)


def rule_sink(prod, excused_at):
    findings, seen, used = [], 0, set()
    for u in prod:
        for pos, body, calls in u.literals:
            if u.is_test(pos):
                continue
            for m in HEADER_SINK.finditer(body):
                seen += 1
                v = m.group("v")
                if v.startswith("$") or v.startswith('"$') or v.startswith('\\"$'):
                    continue
                if u.fn_exposes(pos):
                    continue
                if u.where(pos) in excused_at:
                    used.add(u.where(pos))
                    continue
                findings.append((f"{u.rel}:{u.line(pos)}", f"`{m.group(0).strip()}…` is not built "
                                 "from a Secret — nothing in this function exposes one"))
            if calls and calls[-1] == "env" and TOKEN_ENV.match(body):
                seen += 1
                if not u.fn_exposes(pos):
                    findings.append((f"{u.rel}:{u.line(pos)}", f"`.env(\"{body}\", …)` is not "
                                     "built from a Secret — nothing in this function exposes one"))
    for key in sorted(set(excused_at) - used):
        findings.append((key, "NOT_CREDENTIALS names this and nothing here builds a header any "
                              "more — delete the entry"))
    return findings, seen


def rule_argv(prod, scripts):
    findings, read = [], 0
    for u in prod:
        for m in ARGV_RUST.finditer(u.text):
            if not u.is_test(m.start()):
                findings.append((f"{u.rel}:{u.line(m.start())}", f"`{m.group(0)}` — a credential "
                                 "on argv"))
        for pos, body, _ in u.literals:
            if u.is_test(pos):
                continue
            read += 1
            if ARGV_SHELL.search(body):
                findings.append((f"{u.rel}:{u.line(pos)}", "`-t $TOKEN` in a script this sends — a "
                                 "credential on argv"))
    for rel, raw in scripts:
        for ln, line in enumerate(raw.splitlines(), 1):
            if not line.lstrip().startswith("#") and ARGV_SHELL.search(line):
                findings.append((f"{rel}:{ln}", "`-t $TOKEN` — a credential on argv"))
    return findings, read


def rule_env(prod):
    findings = []
    for u in prod:
        for m in re.finditer(r"\bset_var\s*\(", u.text):
            if not u.is_test(m.start()):
                findings.append((f"{u.rel}:{u.line(m.start())}", "std::env::set_var in shipped code "
                                 "— put the value on the child that needs it with Command::env"))
    return findings


def rule_placement(prod, allowed):
    hits, seen = {}, 0
    for u in prod:
        for pos, body, calls in u.literals:
            if u.is_test(pos):
                continue
            callee = calls[-1] if calls else ""
            parts = [p for p in body.split("/") if p]
            if callee in PATH_CALLS and "\n" not in body:
                pass
            elif "/" in body and ROOTED.match(body) and " " not in body:
                pass
            else:
                continue
            if not any(CRED_COMPONENT.search(p) for p in parts) or NOT_A_FILE_OF_SECRETS.search(body):
                continue
            seen += 1
            if "private" in parts:
                continue
            # A literal joined onto a path that is already under private/ is under private/.
            stmt = u.text[max(u.text.rfind(";", 0, pos), u.text.rfind("{", 0, pos)) + 1 : pos]
            if "private" in stmt:
                continue
            hits.setdefault((u.rel, body), []).append(f"{u.rel}:{u.line(pos)}")
    findings, excused = [], []
    for key, ats in sorted(hits.items()):
        if key in allowed:
            excused.append((", ".join(ats), allowed[key]))
        else:
            findings.append((ats[0], f"credential path `{key[1]}` is not under a private/ "
                                     "directory"))
    for key in sorted(set(allowed) - set(hits)):
        findings.append((key[0], f"PLACEMENT allows `{key[1]}` here and nothing matches it any "
                                 "more — delete the entry"))
    return findings, excused, seen


def rule_fixture(units):
    findings, seen = [], 0
    for u in units:
        for pos, body, calls in u.literals:
            if u.is_test(pos) and CREDENTIAL_SHAPES.match(body):
                findings.append((f"{u.rel}:{u.line(pos)}", f"test literal `{body[:40]}` has the "
                                 f"shape of a live credential — start it `{FIXTURE_PREFIX}`"))
        for m in FIXTURE_SINKS.finditer(u.text):
            if not u.is_test(m.start()):
                continue
            if m.group("name") is not None and not FIXTURE_ENV.search(m.group("name")):
                continue
            seen += 1
            v = m.group("v") if m.group("v") is not None else m.group("w")
            if v and not v.startswith(FIXTURE_PREFIX) and not CREDENTIAL_SHAPES.match(v):
                findings.append((f"{u.rel}:{u.line(m.start())}", f"fixture credential `{v[:40]}` "
                                 f"does not start `{FIXTURE_PREFIX}`"))
    return findings, seen


def tabled(hits, allowed, why, exact=True):
    """Compare what a rule found per key with the allow-list's count for that key."""
    findings, excused = [], []
    for key, ats in sorted(hits.items()):
        if key not in allowed:
            findings.extend((at, why) for at in ats)
            continue
        want, reason = allowed[key]
        if exact and len(ats) != want:
            findings.append((ats[0], f"{key} is allowed {want} and has {len(ats)} — a new one is a "
                                     "new hand-rolled writer; one fewer means tighten the entry"))
        excused.append((", ".join(ats), reason))
    for key in sorted(set(allowed) - set(hits)):
        findings.append((key, "allow-listed and nothing matches it any more — delete the entry"))
    return findings, excused, None


# ---------------------------------------------------------------------------------------------
# The self-check: every rule sees a plant, and none sees one in the clean twin.
# ---------------------------------------------------------------------------------------------

SELF_CHECK = {
    "writer": (
        'fn put(p: &Path) { OpenOptions::new().mode(0o600).open(p); }',
        'fn put(p: &Path, s: &Secret) { crate::secret::write(p, s); }',
    ),
    "exposure-argv": (
        'fn go(t: &Secret) { Command::new("gh").arg(format!("--token={}", t.expose())); }',
        'fn go(t: &Secret) { Command::new("gh").env("GH_TOKEN", t.expose()); }',
    ),
    "sink": (
        'fn h(t: &str) -> String { format!("Authorization: Bearer {t}") }',
        'fn h(t: &Secret) -> String { format!("Authorization: Bearer {}", t.expose()) }',
    ),
    "argv": (
        'fn s() -> &\'static str { "sbx secret set -g github -t \\"$GITHUB_PAT\\"" }',
        'fn s() -> &\'static str { "tmux has-session -t \\"$SESSION\\"" }',
    ),
    "env": (
        'fn e() { std::env::set_var("GH_TOKEN", "x"); }',
        'fn e(c: &mut Command) { c.env("SKEIN_HOME", "x"); }',
    ),
    "placement": (
        'fn p() -> PathBuf { skein_home().join("new-thing.token") }',
        'fn p() -> PathBuf { skein_home().join("private").join("new-thing.token") }',
    ),
    "fixture": (
        '#[cfg(test)]\nmod tests {\n    fn f() { set_read_pat("ghp_looks_live"); }\n}\n',
        '#[cfg(test)]\nmod tests {\n    fn f() { set_read_pat("skein-test-read"); }\n}\n',
    ),
}


def run_rule(name, u):
    if name == "writer":
        return rule_writer([u], [], {})[0]
    if name == "exposure-argv":
        return [f for f in rule_exposure([u], {})[0] if "argv" in f[1]]
    if name == "sink":
        return rule_sink([u], {})[0]
    if name == "argv":
        return rule_argv([u], [])[0]
    if name == "env":
        return rule_env([u])
    if name == "placement":
        return rule_placement([u], {})[0]
    if name == "fixture":
        return rule_fixture([u])[0]
    raise AssertionError(name)


def self_check():
    for name, (bad, good) in SELF_CHECK.items():
        planted = run_rule(name, Unit("self-check/planted.rs", bad, False))
        clean = run_rule(name, Unit("self-check/clean.rs", good, False))
        if not planted:
            raise SystemExit(f"secret-check: its own `{name}` rule did not see the violation planted "
                             f"for it, so every clean verdict it gives is worthless:\n    {bad}")
        if clean:
            raise SystemExit(f"secret-check: its own `{name}` rule fired on the clean twin of its "
                             f"plant, so its findings cannot be trusted:\n    {good}\n    {clean}")


# ---------------------------------------------------------------------------------------------


def main():
    self_check()
    show = "--show" in sys.argv
    prod, tests, scripts = load()
    shipped = [u for u in prod if any(not u.is_test(p) for p, _, _ in u.literals)]
    if not shipped:
        raise Refused("read no shipped Rust at all under src/ and warden/src/")

    report, excused_all = [], []

    f, excused, _ = rule_writer(prod, scripts, WRITERS)
    report += [("writer", *x) for x in f]
    excused_all += [("writer", *x) for x in excused]

    f, excused, (exposed, via_env) = rule_exposure(prod, EXPOSED)
    report += [("exposure", *x) for x in f]
    excused_all += [("exposure", *x) for x in excused]
    if exposed == 0:
        raise Refused("found no `.expose()` in shipped code — the one door out of `Secret` — so "
                      "the reader is broken, not the tree clean")

    f, sinks = rule_sink(prod, NOT_CREDENTIALS)
    report += [("sink", *x) for x in f]
    if sinks == 0:
        raise Refused("found no credential header, export or `.env(\"…_TOKEN\")` in shipped code")

    f, literals = rule_argv(prod, scripts)
    report += [("argv", *x) for x in f]

    report += [("env", *x) for x in rule_env(prod)]

    f, excused, placed = rule_placement(prod, PLACEMENT)
    report += [("placement", *x) for x in f]
    excused_all += [("placement", *x) for x in excused]
    if placed == 0:
        raise Refused("found no credential path literal in shipped code")

    f, fixtures = rule_fixture(prod + tests)
    report += [("fixture", *x) for x in f]
    if fixtures == 0:
        raise Refused("found no credential fixture in any test — the sink patterns stopped matching")

    print(
        f"secret-check: read {len(prod)} crate file(s) ({len(shipped)} with shipped code), "
        f"{len(tests)} test file(s), {len(scripts)} script(s); {literals} shipped string literal(s); "
        f"{exposed} `.expose()` ({via_env} straight into Command::env); {sinks} credential sink(s); "
        f"{placed} credential path(s); {fixtures} test credential(s)"
    )
    print(f"    writers: {', '.join(sorted(THE_WRITERS))}; fixture prefix `{FIXTURE_PREFIX}`; "
          f"credential shapes {CREDENTIAL_SHAPES.pattern}")
    print(f"    allowed: {len(WRITERS)} writer(s), {len(EXPOSED)} exposure(s), "
          f"{len(PLACEMENT)} placement(s) — `--show` prints each with its reason")
    if show:
        for rule, at, why in excused_all:
            print(f"  allowed [{rule}] {at}\n      {why}")
    if report:
        for rule, at, why in report:
            print(f"secret-check [{rule}] {at}: {why}", file=sys.stderr)
        print(f"{len(report)} problem(s).", file=sys.stderr)
        return 1
    print("secret-check: every credential in the tree follows the four rules, or is allowed by "
          "name with its reason")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except rustcut.Blind as e:
        print(f"secret-check: REFUSED — {e}", file=sys.stderr)
        sys.exit(2)
