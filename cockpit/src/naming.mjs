// A box's name, in the browser, agreeing with the one skein computes in Rust.
//
// **A mirror, and mirrors drift** — which is why `src/cockpit.rs` runs this and `util::slug` over
// the same cases and compares. The alternative was asking the server for the name before creating
// the box, and that is a round trip on every keystroke to compute a string neither end disagrees
// about; the alternative to *that* is showing a placeholder shaped like a name, which is how
// somebody ends up creating `thing-<branch>`.
//
// The rule, from `util::slug`: an sbx sandbox name cannot carry every branch character — `/`
// especially — so the *name* is a slug of the branch while the real branch travels in the launch
// spec and reaches `git checkout` intact. Same branch, same name, so reconnect and lookup agree.

export function slug(s) {
  let out = "";
  let prevDash = false;
  for (const c of String(s ?? "")) {
    // ASCII alphanumerics only, deliberately: the Rust side asks `is_ascii_alphanumeric`, and a
    // JavaScript `\w` or a unicode-aware test would keep characters it drops — which is a name the
    // browser shows and the fleet does not have.
    if (/[A-Za-z0-9._-]/.test(c) && c.charCodeAt(0) < 128) {
      out += c;
      prevDash = false;
    } else if (!prevDash) {
      out += "-";
      prevDash = true;
    }
  }
  return out.replace(/^-+/, "").replace(/-+$/, "");
}

// `<repo-id>-<branch-slug>`, which is `repos::box_name`.
export const boxNameFor = (repoId, branch) => `${repoId}-${slug(branch)}`;
