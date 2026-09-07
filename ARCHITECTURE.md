# skein — architecture

**This page is a signpost. It contains no architecture.** The design is written down in four
documents under `docs/`, and this page's only job is to send you to the right one.

## The four documents

They are meant to be read together, and in this order if you are new:

| document | what it is |
|---|---|
| [`docs/inventory.md`](docs/inventory.md) | **what skein actually does**, derived from the code — start here if you want the truth about today |
| [`docs/architecture.md`](docs/architecture.md) | **the destination design** — five primitives, and every feature written as a composition of them |
| [`docs/parity.md`](docs/parity.md) | **the acceptance gate** — what the rewrite must still do when it is finished |
| [`docs/delivery.md`](docs/delivery.md) | **the sequence** — order, migration, and the landmines |

Two more sit beside them: [`docs/live-check.md`](docs/live-check.md) is the residue that no test on a
developer machine can answer because it needs a real sandbox, and [`README.md`](README.md) is how you
*use* skein rather than how it is built.

If you are picking up the **in-fleet install**, the open items are in the work tracker, and
[`CLAUDE.md`](CLAUDE.md) says which project and how to reach it. They used to be in a
`docs/TODO.md`, and before that in a `docs/in-fleet-handoff.md` that was folded into it; both are
gone from this repository. The second of those was a dead link on this page for weeks, which is the
failure the next paragraph is about.

**Neither is on the `ls` line below, and that is the rule rather than an oversight** — see the
paragraph after it. A working document is also the wrong place for open items to live: it is a
workbench, it is written for one reader, and it accumulates the kind of detail about a live fleet
that a repository this one is meant to be readable by strangers cannot carry (SKEIN-631). The
tracker is the record.

## Is this page still true?

This is the question the old version of this page could not answer, so it is answered here in the
only way that survives: **every claim on this page is a claim that a named file exists, and one
command checks all of them.**

```sh
ls docs/inventory.md docs/architecture.md docs/parity.md docs/delivery.md docs/live-check.md \
   docs/modules.toml docs/sources.toml README.md CLAUDE.md \
   tools/module-check.py tools/source-check.py tests/parity_numbers.rs
```

If that command prints twelve paths, this page is true. If it fails, this page is stale in the only
way a signpost *can* be stale — something moved and the sign was not repainted — and the failure
names the file. There is no third state, because there is nothing else on this page to be wrong
about.

**The list is every path this page names, and that is load-bearing.** It was six, and it left out
the handoff document linked in the section above — so the check passed for weeks while the page
carried a dead link, which is precisely the failure it exists to catch. A falsifier that covers most
of a page reports "true" about the part it does not cover. **If you add a path to this page, add it
here in the same edit**; a link that is not on this line is not checked by anything.

It missed one again, and the same way. `CLAUDE.md` is named in the last section — a live file, and
a live claim, since a rename would leave that sentence pointing nowhere — and it was not on the
line; it is now. **The one exception, and it has to be stated or the next edit will add the wrong
thing**: a path named in the PAST TENSE, as gone, must stay off. `docs/in-fleet-handoff.md` and
`docs/TODO.md` are the two, and putting either on the line would make the falsifier fail on a
sentence whose whole content is that the file is not there. So: every path this page names as
EXISTING.

The four documents are held to a much harder standard than a signpost needs, by gates that run
themselves:

```sh
python3 tools/module-check.py     # the module graph against docs/modules.toml
python3 tools/source-check.py     # every cross-unit reach against docs/sources.toml
cargo test --test parity_numbers  # docs/parity.md's own counts, re-derived from the code
```

Each prints the counts it measured. **Those counts are deliberately not repeated on this page** — a
number copied to a second place is a second place to update, and the copy is what goes stale first.
Run the command; it will tell you the number it means today.

## Why a signpost, and not a summary

Because the summary was tried, and this is the file it produced.

Until now this page described a system that had stopped existing: a ratatui terminal UI, a Svelte
diff viewer, a honker job queue on the host, and one microVM kernel per box. Every one of those was a
reasonable statement when it was written and a false one by the time it was read. The damage was not
that the file was wrong — files go wrong — it is that the file was **first**. It is what the root
directory offers before anything else, so it was read before the documents that were correct, and
`CLAUDE.md` ended up carrying a standing warning not to believe the repository's own architecture
document.

A summary of code drifts because the code moves and the prose does not. This page cannot drift,
because it asserts nothing about the code: it asserts only where things are, and `ls` settles that.

The rule the four documents were written to enforce, and the reason they are trustworthy where this
page's predecessor was not:

> **Derive, do not assert.** Where a claim is about the code, cite the file and line, or give the
> command that produces the number. Never paraphrase from memory.

If you are about to add a paragraph here explaining how some part of skein works — that paragraph
belongs in `docs/architecture.md` if it is about the destination, or `docs/inventory.md` if it is
about the code as it stands, and both of those will make you cite something. Adding it here is how
this file got the way it was.

## The old content

It is not lost, and it is not worth restoring. `git log --follow -p ARCHITECTURE.md` has every
version, including the per-VM topology and the phase roadmap, with the dates that say when each
stopped being true.
