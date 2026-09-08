//! The asset root is resolved once, so this is its own test binary.
//!
//! `skein::assets` reads `$SKEIN_COCKPIT_ASSETS` into a `OnceLock` at the first request and never
//! again — deliberately, because a root that can change between requests is a root something else
//! can change *during* one. That is untestable from a process that has already resolved it, so the
//! override gets a binary of its own. One process, one root, which is also how it runs in production.

mod common;

use common::{env_pins, Scratch};

#[test]
fn a_directory_overrides_the_built_in_assets_without_replacing_them() {
    let dir = Scratch::boxes("skein-assets");
    std::fs::write(dir.join("xterm.min.js"), b"console.log('from disk')").unwrap();
    std::fs::write(dir.join("app-1a2b3c4d5e.css"), b"body{}").unwrap();
    std::fs::create_dir_all(dir.join("v2")).unwrap();
    std::fs::write(dir.join("v2/main-deadbeef99.js"), b"// built").unwrap();
    // Bound after `dir`, so the variable stops naming the directory before the directory goes.
    let mut pins = env_pins();
    pins.set("SKEIN_COCKPIT_ASSETS", dir.path());

    // The whole point of the override: changing a stylesheet is a reload, not a `cargo build`.
    let overridden = skein::assets::get("xterm.min.js").expect("served from the directory");
    assert_eq!(
        String::from_utf8_lossy(&overridden.bytes).trim(),
        "console.log('from disk')",
        "the built-in copy won over the directory"
    );

    // **Overrides, does not replace.** A directory holding one file is a developer editing one
    // file, and every other asset still has to work — otherwise the override is an all-or-nothing
    // switch and nobody uses it.
    assert!(
        skein::assets::get("marked.min.js").is_some(),
        "an asset only in the binary stopped being served once a directory existed"
    );

    // A name a build emits: nested, hashed, and cached for a year because it cannot mean anything
    // else. The unhashed one beside it is not, for the same reason the document is `no-store`.
    let built = skein::assets::get("v2/main-deadbeef99.js").expect("nested assets are served");
    assert_eq!(built.cache, "public, max-age=31536000, immutable");
    assert_eq!(built.content_type, "application/javascript; charset=utf-8");
    assert_eq!(
        skein::assets::get("app-1a2b3c4d5e.css").map(|a| a.cache),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(overridden.cache, "no-store");

    // And the directory does not become a way out of itself.
    for climbing in ["../Cargo.toml", "v2/../../Cargo.toml", "v2/../xterm.min.js"] {
        assert!(
            skein::assets::get(climbing).is_none(),
            "{climbing} was served from the asset directory"
        );
    }
}
