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
             `[hosts]`, and so is every BARE hostname written as the whole of a quoted string.
             This is the rule that would have caught the private work-tracker gateway, and it
             catches its replacement without anybody adding a pattern. The bare half is here
             because a URL is not how an endpoint usually reaches a test: a name on a live gTLD
             reached one as the argument to a `page.fill`, and this gate passed on every commit
             that carried it (SKEIN-542). `BARE_HOST` below argues what it looks at, and why.
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

WHERE THE RULES ARE APPLIED, which is two places and used to be one. Tracked file contents and
tracked filenames are the obvious one. The other is COMMIT MESSAGES AND AUTHORSHIP, for the
commits a push would add, and it is here because that is the door a banned string actually walked
back in through: `0a9fa3c` is on `origin/master` and its body names three live Docker volumes
belonging to other people's work, quoted while the commit explained itself (SKEIN-628). Nothing in
this gate had ever read a commit message — `tracked()` builds its list from `git ls-files`, which
reports file contents and filenames and nothing else — so the leak was not missed, it was outside
the gate's reach by construction.

A message is scanned under the same five rules, with two differences that follow from what a
message is:

  * The allow-list for it is `[messages]`, separate from `[hosts]`/`[homes]`/`[addresses]`,
    because what a commit may say about itself is not the same question as what the tree may
    name — a session URL in a trailer is not a host this code reaches. A host already allowed in
    the tree is allowed in a message too; the reverse does not hold.
  * `[messages]` is NOT pruned for disuse, and that is the one place this file departs from "an
    allow-list nobody prunes is a permission nobody granted". The scanned set is the commits not
    yet pushed, so it empties itself on every push: an entry that matched this morning matches
    nothing this afternoon, and pruning on that would have the gate demand the deletion of a line
    it will demand back on the next commit.

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
    python3 tools/residue-check.py --history refs/remotes/origin/master
                                              # the denylist over everything reachable from a
                                              # ref: blob contents, committed path strings, and
                                              # the whole log. Minutes, not seconds — a flag
                                              # somebody runs before a release, and not in CI
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

