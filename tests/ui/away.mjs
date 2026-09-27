// The away digest as the page wires it: what it asks the server, and when it tells the server the
// board has been looked at. Drawn by the real `showAwayDigest` and `markSeen` lifted out of
// index.html; the digest's own reading (several moments to one line per box) is
// `cockpit/test/away.test.mjs`.
//
// What this protects is the half SKEIN-15 left undone: the server keeps the journal and the mark
// (`/api/away`, `/api/away/seen`), and a page that computes its own delta beside them is the design
// the server was built to replace.
//
// Each check below names the change that would make it fail.
//
//   node tests/ui/away.mjs
import { grab, harness } from "./lift.mjs";
import { awayItems } from "../../cockpit/src/away.mjs";
import { groupOf } from "../../cockpit/src/groups.mjs";

const t = harness();

async function run(answer, boxes) {
  const asked = [], drawn = [];
  const fetch = async (url, init = {}) => {
    asked.push([init.method || "GET", url]);
    return { json: async () => answer };
  };
  const src = `
    ${grab("markSeen")}
    ${grab("showAwayDigest")}
    return showAwayDigest;
  `;
  const renderAwayOverlay = (items, since) => drawn.push({ items, since });
  const show = new Function("fetch", "boxes", "awayItems", "groupOf", "renderAwayOverlay", src)(
    fetch, boxes, awayItems, groupOf, renderAwayOverlay);
  await show();
  await new Promise(r => setTimeout(r, 0));
  return { asked, drawn };
}

// A night away: one box finished while nobody looked.
{
  const { asked, drawn } = await run(
    { since: "2026-09-27T06:00:00Z", moments: [{ at: "2026-09-27T07:00:00Z", name: "alpha", from: "working", to: "done" }] },
    [{ name: "alpha", state: "done" }],
  );
  // Fails if the page stops asking the server and goes back to a delta of its own.
  t.check("the digest is read from the server", asked[0], ["GET", "/api/away"]);
  // Fails if the server's moments are not what is drawn.
  t.check("what the server remembered is what is drawn", drawn.map(d => d.items.map(i => [i.name, i.kind])), [[["alpha", "done"]]]);
  // Fails if the server's mark is not passed through, so the header could not say since when.
  t.check("the header is told the server's mark", drawn[0]?.since, "2026-09-27T06:00:00Z");
  // Fails if showing the digest acknowledges it: a digest nobody has read yet must survive a reload.
  t.check("a digest on screen is not yet acknowledged", asked.some(([m]) => m === "POST"), false);
}

// Nothing happened: the board as it stands has been seen, so the mark moves.
{
  const { asked, drawn } = await run({ since: "", moments: [] }, [{ name: "alpha", state: "working" }]);
  // Fails if an empty digest is drawn as an empty card.
  t.check("nothing to say draws nothing", drawn.length, 0);
  // Fails if an empty digest leaves the mark where it was, so the next one repeats old news.
  t.check("an empty digest marks the board seen", asked.slice(1), [["POST", "/api/away/seen"]]);
}

t.done();
