//! Nothing proved a serialised queue field had a reader. This does.
//!
//! A field on a payload type is computed on every refresh, written into the cache and sent over the
//! wire. When the thing that read it goes away, none of that stops — and nothing fails, so the only
//! way anyone finds out is by grepping the whole payload against the page by hand. That census was
//! done once (SKEIN-255). It named ten fields, and by the time this test existed two of them
//! (`Queue.trunk`, `MergedQueue.skipped`) had gained page readers and one (`Pr.labels`) had lost
//! one. A photograph of a moving thing is out of date before it is developed, which is the argument
//! for a gate rather than nine fixes.
//!
//! The rule, and `docs/queue-fields.md` is where the human half lives:
//!
//! > Every field serialised on a queue payload type is read by the page — `src/web/index.html` or
//! > the cockpit bundle it loads — or is named in
//! > `docs/queue-fields.md` with the reader that justifies it.
//!
//! **Both directions are checked**, and the second is the one that keeps the first honest: a
//! declaration for a field the page has since started reading is a failure too. Exceptions nobody
//! prunes are how the original census rotted.
//!
//! **Matching is by field name and deliberately loose.** A name counts as read if it appears in the
//! page as a property access at all, without proving the object it came from is this payload. That
//! is the error worth making: a false accusation gets this test deleted, a miss costs one census.
//! `tests/page_scripts.rs` states the same trade for the same reason.
//!
//! **Loose about which object, strict about comments.** A line that opens a comment is not a read,
//! because the looseness above only buys the first trade if the miss stays the expensive direction:
//! with no comment filter at all, one line of prose naming `.thatname` excused a field nothing
//! reads, and the gate went green over what it exists to catch (SKEIN-992). [`page_reads`] carries
//! the four openers, how they were derived and what the filter does not see.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The modules whose `Serialize` structs are queue payloads: the queue, the merge train, the
/// review pane, the contract signals. Widening the gate is adding a module here.
///
/// **Modules, not files** — each entry is read by [`read_unit`], which takes `src/<name>.rs` and
/// `src/<name>/**.rs` together. A module that outgrows one file and becomes a directory is still
/// one module, and this gate went blind to `src/prwork` the moment it was split (SKEIN-578): the
/// hard-coded `src/prwork.rs` would have panicked on a directory, and naming the submodules one by
/// one would have made every future split a silent hole in the census instead of a loud failure.
const PAYLOAD_UNITS: [&str; 4] = ["src/prq", "src/prwork", "src/review", "src/contracts"];

/// **The page is two files, and reading one of them was a bug in this gate** (SKEIN-328).
///
/// `cockpit/build.mjs` concatenates `cockpit/src/*.mjs` into `src/web/vendor/cockpit.js`, and
/// `index.html` loads that bundle — so a field read from a cockpit module is read BY THE PAGE, and
/// a census that greps only `index.html` reports it as unread.
///
/// The incentive is why this matters more than the six wrong answers it gave. As it stood, moving a
/// rule out of the 10,000-line page into a leaf function `node --test` can drive looked to this gate
/// exactly like deleting the field's reader — and the cheap way to a green build was to add a
/// decorative read back into `index.html`. A gate that pays people to keep logic untestable is worse
/// than no gate.
///
/// The BUNDLE rather than `cockpit/src/*.mjs` directly, because the bundle is what the page actually
/// loads; `node cockpit/build.mjs --check` is the separate gate that keeps it from going stale.
const PAGE: [&str; 2] = ["src/web/index.html", "src/web/vendor/cockpit.js"];
const DECLARED: &str = "docs/queue-fields.md";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// One module's whole text: `src/<name>.rs` plus every `.rs` under `src/<name>/`, joined.
///
/// The join is safe for [`serialised_fields`], whose one structural assumption is a top-level
/// struct closing at column 0 — concatenating whole files preserves that. It panics when a unit
/// resolves to nothing at all, because a census that quietly reads no source is the failure mode
/// this whole file exists to prevent.
fn read_unit(unit: &str) -> String {
    let mut parts = Vec::new();
    let flat = repo().join(format!("{unit}.rs"));
    if flat.is_file() {
        parts.push(std::fs::read_to_string(&flat).expect("readable"));
    }
    let dir = repo().join(unit);
    if dir.is_dir() {
        let mut found: Vec<PathBuf> = walk_rs(&dir);
        found.sort();
        for p in found {
            parts.push(std::fs::read_to_string(&p).expect("readable"));
        }
    }
    assert!(
        !parts.is_empty(),
        "{unit} names neither {unit}.rs nor a {unit}/ directory, so the census would cover none \
         of it and every assertion built on it would pass by reading nothing"
    );
    parts.join("\n")
}

