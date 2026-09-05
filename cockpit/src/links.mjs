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

// Every named character reference whose expansion is a single ASCII character.
//
// The whole set, not the ones that look dangerous: the scheme is what is being protected, and any
// ASCII character can end up in a scheme position. `&colon;`, `&Tab;` and `&NewLine;` are the three
// that were used to walk through this guard, but a list chosen by imagining attacks is the same
// mistake as a deny-list of schemes. The uppercase spellings (`&AMP;`, `&LT;`, `&GT;`, `&QUOT;`) and
// the second names for the same character (`&midast;`, `&lbrack;`, `&vert;`, `&lbrace;`) are here
// for the same reason: a reference this table misses is one the browser still decodes, so it would
// survive normalisation and be decoded *after* the check.
//
// No entry produces a non-ASCII character. `&nbsp;` and its kind are left alone deliberately — they
// cannot contribute a scheme character, and decoding more than the guard needs is its own bug.
const ASCII_NAMED = {
  Tab: "\t", NewLine: "\n",
  excl: "!", quot: '"', QUOT: '"', num: "#", dollar: "$", percnt: "%",
  amp: "&", AMP: "&", apos: "'", lpar: "(", rpar: ")", ast: "*", midast: "*",
  plus: "+", comma: ",", period: ".", sol: "/", colon: ":", semi: ";",
  lt: "<", LT: "<", equals: "=", gt: ">", GT: ">", quest: "?", commat: "@",
  lsqb: "[", lbrack: "[", bsol: "\\", rsqb: "]", rbrack: "]", Hat: "^", lowbar: "_",
  grave: "`", DiacriticalGrave: "`", lcub: "{", lbrace: "{",
  verbar: "|", vert: "|", VerticalLine: "|", rcub: "}", rbrace: "}",
};

// A numeric reference's character, the way the HTML parser produces it: an unpaired surrogate, a
// zero, or anything past the last code point is U+FFFD rather than an invented character.
function fromCode(code) {
  const bad = !(code > 0 && code <= 0x10ffff) || (code >= 0xd800 && code <= 0xdfff);
  return bad ? "\ufffd" : String.fromCodePoint(code);
}

/** How many decoding passes a settled string is allowed to need. See [`asBrowserSeesIt`]. */
const MAX_DECODE_PASSES = 16;

// One pass of the decoding an HTML parser does to an attribute value.
//
// A named reference is decoded only when it ends in `;`. Browsers also decode a handful of legacy
// names without one (`&amp`, `&lt`, `&gt`, `&quot`), but only where the next character is neither
// alphanumeric nor `=`, and none of those four produce a character that can build a scheme — so
// requiring the semicolon costs nothing and keeps this readable. Numeric references are decoded
// with or without it, which is what browsers do.
function decodeOnce(s) {
  return s.replace(
    /&#(\d{1,8});?|&#[xX]([0-9a-fA-F]{1,7});?|&([A-Za-z][A-Za-z0-9]{1,15});/g,
    (whole, dec, hex, name) => {
      if (dec !== undefined) return fromCode(Number(dec));
      if (hex !== undefined) return fromCode(parseInt(hex, 16));
      return Object.prototype.hasOwnProperty.call(ASCII_NAMED, name) ? ASCII_NAMED[name] : whole;
    }
  );
}

/**
 * The string the browser will hand its URL parser, given this href in an attribute.
 *
 * Two transformations, in the order the browser applies them.
 *
 * **Entity decoding, to a fixed point.** This exists because of a bypass in the first version of
 * this guard, found by testing and not by reading: `[c](java&#9;script:alert(1))` was allowed
 * through. `new URL()` refuses `java&#9;script` as a scheme — `&`, `#` and `;` are not scheme
 * characters — so the string parsed as a RELATIVE reference, resolved against the page to `http:`,
 * and passed; the browser then decoded the attribute to `java<TAB>script:` and ran it. The named
 * spellings of the same trick (`java&Tab;script:`, `javascript&colon;`) walked through the fix for
 * it, which decoded numeric references only.
 *
 * It runs to a fixed point rather than once because [`safeHref`] hands this string back to be
 * written into the document: one pass turns `java&amp;#9;script:` into `java&#9;script:`, which is
 * inert in the browser that produced it and live in the one that reads it back. Each pass replaces
 * a reference with a single character, so a pass that changes anything shortens the string — but a
 * README is not obliged to be sane, and `&amp;` nested ten thousand deep would be ten thousand scans
 * of it. Hence [`MAX_DECODE_PASSES`]: past it the string is not settled, and [`safeHref`] refuses
 * rather than guessing which reading the browser will take.
 *
 * **Then what the URL parser discards**: tab, newline and carriage return anywhere in the string,
 * and leading or trailing C0 controls and spaces. Exactly those — an interior space is not removed,
 * because the URL parser percent-encodes it rather than closing the gap, and a normaliser that
 * closed it would turn `http://e.com/a b` into a different address than the one somebody wrote.
 */
export function asBrowserSeesIt(href) {
  let s = String(href);
  for (let i = 0; i < MAX_DECODE_PASSES; i++) {
    const next = decodeOnce(s);
    if (next === s) break;
    s = next;
  }
  return s.replace(/[\t\n\r]/g, "").replace(/^[\u0000-\u0020]+|[\u0000-\u0020]+$/g, "");
}

/**
 * The href a rendered document may follow, normalised as the browser will read it — or `null`.
 *
 * An ALLOW-list of schemes, not a deny-list of `javascript:`, for the reason `util::valid_name`
 * gives on the Rust side: a deny-list has to anticipate every scheme a browser will ever navigate,
 * and the next one is always the one nobody listed.
 *
 * Three readings of the string are checked and any one of them being unsafe refuses the link: as
 * written; as the browser will decode it; and that decoding with every ASCII control and space
 * taken out, which is stricter than any browser and is there to catch a separator this code does
 * not know is a separator. A reference with no scheme in a given reading is relative or in-page, and
 * cannot navigate anywhere the page is not already.
 *
 * **The return value is the decoded form, not the string that came in**, and the caller must write
 * it back into the token it came from. Otherwise the guard judges one string and the browser parses
 * another, which is the whole shape of the bypasses above.
 */
export function safeHref(raw, base) {
  const href = String(raw ?? "").trim();
  if (!href) return null;
  const seen = asBrowserSeesIt(href);
  if (!seen) return null;
  if (decodeOnce(seen) !== seen) return null; // still decoding after MAX_DECODE_PASSES; not a link
  for (const form of [href, seen, seen.replace(/[\u0000-\u0020\u007f]/g, "")]) {
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
  return seen;
}
