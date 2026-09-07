#!/usr/bin/env python3
"""Nothing that identifies a person, a client or an account gets back into this tree.

This repository was written for two years inside another organisation's work, and going public
meant rewriting all 806 commits: names, hosts, paths, filenames and authorship, replaced across
file contents, commit messages and the filenames themselves. That rewrite happens once. This gate
is what stops the same material walking back in on the next commit, and it is the only part of the
scrub that keeps working after the person who did it has stopped looking.

WHY IT IS NOT A LIST OF TODAY'S NAMES. A denylist of the identifiers that were removed catches
exactly the mistake nobody is going to make twice. What actually comes back is the NEXT one: a
different client, a new gateway hostname, a colleague's login, a laptop path in a stack trace
pasted into a comment. So four of the five rules below are about SHAPE — a home directory, an
address, a credential, a host — and each keeps an allow-list of what the tree legitimately has, in
`docs/residue.toml`, with a reason per entry. A new host is then a line in a diff and a decision
somebody made, which is the same bargain `tools/module-check.py` and `tools/source-check.py` strike
about module edges and Sources. The fifth rule is the literal list, and it is there because some
strings must never come back whatever their shape.

THE RULES

  host       Every host in an `http(s)://`, `ssh://` or `git@host:` reference is declared in
             `[hosts]`. This is the rule that would have caught the private work-tracker gateway,
             and it catches its replacement without anybody adding a pattern.
  home       Every `/Users/<who>` and `/home/<who>` prefix is declared in `[homes]`. A home
             directory names a person and usually leaks a machine as well.
  address    Every email-shaped string whose domain is not a reserved example/test domain is
             declared in `[addresses]`. `git@github.com` is a login on a URL, not an address, and
             is declared like anything else rather than special-cased in code.
  secret     Credential SHAPES — provider token prefixes, an AWS key id, a PEM private key header,
             a JWT. No allow-list is expected to be needed; if one ever is, it goes in `[exempt]`
             with the reason, and a real credential is rotated rather than exempted.
  banned     Literal strings that must never appear again. Unlike every other list in this tree
             this one is a DENYLIST: an entry is not a permission, and an entry that matches
             nothing is the correct state rather than a stale one. It is also the only rule whose
             list is not in this repository — see below.

THE REGISTER IS NOT IN THE REPOSITORY, AND THE ENFORCEMENT IS (SKEIN-630). The banned list used to
be a `[banned]` table in `docs/residue.toml`: thirty-eight identifiers in cleartext, each with a
sentence saying whose it was. Every one of them had just been removed from all 806 commits of this
repository's history, and then written back into one published file — a ready-made search-term
list to point at an archive branch, at a fork nobody rewrote, or at code search. So the register
moved out, and what stays here is a matcher that cannot be read:

    docs/residue-banned.txt     sha256 of each needle, lowercased. Sorted, so the order says
                                nothing; the widths listed once as a set, so the file does not
                                say how many needles are six characters long. No reasons, no
                                attribution, nothing readable.
    the register                needle, reason, whose it was. Outside this repository, at
                                `~/shared/skein-notes/residue-register.toml` unless
                                `SKEIN_RESIDUE_REGISTER` says otherwise.

The gate needs only the hashes, so it enforces in CI, which cannot reach the register — and that
is the half that matters: SKEIN-628 is a banned string walking into a pushed commit message. When
the register IS reachable a finding is named and its reason quoted, `--update` regenerates the
hash file from it, and a drift between the two is reported. When it is not, every one of those
says so in as many words rather than behaving as though the list were empty.

A HASH CANNOT BE SUBSTRING-MATCHED, which is the trap in that design. The rule this replaced was
`needle.lower() in line.lower()`, and it caught a needle sitting inside a longer identifier —
which is how these strings actually appear, in a branch name or a fixture filename. Hashing whole
lines would lose that silently. So the matcher hashes CANDIDATE SUBSTRINGS: each line is cut into
maximal runs of the characters a needle may contain, and every substring of every run, at each
width the register holds, is hashed. That is exact rather than approximate — a needle is built
from those characters only, so any occurrence of it lies wholly inside one run — and `self_check`
fires it on a needle embedded in a longer token every time this tool runs.

WHAT IS NOT CHECKED, and deliberately. Prose that describes a person without naming them; a first
name in a sentence; a project's internal vocabulary. Those are the half of a scrub that no pattern
can do — a transcript is redacted by somebody reading it — and pretending otherwise would make the
gate's silence mean more than it does. See `tests/fixtures/panes/README.md` for the half done by
hand.

NOTHING IN THIS FILE MAY SPELL WHAT IT LOOKS FOR. `tools/prose-check.py` learned this the hard
way: a checker whose own source contains the thing under test goes quiet about it, because the
checker is a file in the tree. So the allow-lists live in the TOML, the denylist lives outside the
repository entirely, and the self-check builds its needles out of concatenated fragments at run
time.

    python3 tools/residue-check.py            # the gate
    python3 tools/residue-check.py --show     # every finding, with file:line
    python3 tools/residue-check.py --update   # rewrite docs/residue.toml from the tree, and
                                              # docs/residue-banned.txt from the register
"""

