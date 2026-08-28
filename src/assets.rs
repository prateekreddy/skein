//! What the cockpit is served from: one route, a directory of files, no code per file.
//!
//! # Why this exists before there is a bundle
//!
//! The asset layer was compile-time: four vendored scripts as four `include_str!` constants and four
//! handlers. A build step (§11.7) produces a directory whose file *names* carry content hashes, so
//! neither the count nor the names are known when the code is written — there is nothing to port
//! that shape to. A table generated from a directory is the only shape that serves both.
//!
//! # Embedded, and overridable — the decision, with its reason
//!
//! **Embedded by default**, because skein is one binary that cannot be half-upgraded, and a cockpit
//! whose scripts came from somewhere else is a cockpit that can talk to an API that has moved. That
//! is the property the hand-written constants had and it is worth keeping.
//!
//! **Overridable from a directory**, because most of what a build step buys during development is
//! that changing a stylesheet is a reload rather than a `cargo build`. `$SKEIN_COCKPIT_ASSETS` names
//! it.
//!
//! **Resolved once, at startup, and canonicalised.** The hazard is precise: a directory taken per
//! request from anything a caller sends is a way to serve a box's files through skein's own
//! authenticated origin. So the root is fixed before the first request, every candidate is
//! canonicalised, and anything that does not land inside it is not served — which covers `..`, an
//! absolute path, and a symlink pointing out, none of which a prefix check on the *request* would
//! catch.
//!
//! # Caching
//!
//! Two rules, because there are two kinds of file. A name that carries a content hash can never mean
//! different bytes, so it is `immutable` for a year. Everything else is `no-store`, for the same
//! reason the document is: reusing an older asset after a server restart mixes stale script with new
//! API behaviour.

/// The table generated from `src/web/vendor/` — see `build.rs`.
mod generated {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use generated::EMBEDDED;

/// One asset, ready to hand out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
    /// The `Cache-Control` value. See the module note for the two rules.
    pub cache: &'static str,
}

/// Where assets are read from, decided once.
///
/// `OnceLock` rather than a lookup per request, and that is the security half rather than a
/// performance one: a root that can change between requests is a root that something else can
/// change *during* one.
fn root() -> Option<&'static PathBuf> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let named = std::env::var("SKEIN_COCKPIT_ASSETS").ok()?;
        if named.trim().is_empty() {
            return None;
        }
        match std::fs::canonicalize(&named) {
            Ok(path) if path.is_dir() => {
                eprintln!("skein: serving cockpit assets from {}", path.display());
                Some(path)
            }
            Ok(path) => {
                eprintln!("skein: $SKEIN_COCKPIT_ASSETS is not a directory ({}); serving the built-in assets", path.display());
                None
            }
            Err(e) => {
                eprintln!("skein: $SKEIN_COCKPIT_ASSETS ({named}) could not be read ({e}); serving the built-in assets");
                None
            }
        }
    })
    .as_ref()
}

/// The asset at `path`, or nothing.
///
/// `None` covers "no such asset" and "that path is not allowed to name one" deliberately: a caller
/// probing for the difference learns nothing, and there is nothing a person can do with the
/// distinction that a 404 does not already tell them.
pub fn get(path: &str) -> Option<Asset> {
    let name = clean(path)?;
    let cache = cache_for(&name);
    if let Some(root) = root() {
        if let Some(bytes) = read_under(root, &name) {
            return Some(Asset {
                bytes,
                content_type: content_type(&name),
                cache,
            });
        }
    }
    EMBEDDED
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, bytes)| Asset {
            bytes: bytes.to_vec(),
            content_type: content_type(&name),
            cache,
        })
}

/// A request path reduced to a relative name, or nothing.
///
/// Refused rather than sanitised. Stripping `..` from a path is how a check becomes a transformation
/// that somebody later finds a way through; a path containing one is simply not a name.
fn clean(path: &str) -> Option<String> {
    let name = path.trim_start_matches('/');
    let looks_like_a_name = !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    looks_like_a_name.then(|| name.to_string())
}

/// Read `name` under `root`, and only if it really is under it.
///
/// Canonicalised and checked *after* joining, because that is the only check a symlink cannot walk
/// around: a link inside the directory pointing at `/etc` passes every test done on the request.
fn read_under(root: &Path, name: &str) -> Option<Vec<u8>> {
    let candidate = std::fs::canonicalize(root.join(name)).ok()?;
    if !candidate.starts_with(root) || !candidate.is_file() {
        return None;
    }
    std::fs::read(candidate).ok()
}

