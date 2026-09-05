//! A page is more than one script, and the browser reads them as one scope.
//!
//! `src/web/index.html` loads `/vendor/cockpit.js` and then runs 5,270 lines inline. Both are
//! **classic** scripts — no `type="module"` — so their top-level declarations land in the same
//! global lexical scope, and a name declared in both is `SyntaxError: Identifier 'x' has already
//! been declared`. That does not fail the offending line. It fails the **whole** script, before
//! anything in it runs.
//!
//! That happened, and it shipped: `c33fba3` gave the shared bundle a `slug` for `/v2`'s box-name
//! preview, `index.html` had had `const slug` since `71f71f7`, and from that commit until this test
//! existed the main cockpit was a blank page. Every layer that could have seen it was looking at
//! one file at a time — `cargo test` never loads the page, the module graph and Source checkers read
//! Rust, and reading either script on its own shows nothing wrong. The browser suite *did* see it,
//! and had been silently unrun.
//!
//! **What this checks, and what it deliberately does not.** Only the scripts skein writes: the
//! built bundle and each page's inline block. The vendored third-party files (xterm, marked) are
//! minified and IIFE-wrapped, and scanning them for column-0 declarations would invent collisions
//! out of one-letter minifier names rather than finding real ones.
//!
//! Column-0 declarations only, which is the convention both files hold to — anything indented is
//! inside a function and shadows rather than collides. A missed collision here is a browser suite
//! away; a false one would make this test the thing people delete.
//!
//! A name declared twice *within* one script is checked too, and it is the quieter of the two: the
//! engine raises nothing, so there is no blank page to notice — just a caller wired to the wrong
//! function. See `no_page_declares_a_name_twice_within_its_own_script`.

use std::collections::BTreeMap;
use std::path::Path;

/// Every top-level declaration in a script, in source order, as `(name, kind, 1-based line)`.
///
/// Kept as a list rather than a map because the two checks below want different things from it: one
/// wants each name once, the other wants precisely the repeats.
fn declarations(source: &str) -> Vec<(String, String, usize)> {
    let mut found = Vec::new();
    for (i, line) in source.lines().enumerate() {
        // Column 0 only: an indented declaration is inside something.
        let rest = match line
            .strip_prefix("const ")
            .map(|r| ("const", r))
            .or_else(|| line.strip_prefix("let ").map(|r| ("let", r)))
            .or_else(|| line.strip_prefix("var ").map(|r| ("var", r)))
            .or_else(|| line.strip_prefix("function ").map(|r| ("function", r)))
            .or_else(|| line.strip_prefix("class ").map(|r| ("class", r)))
        {
            Some(pair) => pair,
            None => continue,
        };
        let (kind, tail) = rest;
        let name: String = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if name.is_empty() {
            continue; // destructuring, `function (` — nothing this test can name
        }
        found.push((name, kind.to_string(), i + 1));
    }
    found
}

/// Every top-level declaration in a script, as `name -> (kind, 1-based line)`.
///
/// `kind` is kept because the JavaScript rule is asymmetric: two `var`s or two `function`s at global
/// scope are legal and merge, while a `const`, `let` or `class` collides with *anything* of the same
/// name. Reporting "duplicate" for the legal pair is how a checker earns a `#[ignore]`.
fn top_level(source: &str) -> BTreeMap<String, (String, usize)> {
    let mut found = BTreeMap::new();
    for (name, kind, line) in declarations(source) {
        found.entry(name).or_insert((kind, line));
    }
    found
}

/// One name declared twice: the name, then where it was declared first and where again, each as
/// `(kind, 1-based line)`.
///
/// An alias rather than the tuple written out, because the tuple is three levels deep and reads as
/// noise in a signature (`clippy::type_complexity`).
type Redeclaration = (String, (String, usize), (String, usize));

/// Names declared twice at column 0 in ONE script, as `(name, first, later)`.
///
/// Deliberately blind to `collides`: that rule answers "does the engine reject this", and within one
/// script the answer for `function`+`function` is no — the later declaration simply wins and every
/// earlier caller is silently rewired to it. That is not a legal pair here, it is the quietest bug
/// this file can have, so redeclaration of ANY kind is reported.
fn redeclared(source: &str) -> Vec<Redeclaration> {
    let mut first: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let mut repeats = Vec::new();
    for (name, kind, line) in declarations(source) {
        match first.get(&name) {
            Some(earlier) => repeats.push((name, earlier.clone(), (kind, line))),
            None => {
                first.insert(name, (kind, line));
            }
        }
    }
    repeats
}