import hashlib
import json
import os
import re
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPEC = os.path.join(ROOT, "docs", "residue.toml")
HASHES = os.path.join(ROOT, "docs", "residue-banned.txt")

# Where the cleartext register lives when this machine has one. The default is the shared store
# this project's working notes are kept in; the variable exists so that a run can be made to see
# no register at all, which is the state CI is always in and the one worth testing deliberately.
REGISTER_ENV = "SKEIN_RESIDUE_REGISTER"
REGISTER_DEFAULT = os.path.join("~", "shared", "skein-notes", "residue-register.toml")

# Not walked. `upstream/` is a submodule with its own history and its own scrub; `.claude` in a
# working checkout is a symlink into the owner's live store and is not this repository's; the rest
# is build output and vendored dependencies nobody here edits.
SKIP_DIRS = {
    ".git",
    "target",
    "node_modules",
    "__pycache__",
    "upstream",
    ".claude",
    ".skein",
}

# Reserved for documentation and testing by RFC 2606 / RFC 6761, plus the `.example` label this
# tree uses for its own fixtures. A domain under any of these cannot be reached and cannot belong
# to anybody, so an address or a host there is by construction not somebody's.
RESERVED = (".example", ".invalid", ".test", ".localhost")
RESERVED_EXACT = {"example.com", "example.org", "example.net", "localhost"}


def reserved(host):
    h = host.lower().rstrip(".")
    return h in RESERVED_EXACT or h.endswith(RESERVED)


# ---- the shapes -----------------------------------------------------------------------------
#
# Each returns the *thing to declare* — the host, the home prefix, the address — rather than the
# whole line, so that one entry in the allow-list covers every site that names it and the list
# stays a list of decisions rather than of occurrences.

URL_HOST = re.compile(r"\b(?:https?|ssh|git\+https?)://(?:[^/@\s\"'`<>]+@)?([A-Za-z0-9._-]+)")
SCP_HOST = re.compile(r"\b[A-Za-z0-9._-]+@([A-Za-z0-9-]+\.[A-Za-z0-9.-]+):")
HOME = re.compile(r"(/(?:Users|home)/[A-Za-z0-9._-]+)")
ADDRESS = re.compile(r"\b([A-Za-z0-9._%+-]+@[A-Za-z0-9-]+\.[A-Za-z0-9.-]{2,})\b")

