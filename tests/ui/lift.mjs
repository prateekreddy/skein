// Lift a top-level declaration out of index.html by name, so the page's pure logic can be tested
// in plain node — which works inside a box, where `smoke.mjs` cannot run at all (it needs chromium's
// system libraries and a box has no working sudo to install them).
//
// Shared by `voice.mjs` and `tabs.mjs`. It lives here rather than being copied into each because it
// is a brace matcher, and two copies of a subtle brace matcher is one that quietly drifts.
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
export const page = readFileSync(join(root, "src", "web", "index.html"), "utf8");

// Brace-matched rather than regex-to-end-of-line, because these span lines; a wrong slice throws
// here rather than silently testing a truncated function.
export function grab(name) {
  // `async function` is listed before `function` on purpose: `indexOf("\nfunction attachFiles(")`
  // simply misses an async declaration, and the miss reads as "did it get renamed?" — which is a
  // confusing thing to be told about a function that is right there.
  for (const start of [`async function ${name}(`, `function ${name}(`, `const ${name} =`, `let ${name} =`]) {
    const at = page.indexOf(`\n${start}`);
    if (at < 0) continue;
    const from = at + 1;
    const isFn = start.endsWith("(");
    let depth = 0, opened = false;
    for (let i = from; i < page.length; i++) {
      const c = page[i];
      if (isFn) {
        // Braces only. Counting the parameter list's parens too would end the function at `)` on
        // its very first line, which is a slice that parses and tests nothing.
        if (c === "{") { depth++; opened = true; }
        else if (c === "}" && --depth === 0 && opened) return page.slice(from, i + 1);
        continue;
      }
      if (c === "{" || c === "[" || c === "(") depth++;
      else if (c === "}" || c === "]" || c === ")") depth--;
      // A declaration ends at the first line break outside any bracket — which is also the right
      // answer for `let a = 0, b = null;`, where there are no brackets to have opened at all.
      else if (c === "\n" && depth === 0) return page.slice(from, i);
    }
  }
  throw new Error(`could not lift \`${name}\` out of index.html — did it get renamed?`);
}

// A cockpit module, as source a lifted world can evaluate.
//
// `grab` lifts a declaration out of the page; this lifts a whole module out of `cockpit/src`, which
// is where the page's PURE functions live. A world whose lifted functions call one — the review
// pane's grouping calls `moveOf`, the row calls `moveWhy` — would otherwise have to stub it, and a
// stub of a rule is the rule written twice: exactly what putting it in `cockpit/src` was for.
//
// The transform is `export ` removed and nothing else, byte for byte what `cockpit/build.mjs` does
// to produce the bundle the browser loads, so a node world and a real page run the same code.
export function pure(name) {
  const text = readFileSync(join(root, "cockpit", "src", `${name}.mjs`), "utf8");
  return text.replace(/^export\s+/gm, "");
}

// The `skein-server` binary the browser suites drive — one resolver, because the build policy is
// the part that must not drift between them.
//
// Each suite used to run `cargo build --bin skein-server` itself, which under `cargo test` meant a
// second cargo re-taking the build-directory lock and re-walking the dependency graph mid-run — on
// a box where other builds share that lock, an open-ended stall and a burst of load right as the
// suites' own timeouts start ticking. That contention is SKEIN-119: review.mjs failed its
// ownership check about one workspace run in four, and never standalone.
//
// So there are two paths, and both must keep working:
// - Driven from tests/browser_suites.rs, SKEIN_SERVER_BIN names the binary the surrounding
//   `cargo test` has ALREADY built (`CARGO_BIN_EXE_skein-server`), and no cargo runs here at all.
// - Run by hand (`node tests/ui/review.mjs`), the variable is absent and the build happens here,
//   exactly as before — a fresh checkout still needs only the one command.
export function serverBinary() {
  const given = process.env.SKEIN_SERVER_BIN;
  if (given) return given;
  const build = spawnSync("cargo", ["build", "--bin", "skein-server"], { cwd: root, stdio: "inherit" });
  if (build.status !== 0) throw new Error("cargo build failed");
  return join(root, "target", "debug", "skein-server");
}

// The tiny assert harness both suites share.
export function harness() {
  let failures = 0;
  return {
    check(what, got, want) {
      const ok = JSON.stringify(got) === JSON.stringify(want);
      if (!ok) {
        failures++;
        console.error(`✗ ${what}\n   got  ${JSON.stringify(got)}\n   want ${JSON.stringify(want)}`);
      } else console.log(`✓ ${what}`);
    },
    done() {
      console.log(failures ? `\n${failures} failed` : "\nall good");
      process.exit(failures ? 1 : 0);
    },
  };
}