/// The one inline `<script>` block — the one with no `src` — and the file line it starts on.
///
/// The offset is what turns a block-relative line into something you can open. A duplicate reported
/// as "line 4491" of a 5,000-line block is a search, not a location.
fn inline_script(html: &str) -> (&str, usize) {
    let open = "<script>\n";
    let start = html
        .find(open)
        .expect("the page has an inline <script> block")
        + open.len();
    let end = html[start..]
        .find("\n</script>")
        .expect("the inline <script> block is closed")
        + start;
    // Lines strictly before the block: the last of them is the `<script>` line itself.
    (&html[start..end], html[..start].lines().count())
}

/// A lexical declaration collides with anything; `var`/`function` only with a lexical one.
fn collides(a: &str, b: &str) -> bool {
    let lexical = |k: &str| matches!(k, "const" | "let" | "class");
    lexical(a) || lexical(b)
}

#[test]
fn no_page_declares_a_name_the_shared_bundle_already_declares() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bundle_path = root.join("src/web/vendor/cockpit.js");
    let bundle =
        std::fs::read_to_string(&bundle_path).expect("the built cockpit bundle is present");
    let bundle_names = top_level(&bundle);

    for page in ["src/web/index.html", "src/web/v2.html"] {
        let html =
            std::fs::read_to_string(root.join(page)).unwrap_or_else(|e| panic!("{page}: {e}"));

        // A page that does not load the bundle cannot collide with it. Asserted rather than assumed,
        // so that a page which stops loading it is not silently dropped from this check.
        if !html.contains(r#"src="/vendor/cockpit.js""#) {
            continue;
        }
        // A module has its own scope and none of this applies — but neither page is one today, and
        // if one becomes one this test should be told rather than quietly pass.
        assert!(
            !html.contains(r#"<script type="module">"#),
            "{page} has a module script; this test's whole premise is that these are classic \
             scripts sharing one global scope — decide what the rule is now rather than leaving \
             this check reading the wrong block"
        );

        let (block, _) = inline_script(&html);
        for (name, (kind, line)) in top_level(block) {
            if let Some((bundle_kind, bundle_line)) = bundle_names.get(&name) {
                assert!(
                    !collides(kind.as_str(), bundle_kind.as_str()),
                    "`{name}` is declared at the top level of BOTH \
                     src/web/vendor/cockpit.js:{bundle_line} (`{bundle_kind}`) and \
                     {page}'s inline script (`{kind}`, line {line} of the block).\n\
                     \n\
                     They are classic scripts sharing one global lexical scope, so the browser \
                     raises `SyntaxError: Identifier '{name}' has already been declared` and \
                     abandons the ENTIRE inline script — a blank page, with no clue in it.\n\
                     \n\
                     The fix is one definition, not two that agree: delete the page's copy and use \
                     the bundle's, which is where `cockpit/src` puts the functions that are tested \
                     in node."
                );
            }
        }
    }
}

