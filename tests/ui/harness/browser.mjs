// The ledger a browser suite keeps, and the two things it asks a page.
//
// `check` / `mustSee` / the report at the bottom were copied into five suites apiece. The copies
// had already drifted in the one way that matters to somebody reading a failure: three of them
// printed the failed names BEFORE the server log, and two after — and both `smoke.mjs` and
// `onboarding.mjs` carried a comment explaining that after is the right answer, because
// `browser_suites.rs` shows only the tail of a suite's output and names printed above a 25-line log
// are names nobody sees. One copy, so that argument is settled in one place.
//
// That argument was right about the order and wrong about what it ordered. Ordering NAMES is
// arranging the least useful thing: a name says which check failed and nothing about why, and a
// suite with more failures than the window has lines pushes every message, and the count itself,
// out of the tail whatever the order. `review.mjs` failed 62 of its 82 checks in CI and the 40-line
// window held 38 bare names — no count, no message, no server log — which was read as "the suite
// died before its first check ran" and went into a work item as that (SKEIN-623). So `report` now
// ends with a bounded block carrying the count and the first few failures WITH their reasons, and
// the tail is guaranteed to hold it however many checks failed.

/** A run's checks, and the report that names the ones that failed.
 *
 * `whole` prints a failure's entire message rather than its first line — what a suite about what a
 * first run SAYS wants, since the evidence is usually the part after the colon. */
export function ledger({ whole = false } = {}) {
  const results = [];
  const say = e => {
    const text = String((e && e.message) || e);
    return whole ? text.split("\n").map(l => "        " + l).join("\n") : "        " + text.split("\n")[0];
  };
  return {
    results,
    /** Run one check. A throw is a failure with a name, never a crashed suite.
     *
     * The formatted message is kept beside the name, not only printed: it is printed here at the
     * point of failure, which for a long suite is hundreds of lines above the end, and [`report`]
     * needs it again to put a diagnosis inside the window a reader actually sees. */
    async check(name, fn) {
      try { await fn(); results.push([true, name]); console.log(`  ok    ${name}`); }
      catch (e) { const why = say(e); results.push([false, name, why]); console.log(`  FAIL  ${name}\n${why}`); }
    },
    /** The value form: what was got, what was wanted, compared as JSON. */
    value(name, got, want) {
      const ok = JSON.stringify(got) === JSON.stringify(want);
      const why = `        got ${JSON.stringify(got)} want ${JSON.stringify(want)}`;
      results.push([ok, name, ok ? "" : why]);
      console.log(ok ? `  ok    ${name}` : `  FAIL  ${name}\n${why}`);
    },
    /** Everything that failed, said three times over, cheapest first — because the reader who
     * matters most sees only the end.
     *
     * In order: the full list of names, which is complete and may be cut; the server's log, whose
     * last line is often the whole diagnosis (`skein: reading acme: …`); and last, the block this
     * function exists for — the count, and the first `reasons` failures each with its message.
     *
     * **The closing block is bounded and the last thing printed, so a tail of any workable size
     * holds it whatever N was.** That is the property `browser_suites.rs::tail` is sized against
     * (its doc does the arithmetic): at most `reasons` × (2 + `lines`) + 3 lines — 28 as it stands,
     * 13 when the messages are one line. The old shape had no such bound — 62 failures printed 62 names
     * after the log and pushed everything worth reading out of a 40-line window.
     *
     * FIRST few rather than a sample: an early failure is usually the cause of the later ones, and
     * "first" is a rule a reader can apply without knowing how the list was built.
     *
     * A consequence worth having: the block's absence now means something. A suite whose output
     * ends without it really did die before reporting, which is what the CI log was misread as
     * saying when the block did not exist. */
    report({ log, tail = 25, reasons = 5, lines = 3 } = {}) {
      const failed = results.filter(([ok]) => !ok);
      if (!failed.length) {
        console.log(`\nall ${results.length} checks passed`);
        return failed;
      }
      console.log(`\n${failed.length} of ${results.length} checks failed:`);
      for (const [, name] of failed) console.log(`  ✗ ${name}`);
      if (log) console.log(`\nserver log:\n${log().split("\n").slice(-tail).join("\n")}`);
      const shown = failed.slice(0, reasons);
      console.log(`\n${failed.length} of ${results.length} checks failed. The first ${shown.length}, with the reason:`);
      for (const [, name, why] of shown) {
        // Capped rather than trusted: `whole: true` keeps a whole stack, and one long message
        // would spend the window the other four reasons are supposed to share.
        const said = String(why || "        (no message)").split("\n");
        const cut = said.slice(0, lines).join("\n") + (said.length > lines ? "\n        …" : "");
        console.log(`  ✗ ${name}\n${cut}`);
      }
      if (failed.length > shown.length) {
        console.log(`  … and ${failed.length - shown.length} more, named in the list above.`);
      }
      return failed;
    },
  };
}

/** Present in the DOM is not enough — it has to be on screen.
 *
 * **Waited for, up to `within`, rather than asked once.** A render returns having written the DOM;
 * whether the browser has laid it out yet is the browser's own business, and on a loaded machine
 * that gap is wider than any fixed `settle` a caller can guess. Asked once, the element reads as
 * "in the DOM but not visible", which is a sentence about a stylesheet — so the failure sends
 * whoever reads it looking for a CSS rule that was never there. That is not a hypothetical: it is
 * what `review.mjs`'s expanded-row check reported on a four-lane run, and it is the same defect
 * class as a check that waits on a duration instead of on the thing it is about (SKEIN-621).
 *
 * The assertion itself is unchanged and cannot be satisfied by patience: an element a rule really
 * is hiding has a zero box for as long as anyone waits, and fails with the same sentence and the
 * time it was given. A visible one answers on the first look, so a green run pays nothing. */
export function seeing(page, { within = 5000 } = {}) {
  return async function mustSee(sel, why) {
    const deadline = Date.now() + within;
    let el = null;
    for (;;) {
      el = await page.$(sel);
      const box = el && (await el.boundingBox());
      if (box && box.width > 0 && box.height > 0) return el;
      if (Date.now() >= deadline) break;
      await page.waitForTimeout(50);
    }
    if (!el) throw new Error(`${why}: no element matches ${sel} (waited ${within}ms)`);
    throw new Error(
      `${why}: ${sel} is in the DOM but not visible (zero box after ${within}ms) — a CSS rule is hiding it`);
  };
}

/** A fixed wait, with this suite's own default. UI-9 is the case against most of these; until they
 * are conditions, they are at least one function. */
export function settler(page, ms = 700) {
  return (n = ms) => page.waitForTimeout(n);
}

/** The trimmed text of one selector, or "" when nothing matches. */
export function texter(page) {
  return async sel => ((await page.$eval(sel, e => e.textContent).catch(() => "")) || "").trim();
}