fn walk_rs(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("readable").flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk_rs(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out
}

/// `Type.field` for every field a `Serialize` struct in `source` puts on the wire.
///
/// Hand-written rather than a syn dependency, for the reason `crate::contracts` gives about
/// hand-rolled scanners: this is the whole of what would be used from one. It reads what the file
/// looks like, and the shape it relies on — a top-level struct whose closing brace is in column 0 —
/// is the shape every payload type in this crate has. A struct it fails to parse is reported as
/// having no fields, which shows up as a shrinking census rather than as a silent pass, so
/// [`the_census_covers_every_payload_type`] pins the type count.
fn serialised_fields(source: &str) -> BTreeMap<String, Vec<String>> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (i, line) in lines.iter().enumerate() {
        if !(line.contains("derive(") && line.contains("Serialize")) {
            continue;
        }
        // Forward to the declaration this derive is on. Anything between is another attribute.
        let mut j = i + 1;
        while j < lines.len()
            && !lines[j].trim_start().starts_with("pub struct")
            && !lines[j].trim_start().starts_with("struct")
        {
            if !lines[j].trim_start().starts_with('#') {
                break; // not a struct: an enum, or something this scanner should not guess at
            }
            j += 1;
        }
        let Some(decl) = lines.get(j) else { continue };
        let trimmed = decl.trim_start();
        let Some(rest) = trimmed
            .strip_prefix("pub struct ")
            .or_else(|| trimmed.strip_prefix("struct "))
        else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }

        let mut fields = Vec::new();
        let mut attrs = String::new();
        let mut k = j + 1;
        while k < lines.len() {
            let raw = lines[k];
            if raw == "}" {
                break; // column-0 close: the struct is over
            }
            let t = raw.trim_start();
            if t.starts_with("#[") {
                attrs.push_str(t);
                attrs.push(' ');
                k += 1;
                continue;
            }
            if t.starts_with("//") {
                k += 1;
                continue; // doc comments do not clear the attributes they sit among
            }
            if let Some(field) = field_name(t) {
                // `skip_serializing_if` still serialises; a bare `skip` / `skip_serializing` does not.
                let skipped = (attrs.contains("skip_serializing")
                    && !attrs.contains("skip_serializing_if"))
                    || attrs.contains("serde(skip)")
                    || attrs.contains("skip,");
                if !skipped {
                    fields.push(renamed(&attrs).unwrap_or(field));
                }
            }
            attrs.clear();
            k += 1;
        }
        out.insert(name, fields);
    }
    out
}

/// `name` out of a `pub name: Type,` line, or nothing if this is not a field.
fn field_name(t: &str) -> Option<String> {
    let rest = t.strip_prefix("pub ").unwrap_or(t);
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    // A field is `name:` and not `name(` or `name =`. The colon must be what follows the name.
    rest[name.len()..]
        .trim_start()
        .starts_with(':')
        .then_some(name)
}

/// The wire name from `#[serde(rename = "…")]`, if the attributes set one.
fn renamed(attrs: &str) -> Option<String> {
    let at = attrs.find("rename = \"")? + "rename = \"".len();
    let rest = &attrs[at..];
    Some(rest[..rest.find('"')?].to_string())
}