/// The other direction: one script, one name, twice.
///
/// The check above is about two scripts sharing a scope, where the engine at least raises a
/// `SyntaxError` and takes the page down loudly enough to be found. This one has no such backstop.
/// `function revDraft(pr)` was added next to the review row while `function revDraft(number)` already
/// served the composer (SKEIN-225): two top-level function declarations, same name, and JavaScript
/// permits it — no `SyntaxError`, no console warning. The later one wins, the composer's caller is
/// rewired to a function expecting a different argument, and what reaches the browser is
/// `RangeError: Maximum call stack size exceeded` in a place nothing points at. The node suites see
/// none of it: they lift functions out of the page by name, so they only ever load one of the two.
#[test]
fn no_page_declares_a_name_twice_within_its_own_script() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    for page in ["src/web/index.html", "src/web/v2.html"] {
        let html =
            std::fs::read_to_string(root.join(page)).unwrap_or_else(|e| panic!("{page}: {e}"));
        let (block, offset) = inline_script(&html);

        let repeats = redeclared(block);
        if repeats.is_empty() {
            continue;
        }

        // Every repeat, in one message. A redeclaration is usually one of a family — a rename
        // applied to the copy and not the original — and reporting only the first sends the reader
        // back for the rest one test run at a time.
        let mut sites = String::new();
        for (name, (first_kind, first_line), (later_kind, later_line)) in &repeats {
            sites.push_str(&format!(
                "\n  `{name}`: {page}:{} (`{first_kind} {name}`) and {page}:{} (`{later_kind} {name}`)",
                offset + first_line,
                offset + later_line,
            ));
        }
        panic!(
            "{} name(s) declared twice at the top level of {page}'s inline script:\n{sites}\n\
             \n\
             Whichever runs last is the one every caller in the page gets, including the callers \
             written for the other one. Two `function`s raise nothing at all — no SyntaxError, \
             no warning — and the node suites lift functions by name, so they load one of the \
             two and pass.\n\
             \n\
             The fix is a name each, or one definition serving both callers. Not two that agree.",
            repeats.len(),
        );
    }
}

/// The gate above is worth nothing if it cannot see the shape the bug came in, or cannot say where.
///
/// Written against the real one: two `function` declarations of the same name, which `collides`
/// answers "legal" for and which this check must report anyway. A `redeclared` built on `collides`
/// would have passed the whole suite while the composer called the row's function.
#[test]
fn the_scan_reports_a_function_redeclared_by_a_function_and_names_both_lines() {
    let repeats =
        redeclared("function revDraft(number) {\n}\nconst x = 1;\nfunction revDraft(pr) {\n}\n");
    assert_eq!(repeats.len(), 1, "one name declared twice, reported once");
    let (name, first, later) = &repeats[0];
    assert_eq!(name, "revDraft");
    assert_eq!(first, &("function".to_string(), 1));
    assert_eq!(later, &("function".to_string(), 4));
    assert!(
        collides("function", "const"),
        "the cross-script rule is untouched by any of this"
    );

    // A name declared once is not a repeat, however many other names surround it.
    assert!(redeclared("function a() {}\nfunction b() {}\nconst c = 1;\n").is_empty());

    // And the offset that turns a block line into a file line: the block starts after `<script>`, so
    // a declaration on the block's first line is on the file line after it.
    let (block, offset) = inline_script("<html>\n<script>\nfunction f() {}\n</script>\n");
    assert_eq!(block, "function f() {}");
    assert_eq!(offset, 2, "`<html>` and `<script>` precede the block");
    assert_eq!(offset + 1, 3, "`function f` is file line 3");
}

/// The gate above is worth nothing if `top_level` cannot see the shape the collision came in.
///
/// Written against the real one: `function slug` in the bundle, `const slug` in the page. A version
/// of `top_level` that only matched `const` would have passed the whole suite while the board stayed
/// blank, which is the failure mode this test exists inside.
#[test]
fn the_scan_sees_the_declaration_shapes_the_collision_came_in() {
    let seen = top_level(
        "function slug(s) {\n  const inner = 1;\n}\nconst other = 2;\nlet third = 3;\nclass C {}\nvar v;\n",
    );
    assert_eq!(seen.get("slug").map(|(k, _)| k.as_str()), Some("function"));
    assert_eq!(seen.get("other").map(|(k, _)| k.as_str()), Some("const"));
    assert_eq!(seen.get("third").map(|(k, _)| k.as_str()), Some("let"));
    assert_eq!(seen.get("C").map(|(k, _)| k.as_str()), Some("class"));
    assert_eq!(seen.get("v").map(|(k, _)| k.as_str()), Some("var"));
    assert!(
        !seen.contains_key("inner"),
        "an indented declaration is inside a function and shadows rather than collides"
    );

    // The asymmetry, both ways round — this is what stops the check reporting the legal pair.
    assert!(collides("function", "const"), "the real collision");
    assert!(collides("const", "function"), "and in the order it is read");
    assert!(collides("let", "var"));
    assert!(!collides("var", "var"), "two global `var`s merge, legally");
    assert!(
        !collides("function", "function"),
        "and so do two global function declarations"
    );
}
