#!/usr/bin/env python3
"""The public `CLAUDE.md` is tracker-neutral, `AGENTS.md` is the same file, and `sync` leaves it be;
and the skills the repository ships are tracker-neutral too.

`CLAUDE.md` is the one document every agent in a checkout of this repository reads before anything
else, and since the repository went public it is also read by strangers. It used to carry the
owner's work-tracker ids, the tracker's project id and the names of the owner's machines; the
scrub (SKEIN-1051) took them out and kept the lessons. Nothing stopped them walking back in, and
the one rule that kept the scrub working at all — the heading below — was held by nothing but a
comment in a shell script (SKEIN-1158). So, five rules:

  tracker    No work-tracker id: nothing shaped like a run of capitals, a hyphen and a number.
             A SHAPE, not this tracker's prefix, for the reason `tools/residue-check.py` gives for
             its own rules: the list of today's names catches the mistake nobody makes twice. The
             cost is that a standard's name of the same shape is refused too; none is in the file,
             and one that is needed can be spelled without the hyphen.
  project    No tracker project id: nothing shaped like a UUID.
  banned     No string on the residue register — which is where the names of the owner's boxes
             are. The matcher is `tools/residue-check.py`'s own, imported, so this holds no list;
             `residue-check` would also refuse such a string anywhere in the tree, and this says
             so about the one file where it matters most, in its own words.
  fleet      No path under the fleet root: the default `util::fleet_root` falls back to, read out
             of `src/util.rs` rather than written here. A box name is caught by `banned`, but a
             path like the fleet root's toolchain directory names no box and matched nothing, and
             it is exactly what had to be scrubbed out of the change-discipline skill by hand
             (SKEIN-1164). It is an instruction that only works on the owner's fleet, handed to
             every agent in every checkout. The rule is here and not in `residue-check`, which was
             the other choice (SKEIN-1166): the code, its tests and the docs about the fleet name
             that root on purpose, in 88 tracked files when it was decided (`git grep -l` on the
             root), so a tree-wide rule would be an allow-list of nearly every file that mentions
             it. It is only in the documents an agent obeys that the path is wrong. Say
             `$SKEIN_FLEET_ROOT`, or "the main checkout", instead.
  sync       `src/store/sync-install.sh` leaves a checkout's `CLAUDE.md` byte-identical and
             records no `block` in its manifest. The script appends the owner's tracker section to
             any `CLAUDE.md` or `AGENTS.md` that lacks a `## Work tracking` heading, on every box
             that has `sync` — so renaming that heading in this file would put a tracker block into
             a tracked file on the next start of every such box, and show up only as somebody's
             unexplained diff. This runs the script's own doc step, cut out of the script rather
             than rewritten here, over a copy of this file with `AGENTS.md` a symlink beside it.

The first four rules — tracker, project, banned, fleet — apply as well to every TRACKED Markdown
file under `.claude/skills/`, with the same matchers. A shipped skill is read by every agent in a
checkout, as `CLAUDE.md` is, and the change-discipline skill had to be scrubbed the same way after
it was moved into the repository. The set is read from `git ls-files`, not from a list here, so a
new skill is checked from the commit that adds it; and a set that comes back empty refuses rather
than passing, because a rename of the directory would otherwise turn this half into a silent green.

Plus the shape of `AGENTS.md`: tracked as a symlink (mode 120000) whose target is `CLAUDE.md`, so
Codex, which reads `AGENTS.md`, reads the same words. A copy would drift.

The sync rule is checked against itself before it is trusted: the same step is run over a copy with
the heading renamed, and it must append there. A step that does not append when it should is a
harness that is not running the script, and a green from it would mean nothing — so that refuses
(exit 2) rather than passing. It also refuses when the step cannot be found in the script, when
the residue matcher has no hashes to match with, or when the fleet root's default cannot be read.
`$SKEIN_HOME` and `$SKEIN_FLEET_ROOT` are pinned to the scratch directory for the step, as every
test here pins them.

Exit 0 clean, 1 on a finding, 2 when it could not check.
"""

import importlib.util
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOC = "CLAUDE.md"
TWIN = "AGENTS.md"
HEADING = "## Work tracking"
SYNC = os.path.join("src", "store", "sync-install.sh")
BLOCK_DIR = os.path.join("src", "store", "sync")
UTIL = os.path.join("src", "util.rs")

SKILLS = os.path.join(".claude", "skills")

