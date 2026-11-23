// The take-over banner, loaded by both cockpit pages. It covers the page only when the server says
// another fleet owns this volume. The button asks first, in the page, with Cancel focused.
(() => {
  const el = (tag, css, text) => {
    const node = document.createElement(tag);
    if (css) node.style.cssText = css;
    if (text !== undefined) node.textContent = text;
    return node;
  };
  const BUTTON = "font:inherit;padding:7px 14px;border-radius:6px;border:1px solid #4a4f58;"
    + "background:#24272d;color:#e6e8ec;cursor:pointer";
  const PRIMARY = BUTTON + ";background:#e0a458;border-color:#e0a458;color:#16181c";

  function show(state, note) {
    document.getElementById("fence-banner")?.remove();
    const cover = el("div", "position:fixed;inset:0;z-index:2147483647;background:#0e0f11;"
      + "color:#e6e8ec;display:flex;align-items:center;justify-content:center;padding:16px;"
      + "font:14px/1.5 ui-sans-serif,system-ui,sans-serif;overflow:auto");
    cover.id = "fence-banner";
    cover.setAttribute("role", "alertdialog");
    cover.setAttribute("aria-labelledby", "fence-title");
    const card = el("div", "max-width:560px;width:100%;background:#16181c;border:1px solid #24272d;"
      + "border-radius:10px;padding:22px");
    cover.append(card);
    document.body.append(cover);
    banner(card, state, note);
  }

  function banner(card, state, note) {
    const { owner, claim } = state;
    card.replaceChildren();
    const title = el("h2", "margin:0 0 10px;font-size:18px", "Another fleet owns these boxes");
    title.id = "fence-title";
    card.append(title);
    const who = el("p", "margin:0 0 10px");
    who.id = "fence-owner";
    who.textContent = `They belong to fleet ${owner.fleet_id} on ${owner.machine}. `
      + "Boxes will not start here while that is so.";
    card.append(who);
    card.append(el("p", "margin:0 0 14px", "Take over only if that fleet is stopped. If it is "
      + "still running, two fleets would run the same boxes and could overwrite each other's work."));
    if (note) {
      const said = el("p", "margin:0 0 14px;color:#e0a458", note);
      said.id = "fence-note";
      said.setAttribute("role", "status");
      card.append(said);
    }
    const take = el("button", PRIMARY, "Take over this fleet");
    take.type = "button";
    take.id = "fence-take";
    take.onclick = () => ask(card, state);
    card.append(take);
    if (claim) {
      card.append(el("p", "margin:16px 0 4px;color:#9aa0aa;font-size:12px", "Or run this inside the sandbox:"));
      const line = el("code", "display:block;padding:8px;background:#0e0f11;border-radius:6px;"
        + "font-size:12px;overflow-wrap:anywhere;user-select:all", claim);
      line.id = "fence-claim";
      card.append(line);
    }
    take.focus();
  }

  function ask(card, state) {
    const { owner } = state;
    card.replaceChildren();
    const title = el("h2", "margin:0 0 10px;font-size:18px", `Take over from ${owner.fleet_id}?`);
    title.id = "fence-title";
    card.append(title);
    card.append(el("p", "margin:0 0 16px", `Confirm that the fleet on ${owner.machine} is stopped. `
      + "Once this fleet takes over, that one is refused, even if it starts again. No box or file "
      + "is deleted."));
    const row = el("div", "display:flex;gap:8px;justify-content:flex-end");
    const cancel = el("button", BUTTON, "Cancel");
    cancel.type = "button";
    cancel.id = "fence-cancel";
    cancel.onclick = () => banner(card, state);
    const yes = el("button", PRIMARY, "Take over");
    yes.type = "button";
    yes.id = "fence-confirm";
    yes.onclick = () => takeOver(card, state);
    row.append(cancel, yes);
    card.append(row);
    card.onkeydown = e => { if (e.key === "Escape" && card.contains(cancel)) cancel.click(); };
    cancel.focus();
  }

  function working(card, text) {
    card.replaceChildren();
    const said = el("p", "margin:0", text);
    said.id = "fence-result";
    said.setAttribute("role", "status");
    card.append(said);
  }

  async function takeOver(card, state) {
    working(card, "Taking over…");
    let res, body;
    try {
      res = await fetch("/api/fleet/take-over", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ fleet_id: state.owner.fleet_id, epoch: state.owner.epoch }),
      });
      body = await res.json();
    } catch {
      return banner(card, state, "The cockpit did not answer, so this may not have happened. Reload to see who owns the fleet.");
    }
    if (!res.ok || !body.ok) {
      const fresh = await read();
      if (fresh?.fenced) return banner(card, fresh, body.error || "Nothing was taken over.");
      return banner(card, state, body.error || "Nothing was taken over.");
    }
    if (!body.restarting) {
      return working(card, "This fleet owns the boxes now. Restart skein-server to finish.");
    }
    working(card, "This fleet owns the boxes now. Restarting the cockpit…");
    for (let i = 0; i < 120; i++) {
      await new Promise(r => setTimeout(r, 500));
      const now = await read();
      if (now && !now.fenced) {
        const health = await fetch("/api/health").then(r => r.json()).catch(() => null);
        if (health && !health.fenced) return location.reload();
      }
    }
    working(card, "This fleet owns the boxes, but the cockpit has not come back. Reload this page.");
  }

  async function read() {
    try {
      const res = await fetch("/api/fleet/owner");
      return res.ok ? await res.json() : null;
    } catch { return null; }
  }

  const start = async () => {
    const state = await read();
    if (state?.fenced) show(state);
  };
  if (document.body) start();
  else document.addEventListener("DOMContentLoaded", start);
})();