# ---- a hostname with no URL around it ---------------------------------------------------------
#
# `URL_HOST` and `SCP_HOST` find a host because the line SAYS it is one — a scheme in front, or an
# `@` before it and a `:` after. A bare hostname says nothing, and that is the form a real endpoint
# most often takes in a test: a form field, a config value, an assertion on a rendered string. One
# reached this tree exactly that way, a `.dev` name in a `page.fill()`, and this gate passed on
# every commit that carried it (SKEIN-542). The exposure was small; the shape is not.
#
# WHY THIS IS NOT `\S+\.\w+`. Because a dotted name is also every attribute access in four
# languages and most of the filenames in the tree. Counted over the files `files()` yields, with
# `(label\.)+label` and a label of `[A-Za-z0-9][A-Za-z0-9-]*`: 10,558 distinct matches, and 1,502
# still left after demanding three labels — `out.status.success`, `this.lexer.state.top`,
# `os.path.join`. No pattern tells a hostname from an attribute chain,
# because there is no difference to see. So this rule stops trying, and cuts the problem down with
# two constraints instead, one about CONTEXT and one about the TAIL. Each is load-bearing:
#
#   the WHOLE of a quoted string   A host a program uses is data, and data in this tree is written
#                                  between quotes: the leaked name was the entire second argument
#                                  to a `page.fill`, and nothing in a sentence is. Requiring the
#                                  quotes, and requiring the name to be all that is between them,
#                                  is what drops the three cases SKEIN-542 named as the test —
#                                  `meta.dev()` and `m.dev()`, which are `MetadataExt::dev` in
#                                  src/fleet.rs, and a citation in docs/UX-AUDIT.md whose last
#                                  label is a live gTLD. A naive matcher reports all three.
#                                  Markdown's backticks count as quotes, so a host in a code span
#                                  in a doc is read like one in a `.rs` string — which is also why
#                                  none of the three can be spelled in this comment. The
#                                  docstring's NOTHING IN THIS FILE MAY SPELL WHAT IT LOOKS FOR
#                                  is enforced by this rule as of now: the first draft of this
#                                  block named all three and the gate failed on itself.
#   a tail from `HOST_TLDS`        "the last label is a real top-level domain" is not a test any
#                                  more: the 2012 gTLD round made `store`, `email`, `link`,
#                                  `review`, `open` and several hundred other ordinary words into
#                                  TLDs. Against the whole IANA list of 1,292 of them, quoted
#                                  whole strings still leave 26 candidates on this tree, 19 being
#                                  `"user.email"`, `"memory.events"`, `"repo.store"`,
#                                  `"window.open"` and their kind. So the tail must be one this
#                                  rule believes — see the set.
#
# WHAT IT THEREFORE MISSES, said out loud, because a gate whose silence is read as an answer has to
# be honest about the question it did not ask: a bare host under a TLD outside the set; one written
# unquoted, in prose or in unquoted YAML; and one whose first label is a single character, which is
# excluded because `"h.ai"` and `"d.app"` in this tree are a key and a variable rather than hosts.
# None of that touches `URL_HOST`, which still takes any TLD in any context. What is lost is the
# second net, not the first.
#
# RFC 2606, AND THE ONE PLACE THIS PARTS COMPANY WITH `reserved()`. SKEIN-542 asks whether to
# refuse any host outside RFC 2606 reserved space unless it is declared. That is what this rule
# does, and it is why it would have caught the leak on its merits rather than by luck: `.dev` is a
# live gTLD, so the leaked name's registrable domain could be bought by anybody and is NOT reserved
# however much the label in front of the dot reads as if it were — which is exactly how the sweep
# came to find it by accident rather than on purpose. The difference between this rule and
# `reserved()` is that reserved-ness is asked of the REGISTRABLE DOMAIN — the last two
# labels — because that is what RFC 2606 reserves: nobody can hold a name under `example.net`,
# since nobody but IANA holds `example.net`. `reserved()` matches those three names exactly and so
# calls `mcp.example.net` somebody's host, which is why `[hosts]` carries four `*.example.com/.net`
# entries. Making the older rule agree would strand all four, so it is a separate change with its
# own decision to make (SKEIN-686) and not this one.
HOST_TLDS = frozenset(
    (
        # The seven original generic domains, the two from 2001, and the generic ones that
        # infrastructure is actually published under today.
        "com net org edu gov mil int info biz dev app cloud tech xyz "
        # Every two-letter country code, minus two kinds of collision. The country codes that are
        # also ordinary code words: .as .at .be .by .do .id .im .in .is .it .me .no .re .so .to
        # .us .ws — `"repo.id"` and `"sudo.ws"` are in this tree and are not hosts. And the ones
        # that are a source-file suffix: .ac .am .cc .md .mk .ml .mm .pl .pm .py .rs .sh .tf, so
        # that `"box-session.sh"` and `"fleet.rs"` stay filenames. Both exclusions are LEXICAL —
        # about the words a programmer types, not about the files this tree holds this morning —
        # which is what keeps the set from needing an edit every time somebody adds a fixture.
        "ad ae af ag ai al ao aq ar au aw ax az ba bb bd bf bg bh bi bj bm bn bo br bs bt bv bw "
        "bz ca cd cf cg ch ci ck cl cm cn co cr cu cv cw cx cy cz de dj dk dm dz ec ee eg er es "
        "et eu fi fj fk fm fo fr ga gb gd ge gf gg gh gi gl gm gn gp gq gr gs gt gu gw gy hk hm "
        "hn hr ht hu ie il io iq ir je jm jo jp ke kg kh ki km kn kp kr kw ky kz la lb lc li lk "
        "lr ls lt lu lv ly ma mc mg mh mn mo mp mq mr ms mt mu mv mw mx mz na nc ne nf ng ni nl "
        "np nr nu nz om pa pe pf pg ph pk pn pr ps pt pw qa ro ru rw sa sb sc sd se sg si sj sk "
        "sl sm sn sr ss st su sv sx sy sz tc td tg th tj tk tl tm tn tr tt tv tw tz ua ug uk uy "
        "uz va vc ve vg vi vn vu wf ye yt za zm zw"
    ).split()
)

# One quoted string that is nothing but a dotted, lowercase, hyphenated name. The first label is
# two characters or more; the tail is letters only, which is what keeps `"1.2.3"` and every version
# string out. Group 2 is the host to declare and group 3 is its tail.
BARE_HOST = re.compile(r"""(['"`])([a-z0-9][a-z0-9-]+(?:\.[a-z0-9][a-z0-9-]*)*\.([a-z]{2,24}))\1""")


def bare_reserved(host):
    """Whether a BARE name is in reserved space, asked of its registrable domain as well as itself.

    `reserved()` answers for the name as written, which is right for a URL host and wrong here for
    the reason above: RFC 2606 reserves `example.com`, `.net` and `.org` and therefore everything
    under them, so `mcp.example.net` — which is what SKEIN-520's rewrite turned the leaked name
    into — belongs to nobody and is not a decision anybody needs to make twice.
    """
    return reserved(host) or reserved(".".join(host.rsplit(".", 2)[-2:]))


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
        # Inside `scan_text` and not beside it, so there is one matcher and not two — the mistake
        # the `--history` section below was written about. A bare host is then a `[hosts]` decision
        # like any other, and every caller gets it at once: the tree survey, the commit-message
        # scan, and the history sweep, which hands blobs and log entries to this same function.
        # What history REPORTS is still the denylist alone, for the reason stated there.
        for quoted_host in BARE_HOST.finditer(line):
            host, tail = quoted_host.group(2), quoted_host.group(3)
            if tail in HOST_TLDS and not bare_reserved(host):
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