/// Does the page read a property by this name, anywhere?
///
/// **A line that opens a comment is not a read** (SKEIN-992), and this scanner had no comment
/// filter of any kind until it was. The direction that costs is reassurance: the gate below refuses
/// a serialised field nothing reads, so one comment line writing `.thatname` anywhere in 13,233
/// lines of page was enough to excuse it — a gate going green over exactly what it exists to catch.
/// It is the defect SKEIN-987 fixed in the route gate, in the census that watches the other wire.
///
/// **The four openers are the ones the two scanned files actually use**, counted rather than copied
/// from SKEIN-987's list, since [`PAGE`] is not the set that fix scanned:
///
/// ```sh
/// for f in src/web/index.html src/web/vendor/cockpit.js; do
///   for op in '<!--' '//' '/\*' '\*'; do printf '%s %s %s\n' "$f" "$op" \
///     "$(grep -cE "^[[:space:]]*$op" $f)"; done; done
/// ```
///
/// `index.html` is HTML with its script inline, so it has all four — 16 `<!--`, 3,514 `//`, 160
/// `/*`, 3 `*`; `cockpit.js` is JavaScript and has three of them — 622, 5, 48. (Two of the three
/// `*` lines in `index.html` are CSS universal selectors rather than comment interiors, at 29 and
/// 1386. Skipping them can only lose matches, never invent one, and a stylesheet cannot read a
/// field.)
///
/// Measured before the change, across the 118 serialised fields: 108 read by the page raw, 108 read
/// with comment-opening lines dropped — **no field is excused this way today**, so this is a latent
/// hole rather than a live one. Fifteen fields already match on a comment line, `Pr.url` and
/// `FailedCheck.url` on three each; every one of them also has a real reader, which is the only
/// reason the count did not move.
///
/// The honest limit, because it reads as more coverage than it is: this is per LINE, so it sees the
/// line a comment OPENS on and not the interior of one that runs on. `src/web/index.html:1733`
/// names `.claude` inside a comment that opened at 1732 and closes at 1744, and it is still
/// scanned — one of four such interior lines in that file, none of which happens to name a payload
/// field today (`.claude`, `.md`, `.mjs`, `.md`). `cockpit.js` has none, because its block comments
/// indent their interiors with `*`, which is one of the four. SKEIN-996 carries what is left, with
/// the reason a comment tracker is not obviously an improvement: a `<!--` inside a string would
/// swallow every read after it, which is the direction that makes this gate green over anything.
fn page_reads(page: &str, field: &str) -> bool {
    page.lines().any(|line| line_reads(line, field))
}

/// One line of the page, read the way [`page_reads`] reads the whole of it.
///
/// Split out so the comment filter and the match live at the same granularity: the check is per
/// line, and a whole-page `match_indices` cannot tell which line it landed on.
fn line_reads(line: &str, field: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("//") || t.starts_with('*') || t.starts_with("/*") || t.starts_with("<!--") {
        return false;
    }
    let dotted = format!(".{field}");
    let boundary = |s: &str, at: usize, len: usize| {
        s[at + len..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '$')
    };
    line.match_indices(&dotted)
        .any(|(at, _)| boundary(line, at, dotted.len()))
        || line.contains(&format!("[\"{field}\"]"))
        || line.contains(&format!("['{field}']"))
}

/// Every `Type.field` named in `docs/queue-fields.md`'s machine-readable list.
fn declared() -> BTreeSet<String> {
    read(DECLARED)
        .lines()
        .filter_map(|l| {
            let t = l.trim_start();
            let rest = t.strip_prefix("- `")?;
            let name = &rest[..rest.find('`')?];
            name.contains('.').then(|| name.to_string())
        })
        .collect()
}

/// The whole census: every serialised field, and whether the page reads it.
fn census() -> Vec<(String, bool)> {
    let units: Vec<String> = PAYLOAD_UNITS.iter().map(|u| read_unit(u)).collect();
    census_of(&units, &PAGE.map(read).join("\n"))
}

