// Lift a top-level declaration out of index.html by name, so the page's pure logic can be tested
// in plain node — which works inside a box, where `smoke.mjs` cannot run at all (it needs chromium's
// system libraries and a box has no working sudo to install them).
//
// Shared by `voice.mjs` and `tabs.mjs`. It lives here rather than being copied into each because it
// is a brace matcher, and two copies of a subtle brace matcher is one that quietly drifts.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
export const page = readFileSync(join(root, "src", "web", "index.html"), "utf8");

// Brace-matched rather than regex-to-end-of-line, because these span lines; a wrong slice throws
// here rather than silently testing a truncated function.
export function grab(name) {
  for (const start of [`function ${name}(`, `const ${name} =`, `let ${name} =`]) {
    const at = page.indexOf(`\n${start}`);
    if (at < 0) continue;
    const from = at + 1;
    const isFn = start.startsWith("function");
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