# ---- the commits a push would add -------------------------------------------------------------
#
# `tracked()` above answers "what is committed", which is what goes public in the FILES. This
# answers the other half: what goes public in the LOG. They are different sets and the gate only
# ever read the first.

# A base to compare against, for a checkout that cannot work one out for itself. CI is the case:
# `actions/checkout` fetches one commit by default, so there is no upstream ref to subtract and
# no history to subtract it from. Set this to a ref or a sha and the scan covers exactly
# `<base>..HEAD`.
BASE_ENV = "SKEIN_RESIDUE_BASE"


def _git(*args):
    """stdout of one git command, or `None` if git could not answer."""
    import subprocess

    try:
        out = subprocess.run(
            ["git", "-C", ROOT, *args], capture_output=True, check=True, timeout=30
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout.decode("utf-8", "replace").strip()


def base_ref():
    """The ref this branch would be pushed onto, and how it was worked out.

    Returns `(ref, how)`, or `(None, why)` when no base can be found. The `how` is carried back
    out to be PRINTED: a scan whose coverage is not stated is a scan whose coverage nobody
    checks, and the coverage here is the difference between "every commit you are about to push"
    and "the one on top".
    """
    named = os.environ.get(BASE_ENV, "").strip()
    if named:
        if _git("rev-parse", "--verify", "--quiet", named + "^{commit}") is None:
            return None, f"${BASE_ENV} is set to {named!r}, which this checkout cannot resolve"
        return named, f"${BASE_ENV}"
    up = _git("rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}")
    if up:
        return up, "the branch's upstream"
    for guess in ("origin/HEAD", "origin/master", "origin/main"):
        if _git("rev-parse", "--verify", "--quiet", guess + "^{commit}"):
            return guess, f"no upstream is set, so {guess}"
    return None, (
        "this checkout has no upstream and no origin/* ref to subtract — a shallow clone, or a "
        "repository with no remote"
    )


def commits():
    """`(coverage, [(sha, text), …])` for the commits a push would add.

    The text is the message and the two identity lines, because an identity is the other half of
    what a commit publishes about a person and the cheaper half to get wrong: an agent committing
    under the wrong address writes it into history exactly as permanently as the message.

    When no base can be found this falls back to HEAD ALONE rather than to nothing. Nothing is the
    dangerous answer — it is indistinguishable from a clean run — and HEAD alone is a true partial
    that the coverage line then says out loud.
    """
    fmt = "%H%x00%B%n%an <%ae>%n%cn <%ce>%x01"
    ref, how = base_ref()
    if ref is None:
        out = _git("log", "-1", f"--format={fmt}")
        return (
            f"HEAD only — {how}. A push of more than one commit is NOT fully covered; "
            f"set ${BASE_ENV} to the ref you are pushing onto",
            _entries(out),
        )
    out = _git("log", f"--format={fmt}", f"{ref}..HEAD")
    if out is None:
        return f"nothing — git could not list {ref}..HEAD", []
    got = _entries(out)
    return f"{len(got)} commit(s) not yet on {ref} ({how})", got


def _entries(out):
    got = []
    for entry in (out or "").split("\x01"):
        sha, sep, text = entry.partition("\x00")
        if sep:
            got.append((sha.strip(), text))
    return got


def survey_messages(banned, entries=None):
    """`{rule: {what: ["commit <sha>:<n>", …]}}` over those commits.

    The same `scan_text` the files go through, so a rule cannot be sharp in one place and blunt in
    the other — the way for these to drift is for there to be two scanners, so there is one.
    """
    coverage, got = commits() if entries is None else ("a fixture", entries)
    found = {r: {} for r in RULES}
    for sha, text in got:
        for rule, hits in scan_text(text, banned).items():
            for what, lines in hits.items():
                found[rule].setdefault(what, []).extend(f"commit {sha[:8]}:{n}" for n in lines)
    return coverage, found


def message_problems(found, spec, banned):
    """The findings in `found` that are not declared, as sentences.

    Deliberately NOT a second copy of `problems`: the two differ in three ways and each is a
    decision. There is no staleness half, for the reason `[messages]`' own comment gives. A host
    or address allowed in the TREE is allowed here without being written twice — the tree's list
    is the stronger claim, since it says the code reaches the thing. And the denylist findings say
    something `problems` cannot, which is that the fix is not an edit: a message is not a file,
    and the only way to change one that is already written is to rewrite the commit.
    """
    said = []
    allowed = dict(spec.get("messages", {}))
    for rule in ("host", "home", "address"):
        tree = spec.get(SECTION[rule], {})
        for what in sorted(found[rule]):
            if what in allowed or what in tree:
                continue
            where = ", ".join(found[rule][what][:4])
            more = f" (+{len(found[rule][what]) - 4} more)" if len(found[rule][what]) > 4 else ""
            said.append(
                f"residue-check: undeclared {rule} {what!r} in a commit message or authorship "
                f"line\n"
                f"               at {where}{more}\n"
                f"               rule: a commit message goes public exactly as a file does, and "
                f"cannot be edited afterwards without rewriting history. Reword the commit while "
                f"it is still unpushed, or declare it in docs/residue.toml [messages] with whose "
                f"it is and why a commit here says it"
            )
    exempt = spec.get("exempt", {})
    for what in sorted(found["secret"]):
        if what in exempt:
            continue
        said.append(
            f"residue-check: something shaped like {what} in a commit message or authorship "
            f"line\n"
            f"               at {', '.join(found['secret'][what][:4])}\n"
            f"               rule: rotate it first — a credential in a message that has been "
            f"pushed is disclosed whatever happens to the commit afterwards"
        )
    unnamed = banned.register is None
    for what in sorted(found["banned"]):
        where = ", ".join(found["banned"][what][:4])
        more = f" (+{len(found['banned'][what]) - 4} more)" if len(found["banned"][what]) > 4 else ""
        said.append(
            f"residue-check: {banned.describe(what)} is in a commit message or authorship line\n"
            f"               at {where}{more}\n"
            + (f"               reason it is banned: {banned.reason(what)}\n" if not unnamed else "")
            + f"               rule: reword the commit now, while it is still unpushed — "
            f"`git commit --amend` for the tip, `git rebase -i` behind it. Once pushed there is "
            f"no fix short of the rewrite that removed this string from history the first time"
            + (f"\n{REGISTER_ABSENT}" if unnamed else "")
        )
    return said

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


# The five tables this gate reads. Anything else in the spec is not a list it consults — and
# `banned` is deliberately no longer among them, so a `[banned]` table written back into
# `docs/residue.toml` is reported as a table nothing reads rather than quietly enforcing from the
# one place this project decided its needles may not be written.
TABLES = ("hosts", "homes", "addresses", "messages", "exempt")


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
    # Copied through, never derived from `found`. The other three tables are rebuilt from what
    # the tree currently names, which is right for the tree and wrong here: what a commit message
    # names is whatever is unpushed at the moment `--update` runs, so deriving this table would
    # empty it on the first run after a push and delete decisions somebody made.
    out.append("# What a COMMIT MESSAGE or an authorship line may name, on top of everything the")
    out.append("# tables above allow. Not pruned when nothing matches, and not regenerated by")
    out.append("# `--update`: the set of commits this is checked against is the set not yet")
    out.append("# pushed, so it empties itself every time somebody pushes.")
    out.append("[messages]")
    for what, reason in sorted(old.get("messages", {}).items()):
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


# ---- the whole of history ---------------------------------------------------------------------
#
# Everything above reads the CURRENT tree (`git ls-files`) and the commits a push would add
# (`<base>..HEAD`). Neither can see a string that was committed once and edited out afterwards,
# and that is the set a public release actually turns on: `git clone` fetches every commit.
#
# WHY THIS IS A FLAG AND NOT A RULE. It is minutes of work over every reachable blob — 427 MB
# across 3,796 blobs on this repository's master alone — and the tree gate runs on every commit
# and in CI. So this is a mode somebody runs before a release, or before scoping a history
# rewrite, and nothing in `.github/workflows/ci.yml` calls it.
#
# WHY IT IS IN THIS FILE AT ALL, rather than the hand-written script it replaces. There have been
# three such scripts and they gave three different answers to the same question. The last one
# hashed raw bytes where `Banned.find` lowercases first, so it undercounted — 18 needles in
# ordinary files where the real figure is 26 — and the undercount was published on SKEIN-652 and
# SKEIN-605 as a measurement with a stated method. The second implementation of a rule is the one
# that is wrong, and nothing detects it, because both run and only one is ever checked against
# reality. So there is no matching code below. `scan_text` and `scan_name` are called exactly as
# `survey` and `survey_messages` call them — the same two calls, on a blob instead of a working
# file and on the whole log instead of `<base>..HEAD` — and what is new here is only which bytes
# they are handed.
#
# WHAT IT REPORTS, AND WHAT IT DOES NOT. The denylist rule only. The other four are ALLOW-list
# rules, and an allow-list describes what the tree may name today — `docs/residue.toml` cannot
# answer for a host some deleted file reached in 2024, and reporting one as undeclared would be
# reporting a decision nobody was ever asked to make. The denylist is the one rule whose claim is
# about all of time: these strings must never appear again, anywhere.


def _reachable(refs):
    """`[(sha, path), …]` for every object `git rev-list --objects` names, or `None`.

    Reachability is the whole point of using `rev-list` rather than `cat-file --batch-all-objects`:
    in a shared object database the latter also reports unreachable pre-rewrite leftovers, which
    is how SKEIN-605's first measurement badly overstated the problem.

    Only NAMED objects — the lines carrying a path — are returned, which drops the commits and
    keeps every tree and blob. A blob is always reached through a tree entry, so nothing with
    content is lost; the commits are covered by the log half instead, and counting them here as
    objects is what put an unexplained 8,133 next to a published 7,179. A root tree's name is the
    empty string and it is kept: it is a named object, and the separator is what says so.
    """
    out = _git("rev-list", "--objects", *refs)
    if out is None:
        return None
    pairs = []
    for line in out.split("\n"):
        sha, sep, path = line.partition(" ")
        if sep:
            pairs.append((sha, path))
    return pairs


def _blob_texts(pairs):
    """`(path, text)` for each of `pairs` that is a blob, through one `cat-file --batch`.

    NOT filtered by `is_text`, and that is the difference from `files()`. Two `__pycache__/*.pyc`
    files are committed here and each carries a needle inside a marshalled string constant
    (SKEIN-652); a binary filter would report history as cleaner than it is. Decoding with
    `replace` costs the matcher nothing — a needle is ASCII, and a byte that is not becomes a
    character outside `RUN`, which is where the run it was in would have ended anyway.
    """
    import subprocess

    proc = subprocess.Popen(
        ["git", "-C", ROOT, "cat-file", "--batch"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
    )
    try:
        for sha, path in pairs:
            proc.stdin.write((sha + "\n").encode("ascii"))
            proc.stdin.flush()
            header = proc.stdout.readline().split()
            if len(header) != 3:
                continue
            data = proc.stdout.read(int(header[2]))
            proc.stdout.read(1)
            if header[1] == b"blob":
                yield path, data.decode("utf-8", "replace")
    finally:
        proc.stdin.close()
        proc.stdout.close()
        proc.wait()


def path_label(path, banned):
    """What a report may call a committed path.

    Some of the paths this mode finds ARE needles — five of them on this repository's master, the
    pane fixtures SKEIN-540 renamed — so a path gets the same treatment its contents get: printed
    while it is clean, and reduced to a digest once it is not. `banned.describe` is deliberately
    not used: it spells the needle wherever the register is reachable, which is right for a gate
    telling somebody which line to edit and wrong for a sweep whose entire subject is strings that
    must not be written down again.
    """
    if not path:
        return "<a root tree>"
    if banned.find(path):
        return f"<a path with sha256 {digest(path).hex()[:16]}…>"
    return path


def history_parts(entries):
    """`[(sha, subject, body, identity), …]` from what `_entries` returns.

    The log format is `%B` and then the two identity lines, so an entry's last two lines are the
    author and the committer and its first is the subject. They are split apart because they
    answer different questions and cost different amounts to be wrong about: a needle in a SUBJECT
    is in every `git log --oneline` anybody ever runs, and one in an IDENTITY was never written by
    anybody — it is who the commit says it is by.
    """
    parts = []
    for sha, text in entries:
        lines = text.split("\n")
        identity = "\n".join(lines[-2:])
        message = lines[:-2]
        subject = message[0] if message else ""
        parts.append((sha, subject, "\n".join(message[1:]), identity))
    return parts


def history_findings(banned, paths, blobs, entries):
    """What a sweep over `paths`, `blobs` and `entries` found — counts, and where.

    All three are handed in rather than read from git, so that `self_check` can put this whole
    rule through a payload it built. `blobs` is `(path, text)` and is consumed lazily, because it
    is hundreds of megabytes when it comes from a real ref.

    Findings are keyed by HASH, never by string, so nothing this returns can name a needle.
    """
    found = {"content": {}, "names": {}, "per_path": {}}
    for path in paths:
        for h in scan_name(path, banned):
            found["names"].setdefault(h, set()).add(path)
    blob_count = 0
    for path, text in blobs:
        blob_count += 1
        for h in scan_text(text, banned).get("banned", {}):
            found["content"].setdefault(h, set()).add(path)
            found["per_path"].setdefault(path, set()).add(h)
    parts = history_parts(entries)
    # `scan_text` is what `survey_messages` calls on a commit, so this is the tree gate's own
    # message rule over the whole log instead of over `<base>..HEAD`. Counted as a set of COMMITS
    # rather than of findings, which is what makes "12 of 954" mean what it says.
    for slot, index in (("subject", 1), ("body", 2), ("identity", 3)):
        found[slot] = {p[0] for p in parts if scan_text(p[index], banned).get("banned")}
    found["objects"] = len(paths)
    found["blobs"] = blob_count
    found["commits"] = len(parts)
    return found


def history_report(banned, found, coverage, register):
    """The sweep's result, as lines. Digest prefixes and clean paths, and nothing else.

    `register` is the sha256 of `docs/residue-banned.txt`, passed in rather than read here so that
    a missing hash file stays `main`'s sentence to say and not a traceback out of `self_check`.

    Every count is stated even when it is zero, the register's above all. A sweep is only ever
    valid for the register it used — this one grew from about fourteen needles to forty-three
    while SKEIN-605's measurement sat on the tracker reading as a claim about today — and nothing
    had ever recorded which register a given sweep meant.
    """
    old = os.path.relpath(SPEC, ROOT)
    ordinary = {h for h, seen in found["content"].items() if any(p != old for p in seen)}
    said = [
        "residue-check: --history — a sweep of everything reachable from these refs. NOT a gate:",
        "               nothing it finds can be fixed by an edit, and CI does not run it.",
        f"               refs      {coverage}",
        f"               register  {os.path.relpath(HASHES, ROOT)} sha256 {register}",
        f"                         {len(banned)} needle(s), widths "
        f"{' '.join(str(w) for w in banned.widths)}",
        f"               reached   {found['objects']} named object(s), {found['blobs']} blob(s), "
        f"{found['commits']} commit(s)",
        "",
        "blob CONTENT",
        f"  {len(found['content'])} of {len(banned)} register needle(s) appear in a blob",
        f"  {len(ordinary)} of those in an ordinary file (a path other than {old})",
        f"  {len(found['content']) - len(ordinary)} of those only ever in {old}",
    ]
    ranked = sorted(
        ((p, hs) for p, hs in found["per_path"].items() if p != old),
        key=lambda kv: (-len(kv[1]), kv[0]),
    )
    if ranked:
        said.append("  ordinary files, by how many distinct needles each carries:")
        said += [f"    {len(hs):3d}  {path_label(p, banned)}" for p, hs in ranked]
    said += [
        "",
        "committed FILENAMES — a rename fixes the tree and not the history, so the name a file",
        "was committed under is still in every commit that carried it",
        f"  {len(found['names'])} distinct needle(s) across "
        f"{len({p for ps in found['names'].values() for p in ps})} path(s)",
    ]
    said += [
        f"    sha256 {h.hex()[:16]}…  in {len(ps)} path(s)"
        for h, ps in sorted(found["names"].items())
    ]
    said += [
        "",
        f"commit MESSAGES AND IDENTITIES — {found['commits']} commit(s)",
        f"  {len(found['body'])} carry a needle in the message body",
        f"  {len(found['subject'])} in the subject line",
        f"  {len(found['identity'])} in the author or committer identity",
    ]
    return said


def history(banned, refs):
    """`--history`: the sweep, and the exit code a release would read.

    Refusing an unresolvable ref rather than skipping it is the point of the first check. A sweep
    that covered nothing and a sweep that found nothing print the same number of findings, and
    this whole mode exists because measurements of this history have been believed and wrong.
    """
    if not banned.hashes or not banned.widths:
        print(
            "residue-check: --history has no denylist to sweep for — docs/residue-banned.txt is\n"
            "               missing, empty, or declares no width to search at\n"
            "               rule: a sweep that looked for nothing reports the same clean history "
            "as one that looked properly. Regenerate the file with `--update`"
        )
        return 1
    refs = refs or ["HEAD"]
    for ref in refs:
        if _git("rev-parse", "--verify", "--quiet", ref) is None:
            print(
                f"residue-check: --history cannot resolve {ref!r} in this checkout\n"
                f"               rule: a ref that does not resolve would be swept as nothing and "
                f"reported as clean. Name a ref this clone has — `git rev-list --objects` is what "
                f"reads it"
            )
            return 1
    pairs = _reachable(refs)
    if pairs is None:
        print("residue-check: --history could not list the objects reachable from those refs")
        return 1
    fmt = "%H%x00%B%n%an <%ae>%n%cn <%ce>%x01"
    # `_entries` turns a `None` here into an empty list, which the report would then state as
    # "0 commit(s)" — the shape of a clean answer. A log this checkout could not produce is not a
    # log with nothing in it.
    log = _git("log", f"--format={fmt}", *refs)
    if log is None:
        print(
            f"residue-check: --history could not read the log of {', '.join(refs)}\n"
            f"               rule: a log that did not come back reports as a log with no commits "
            f"in it, which is what a clean history looks like. Nothing below is measured"
        )
        return 1
    found = history_findings(banned, [p for _, p in pairs], _blob_texts(pairs), _entries(log))
    register = digest(open(HASHES, "rb").read()).hex()
    for line in history_report(banned, found, ", ".join(refs), register):
        print(line)
    dirty = found["content"] or found["names"] or found["body"] or found["subject"]
    return 1 if dirty or found["identity"] else 0


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
        # A host with no URL around it, in the place SKEIN-542 found one: filled into a form field
        # by a browser test. Built by concatenation like the rest, so this file still spells no
        # host of its own — and the fragments cannot fire the rule either, because each is its own
        # quoted string and none of them is a whole name.
        "bare": 'await page.fill(field, "' + "gate" + "way." + "acme" + "corp" + '.dev")',
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
    # ---- the bare hostname, from both sides ----------------------------------------------------
    #
    # The rule has no URL to tell it a host is a host, so it is the one most able to go quiet
    # without anything else here noticing: `scan_text` would keep reporting every URL it was ever
    # shown, and `--show` would keep printing them. Hence a needle of its own.
    if not scan_text(n["bare"], none).get("host"):
        fail(
            "residue-check: SELF-CHECK FAILED — a hostname written as a bare quoted string was",
            "               not reported. That is the form an endpoint takes in a test — a form",
            "               field, a config value, an assertion — and it is how a live `.dev`",
            "               name sat in this tree through every green run of this gate.",
        )
    # And the other side, which is the harder half and the reason this rule is shaped the way it
    # is. These three are real lines in this repository: `MetadataExt::dev` called twice in
    # src/fleet.rs, and a citation in docs/UX-AUDIT.md. A `\S+\.\w+` matcher reports all three.
    # The quoted four are the other trap — ordinary strings whose last label is somebody's
    # top-level domain, which is what `HOST_TLDS` exists to keep out.
    for quiet in (
        "meta.dev() and m.dev() are MetadataExt::dev, and a doc cites performance.dev",
        'the strings "user.email", "repo.store", "memory.events" and "box-session.sh" are a '
        "config key, a field, a cgroup file and a script",
    ):
        said = scan_text(quiet, none).get("host")
        if said:
            fail(
                "residue-check: SELF-CHECK FAILED — the bare-host rule reported "
                f"{sorted(said)},",
                "               which is code and not a hostname. Every one of these is a real",
                "               line in this tree, and a gate that fails the build over an",
                "               attribute access is a gate somebody deletes the call to.",
            )
    # RFC 2606 reserves those three names and everything under them, so a fixture host below one
    # belongs to nobody and is not a decision to write down. This is the assertion that pins
    # `bare_reserved` to the registrable domain rather than to the name as written.
    if scan_text('await page.fill(field, "mcp.example.net")', none).get("host"):
        fail(
            "residue-check: SELF-CHECK FAILED — a name under an RFC 2606 reserved domain was",
            "               reported as somebody's host. Nobody can hold one, so every fixture",
            "               that uses one would have to be declared, which is how an allow-list",
            "               fills up with entries that mean nothing.",
        )
    # ---- the denylist, on a hash of a SYNTHETIC needle -----------------------------------------
    #
    # Never a real one: a real needle here would be the disclosure the register was moved to
    # prevent, in the one file that must not spell what it looks for. The canary is built by
    # concatenation for the same reason the shape needles above are.
    # ---- the commit-message door ---------------------------------------------------------------
    #
    # `_entries` is the piece of this whole rule that can fail SILENTLY. Everything else reports
    # what it found; a format string or a separator that drifts makes `_entries` return nothing,
    # `survey_messages` find nothing, and the run go green having read no commit at all. So the
    # parse is exercised on a payload built here, and the scan is exercised through it.
    who = "A Name <" + n["address"].split()[-2] + ">"
    payload = (
        "1111111111111111111111111111111111111111\x00first message\n"
        + n["host"]
        + f"\n{who}\n{who}\x01"
        "2222222222222222222222222222222222222222\x00second, with nothing in it\x01"
    )
    got = _entries(payload)
    if len(got) != 2 or got[0][0][:4] != "1111":
        fail(
            "residue-check: SELF-CHECK FAILED — the commit-message parse returned "
            f"{len(got)} entr(ies) for a payload holding two.",
            "               Nothing downstream of it reports an empty list, so the message rule "
            "would be silent.",
        )
    _, seen = survey_messages(none, got)
    if not seen["host"] or not seen["address"]:
        fail(
            "residue-check: SELF-CHECK FAILED — a commit message carrying a host and an address",
            "               was scanned and neither rule fired. The message door is open.",
        )
    if any(loc.startswith("commit 22222222") for locs in seen["host"].values() for loc in locs):
        fail("residue-check: SELF-CHECK FAILED — a finding was attributed to the wrong commit.")

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
    # ---- the history door ----------------------------------------------------------------------
    #
    # `--history` is a mode nobody runs often, over data nothing else in this file touches, and it
    # exists because three hand-written sweeps gave three different answers. So the parts of it
    # that are not the shared matcher — which is to say the parts that COULD be wrong on their own
    # — are exercised here, on a payload built the same way every other needle in this function is.
    #
    # Two of them are silent failures. The three-way split of a log entry decides whether a needle
    # is reported as a subject, a body or an authorship line, and getting it wrong moves a finding
    # rather than losing it, so no count would look odd. And `path_label` is the only thing
    # standing between a sweep of strings that must never be written down and a terminal with five
    # of them printed on it.
    other = "Other" + "Canary"
    two = Banned(
        [len(canary), len(other)], [digest(canary.lower()), digest(other.lower())]
    )
    poisoned = f"tests/fixtures/panes/claude-idle.{canary.lower()}.2026-01-01.txt"
    old = os.path.relpath(SPEC, ROOT)
    log = (
        "1111111111111111111111111111111111111111\x00a clean subject\n\nbody naming "
        + canary.lower()
        + "\n\nA Name <a@b.example>\nA Name <a@b.example>\x01"
        "2222222222222222222222222222222222222222\x00another clean subject\n\nclean body\n\n"
        + canary.lower()
        + " <c@d.example>\n"
        + canary.lower()
        + " <c@d.example>\x01"
    )
    seen = history_findings(
        two,
        [old, "src/thing.rs", "clean/path.txt", poisoned],
        [
            # The old in-repo register carried every needle, which is why "in an ordinary file" is
            # the figure the rewrite is scoped against and "in a blob" is not.
            (old, f"the register held {canary.lower()} and {other.lower()}"),
            ("src/thing.rs", f"a line naming {canary.lower()}"),
            ("clean/path.txt", "nothing in here at all"),
            (poisoned, f"a fixture body naming {canary.lower()}"),
        ],
        _entries(log),
    )
    if (
        seen["body"] != {"1111111111111111111111111111111111111111"}
        or seen["identity"] != {"2222222222222222222222222222222222222222"}
        or seen["subject"]
    ):
        fail(
            "residue-check: SELF-CHECK FAILED — a log entry's subject, body and authorship lines",
            "               were not told apart. A needle in an identity reported as a message,",
            "               or the other way round, is a finding filed against the wrong thing —",
            "               and every count still adds up, so nothing else here would notice.",
        )
    ordinary = {h for h, at in seen["content"].items() if any(p != old for p in at)}
    if len(seen["content"]) != 2 or len(ordinary) != 1 or seen["blobs"] != 4:
        fail(
            "residue-check: SELF-CHECK FAILED — the history sweep miscounted a payload holding",
            "               two needles in four blobs, one of them only in the old register.",
            "               'In an ordinary file' is the figure a rewrite is scoped against.",
        )
    if len(seen["names"]) != 1 or {p for ps in seen["names"].values() for p in ps} != {poisoned}:
        fail(
            "residue-check: SELF-CHECK FAILED — the history sweep did not find a needle in a",
            "               committed PATH. Renaming fixes the tree and not the history, so this",
            "               is the half that a tree scan can never report.",
        )
    shown = "\n".join(history_report(two, seen, "a fixture", "0" * 64)).lower()
    for spelling in (canary.lower(), other.lower()):
        if spelling in shown:
            fail(
                "residue-check: SELF-CHECK FAILED — the history report spelled a needle. Some of",
                "               the paths this mode finds ARE needles, so a path is printed only",
                "               while it is clean; printing one is the disclosure the register",
                "               was moved out of this repository to prevent.",
            )
    if "0" * 64 not in shown:
        fail(
            "residue-check: SELF-CHECK FAILED — the history report did not name the register it",
            "               ran against. A sweep is only valid for the register it used, and one",
            "               that does not say which reads later as a claim about today.",
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
    # Before the tree survey, because `--history` needs none of it and the survey is the slow part
    # of an ordinary run. Before the spec checks too: `docs/residue.toml` is an allow-list for the
    # tree as it stands, and this mode asks a question about history that no allow-list answers.
    if "--history" in sys.argv:
        return history(banned, sys.argv[sys.argv.index("--history") + 1 :])
    found = survey(banned)
    coverage, in_messages = survey_messages(banned)

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

    said = problems(found, spec, banned) + message_problems(in_messages, spec, banned)
    for p in said:
        print(p)
    # Said on every run, green or red, and this is the point of it: the message scan covers a
    # RANGE, and a range can be empty for a reason nobody noticed — a shallow clone, a branch with
    # no upstream, a base that does not resolve. A gate that scanned nothing and a gate that found
    # nothing print the same thing unless one of them says which it was.
    print(f"residue-check: commit messages scanned: {coverage}")
    return 1 if said else 0


if __name__ == "__main__":
    sys.exit(main())
