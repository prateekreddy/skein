// What the away digest lists, from the server's journal of state changes (`GET /api/away`).
//
// The journal is the server's, and so is the mark it is read from (`src/stream.rs`): a delta kept in
// a tab's memory dies on reload and disagrees with every other tab, which is why the page no longer
// computes one of its own. What is left here is only the reading — several moments for one box
// become one line about where it ended up.
//
// `groupOf` is an argument rather than an import because this module is concatenated into the page
// beside `groups.mjs` (see cockpit/build.mjs), and a leaf takes its dependencies as arguments.

// What a box that ENDED UP in each group did while you were away, and how high it sorts: what needs
// you first, then what finished, then what merely moved.
const SAID = {
  attn: [0, "now needs a decision"],
  waiting: [0, "is waiting for your next instruction"],
  error: [0, "hit an error"],
  done: [1, "finished"],
  working: [3, "went back to work"],
  ended: [2, "ended"],
  idle: [3, "went idle"],
  stale: [2, "stopped reporting"],
};

export function awayItems(moments, boxes, groupOf) {
  const now = new Map((boxes || []).map(b => [b.name, b]));
  // Oldest first, as the server keeps them, so the first moment holds where a box started from and
  // the last where it is now.
  const by = new Map();
  for (const m of moments || []) {
    const seen = by.get(m.name);
    if (seen) seen.to = m.to;
    else by.set(m.name, { from: m.from, to: m.to });
  }
  const items = [];
  for (const [name, { from, to }] of by) {
    const box = now.get(name);
    if (!box) {
      items.push({ pri: 2, name, kind: "gone", text: "left the board (destroyed or merged)" });
      continue;
    }
    const g = groupOf(to);
    // A box that went somewhere and came back is where you left it, and saying otherwise is noise.
    if (g === groupOf(from)) continue;
    const [pri, text] = SAID[g] || SAID.stale;
    const headline = box.headline ? ` — ${box.headline}` : "";
    items.push({ pri, name, kind: g, text: text + (pri === 0 ? headline : "") });
  }
  items.sort((a, b) => a.pri - b.pri || a.name.localeCompare(b.name));
  return items;
}