/// A name carries a content hash, so its bytes can never change.
///
/// The shape a build emits: `<something>-<hex>.<ext>`, with enough hex to be a hash rather than a
/// version somebody typed. Anything else is `no-store` — the conservative side, because an asset
/// wrongly cached for a year is a cockpit that cannot be fixed by restarting the server.
fn cache_for(name: &str) -> &'static str {
    let stem = name.rsplit('/').next().unwrap_or(name);
    let hashed = stem
        .rsplit_once('.')
        .and_then(|(base, _)| base.rsplit_once('-'))
        .is_some_and(|(_, tag)| tag.len() >= 8 && tag.chars().all(|c| c.is_ascii_hexdigit()));
    match hashed {
        true => "public, max-age=31536000, immutable",
        false => "no-store",
    }
}

/// What to call it, from the extension. Unknown means bytes, because guessing a type for a file
/// nobody anticipated is how a `.svg` gets served as something a browser will run.
pub fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four vendored files are in the table, by the names they have on disk.
    ///
    /// Generated rather than listed, which is the whole point: adding a fifth is putting a file in a
    /// directory, not writing a constant and a handler and a route.
    #[test]
    fn the_built_in_assets_are_whatever_is_in_the_directory() {
        let names: Vec<&str> = EMBEDDED.iter().map(|(n, _)| *n).collect();
        for expected in [
            "xterm.min.js",
            "xterm.min.css",
            "addon-fit.min.js",
            "marked.min.js",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} is not embedded: {names:?}"
            );
        }
        for (name, bytes) in EMBEDDED {
            assert!(!bytes.is_empty(), "{name} is embedded as nothing");
        }
        assert_eq!(
            get("xterm.min.js").map(|a| a.content_type),
            Some("application/javascript; charset=utf-8")
        );
        assert_eq!(get("nothing-like-this.js"), None);
    }

    /// A path that is not a name is refused, and refusal is where the check lives.
    ///
    /// Sanitising is the alternative and it is worse: stripping `..` turns a check into a
    /// transformation, and a transformation is something somebody later finds a way through.
    #[test]
    fn a_path_that_climbs_out_is_not_a_name() {
        for climbing in [
            "../Cargo.toml",
            "a/../../etc/passwd",
            "..",
            "./x",
            "",
            "/",
            "a//b",
            "a\\b",
            "x\0y",
        ] {
            assert_eq!(clean(climbing), None, "{climbing:?} was accepted as a name");
            assert_eq!(get(climbing), None, "{climbing:?} was served");
        }
        // A leading slash is stripped rather than refused, because the route's wildcard may carry
        // one — and what is left is a name *relative to the root*, which is why `/etc/passwd` is not
        // a way out. It names `<root>/etc/passwd`, which does not exist, and `read_under` would
        // refuse it even if it did.
        assert_eq!(clean("/xterm.min.js").as_deref(), Some("xterm.min.js"));
        assert_eq!(clean("/etc/passwd").as_deref(), Some("etc/passwd"));
        assert_eq!(
            get("/etc/passwd"),
            None,
            "an absolute-looking path was served"
        );
        assert_eq!(clean("v2/app.js").as_deref(), Some("v2/app.js"));
    }

    /// And a symlink inside the directory pointing out of it is refused too.
    ///
    /// The case a check on the *request* cannot see: every part of the path is a name, and the file
    /// it resolves to is somewhere else entirely. This is why the check is done after joining, on
    /// the canonical path.
    #[test]
    fn a_link_out_of_the_directory_is_not_served() {
        let dir = crate::testutil::tempdir();
        let served = dir.join("served");
        std::fs::create_dir_all(&served).unwrap();
        std::fs::write(dir.join("secret"), b"not yours").unwrap();
        std::fs::write(served.join("ok.js"), b"yours").unwrap();
        let _ = std::os::unix::fs::symlink(dir.join("secret"), served.join("escape.js"));
        let root = std::fs::canonicalize(&served).unwrap();

        assert_eq!(read_under(&root, "ok.js"), Some(b"yours".to_vec()));
        assert_eq!(
            read_under(&root, "escape.js"),
            None,
            "a symlink walked out of the asset directory"
        );
        assert_eq!(read_under(&root, "missing.js"), None);
    }

    /// Two cache rules, and the conservative one is the default.
    ///
    /// An asset wrongly cached for a year is a cockpit that restarting the server cannot fix, so
    /// only a name that *cannot* mean different bytes gets the long life.
    #[test]
    fn only_a_name_that_carries_a_hash_is_cached_for_ever() {
        for hashed in [
            "app-1a2b3c4d.js",
            "v2/main-deadbeefcafe.css",
            "x-00000000.js",
        ] {
            assert_eq!(
                cache_for(hashed),
                "public, max-age=31536000, immutable",
                "{hashed} carries a hash and was not cached"
            );
        }
        for not in [
            "xterm.min.js",
            "app.js",
            "app-2.js",
            "app-1a2b3c.js",
            "app-nothexnope.js",
            "index.html",
        ] {
            assert_eq!(cache_for(not), "no-store", "{not} was cached for a year");
        }
    }
}
