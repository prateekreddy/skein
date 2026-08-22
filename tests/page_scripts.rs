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

use std::collections::BTreeMap;
use std::path::Path;

/// Every top-level declaration in a script, as `name -> (kind, 1-based line)`.
///
/// `kind` is kept because the JavaScript rule is asymmetric: two `var`s or two `function`s at global
/// scope are legal and merge, while a `const`, `let` or `class` collides with *anything* of the same
/// name. Reporting "duplicate" for the legal pair is how a checker earns a `#[ignore]`.
fn top_level(source: &str) -> BTreeMap<String, (String, usize)> {
    let mut found = BTreeMap::new();
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
        found.entry(name).or_insert((kind.to_string(), i + 1));
    }
    found
}

/// The one inline `<script>` block — the one with no `src`.
fn inline_script(html: &str) -> &str {
    let open = "<script>\n";
    let start = html
        .find(open)
        .expect("the page has an inline <script> block")
        + open.len();
    let end = html[start..]
        .find("\n</script>")
        .expect("the inline <script> block is closed")
        + start;
    &html[start..end]
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

        for (name, (kind, line)) in top_level(inline_script(&html)) {
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
