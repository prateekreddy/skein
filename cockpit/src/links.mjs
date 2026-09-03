// Which URLs the cockpit will follow out of text it did not write.
//
// Markdown is rendered from four sources skein does not control — a `.md` file in a box's tree, the
// model's reading of a pull request, a model answer, and an assistant message. `marked` v12 removed
// its `sanitize` option, so **nothing in the library filters URL schemes**: a `javascript:` href
// written in any of those four is emitted verbatim, and one click runs it in the cockpit's origin.
// The session cookie is `HttpOnly`, so injected script cannot read it — but it is `SameSite=Strict`
// and `Path=/`, so every same-origin `fetch` that script makes carries it. That is the whole API.
//
// The page already overrode marked's `html` renderer, which closes TAG injection completely. This is
// the other half, and it is here rather than in the page for the reason `cockpit/README.md` gives:
// a function that cannot be imported cannot be tested except by reimplementing it. This one is a
// security boundary, so it is the last function in the cockpit that should be untestable.
//
// `base` is a parameter rather than `document.baseURI` because the build refuses a module that
// touches the DOM — and because the tests need to name the base to be about anything.

/** The schemes a rendered document may navigate to. Everything else is refused. */
export const SAFE_SCHEMES = ["http:", "https:", "mailto:"];

// What the BROWSER will see in the attribute, which is not what marked puts there.
//
// This exists because of a bypass in the first version of this guard, found by testing and not by
// reading: `[c](java&#9;script:alert(1))` was allowed through. `new URL()` refuses `java&#9;script`
// as a scheme — `&`, `#` and `;` are not scheme characters — so the string parsed as a RELATIVE
// reference, resolved against the page to `http:`, and passed. The browser then HTML-decodes the
// attribute to `java<TAB>script:`, strips the control character, and executes it.
//
// So the check runs on the string after the two transformations a browser applies before it
// navigates: numeric entity decoding, and removal of the ASCII control and space characters a URL
// may not contain.
export function asBrowserSeesIt(href) {
  return String(href)
    .replace(/&#(\d+);?/g, (_, d) => String.fromCharCode(Number(d)))
    .replace(/&#x([0-9a-f]+);?/gi, (_, h) => String.fromCharCode(parseInt(h, 16)))
    .replace(/[\u0000-\u0020\u007f]/g, "");
}

/**
 * `href` if a rendered document may follow it, else `null`.
 *
 * An ALLOW-list of schemes, not a deny-list of `javascript:`, for the reason `util::valid_name`
 * gives on the Rust side: a deny-list has to anticipate every scheme a browser will ever navigate,
 * and the next one is always the one nobody listed.
 *
 * Both readings of the string are checked — as written, and as the browser will decode it — and
 * either one being unsafe refuses the link. A reference with no scheme in a given reading is
 * relative or in-page, and cannot navigate anywhere the page is not already.
 */
export function safeHref(raw, base) {
  const href = String(raw ?? "").trim();
  if (!href) return null;
  for (const form of [href, asBrowserSeesIt(href)]) {
    if (!form) return null;
    if (!form.includes(":") && !form.startsWith("//")) continue; // relative in this reading
    let scheme;
    try {
      scheme = new URL(form, base).protocol;
    } catch {
      return null; // unparseable is not a URL this page will follow
    }
    if (!SAFE_SCHEMES.includes(scheme)) return null;
  }
  return href;
}
