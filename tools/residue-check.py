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
  banned     Literal strings that must never appear again, listed in `[banned]`. Unlike every
             other list in this tree this one is a DENYLIST: an entry is not a permission, and an
             entry that matches nothing is the correct state rather than a stale one.

WHAT IS NOT CHECKED, and deliberately. Prose that describes a person without naming them; a first
name in a sentence; a project's internal vocabulary. Those are the half of a scrub that no pattern
can do — a transcript is redacted by somebody reading it — and pretending otherwise would make the
gate's silence mean more than it does. See `tests/fixtures/panes/README.md` for the half done by
hand.

NOTHING IN THIS FILE MAY SPELL WHAT IT LOOKS FOR. `tools/prose-check.py` learned this the hard
way: a checker whose own source contains the thing under test goes quiet about it, because the
checker is a file in the tree. So the literal list lives in the TOML, and the self-check builds its
needles out of concatenated fragments at run time.

    python3 tools/residue-check.py            # the gate
    python3 tools/residue-check.py --show     # every finding, with file:line
    python3 tools/residue-check.py --update   # rewrite docs/residue.toml from the tree
"""

import os
import re
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPEC = os.path.join(ROOT, "docs", "residue.toml")

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
        low = line.lower()
        for needle in banned:
            if needle.lower() in low:
                out["banned"].setdefault(needle, []).append(n)
    return {r: v for r, v in out.items() if v}


def survey(banned):
    """{rule: {what: ["file:line", …]}} across the tree.

    `docs/residue.toml` is the one file not read, and it has to be. It names every allowed host
    and every banned string, so scanning it would (a) make every allow-list entry cite itself as
    its own use, so no entry could ever go stale, and (b) report every banned string as back in
    the tree the moment it was banned. It is the register, not the tree.
    """
    found = {r: {} for r in ("host", "home", "address", "secret", "banned")}
    for label, path in files():
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


# The five tables this gate reads. Anything else in the spec is not a list it consults.
TABLES = ("hosts", "homes", "addresses", "banned", "exempt")


def misfiled(spec):
    """Keys the spec declares that no rule will ever read.

    A denylist entry written above the first `[table]` header — which is easy, because the header
    is thirty lines of prose down — is a ROOT key in TOML, not a member of `[banned]`. Nothing
    reads root keys, so the entry is inert: the string it names can come back and the gate stays
    green. It happened here to eleven entries at once (SKEIN-540), among them the client project,
    the tracker gateway and the owner's home directory, and none of them was enforced for as long
    as they sat there. `--update` would then have deleted them, since `render` copies the tables
    and rebuilds everything else.

    A misspelled table header (`[bannned]`) fails the same way and silently, so both are caught
    here: a top-level key that is not one of the five tables, and one of the five holding something
    that is not a table.
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


def render(found, spec):
    """`docs/residue.toml`, rewritten from the tree, keeping every reason already written."""
    old = spec or {}
    out = [
        "# What this tree is allowed to name — and what it may never name again.",
        "#",
        "# Read by `tools/residue-check.py`. Three of these tables are ALLOW-lists: a host, a home",
        "# directory or an address that is not here fails the build, and an entry here that",
        "# nothing uses fails it too, because an allow-list nobody prunes is a permission nobody",
        "# granted. `[banned]` is the opposite and is the one place in this repository where an",
        "# entry matching nothing is the correct state.",
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
            out.append(f'{what!r} = {reason!r}'.replace("'", '"'))
        out.append("")
    out.append("# Strings that must never appear again. NOT an allow-list: an entry here that")
    out.append("# matches nothing is the point of the entry.")
    out.append("[banned]")
    for what, reason in sorted(old.get("banned", {}).items()):
        out.append(f'{what!r} = {reason!r}'.replace("'", '"'))
    out.append("")
    out.append("# Credential-shaped strings that are deliberate — a documented example, a fixture.")
    out.append("# Expected to stay empty: a real credential is rotated, not exempted.")
    out.append("[exempt]")
    for what, reason in sorted(old.get("exempt", {}).items()):
        out.append(f'{what!r} = {reason!r}'.replace("'", '"'))
    out.append("")
    return "\n".join(out)


def problems(found, spec):
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
    for what in sorted(found["banned"]):
        where = ", ".join(found["banned"][what][:4])
        more = f" (+{len(found['banned'][what]) - 4} more)" if len(found["banned"][what]) > 4 else ""
        said.append(
            f"residue-check: {what!r} is on the banned list and is back in the tree\n"
            f"               at {where}{more}\n"
            f"               rule: this string was removed from all of history on purpose. It is "
            f"not a naming question — replace it, and if the replacement is wrong, argue about it "
            f"in docs/residue.toml rather than here"
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


def self_check():
    n = _needles()
    for rule in ("host", "home", "address", "secret"):
        hits = scan_text(n[rule], [])
        if rule not in hits:
            print(f"residue-check: SELF-CHECK FAILED — the {rule} rule did not fire on its own")
            print(f"               needle. The gate is blind to {rule} and would pass anything.")
            sys.exit(2)
    clean = scan_text(n["innocent"], [])
    if clean:
        print("residue-check: SELF-CHECK FAILED — a line with nothing in it was reported as")
        print(f"               {sorted(clean)}. A gate that cries wolf is a gate somebody turns off.")
        sys.exit(2)
    banned = scan_text("a line containing thecanaryword here", ["TheCanaryWord"])
    if "banned" not in banned:
        print("residue-check: SELF-CHECK FAILED — the banned list is not matched case-insensitively")
        sys.exit(2)
    # The shape rule, from both sides: a spec that is wrong must be caught, and a spec that is
    # right must not be. Only the second half would have gone unnoticed, and it is the half that
    # makes the gate cry wolf.
    if len(misfiled({"hosts": {}, "stray": "a reason", "bannned": {}})) != 2:
        print("residue-check: SELF-CHECK FAILED — a key outside every table, or a table no rule")
        print("               reads, was not caught. Entries filed there are enforced by nothing.")
        sys.exit(2)
    if misfiled({t: {} for t in TABLES}):
        print("residue-check: SELF-CHECK FAILED — a well-formed spec was called misfiled.")
        sys.exit(2)

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
    banned = list((spec or {}).get("banned", {}))
    found = survey(banned)

    if "--show" in sys.argv:
        for rule in ("host", "home", "address", "secret", "banned"):
            for what in sorted(found[rule]):
                where = found[rule][what]
                print(f"{rule:8} {what:44} {len(where):4}  {', '.join(where[:3])}")
        return 0
    if "--update" in sys.argv:
        open(SPEC, "w", encoding="utf-8").write(render(found, spec))
        print(f"wrote {os.path.relpath(SPEC, ROOT)} — every TODO reason is a decision to write")
        return 0
    if spec is None:
        print(
            "residue-check: docs/residue.toml is missing, so nothing is declared and the gate\n"
            "               cannot tell an allowed host from a leaked one\n"
            "               rule: run `python3 tools/residue-check.py --update` and write the "
            "reasons"
        )
        return 1

    said = problems(found, spec)
    for p in said:
        print(p)
    return 1 if said else 0


if __name__ == "__main__":
    sys.exit(main())