/// The census over source and page text handed in, rather than read off disk.
///
/// Lifted out of [`census`] so a test can drive what both gates below actually consume — the
/// composition of [`serialised_fields`] and [`page_reads`] — over fixtures, instead of a second
/// copy of it (SKEIN-992, following SKEIN-987's `unasked`). Neither scanner was wrong on its own
/// there either, and an assertion on `page_reads` alone would not watch a field go from excused to
/// needing a reader.
fn census_of(units: &[String], page: &str) -> Vec<(String, bool)> {
    let mut all = Vec::new();
    for source in units {
        for (ty, fields) in serialised_fields(source) {
            for f in fields {
                let read_by_page = page_reads(page, &f);
                all.push((format!("{ty}.{f}"), read_by_page));
            }
        }
    }
    all.sort();
    all.dedup();
    all
}

/// Every field in `all` that the page does not read and `declared` does not excuse — the gate's
/// own verdict, lifted out of [`every_serialised_queue_field_has_a_reader_or_a_declared_reason`]
/// for the reason [`census_of`] gives.
fn unread_and_undeclared<'a>(
    all: &'a [(String, bool)],
    declared: &BTreeSet<String>,
) -> Vec<&'a str> {
    all.iter()
        .filter(|(_, read)| !read)
        .map(|(name, _)| name.as_str())
        .filter(|name| !declared.contains(*name))
        .collect()
}

