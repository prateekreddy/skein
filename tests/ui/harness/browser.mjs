// The ledger a browser suite keeps, and the two things it asks a page.
//
// `check` / `mustSee` / the report at the bottom were copied into five suites apiece. The copies
// had already drifted in the one way that matters to somebody reading a failure: three of them
// printed the failed names BEFORE the server log, and two after — and both `smoke.mjs` and
// `onboarding.mjs` carried a comment explaining that after is the right answer, because
// `browser_suites.rs` shows only the tail of a suite's output and names printed above a 25-line log
// are names nobody sees. One copy, so that argument is settled in one place.

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
    /** Run one check. A throw is a failure with a name, never a crashed suite. */
    async check(name, fn) {
      try { await fn(); results.push([true, name]); console.log(`  ok    ${name}`); }
      catch (e) { results.push([false, name]); console.log(`  FAIL  ${name}\n${say(e)}`); }
    },
    /** The value form: what was got, what was wanted, compared as JSON. */
    value(name, got, want) {
      const ok = JSON.stringify(got) === JSON.stringify(want);
      results.push([ok, name]);
      console.log(ok ? `  ok    ${name}`
        : `  FAIL  ${name}\n        got ${JSON.stringify(got)} want ${JSON.stringify(want)}`);
    },
    /** Everything that failed, after saying so. The server's log first and the names last, because
     * a truncated view shows the tail: names above the log are names nobody reads. */
    report({ log, tail = 25 } = {}) {
      const failed = results.filter(([ok]) => !ok);
      if (failed.length && log) console.log(`\nserver log:\n${log().split("\n").slice(-tail).join("\n")}`);
      console.log(failed.length
        ? `\n${failed.length} of ${results.length} checks failed:`
        : `\nall ${results.length} checks passed`);
      for (const [, name] of failed) console.log(`  ✗ ${name}`);
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
