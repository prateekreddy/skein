// The Settings → Diagnostics row for containers and cgroups no box owns, drawn by the real
// `renderDiagnostics` lifted out of index.html.
//
// What the Rust side cannot show: what a person actually sees. The owner's decisions about this row
// are all about the page. It is marked `!`, never ✗, because nothing is broken. It adds nothing to
// the fault count on the pill. Its commands are shown to be copied and there is NO button, because
// a Stop button was declined so that the server never runs `docker stop` itself. The destructive
// commands carry the same warning the other rows use.
//
// Each check below names the change that would make it fail.
//
//   node tests/ui/unowned.mjs
import { esc, grab, harness, stubDom } from "./lift.mjs";

const t = harness();

function draw(health) {
  const { reg, document } = stubDom(["set-diag", "set-diagn"]);
  const src = `
    ${grab("esc")}
    let lastHealth = null;
    ${grab("renderDiagnostics")}
    return h => { lastHealth = h; renderDiagnostics(); };
  `;
  new Function("document", src)(document)(health);
  return { html: reg.get("set-diag").innerHTML, pill: reg.get("set-diagn").textContent };
}

// The row as the server sends it for the approved example: two containers, one gone box's cgroup
// still holding processes.
const found = {
  ok: true,
  unowned: {
    clear: false,
    said: [
      { text: "2 containers belong to no box skein can name: pg-scratch (1.4G memory, 3.2G on disk) and a3f9c01e (210M, 0.4G), 1.6G memory and 3.6G disk in all. Stopping or destroying a box never stops them, and what they use counts against every box." },
      {
        text: "A container whose label names a gone box: pg-scratch was started by box-old, which no longer exists.",
        offers: [
          { lead: "stopping keeps them and frees their memory:", command: "sbx exec example docker stop pg-scratch a3f9c01e" },
          { lead: "skein did not start them and does not remove them; if they are yours to delete:", command: "sbx exec example docker rm -f pg-scratch a3f9c01e", destructive: true },
        ],
      },
      {
        text: "box-old no longer exists, but its cgroup still holds 3 processes using 180M, and they count against the fleet's memory. skein removes a gone box's cgroup only once it is empty.",
        offers: [
          { lead: "see them:", command: "sbx exec example cat /sys/fs/cgroup/skein/box-old/cgroup.procs" },
          { lead: "if they are yours to end:", command: "sbx exec example sudo sh -c 'echo 1 > /sys/fs/cgroup/skein/box-old/cgroup.kill'", destructive: true },
        ],
      },
    ],
  },
};
const { html, pill } = draw(found);

// Fails if the row is drawn as ✗ (class `bad`) or loses its label.
t.check("found is marked ! under the approved label",
  /<div class="dg-row warn">\s*<span class="dg-mark">!<\/span>\s*<span class="dg-name">unowned containers<\/span>/.test(html), true);
// Fails if the row is counted as a fault on the pill.
t.check("the pill counts no fault for it", pill, "");
// Fails if a command stops being shown whole, as something to copy.
for (const command of [
  "sbx exec example docker stop pg-scratch a3f9c01e",
  "sbx exec example docker rm -f pg-scratch a3f9c01e",
  "sbx exec example cat /sys/fs/cgroup/skein/box-old/cgroup.procs",
  "sbx exec example sudo sh -c 'echo 1 > /sys/fs/cgroup/skein/box-old/cgroup.kill'",
]) t.check(`copyable: ${command}`, html.includes(`<code>${esc(command)}</code>`), true);
// Fails if a Stop (or any) button appears: the owner declined one.
t.check("no button anywhere in the row", /<button|onclick/i.test(html), false);
// Fails if the destructive warning goes missing, or lands on a safe command too.
t.check("the two destructive commands, and only those, say so",
  (html.match(/destroys something — copy it and run it yourself/g) || []).length, 2);
t.check("the safe stop is not marked destructive",
  /docker stop pg-scratch a3f9c01e<\/code><\/div>/.test(html), true);
// Fails if the approved sentences are not what reaches the page.
for (const s of found.unowned.said) {
  t.check(`said: ${s.text.slice(0, 40)}…`, html.includes(s.text.replace(/'/g, "&#39;")), true);
}

// Fails if a clear report is drawn as anything but a tick.
const clear = draw({
  ok: true,
  unowned: { clear: true, said: [{ text: "every running container belongs to a box" }] },
}).html;
t.check("nothing unowned is a tick",
  /<div class="dg-row ok">\s*<span class="dg-mark">✓<\/span>\s*<span class="dg-name">unowned containers<\/span>/.test(clear), true);

// Fails if a row is invented before the server's first pass has said anything.
t.check("no report yet, no row", draw({ ok: true }).html.includes("unowned containers"), false);

t.done();