/// The gate. A serialised field is read by the page, or `docs/queue-fields.md` says why not.
#[test]
fn every_serialised_queue_field_has_a_reader_or_a_declared_reason() {
    let declared = declared();
    let all = census();
    let undeclared = unread_and_undeclared(&all, &declared);

    println!(
        "queue payloads: {} serialised fields, {} read by the page, {} declared in {DECLARED}",
        all.len(),
        all.iter().filter(|(_, r)| *r).count(),
        declared.len()
    );

    assert!(
        undeclared.is_empty(),
        "these fields are serialised onto a queue payload and nothing reads them:\n\n{}\n\n\
         Each one is computed on every refresh, cached, and sent over the wire, and no failure will \
         ever mention it. Three ways out, in the order they are usually right:\n\
         \x20 1. the page SHOULD read it — then this field is the evidence of the bug, not the bug;\n\
         \x20 2. skein itself reads it after a cache round-trip — add it to {DECLARED} NAMING THE \
         READER, because \"used server-side\" with no site is the claim that rots;\n\
         \x20 3. nothing reads it — it should not be serialised.\n\n\
         The list this replaced is in {DECLARED}, with what each existing exemption is for.",
        undeclared
            .iter()
            .map(|n| format!("  - {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The other direction: an exemption that outlived its reason.
///
/// Not politeness. The census this gate replaced listed `Queue.trunk` and `MergedQueue.skipped` as
/// unread, and the page had started reading both — so the document was still claiming a hole that
/// had been filled. A list of exceptions nobody prunes becomes a list nobody checks.
#[test]
fn no_declaration_outlives_the_field_it_excuses() {
    let all = census();
    let read_now: BTreeSet<&str> = all
        .iter()
        .filter(|(_, r)| *r)
        .map(|(n, _)| n.as_str())
        .collect();
    let known: BTreeSet<&str> = all.iter().map(|(n, _)| n.as_str()).collect();

    let declared = declared();
    let stale: Vec<&String> = declared
        .iter()
        .filter(|d| read_now.contains(d.as_str()))
        .collect();
    assert!(
        stale.is_empty(),
        "{DECLARED} excuses these fields for not being read, and the page reads them now:\n{}\n\n\
         Delete the lines. The exemption did its job.",
        stale
            .iter()
            .map(|n| format!("  - {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let gone: Vec<&String> = declared
        .iter()
        .filter(|d| !known.contains(d.as_str()))
        .collect();
    assert!(
        gone.is_empty(),
        "{DECLARED} names fields that no longer exist on any payload:\n{}\n\n\
         If they were deleted, delete their lines too — an entry for nothing reads as an entry for \
         something.",
        gone.iter()
            .map(|n| format!("  - {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The census still reaches every payload type, so a parse failure cannot look like a clean bill.
///
/// [`serialised_fields`] reports a struct it cannot parse as having no fields, which would pass
/// every check above by finding nothing to check. This is the assertion that notices.
#[test]
fn the_census_covers_every_payload_type() {
    for file in PAYLOAD_UNITS {
        let src = read_unit(file);
        let found = serialised_fields(&src);
        let derives = src
            .lines()
            .filter(|l| l.contains("derive(") && l.contains("Serialize"))
            .count();
        assert!(
            !found.is_empty(),
            "{file} has {derives} Serialize derives and the scanner found no payload types in it. \
             Either the module stopped holding payloads — remove it from PAYLOAD_UNITS — or the \
             scanner stopped understanding it, which would make every other check here pass by \
             finding nothing."
        );
        for (ty, fields) in &found {
            assert!(
                !fields.is_empty(),
                "{file}: payload type {ty} parsed with no fields at all. A struct the scanner \
                 cannot read is a struct this gate silently stops covering."
            );
        }
    }
    let types: usize = PAYLOAD_UNITS
        .iter()
        .map(|f| serialised_fields(&read_unit(f)).len())
        .sum();
    assert!(
        types >= 10,
        "only {types} payload types found across {} modules — the scanner has lost its grip",
        PAYLOAD_UNITS.len()
    );
}

// ---------------------------------------------------------------------------------------------
// The gate's own tests. A gate that has never gone red proves nothing, and these are the red.
// ---------------------------------------------------------------------------------------------

/// A field nothing reads is named. This is the tenth field, without touching anyone's source.
#[test]
fn the_gate_names_a_field_the_page_does_not_read() {
    let src = "\
#[derive(Debug, Clone, Serialize)]
pub struct Invented {
    pub read_by_the_page: String,
    /// Nothing anywhere reads this.
    pub nobody_reads_this_one: u64,
}
";
    let found = serialised_fields(src);
    let fields = found.get("Invented").expect("the scanner found the struct");
    assert_eq!(
        fields,
        &vec![
            "read_by_the_page".to_string(),
            "nobody_reads_this_one".to_string()
        ]
    );

    let page = PAGE.map(read).join("\n");
    assert!(
        !page_reads(&page, "nobody_reads_this_one"),
        "the page does not read it, so the gate would name it"
    );
    // And the gate's verdict is a function of exactly that: unread and undeclared.
    assert!(
        !declared().contains("Invented.nobody_reads_this_one"),
        "a field nobody declared and nobody reads is what this gate refuses"
    );
}

/// And it does not accuse a field the page does read — the failure that would get it deleted.
#[test]
fn the_gate_does_not_accuse_a_field_the_page_reads() {
    let page = PAGE.map(read).join("\n");
    for live in ["number", "title", "head_sha", "lane", "reasons", "prs"] {
        assert!(
            page_reads(&page, live),
            "the page reads `{live}`, and a gate that says otherwise is one nobody will keep"
        );
    }
    // Word boundaries, both ends: `.lane` must not be satisfied by `.lanes` or by `.mylane`.
    assert!(!page_reads("x.laneish", "lane"));
    assert!(!page_reads("x.lane_two", "lane"));
    assert!(page_reads("x.lane;", "lane"));
    assert!(page_reads("x[\"lane\"]", "lane"));
}

/// **A field named only in a page comment still has no reader, and the gate has to say so**
/// (SKEIN-992).
///
/// [`page_reads`] searched the whole page text with no comment filter of any kind, and the pages
/// discuss field names as readily as they read them — 15 of the 118 serialised fields match on a
/// comment line today. So one comment writing `.thatname` was enough to satisfy
/// [`every_serialised_queue_field_has_a_reader_or_a_declared_reason`] for a field nothing reads:
/// the gate whose own message says the field "is computed on every refresh, cached, and sent over
/// the wire, and no failure will ever mention it", talked out of it by prose. Both gates take the
/// same census, so the other one inverts the same way — a comment can make a live exemption in
/// `docs/queue-fields.md` look outlived and get a true declaration deleted. That is why one of the
/// three assertions in the loop is on the census entry itself: it is the input both gates share,
/// and asserting it covers the second gate without a second copy of that gate's arithmetic.
///
/// The verdict is driven through [`unread_and_undeclared`], the gate's own composition, and not
/// through [`page_reads`]: neither scanner was wrong by itself, and an assertion on the scanner
/// would not have watched a field go from excused to named.
///
/// All four comment forms, because [`PAGE`] is HTML with inline script plus a JavaScript bundle and
/// uses all four — the list is derived in [`page_reads`]'s doc comment rather than copied from
/// SKEIN-987, which scanned a different set of files.
#[test]
fn a_field_named_only_in_a_page_comment_still_has_no_reader() {
    let source = "\
#[derive(Serialize)]
pub struct Invented {
    pub only_in_a_comment: String,
}
"
    .to_string();
    assert_eq!(
        serialised_fields(&source).get("Invented"),
        Some(&vec!["only_in_a_comment".to_string()]),
        "the fixture payload did not parse, so what follows proves nothing"
    );
    let units = [source];
    let no_declarations = BTreeSet::new();

    for (form, page) in [
        (
            "<!--",
            "        <!-- The pane is rendered from `q.only_in_a_comment` rather than here. -->",
        ),
        (
            "//",
            "        // `q.only_in_a_comment` used to be drawn here; the row went away.",
        ),
        (
            "/*",
            "        /* the row read q.only_in_a_comment before the pane was rewritten */",
        ),
        (
            "*",
            "         * and the caption came from q.only_in_a_comment, which nothing draws now",
        ),
    ] {
        // The verdict first, deliberately: it is the assertion whose failure says what the defect
        // costs, and a reader who sees it go red should read that sentence before the two below,
        // which only localise it.
        assert_eq!(
            unread_and_undeclared(&census_of(&units, page), &no_declarations),
            vec!["Invented.only_in_a_comment"],
            "the gate did not name a field whose only mention in the page is a {form} comment, \
             which is the gate going green over exactly what it exists to catch"
        );
        assert_eq!(
            census_of(&units, page),
            vec![("Invented.only_in_a_comment".to_string(), false)],
            "the census BOTH gates take says a field named only in a {form} comment is read by \
             the page — so a serialised field nothing reads is excused, and a live exemption for \
             it in {DECLARED} reads as outlived"
        );
        assert!(
            !page_reads(page, "only_in_a_comment"),
            "a line opening a {form} comment only NAMES the field, and it was read as a read of it"
        );
    }

    // The control: the same field, the same fixture, an actual read. Without this the assertions
    // above would pass just as happily on a scanner that had stopped matching anything at all.
    let drawn = "        row.textContent = q.only_in_a_comment;";
    assert!(
        page_reads(drawn, "only_in_a_comment"),
        "a line that does read the field was not read as reading it, which would make every \
         assertion above pass by matching nothing at all"
    );
    assert_eq!(
        census_of(&units, drawn),
        vec![("Invented.only_in_a_comment".to_string(), true)],
        "the census says a field the page reads on a plain code line is unread"
    );
    assert!(
        unread_and_undeclared(&census_of(&units, drawn), &no_declarations).is_empty(),
        "the page reads the field and the gate named it anyway — the false accusation that gets a \
         gate like this deleted"
    );
}

/// `skip_serializing_if` still puts the field on the wire; a bare skip does not.
#[test]
fn a_field_that_is_never_serialised_is_not_the_gates_business() {
    let src = "\
#[derive(Serialize)]
pub struct S {
    #[serde(skip_serializing)]
    pub never_sent: String,
    #[serde(default, skip_serializing_if = \"Vec::is_empty\")]
    pub sent_when_non_empty: Vec<String>,
    #[serde(rename = \"onTheWire\")]
    pub in_the_source: String,
}
";
    assert_eq!(
        serialised_fields(src).get("S"),
        Some(&vec![
            "sent_when_non_empty".to_string(),
            "onTheWire".to_string()
        ]),
        "a skipped field is not serialised; a conditionally-skipped one is; and a renamed one goes \
         on the wire under its wire name, which is the name the page would read"
    );
}