TRACKER_ID = re.compile(r"\b[A-Z][A-Z0-9]*-[0-9]+\b")
UUID = re.compile(r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b")

# Where the doc step starts and ends in the script: the manifest it records into, then the loop over
# the two documents, ending at that loop's own `done`.
STEP_START = 'manifest="$state/sync-$slug.manifest"'
LOOP_HEAD = "for doc in CLAUDE.md AGENTS.md; do"


class Refused(Exception):
    pass


def residue_matcher():
    path = os.path.join(ROOT, "tools", "residue-check.py")
    spec = importlib.util.spec_from_file_location("residue_check", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    listed = mod.load_hashes()
    if not listed or not listed[1]:
        raise Refused("docs/residue-banned.txt gave residue-check's matcher no hashes, so the "
                      "banned rule would match nothing")
    return mod.Banned(listed[0], listed[1])


def fleet_matcher():
    """A pattern for the fleet root's default and any path under it, the default read out of the
    string literal `util::fleet_root` returns when `$SKEIN_FLEET_ROOT` is unset."""
    try:
        with open(os.path.join(ROOT, UTIL), encoding="utf-8") as fh:
            source = fh.read()
    except OSError as exc:
        raise Refused(f"could not read {UTIL} for the fleet root's default: {exc}")
    body = re.search(r"^pub fn fleet_root\(\) -> String \{\n(.*?)^\}", source, re.M | re.S)
    found = re.findall(r'^\s*"(/[^"]+)"\.to_string\(\)\s*$', body.group(1), re.M) if body else []
    if len(found) != 1:
        raise Refused(f"{UTIL}'s `fleet_root` no longer ends in one `\"/...\".to_string()` default "
                      f"(found {len(found)}), so the fleet rule would match nothing")
    return re.compile(r"(?<![\w./-])" + re.escape(found[0])
                      + r"(?![\w-])(?!\.\w)(?:/[^\s`'\")\]]*)?")


def text_findings(text, banned, fleet):
    """(line, rule, what) for every tracker id, project id, banned string and fleet path."""
    out = []
    for n, line in enumerate(text.split("\n"), 1):
        for m in TRACKER_ID.finditer(line):
            out.append((n, "tracker", f"`{m.group()}` is shaped like a work-tracker id"))
        for m in UUID.finditer(line):
            out.append((n, "project", f"`{m.group()}` is shaped like a tracker project id"))
        for h in banned.find(line):
            out.append((n, "banned", f"{banned.describe(h)} is on the residue register"))
        for m in fleet.finditer(line):
            path = m.group().rstrip(".,;:")
            out.append((n, "fleet", f"`{path}` is a path on the owner's fleet, not in a checkout"))
    return out


def skill_docs():
    """Every tracked Markdown file under `.claude/skills/`, as a path relative to the root."""
    try:
        listed = subprocess.run(["git", "-C", ROOT, "ls-files", "-z", "--", SKILLS],
                                capture_output=True, text=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError) as exc:
        raise Refused(f"git could not list {SKILLS}: {exc}")
    docs = sorted(p for p in listed.split("\0") if p.endswith(".md"))
    if not docs:
        raise Refused(f"git lists no Markdown file under {SKILLS}, so the skills would be checked "
                      f"against nothing; if the directory moved, point `SKILLS` in this script at it")
    return docs


def twin_findings():
    """What is wrong with `AGENTS.md`, from the index and from the disk."""
    out = []
    try:
        staged = subprocess.run(["git", "-C", ROOT, "ls-files", "-s", "--", TWIN],
                                capture_output=True, text=True, check=True).stdout.split()
    except (OSError, subprocess.CalledProcessError) as exc:
        raise Refused(f"git could not list {TWIN}: {exc}")
    if not staged:
        out.append(f"{TWIN} is not tracked; it is a symlink to {DOC}, so Codex reads the same file")
    elif staged[0] != "120000":
        out.append(f"{TWIN} is tracked with mode {staged[0]}, not as a symlink (120000) to {DOC}")
    else:
        target = subprocess.run(["git", "-C", ROOT, "cat-file", "-p", staged[1]],
                                capture_output=True, text=True).stdout
        if target != DOC:
            out.append(f"{TWIN} is tracked as a symlink to {target!r}, not to {DOC}")
    path = os.path.join(ROOT, TWIN)
    if not os.path.islink(path) or os.readlink(path) != DOC:
        out.append(f"{TWIN} in this checkout is not a symlink to {DOC}")
    return out


def doc_step(script):
    """The script's doc step, as bash: from the manifest line through the loop's `done`."""
    lines = script.split("\n")
    try:
        start = lines.index(STEP_START)
        head = lines.index(LOOP_HEAD, start)
        end = lines.index("done", head)
    except ValueError:
        raise Refused(f"{SYNC} no longer has its doc step in the shape this reads: a line "
                      f"`{STEP_START}`, then `{LOOP_HEAD}`, then that loop's `done`")
    return "\n".join(lines[start:end + 1])


def run_step(step, doc_bytes):
    """Run the doc step over a scratch checkout holding `doc_bytes` as CLAUDE.md, with AGENTS.md a
    symlink beside it. Returns (CLAUDE.md afterwards, the manifest's text)."""
    with tempfile.TemporaryDirectory(prefix="claude-md-check-") as tmp:
        project = os.path.join(tmp, "checkout")
        state = os.path.join(tmp, "state")
        os.makedirs(project)
        with open(os.path.join(project, DOC), "wb") as fh:
            fh.write(doc_bytes)
        os.symlink(DOC, os.path.join(project, TWIN))
        prelude = "\n".join([
            "set -uo pipefail",
            f"project='{project}'",
            f"src='{os.path.join(ROOT, BLOCK_DIR)}'",
            f"state='{state}'",
            "slug=claude-md-check",
        ])
        env = dict(os.environ, HOME=tmp, SKEIN_HOME=os.path.join(tmp, "home"),
                   SKEIN_FLEET_ROOT=os.path.join(tmp, "fleet"))
        subprocess.run(["bash", "-c", prelude + "\n" + step], env=env, cwd=tmp,
                       capture_output=True, timeout=30)
        with open(os.path.join(project, DOC), "rb") as fh:
            after = fh.read()
        manifest = os.path.join(state, "sync-claude-md-check.manifest")
        recorded = open(manifest, encoding="utf-8").read() if os.path.exists(manifest) else ""
    return after, recorded


def sync_findings(doc_bytes):
    with open(os.path.join(ROOT, SYNC), encoding="utf-8") as fh:
        step = doc_step(fh.read())
    # The control: without the heading the step must append and record, or it is not running.
    renamed = doc_bytes.replace(HEADING.encode(), b"## Tracking work")
    after, recorded = run_step(step, renamed)
    if after == renamed or not re.search(r"^block\t", recorded, re.M):
        raise Refused(f"{SYNC}'s doc step, run over a {DOC} with no `{HEADING}` heading, did not "
                      f"append the tracker section and record it — so a clean result from the "
                      f"same run over the real file would prove nothing")
    after, recorded = run_step(step, doc_bytes)
    out = []
    if after != doc_bytes:
        out.append(f"{SYNC}'s doc step changed {DOC}: {len(after) - len(doc_bytes)} byte(s) "
                   f"appended. It skips a file carrying a `{HEADING}` heading, so that heading "
                   f"must stay, spelt exactly so")
    if re.search(r"^block\t", recorded, re.M):
        out.append(f"{SYNC}'s doc step recorded a `block` in its manifest, claiming a section "
                   f"of {DOC} as its own")
    return out


def main():
    try:
        path = os.path.join(ROOT, DOC)
        with open(path, "rb") as fh:
            doc_bytes = fh.read()
        text = doc_bytes.decode("utf-8")
        banned = residue_matcher()
        fleet = fleet_matcher()
        found = [f"{DOC}:{n}: {what} ({rule})"
                 for n, rule, what in text_findings(text, banned, fleet)]
        skills = skill_docs()
        for rel in skills:
            with open(os.path.join(ROOT, rel), encoding="utf-8") as fh:
                found += [f"{rel}:{n}: {what} ({rule})" for n, rule, what in
                          text_findings(fh.read(), banned, fleet)]
        found += twin_findings()
        found += sync_findings(doc_bytes)
    except Refused as exc:
        print(f"claude-md-check: could not check, so this is not a pass: {exc}", file=sys.stderr)
        return 2
    except (OSError, UnicodeDecodeError, subprocess.SubprocessError) as exc:
        print(f"claude-md-check: could not check, so this is not a pass: {exc}", file=sys.stderr)
        return 2
    for line in found:
        print(line)
    if found:
        print(f"claude-md-check: {len(found)} finding(s)", file=sys.stderr)
        return 1
    print(f"claude-md-check: {DOC} and the {len(skills)} tracked skill file(s) under {SKILLS} name "
          f"no tracker id, project id, banned string or fleet path, {TWIN} is a symlink to "
          f"{DOC}, and sync's doc step leaves it byte-identical")
    return 0


if __name__ == "__main__":
    sys.exit(main())
