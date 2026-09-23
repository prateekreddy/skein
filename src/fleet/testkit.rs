//! Test helpers shared by more than one of this module's test files.

/// One function's body, brace-matched from its signature.
///
/// Brace-matched rather than "up to the next `\n}`", for the reason `tools/module-check.py`
/// gives about cutting test modules: the cheap version stops at the first nested block and
/// everything below it becomes invisible — which for a check like the one above would mean
/// silently passing on a body it never read.
pub(super) fn fn_body<'a>(src: &'a str, signature: &str) -> &'a str {
    let at = src
        .find(signature)
        .unwrap_or_else(|| panic!("no `{signature}` in this file"));
    let open = at + src[at..].find('{').expect("a fn with no body");
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..open + i];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces after `{signature}`")
}

/// [`fn_body`] with the prose taken out — **the half that makes "the body calls this" an
/// assertion rather than a spelling check.**
///
/// A body-reading test asks whether a call is there, and `fn_body` hands back comments too. So
/// deleting the call while leaving the sentence above it explaining why the call is there left
/// the test green: a test asserting the agent install swept away the readable copy of its token
/// survived its own sabotage, which is the one outcome that means the test is decoration. The
/// comments in this file name the functions they are about, deliberately and everywhere, so
/// stripping them is not optional here.
pub(super) fn code_of(body: &str) -> String {
    body.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every path under the volume, files and directories alike.
pub(super) fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        out.push(path.clone());
        if path.is_dir() && !path.is_symlink() {
            walk(&path, out);
        }
    }
}