# Credential shapes. Each is a prefix a provider assigns, so a match is a real credential or a
# deliberate imitation of one — there is no third case, which is why this rule has no allow-list.
SECRETS = [
    # GitHub's own token prefixes, and its fine-grained personal access tokens.
    (re.compile(r"\bgh[pousr]_[A-Za-z0-9]{16,}"), "a GitHub token"),
    (re.compile(r"\bgithub_pat_[A-Za-z0-9_]{20,}"), "a GitHub fine-grained token"),
    (re.compile(r"\bsk-[A-Za-z0-9-]{6,}-[A-Za-z0-9_-]{20,}"), "a model-provider API key"),
    (re.compile(r"\bAKIA[0-9A-Z]{16}\b"), "an AWS access key id"),
    (re.compile(r"\bxox[abprs]-[A-Za-z0-9-]{10,}"), "a Slack token"),
    (re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"), "a private key"),
    (re.compile(r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\."), "a JWT"),
]

# ---- the denylist, as the gate sees it ------------------------------------------------------
#
# Hashes and widths, never strings. Everything that reads a needle is below `load_register`, and
# every one of those paths states plainly what it cannot do when the register is not there.

# The characters a needle may be built from — letters, digits, and `. _ - /`, the last because a
# banned string can be a path segment in a fixture's filename. A superset is safe here and a
# subset is not: an occurrence of a needle lies wholly inside one run of these characters, so
# cutting a line into runs and searching inside them loses nothing, whereas a class that omitted a
# character some needle uses would split that needle in half and never find it. `load_register`
# refuses a needle with anything else in it rather than writing a hash that could never fire.
RUN = re.compile(r"[A-Za-z0-9._/-]+")


def digest(text):
    """sha256 of an ASCII fragment, as bytes. The hex form is for the file and for reports."""
    return hashlib.sha256(text.encode("utf-8") if isinstance(text, str) else text).digest()


class Banned:
    """The fifth rule's list, holding widths and hashes and — only sometimes — the strings.

    `widths` is why this class exists. A hash cannot be substring-matched, so the scan has to
    hash candidate substrings, and it can only do that if it knows how wide a needle is. The
    widths therefore have to travel with the hashes, and they are the one thing about the
    register the repository does disclose.

    `register` is `{needle: reason}` when the cleartext register is reachable and `None` when it
    is not. Nothing about MATCHING depends on it: the gate is exactly as strict either way, and
    only what a report can SAY changes.
    """

    def __init__(self, widths, hashes, register=None):
        self.widths = sorted({int(w) for w in widths})
        self.hashes = set(hashes)
        self.register = register
        self.named = {digest(n.lower()): n for n in (register or {})}

    @classmethod
    def empty(cls):
        return cls([], [])

    def __len__(self):
        return len(self.hashes)

    def find(self, text):
        """Every banned hash whose needle occurs in `text`, case-insensitively as before.

        One entry per distinct hash, in the order first met, so a needle appearing twice on a
        line is one finding rather than two.
        """
        if not self.hashes:
            return []
        out, seen = [], set()
        for match in RUN.finditer(text.lower()):
            run = match.group().encode("ascii")
            span = len(run)
            for width in self.widths:
                if width > span:
                    break
                for i in range(span - width + 1):
                    h = digest(run[i : i + width])
                    if h in self.hashes and h not in seen:
                        seen.add(h)
                        out.append(h)
        return out

    def describe(self, h):
        """What a report may call one finding. The needle when the register is reachable, and a
        truncated hash when it is not — never a blank, and never an invented name."""
        if h in self.named:
            return repr(self.named[h])
        return f"the banned string with sha256 {h.hex()[:16]}…"

    def reason(self, h):
        """The register's sentence for one finding, or None."""
        return (self.register or {}).get(self.named.get(h))


def register_path():
    return os.path.expanduser(os.environ.get(REGISTER_ENV) or REGISTER_DEFAULT)


def load_register():
    """`{needle: reason}` from the cleartext register, or `None` when it is not reachable.

    A needle outside `RUN`'s character class is refused rather than hashed: the matcher cuts a
    line into runs of those characters, so such a needle could never be found and a hash for it
    would sit in the file looking like enforcement.
    """
    path = register_path()
    if not os.path.exists(path):
        return None
    with open(path, "rb") as fh:
        entries = tomllib.load(fh).get("banned", {})
    bad = [n for n in entries if not RUN.fullmatch(n)]
    if bad:
        print(
            f"residue-check: the register at {path} holds {len(bad)} needle(s) with a character "
            f"the matcher cannot search for\n"
            f"               rule: a needle is looked for inside runs of letters, digits and "
            f"`. _ - /`, so a needle containing anything else — a space above all — can never be "
            f"found. Fix the entry; a hash that cannot fire is worse than no entry"
        )
        sys.exit(2)
    return entries


def render_hashes(register):
    """`docs/residue-banned.txt`, derived from the cleartext register."""
    widths = sorted({len(n) for n in register})
    out = [
        "# The banned list, as the only form of it this repository may carry: sha256 of each",
        "# needle, lowercased. Read by `tools/residue-check.py`; see its docstring for why the",
        "# readable register — the needles, the reasons, whose each one was — lives outside this",
        "# repository, and where.",
        "#",
        "# `widths` is the set of needle lengths, and the matcher cannot work without it: a hash",
        "# cannot be substring-matched, so the scan hashes every substring of every identifier run",
        "# at each of these widths. It is written as a SET, sorted, and the hashes are sorted",
        "# too — so the file says how wide the shortest needle is and nothing about which hash",
        "# has which width.",
        "#",
        "# NOT an allow-list. An entry here that matches nothing is the correct state and is the",
        "# point of the entry, so nothing prunes this file. Regenerate it with",
        "# `python3 tools/residue-check.py --update` on a machine that can read the register.",
        "",
        "widths " + " ".join(str(w) for w in widths),
        "",
    ]
    out += sorted(digest(n.lower()).hex() for n in register)
    out.append("")
    return "\n".join(out)


HEX64 = re.compile(r"[0-9a-f]{64}\Z")


def load_hashes():
    """`(widths, hashes)` from `docs/residue-banned.txt`, or `None` when the file is not there.

    Malformed is not the same as missing and is not survivable: a line this cannot parse is a
    needle that is silently no longer enforced, so it exits rather than dropping the line.
    """
    if not os.path.exists(HASHES):
        return None
    widths, hashes = [], []
    for n, raw in enumerate(open(HASHES, encoding="utf-8").read().split("\n"), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("widths "):
            widths = [int(w) for w in line.split()[1:]]
            continue
        if not HEX64.match(line):
            print(
                f"residue-check: docs/residue-banned.txt:{n} is neither a width line nor a "
                f"sha256\n"
                f"               rule: every line here is enforcement — one this tool cannot read "
                f"is a banned string nothing is looking for. Regenerate the file with "
                f"`--update` from the register rather than editing it by hand"
            )
            sys.exit(2)
        hashes.append(bytes.fromhex(line))
    return widths, hashes


# A file with a NUL in its first block is binary and is not read. Cheaper and more reliable than
# trusting a suffix list, which is how a gate goes quiet about a file type nobody thought of.
def is_text(path):
    try:
        with open(path, "rb") as fh:
            return b"\0" not in fh.read(8192)
    except OSError:
        return False


def tracked():
    """Every file git has, or `None` when git cannot say.

    **What is committed is what goes public**, and that is not the same set as what is on disk. A
    walk reads a developer's scratch file and an ignored lock file and fails the build over them,
    which is how a gate earns its reputation for crying wolf; it also reads NOTHING about a file
    that was deleted from the tree but is still in history, which this gate never claimed to cover.
    So the list comes from git when git is there, and the walk is only the fallback.
    """
    import subprocess

    try:
        out = subprocess.run(
            ["git", "-C", ROOT, "ls-files", "-z"],
            capture_output=True,
            check=True,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return [p for p in out.stdout.decode("utf-8", "replace").split("\0") if p]


def files():
    listed = tracked()
    if listed is not None:
        for rel in sorted(listed):
            if rel.split("/", 1)[0] in SKIP_DIRS:
                continue
            path = os.path.join(ROOT, rel)
            if not os.path.isfile(path) or os.path.islink(path) or not is_text(path):
                continue
            yield rel, path
        return
    for base, dirs, names in os.walk(ROOT):
        dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS)
        for name in sorted(names):
            path = os.path.join(base, name)
            if os.path.islink(path) or not is_text(path):
                continue
            yield os.path.relpath(path, ROOT), path


def scan_text(text, banned):
    """{rule: {what: [line, …]}} for one file's text.

    Line numbers are collected per distinct `what`, so the report says "this host, at these four
    places" instead of four findings that are one decision.
    """
    out = {r: {} for r in ("host", "home", "address", "secret", "banned")}
    for n, line in enumerate(text.split("\n"), 1):
        for host in URL_HOST.findall(line) + SCP_HOST.findall(line):
            if not reserved(host):
                out["host"].setdefault(host, []).append(n)
        for home in HOME.findall(line):
            out["home"].setdefault(home, []).append(n)
        for addr in ADDRESS.findall(line):
            domain = addr.split("@", 1)[1]
            # `xterm@5.5.0` in a vendored bundle's banner is a package at a version, and every
            # minified dependency this tree ever vendors will carry one. A last label that is all
            # digits is not a top-level domain, so it is not an address.
            if domain.rsplit(".", 1)[-1].isdigit():
                continue
            if not reserved(domain):
                out["address"].setdefault(addr, []).append(n)
        for pattern, what in SECRETS:
            if pattern.search(line):
                out["secret"].setdefault(what, []).append(n)
        for h in banned.find(line):
            out["banned"].setdefault(h, []).append(n)
    return {r: v for r, v in out.items() if v}


# Every rule, in the order findings are reported. `filename` is the only one whose findings are
# paths rather than `file:line`, because what it found IS the path.
RULES = ("host", "home", "address", "secret", "banned", "filename")


def scan_name(label, banned):
    """Every banned string in one path, as hashes. Case-insensitive, like the content rule.

    A path is one argument to the same matcher a line is, and `/` is inside `RUN`'s class, so a
    needle that spans a directory separator is found the way one inside a filename is.
    """
    return banned.find(label)


def survey(banned):
    """{rule: {what: ["file:line", …]}} across the tree.

    `docs/residue.toml` is the one file not read, and it has to be. It names every allowed host,
    home and address, so scanning it would make every allow-list entry cite itself as its own use
    and no entry could ever go stale. It is the register, not the tree.

    `docs/residue-banned.txt` needs no such exemption, and that is a property of the move rather
    than an oversight: it holds hashes, and the strings whose presence it is looking for are not
    in it. The file the old `[banned]` table had to be exempted from reporting itself is now
    outside the repository altogether.
    """
    found = {r: {} for r in RULES}
    for label, path in files():
        # A PATH carries names too, and this is the half a content scan cannot see. The pane
        # fixtures are named `<agent>-<state>.<box>.<what>.<date>.txt`, so the `<box>` segment is
        # a real box name sitting in a filename — the case SKEIN-540 opened with, and the reason
        # the argument there was that a rewrite map rather than a hand edit is the right tool: an
        # edit cannot reach the name a file was committed under. Checked against `[banned]` only.
        # The shape rules are about what a line SAYS; a path is not prose and a `/home/x` segment
        # inside one is almost always the tree's own layout.
        for needle in scan_name(label, banned):
            found["filename"].setdefault(needle, []).append(label)
        if os.path.abspath(path) == os.path.abspath(SPEC):
            continue
        try:
            text = open(path, encoding="utf-8").read()
        except (OSError, UnicodeDecodeError):
            continue
        for rule, hits in scan_text(text, banned).items():
            for what, lines in hits.items():
                found[rule].setdefault(what, []).extend(f"{label}:{n}" for n in lines)
    return found


# ---- the allow-list -------------------------------------------------------------------------

SECTION = {"host": "hosts", "home": "homes", "address": "addresses"}
# Written to read as the subject of "… is a decision — declare it in …", and as the heading over
# its own table in the generated file.
RULE_TEXT = {
    "host": "every host this tree reaches or names",
    "home": "every home directory named in the tree — a home directory names a person, and usually a machine as well",
    "address": "every email address in the tree",
}


def load_spec():
    if not os.path.exists(SPEC):
        return None
    with open(SPEC, "rb") as fh:
        return tomllib.load(fh)


# The four tables this gate reads. Anything else in the spec is not a list it consults — and
# `banned` is deliberately no longer among them, so a `[banned]` table written back into
# `docs/residue.toml` is reported as a table nothing reads rather than quietly enforcing from the
# one place this project decided its needles may not be written.
TABLES = ("hosts", "homes", "addresses", "exempt")


def misfiled(spec):
    """Keys the spec declares that no rule will ever read.

    An entry written above the first `[table]` header — which is easy, because the header is
    thirty lines of prose down — is a ROOT key in TOML, not a member of any table. Nothing reads
    root keys, so the entry is inert: the host it names can come back and the gate stays green. It
    happened here to eleven entries at once (SKEIN-540), among them the client project, the
    tracker gateway and the owner's home directory, and none of them was enforced for as long as
    they sat there. `--update` would then have deleted them, since `render` copies the tables and
    rebuilds everything else.

    A misspelled table header (`[addressses]`) fails the same way and silently, so both are caught
    here: a top-level key that is not one of the four tables, and one of the four holding
    something that is not a table.
    """
    said = []
    for key, value in sorted(spec.items()):
        if not isinstance(value, dict):
            said.append(
                f"residue-check: docs/residue.toml declares {key!r} at the top level, outside "
                f"every table\n"
                f"               rule: a key above the first [table] header belongs to no list "
                f"and is read by nothing — it is an entry that looks made and was not. Move it "
                f"under the table it was meant for ({', '.join(TABLES)})"
            )
        elif key not in TABLES:
            said.append(
                f"residue-check: docs/residue.toml has a table [{key}] that no rule reads\n"
                f"               rule: the gate consults {', '.join(TABLES)} and nothing else, "
                f"so every entry under [{key}] is inert. Fix the header's spelling, or delete the "
                f"table if it was never meant to be read"
            )
    return said


def quoted(text):
    """One TOML basic string. `repr` with the quotes swapped is what this used to be, and it
    mangles every reason containing an apostrophe — `a collaborator's login` came back out of
    `--update` as `a collaborator"s login`, which is not the sentence and, past the second one on
    a line, not TOML either. JSON's string syntax is TOML's basic-string syntax."""
    return json.dumps(text, ensure_ascii=False)


def render(found, spec):
    """`docs/residue.toml`, rewritten from the tree, keeping every reason already written."""
    old = spec or {}
    out = [
        "# What this tree is allowed to name.",
        "#",
        "# Read by `tools/residue-check.py`. Every table here is an ALLOW-list: a host, a home",
        "# directory or an address that is not here fails the build, and an entry here that",
        "# nothing uses fails it too, because an allow-list nobody prunes is a permission nobody",
        "# granted.",
        "#",
        "# What this file does NOT hold is the denylist. That register — the literal strings that",
        "# may never come back, and the sentence saying whose each one was — is the disclosure it",
        "# was written to prevent, so it lives outside this repository (SKEIN-630) and the tree",
        "# carries `docs/residue-banned.txt`, hashes of it. The gate enforces from the hashes and",
        "# needs no register to do it.",
        "#",
        "# Regenerate the allow-lists with `python3 tools/residue-check.py --update`; it keeps the",
        "# reasons already written and leaves TODO against anything new, which is the line you are",
        "# meant to stop and think about.",
        "",
    ]
    for rule in ("host", "home", "address"):
        section = SECTION[rule]
        out.append(f"# {RULE_TEXT[rule]}.")
        out.append(f"[{section}]")
        have = old.get(section, {})
        for what in sorted(found[rule]):
            reason = have.get(what, "TODO: why is this here, and whose is it?")
            out.append(f"{quoted(what)} = {quoted(reason)}")
        out.append("")
    out.append("# Credential-shaped strings that are deliberate — a documented example, a fixture.")
    out.append("# Expected to stay empty: a real credential is rotated, not exempted.")
    out.append("[exempt]")
    for what, reason in sorted(old.get("exempt", {}).items()):
        out.append(f"{quoted(what)} = {quoted(reason)}")
    out.append("")
    return "\n".join(out)


# Said whenever a report would have named a banned string and could not. It is a sentence rather
# than a silence because the failure this replaces is a gate that reports an empty list and reads
# as green.
REGISTER_ABSENT = (
    "               the register that would name it is not reachable from here, so this report "
    "cannot\n"
    "               spell the string: it is outside this repository on purpose. Open the line "
    "above,\n"
    f"               or set {REGISTER_ENV} to a copy of the register"
)


def problems(found, spec, banned):
    said = []
    for rule in ("host", "home", "address"):
        section = SECTION[rule]
        allowed = spec.get(section, {})
        for what in sorted(found[rule]):
            if what in allowed:
                continue
            where = ", ".join(found[rule][what][:4])
            more = f" (+{len(found[rule][what]) - 4} more)" if len(found[rule][what]) > 4 else ""
            said.append(
                f"residue-check: undeclared {rule} {what!r}\n"
                f"               at {where}{more}\n"
                f"               rule: {RULE_TEXT[rule]} is a decision — declare it in "
                f"docs/residue.toml [{section}] with whose it is and why this tree names it, "
                f"or take it out"
            )
        for what in sorted(set(allowed) - set(found[rule])):
            said.append(
                f"residue-check: docs/residue.toml [{section}] allows {what!r} and nothing "
                f"names it\n"
                f"               rule: an allow-list nobody prunes is a permission nobody "
                f"granted — drop the entry in the change that removed the last use"
            )
    exempt = spec.get("exempt", {})
    for what in sorted(found["secret"]):
        if what in exempt:
            continue
        where = ", ".join(found["secret"][what][:4])
        said.append(
            f"residue-check: something shaped like {what}\n"
            f"               at {where}\n"
            f"               rule: a credential in the tree is rotated first and removed second — "
            f"exempting it in docs/residue.toml [exempt] is only right for a string that was "
            f"never a credential"
        )
    for what in sorted(set(exempt) - set(found["secret"])):
        said.append(
            f"residue-check: docs/residue.toml [exempt] excuses {what!r} and nothing matches it\n"
            f"               rule: drop the entry — an exemption for a finding that is gone is a "
            f"licence waiting for the next one"
        )
    # The two denylist rules report the LOCATION and, only where the register is reachable, the
    # string. In CI it is not, and that is the right way round: the finding's own line is on
    # screen for whoever reads it, and printing the needle would write a banned string into a
    # public build log — the disclosure SKEIN-630 moved the register to avoid.
    unnamed = banned.register is None
    for what in sorted(found["banned"]):
        where = ", ".join(found["banned"][what][:4])
        more = f" (+{len(found['banned'][what]) - 4} more)" if len(found["banned"][what]) > 4 else ""
        said.append(
            f"residue-check: {banned.describe(what)} is back in the tree\n"
            f"               at {where}{more}\n"
            + (f"               reason it is banned: {banned.reason(what)}\n" if not unnamed else "")
            + f"               rule: this string was removed from all of history on purpose. It "
            f"is not a naming question — replace it, and if the replacement is wrong, argue about "
            f"it in the register rather than here"
            + (f"\n{REGISTER_ABSENT}" if unnamed else "")
        )
    for what in sorted(found["filename"]):
        where = ", ".join(found["filename"][what][:4])
        more = f" (+{len(found['filename'][what]) - 4} more)" if len(found["filename"][what]) > 4 else ""
        said.append(
            f"residue-check: {banned.describe(what)} is in the NAME of a file\n"
            f"               at {where}{more}\n"
            + (f"               reason it is banned: {banned.reason(what)}\n" if not unnamed else "")
            + f"               rule: renaming the file fixes the tree and not the history — the "
            f"name it was committed under stays in every commit that carried it. Rename it here "
            f"AND add the string to the rewrite map, or the next clone still has it"
            + (f"\n{REGISTER_ABSENT}" if unnamed else "")
        )
    return said


# ---- self-check -----------------------------------------------------------------------------
#
# Built by concatenation so that this file never contains a token, an address or a host that its
# own rules would have to judge. A checker whose source spells what it looks for teaches itself to
# be quiet (prose-check's WTS-8, from the other direction).

def _needles():
    return {
        "host": "see http" + "s://tracker." + "internal-corp" + ".dev/x for the queue",
        "home": "the path was /Us" + "ers/somebody/code/thing",
        "address": "mail a" + "sked to " + "someone" + "@" + "acmecorp" + ".co.uk please",
        "secret": "token = \"gh" + "p_" + "A" * 36 + "\"",
        "innocent": (
            "https://example.com and t@example.com and a JWT-ish word eyJust and "
            "/home/ and gh_ and sk- and https://sub.example are all fine"
        ),
    }


def fail(*said):
    for line in said:
        print(line)
    sys.exit(2)


def self_check():
    n = _needles()
    none = Banned.empty()
    for rule in ("host", "home", "address", "secret"):
        hits = scan_text(n[rule], none)
        if rule not in hits:
            fail(
                f"residue-check: SELF-CHECK FAILED — the {rule} rule did not fire on its own",
                f"               needle. The gate is blind to {rule} and would pass anything.",
            )
    clean = scan_text(n["innocent"], none)
    if clean:
        fail(
            "residue-check: SELF-CHECK FAILED — a line with nothing in it was reported as",
            f"               {sorted(clean)}. A gate that cries wolf is a gate somebody turns off.",
        )
    # ---- the denylist, on a hash of a SYNTHETIC needle -----------------------------------------
    #
    # Never a real one: a real needle here would be the disclosure the register was moved to
    # prevent, in the one file that must not spell what it looks for. The canary is built by
    # concatenation for the same reason the shape needles above are.
    canary = "The" + "Canary" + "Word"
    one = Banned([len(canary)], [digest(canary.lower())])
    if not scan_text("a line containing thecanaryword here", one).get("banned"):
        fail("residue-check: SELF-CHECK FAILED — the banned rule did not fire on its own needle.")
    if not scan_text(f"a line containing {canary} here", one).get("banned"):
        fail(
            "residue-check: SELF-CHECK FAILED — the banned list is not matched case-insensitively"
        )
    # THE TRAP THE MOVE TO HASHES WALKS INTO (SKEIN-630). The rule this replaced was
    # `needle.lower() in line.lower()`, and it caught a needle sitting INSIDE a longer identifier
    # — which is how these strings actually turn up: a branch name, a fixture filename, a package
    # slug. A matcher that hashed whole lines, whole words, or word-boundary tokens would pass
    # every check above and lose this one silently. It is checked from both sides, and the
    # negative matters as much: a run that merely contains the needle's characters, broken up,
    # must not fire.
    for spelling in (
        f"xx-{canary.lower()}-yy",
        f"prefix{canary.lower()}suffix",
        f"a/path/with-{canary.lower()}-inside.txt",
    ):
        if not scan_text(spelling, one).get("banned"):
            fail(
                "residue-check: SELF-CHECK FAILED — a banned needle inside a longer identifier",
                "               was not found. Hashing cannot substring-match, so a matcher that",
                "               hashes whole lines or whole words weakens this rule to nothing",
                "               while every other check in here still passes.",
            )
    for innocent in ("the-canary-word", "thecanary word", "thecanarywor", "hecanaryword"):
        if scan_text(innocent, one).get("banned"):
            fail(
                "residue-check: SELF-CHECK FAILED — the banned rule fired on a line that does not",
                "               contain the needle. A gate that cries wolf is a gate somebody",
                "               turns off.",
            )
    if scan_text("a line containing thecanaryword here", none).get("banned"):
        fail("residue-check: SELF-CHECK FAILED — an empty denylist matched something.")
    # The shape rule, from both sides: a spec that is wrong must be caught, and a spec that is
    # right must not be. Only the second half would have gone unnoticed, and it is the half that
    # makes the gate cry wolf.
    if len(misfiled({"hosts": {}, "stray": "a reason", "addressses": {}})) != 2:
        fail(
            "residue-check: SELF-CHECK FAILED — a key outside every table, or a table no rule",
            "               reads, was not caught. Entries filed there are enforced by nothing.",
        )
    if misfiled({t: {} for t in TABLES}):
        fail("residue-check: SELF-CHECK FAILED — a well-formed spec was called misfiled.")
    # The filename rule fires on a PATH, which no other rule reads, so nothing else in this
    # function would notice it going quiet. Both directions again: the needle must be found in a
    # path, and a path that merely resembles it must not be.
    if scan_name(
        f"tests/fixtures/panes/claude-idle.{canary.lower()}.2026-01-01.txt", one
    ) != [digest(canary.lower())]:
        fail(
            "residue-check: SELF-CHECK FAILED — the filename rule cannot see a banned name in a",
            "               path, so a fixture named after a client would go unreported.",
        )
    if scan_name("tests/fixtures/panes/claude-idle.the-canary-word.txt", one):
        fail("residue-check: SELF-CHECK FAILED — the filename rule matched a path it should not.")
    # The hash file is enforcement, so a round trip through it has to be lossless. `--update`
    # writes it and the gate reads it back on every run; a widths line or a hex format the reader
    # and the writer disagreed about would drop needles without saying anything.
    written = render_hashes({canary: "the self-check's own needle, which is not a real one"})
    widths = [int(w) for w in re.search(r"^widths (.*)$", written, re.M).group(1).split()]
    read = [bytes.fromhex(h) for h in re.findall(r"^[0-9a-f]{64}$", written, re.M)]
    if widths != [len(canary)] or read != [digest(canary.lower())]:
        fail(
            "residue-check: SELF-CHECK FAILED — a needle did not survive the round trip through",
            "               docs/residue-banned.txt's format. Every needle that does not is a",
            "               string nothing is looking for, and the file still looks enforced.",
        )


self_check()


def main():
    spec = load_spec()
    # Before anything reads the spec, and before `--update` can rewrite it: an entry filed where no
    # rule looks is worse than a missing one, because the file still reads as though it were made.
    if spec is not None:
        broken = misfiled(spec)
        if broken:
            for p in broken:
                print(p)
            return 1
    register = load_register()
    listed = load_hashes() or ([], [])
    banned = Banned(listed[0], listed[1], register)
    found = survey(banned)

    if "--show" in sys.argv:
        for rule in RULES:
            for what in sorted(found[rule]):
                where = found[rule][what]
                label = banned.describe(what) if rule in ("banned", "filename") else what
                print(f"{rule:8} {label:44} {len(where):4}  {', '.join(where[:3])}")
        # Said whether or not anything was found, and above all when nothing was: the failure this
        # line exists to prevent is a `--show` that read no register, printed no banned finding,
        # and looked exactly like a tree with none in it.
        if register is None:
            print(
                f"\nresidue-check: {len(banned)} banned string(s) are enforced from hashes and "
                f"NOT named above —\n"
                f"               the register is not reachable at {register_path()}. It is "
                f"outside this\n"
                f"               repository on purpose; point {REGISTER_ENV} at it to see the "
                f"names."
            )
        else:
            print(
                f"\nresidue-check: {len(banned)} banned string(s) read from {register_path()}."
            )
        return 0
    if "--update" in sys.argv:
        open(SPEC, "w", encoding="utf-8").write(render(found, spec))
        print(f"wrote {os.path.relpath(SPEC, ROOT)} — every TODO reason is a decision to write")
        # The hash file is regenerated only from the register, and NOT TOUCHED without one. The
        # alternative — writing what an absent register implies — is a one-command way to empty
        # the denylist and commit it, from a machine where nothing would have looked wrong.
        if register is None:
            print(
                f"       left {os.path.relpath(HASHES, ROOT)} alone — the register is not "
                f"reachable at\n"
                f"       {register_path()}, and regenerating the hashes without it would write an "
                f"empty\n"
                f"       denylist over the only enforcement this repository has"
            )
        else:
            open(HASHES, "w", encoding="utf-8").write(render_hashes(register))
            print(
                f"wrote {os.path.relpath(HASHES, ROOT)} — {len(register)} hashes from "
                f"{register_path()}"
            )
        return 0
    if spec is None:
        print(
            "residue-check: docs/residue.toml is missing, so nothing is declared and the gate\n"
            "               cannot tell an allowed host from a leaked one\n"
            "               rule: run `python3 tools/residue-check.py --update` and write the "
            "reasons"
        )
        return 1
    # THE FAILURE THIS GATE IS MOST EXPOSED TO SINCE THE REGISTER LEFT: the hash file absent, or
    # present with nothing the matcher can use, and the run green because nothing matched. Both
    # are checked here rather than at load time so that `--show` and `--update` can still be used
    # to fix them.
    if not os.path.exists(HASHES):
        print(
            "residue-check: docs/residue-banned.txt is missing, so the denylist rule is enforced\n"
            "               by nothing at all, and it is the rule whose strings must never come "
            "back\n"
            "               rule: the file is derived from the register — restore it from git, or "
            "regenerate it with `--update` on a machine that can read the register"
        )
        return 1
    if not banned.hashes or not banned.widths:
        print(
            "residue-check: docs/residue-banned.txt declares no hash, or no width to look for one\n"
            "               at, so nothing it lists can ever be matched\n"
            "               rule: an empty denylist and an unenforceable one look identical from "
            "outside — regenerate the file with `--update` rather than editing it by hand"
        )
        return 1
    # A drift between register and hashes is visible only where both are, which is a developer's
    # machine and never CI. Reported rather than silently corrected: `--update` is what writes the
    # file, and a gate that rewrote a tracked file mid-run would be a gate making its own evidence.
    if register is not None and render_hashes(register) != open(HASHES, encoding="utf-8").read():
        print(
            f"residue-check: docs/residue-banned.txt is not what the register at "
            f"{register_path()}\n"
            f"               would generate — one of them has been edited without the other\n"
            f"               rule: the hashes are the whole of this rule's enforcement inside the "
            f"repository and the register is their source. Run `python3 "
            f"tools/residue-check.py --update` and commit the result"
        )
        return 1

    said = problems(found, spec, banned)
    for p in said:
        print(p)
    return 1 if said else 0


if __name__ == "__main__":
    sys.exit(main())
