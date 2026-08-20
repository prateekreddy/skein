#!/usr/bin/env python3
"""The real module graph, resolved through `lib.rs`'s glob re-exports.

Why this exists: `src/lib.rs` re-exports sixteen modules with `pub use <mod>::*`, so almost every
cross-module reference goes through a flat root namespace. Grepping for `crate::signals::` from
other modules returns **zero** — not because nothing uses `signals`, but because everything reaches
it by a bare name. The declared `mod` graph therefore carries no information, and none of
`docs/architecture.md` §14's dependency rules can be checked.

So this resolves the other way round: collect each module's exported names, then count which other
modules mention them. Approximate by construction — a bare name that happens to collide with a local
one is counted, and a name used only in a comment is counted — but it is *reproducible*, and it is
the baseline the façade removal is verified against rather than believed.

Usage:  python3 tools/module-edges.py [--check baseline.tsv]
"""
import os, re, sys, collections

SRC = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src")
# `re.M` is load-bearing: without it `^` anchors to the start of the whole file, the regex matches at
# most once, and every edge comes from the qualified-path fallback — i.e. the tool reproduces the
# exact blind spot it exists to remove.
EXPORT = re.compile(
    r"^\s*pub(?:\(crate\))?\s+(?:fn|struct|enum|trait|const|static|type)\s+([A-Za-z_][A-Za-z0-9_]*)",
    re.M,
)
QUALIFIED = re.compile(r"crate::([a-z_]+)::")
# Names too generic to attribute — counting them would drown the signal in noise.
IGNORE = {"new", "default", "from", "get", "set", "run", "name", "path", "id", "len", "main"}

# A bare name belongs to a provider only if the CONSUMER does not itself bind or define it. Without
# this, `gitgate` reads as depending on `apiauth` 164 times because both use the word `token` — and
# gitgate's are its own GitHub tokens. Hand-extending IGNORE would never end; this asks the right
# question instead, and it is why the count is a lower bound rather than an estimate.
LOCAL = r"(?:^\s*(?:pub(?:\(crate\))?\s+)?(?:fn|struct|enum|trait|const|static|type)\s+%s\b|\blet\s+(?:mut\s+)?%s\b|\b%s\s*:)"


def declares(text, sym):
    esc = re.escape(sym)
    return re.search(LOCAL % (esc, esc, esc), text, re.M) is not None


def modules():
    """The modules that can PROVIDE — one file each, `lib.rs` excluded (it is the façade)."""
    for entry in sorted(os.listdir(SRC)):
        if entry.endswith(".rs") and entry != "lib.rs":
            yield entry[:-3], os.path.join(SRC, entry)


def consumers():
    """Everything that can CONSUME — the modules, plus `lib.rs` and the binaries.

    Leaving these out was the tool's own version of the bug it exists to find: `signals`, `sandbox`,
    `files`, `diff` and `transcript` resolved to zero consumers, because theirs live in `lib.rs` and
    `src/bin/` rather than in a sibling module."""
    for name, path in modules():
        yield name, path
    lib = os.path.join(SRC, "lib.rs")
    if os.path.exists(lib):
        yield "lib", lib
    bins = os.path.join(SRC, "bin")
    if os.path.isdir(bins):
        for entry in sorted(os.listdir(bins)):
            if entry.endswith(".rs"):
                yield "bin/" + entry[:-3], os.path.join(bins, entry)


def body(path):
    """Source with the test module stripped — a fixture is not a caller."""
    text = open(path, encoding="utf-8", errors="replace").read()
    cut = text.find("\nmod tests {")
    return text if cut < 0 else text[:cut]


def main():
    src = {name: body(path) for name, path in consumers()}

    exports = {}
    for name, path in modules():
        text = src[name]
        found = {m.group(1) for m in EXPORT.finditer(text)} - IGNORE
        exports[name] = {n for n in found if len(n) > 3}

    edges = collections.Counter()
    for consumer, text in src.items():
        for provider, names in exports.items():
            if provider == consumer:
                continue
            n = sum(
                len(re.findall(r"\b%s\b" % re.escape(sym), text))
                for sym in names
                if not declares(text, sym)
            )
            n += len(re.findall(r"crate::%s::" % re.escape(provider), text))
            if n:
                edges[(consumer, provider)] = n

    rows = sorted(edges.items(), key=lambda kv: (-kv[1], kv[0]))
    out = ["%s\t%s\t%d" % (c, p, n) for (c, p), n in rows]

    if len(sys.argv) > 2 and sys.argv[1] == "--check":
        want = [l.rstrip("\n") for l in open(sys.argv[2]) if l.strip() and not l.startswith("#")]
        if want != out:
            missing = set(want) - set(out)
            added = set(out) - set(want)
            for line in sorted(added):
                print("NEW EDGE   %s" % line)
            for line in sorted(missing):
                print("GONE       %s" % line)
            print("\n%d edges now, %d in the baseline." % (len(out), len(want)))
            return 1
        print("module graph matches the baseline (%d edges)" % len(out))
        return 0

    print("# consumer\tprovider\treferences")
    print("# Regenerate: python3 tools/module-edges.py > tools/module-edges.tsv")
    for line in out:
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main())
