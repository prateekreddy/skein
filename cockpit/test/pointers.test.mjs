// Every "Settings → X" a person can read names a place the cockpit actually has (SKEIN-1188).
//
// A pointer is only worth anything if following it arrives somewhere. They are written in Rust
// error strings, shell scripts and the page, far from `shell.html` where the panes are, so they
// drifted: "Settings → Repos" on a button whose pane reads "Repositories", "Settings → GitHub App
// ID" with the pane between them left out, and "Settings → scope each box's access to its own
// repo" for a switch that is under "GitHub & keys" and spelled differently. None of those failed
// anything.
//
// The rule, read out of the markup rather than kept here: the words after "Settings → " start with
// the label of one of the panes in the settings nav. A further "→ X" whose X starts with a capital
// is the name of a place too, and has to be a heading or a field title in THAT pane; a lower-case
// one is an instruction ("→ add a GitHub App…") and is not a place.
//
// Only what a person reads is checked: comments are skipped, since they are written for the next
// programmer and are allowed to say how a pointer used to read.
//
// Fails on: writing "Settings → Repos" back into review.js; writing "Settings → GitHub App ID" back
// into src/gitgate/mint.rs; renaming the "Fleet" pane without its pointers.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const ROOT = new URL("../../", import.meta.url).pathname;
const read = rel => readFileSync(join(ROOT, rel), "utf8");

const decode = s => s.replace(/&amp;/g, "&").replace(/&rarr;/g, "→").replace(/&#39;/g, "'");
const text = html => decode(html.replace(/<svg[\s\S]*?<\/svg>/g, "").replace(/<[^>]+>/g, "")).trim();

// The panes, and what each one has in it that a pointer could name.
function panes() {
  const shell = read("src/web/app/shell.html").replace(/<!--[\s\S]*?-->/g, "");
  const found = new Map();
  for (const m of shell.matchAll(/<button[^>]*class="set-navi[^"]*"[^>]*data-pane="([a-z]+)"[^>]*>([\s\S]*?)<\/button>/g)) {
    // The label is the nav's own text, less the count pill beside it.
    const label = text(m[2].replace(/<span class="pill"[^>]*><\/span>/g, ""));
    const body = shell.match(new RegExp(`<section class="set-pane[^"]*" data-pane="${m[1]}">([\\s\\S]*?)</section>`));
    const places = [...(body?.[1] || "").matchAll(/<(?:span|div) class="set-(?:title|head)">([\s\S]*?)(?:<span class="(?:sub|set-opt|pill)"|<\/span>|<\/div>)/g)]
      .map(t => text(t[1]))
      .filter(Boolean);
    found.set(label, places);
  }
  return found;
}

// Every file a person-facing string can come from, less the built page (it is these parts).
function sources() {
  const out = [];
  const walk = dir => {
    for (const name of readdirSync(join(ROOT, dir))) {
      const rel = join(dir, name);
      if (statSync(join(ROOT, rel)).isDirectory()) walk(rel);
      else if (/\.(rs|js|mjs|sh|html)$/.test(name) && rel !== "src/web/index.html" && !rel.startsWith("src/web/vendor/")) out.push(rel);
    }
  };
  walk("src");
  out.push("bootstrap.sh");
  return out;
}

// A file's text with its comments gone and its Rust `\` continuations joined, so a pointer broken
// across two source lines is read as the one sentence it prints as.
function readable(rel) {
  let body = read(rel);
  if (rel.endsWith(".html")) body = body.replace(/<!--[\s\S]*?-->/g, "");
  const comment = rel.endsWith(".sh") ? /^\s*#/ : /^\s*(\/\/|\*|\/\*)/;
  const lines = body.split("\n").filter(l => !comment.test(l));
  return rel.endsWith(".rs") ? lines.join("\n").replace(/\\\n\s*/g, "") : lines.join("\n");
}

// Pointers into OTHER products' settings, which are not the cockpit's to have. Named, each with
// where it is, so this list cannot quietly become a way to excuse the cockpit's own.
const ELSEWHERE = [
  "Developer settings",      // github.com's, in src/health/token.rs
  "Personal access tokens",  // Plane's, in src/web/app/settings.js
];

// Wrong pointers in files another lane owns while this was written (SKEIN-1188). Each is theirs to
// fix; the entry is deleted when they do, and this test fails until it is.
const PENDING = [
  // src/bin/skein.rs (`skein doctor`): the "Overwrite token on startup" switch was deleted.
  "Overwrite token on startup",
];

test("every Settings pointer names a pane, and a named place in it exists", () => {
  const known = panes();
  // The markup was read, not assumed: every pane the nav offers today, each with something in it.
  assert.deepEqual([...known.keys()].sort(), ["Boxes", "Diagnostics", "Fleet", "GitHub & keys",
    "Repositories", "Shortcuts", "Update", "Usage", "Work tracking"]);
  assert.ok(known.get("GitHub & keys").includes("GitHub App ID"), "the pane's titles were not read");

  const wrong = [];
  let seen = 0;
  for (const rel of sources()) {
    for (const m of readable(rel).matchAll(/Settings (?:→|->) ([^"`<\n]{1,120})/g)) {
      const said = decode(m[1]);
      if (ELSEWHERE.some(e => said.startsWith(e))) continue;
      seen++;
      const pane = [...known.keys()].find(p => said.startsWith(p) && !/^[\w]/.test(said.slice(p.length)));
      if (!pane) {
        if (!PENDING.some(p => said.startsWith(p))) wrong.push(`${rel}: "Settings → ${said.slice(0, 50)}" — no pane is called that`);
        continue;
      }
      const deeper = said.slice(pane.length).match(/^ (?:→|->) (.*)$/);
      if (deeper && /^[A-Z]/.test(deeper[1])) {
        const places = known.get(pane);
        if (!places.some(p => deeper[1].toLowerCase().startsWith(p.toLowerCase())))
          wrong.push(`${rel}: "Settings → ${pane} → ${deeper[1].slice(0, 50)}" — ${pane} has no such heading (it has: ${places.join(" · ")})`);
      }
    }
  }
  // Non-vacuity: a scan that matched nothing would pass everything.
  assert.ok(seen > 20, `only ${seen} pointers were found, so this read the wrong files`);
  assert.deepEqual(wrong, []);
});

test("every pending exception is still pending", () => {
  // An exception that no longer matches anything is one somebody fixed; it goes, so the list is
  // never a place a new wrong pointer can hide behind an old entry.
  const all = sources().map(readable).join("\n");
  for (const p of PENDING) assert.ok(all.includes(`Settings → ${p}`), `"${p}" was fixed — delete it from PENDING`);
});
