// Does `serverBinary()` refuse a `$SKEIN_SERVER_BIN` that is older than a page it embeds, and
// leave a fresh one alone? (SKEIN-887)
//
// **What this replaces.** `serverBinary()` used to return `$SKEIN_SERVER_BIN` verbatim: a binary
// built before the last edit to `src/web/index.html` still ran and still answered HTTP requests, so
// a browser suite driven by hand (`SKEIN_SERVER_BIN=<old build> node tests/ui/<suite>.mjs`) checked
// markup that was not in the tree, and passed or failed against a page nobody could see by reading
// the repository. `tests/browser_suites.rs` never hit this — it always hands over a binary `cargo
// test`'s own build phase just produced, strictly before any suite runs — which is why the hole was
// in the by-hand loop and nothing a `cargo test` run could have caught on its own.
//
// **No fixture repo for the staleness cases.** [`embeddedWebAssets`] and [`serverBinary`] are
// exercised against the checkout this test runs in — a stand-in `src/` would only prove a reader
// that never runs for real works on a page that never ships. What is faked is the one thing the item
// is about: the BINARY. It is a plain file, never executed, because only its mtime is read. The
// empty-repo case at the bottom is the exception, and says why there.
import { mkdirSync, mkdtempSync, rmSync, statSync, utimesSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { embeddedWebAssets, harness, serverBinary } from "./lift.mjs";

const { check, done } = harness();

const scratch = mkdtempSync(path.join(os.tmpdir(), "skein-stalebin-"));
const fakeBin = path.join(scratch, "skein-server");
writeFileSync(fakeBin, "stand-in for a built skein-server; only its mtime is read by this suite");

const savedEnv = Object.prototype.hasOwnProperty.call(process.env, "SKEIN_SERVER_BIN")
  ? process.env.SKEIN_SERVER_BIN
  : undefined;
const restoreEnv = () => {
  if (savedEnv === undefined) delete process.env.SKEIN_SERVER_BIN;
  else process.env.SKEIN_SERVER_BIN = savedEnv;
};

try {
  const assets = embeddedWebAssets();
  check("derives at least one embedded web asset from src/**/*.rs", assets.length > 0, true);
  // Not a hardcoded count: `src/cockpit.rs` alone embeds `web/index.html`, `web/v2.html` and
  // `web/vendor/cockpit.js` today, and the whole point of deriving rather than listing is that this
  // suite must not need updating the day a fourth site is added. What it DOES assert is the shape:
  // every derived path is a real file inside `src/web/`, which is the guarantee `serverBinary()`'s
  // refusal depends on to say anything true.
  check("every derived asset is a real file under src/web/",
    assets.every(a => a.includes(`${path.sep}src${path.sep}web${path.sep}`) && statSync(a).isFile()),
    true);

  const newestAssetMs = Math.max(...assets.map(a => statSync(a).mtimeMs));

  // Case 1: a binary built a full hour before the newest embedded page.
  //
  // Named change that breaks this: drop the staleness check out of `serverBinary()` — restore
  // `if (given) return given;` with no call to `refuseIfStale` — and this assertion goes from
  // "throws" to "does not throw", because nothing SKEIN_SERVER_BIN names is ever inspected again.
  process.env.SKEIN_SERVER_BIN = fakeBin;
  const older = new Date(newestAssetMs - 60 * 60 * 1000);
  utimesSync(fakeBin, older, older);
  let staleError = null;
  try {
    serverBinary();
  } catch (e) {
    staleError = e;
  }
  check("refuses a $SKEIN_SERVER_BIN older than an embedded page", Boolean(staleError), true);
  check("the refusal names the file it is older than",
    staleError ? assets.some(a => staleError.message.includes(path.basename(a))) : false,
    true);
  check("the refusal names the rebuild command",
    staleError ? staleError.message.includes("cargo build --bin skein-server --bin skein") : false,
    true);

  // Case 2: the same file, now built an hour after the newest embedded page.
  //
  // Named change that breaks this: invert the sense of the comparison in `refuseIfStale`
  // (`binMtime > assetMtime` triggers the refusal instead of `binMtime < assetMtime`), which turns
  // "newer is fine" into "newer is refused" — this assertion goes from "does not throw, returns the
  // path" to "throws".
  const newer = new Date(newestAssetMs + 60 * 60 * 1000);
  utimesSync(fakeBin, newer, newer);
  let result = null;
  let freshError = null;
  try {
    result = serverBinary();
  } catch (e) {
    freshError = e;
  }
  check("accepts a $SKEIN_SERVER_BIN newer than every embedded page", freshError, null);
  check("and returns it unchanged", result, fakeBin);
} finally {
  restoreEnv();
  rmSync(scratch, { recursive: true, force: true });
}

// A "repo" that embeds nothing the way `src/cockpit.rs` does is refused rather than answered with an
// empty, silently-useless list — this IS a fixture repo, because the whole point is a `src/` that
// derives no call site, and the real checkout can never be that.
//
// Named change that breaks this: change `if (!found.size)` to `if (false)` (or otherwise drop the
// guard) in `embeddedWebAssets` — this assertion goes from "throws" to "returns []", which is
// exactly the SKEIN-647 shape the rest of this repository's derive-and-refuse functions exist to
// avoid: a scan a rename has quietly broken keeps answering "clean" forever.
const emptyRepo = mkdtempSync(path.join(os.tmpdir(), "skein-stalebin-empty-"));
try {
  mkdirSync(path.join(emptyRepo, "src"), { recursive: true });
  writeFileSync(path.join(emptyRepo, "src", "lib.rs"),
    '// no include_str!/include_bytes! of anything under web/ here\nconst X: &str = "hello";\n');
  let refused = null;
  try {
    embeddedWebAssets(emptyRepo);
  } catch (e) {
    refused = e;
  }
  check("refuses to run at all when it derives no embedded web asset", Boolean(refused), true);
} finally {
  rmSync(emptyRepo, { recursive: true, force: true });
}

done();
