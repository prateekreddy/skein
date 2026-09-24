// ---------- per-box load: which box is eating the machine ----------
//
// On demand rather than on the strip, for two reasons. It costs a measured half-second in the
// sandbox — CPU is a rate, and a cgroup only keeps a running total — and the question is one you ask
// at a moment ("why is my laptop hot"), not one worth answering thirty times an hour.
//
// A box using every core is legitimate: `cpu.weight` is equal and uncapped on purpose, so a lone box
// gets the whole machine and hands it back the moment another wants it. What was missing is the
// name — "the fleet is busy" is not something you can act on, and "one box is running its test suite
// on ten workers" is.
async function showLoad() {
  const ov = loadOverlay();
  ov.querySelector(".away-list").innerHTML = `<div class="lrow"><span class="nm">measuring…</span></div>`;
  ov.classList.add("open");
  let rows = [];
  try { rows = await fetch("/api/fleet/load").then(r => r.ok ? r.json() : []); } catch {}
  renderLoad(ov, rows);
}
function loadOverlay() {
  let ov = document.getElementById("load");
  if (!ov) {
    ov = document.createElement("div"); ov.id = "load"; document.body.append(ov);
    ov.addEventListener("click", e => {
      if (e.target === ov || e.target.classList.contains("x")) ov.classList.remove("open");
    });
  }
  ov.innerHTML = `<div class="away-card"><div class="away-head"><b>Load by box</b>`
    + `<span class="sub">right now</span><span class="x">✕</span></div><div class="away-list"></div></div>`;
  return ov;
}
function renderLoad(ov, rows) {
  const list = ov.querySelector(".away-list");
  if (!rows.length) {
    list.innerHTML = `<div class="lrow"><span class="nm">nothing to report</span>`
      + `<span class="fig">the sandbox did not answer</span></div>`;
    return;
  }
  // Scaled against the busiest box rather than against the core count: one box at 8 cores of 11 and
  // one at 0.2 is the shape you want to see, and a bar drawn against 11 renders both as slivers.
  const top = Math.max(...rows.map(r => r.cores), 0.5);
  list.innerHTML = "";
  for (const r of rows) {
    const row = document.createElement("div");
    // `containers` is the Docker daemon's own cgroup, not a box — named as such so it is not read
    // as one that has gone missing from the board.
    const isBox = boxes.some(b => b.name === r.name);
    row.className = "lrow" + (r.cores >= top && r.cores > 1 ? " hot" : "");
    row.innerHTML = `<span class="nm">${esc(isBox ? r.name : r.name + " (docker)")}</span>`
      + `<span class="bar"><i style="width:${Math.round(Math.min(1, r.cores/top)*100)}%"></i></span>`
      // `mem_anon`, not the total the box is charged. The rest is page cache the kernel hands back
      // on demand, and adding it to the headline makes a box that read a big repository look like a
      // box that is holding one — four times over, measured.
      + `<span class="fig"><b>${r.cores.toFixed(1)}</b> cpu · ${fmtGB(r.mem_anon)} · ${r.pids|0}p`
      + (r.disk_mb ? ` · ${fmtGb(r.disk_mb)} disk` : "") + `</span>`;
    if (isBox) row.addEventListener("click", () => { ov.classList.remove("open"); select(r.name); });
    else row.style.cursor = "default";
    list.append(row);
  }
}

// ---------- away digest: "while you were gone" (free — client-side fleet delta) ----------
let awaySnap = null;
function snapFleet() {
  const m = {};
  for (const b of boxes) m[b.name] = { g: groupOf(b.state), pause: b.pause, headline: b.headline, d: (b.diff?.ins||0)+(b.diff?.del||0) };
  return { t: Date.now(), m };
}
document.addEventListener("visibilitychange", () => {
  if (document.hidden) awaySnap = snapFleet();
  else if (awaySnap) { if ((Date.now()-awaySnap.t)/60000 >= 0.75) showAwayDigest(awaySnap); awaySnap = null; }
});
function showAwayDigest(snap) {
  const items = [], now = {};
  for (const b of boxes) now[b.name] = b;
  for (const b of boxes) {
    const before = snap.m[b.name], g = groupOf(b.state), dnow = (b.diff?.ins||0)+(b.diff?.del||0);
    if (!before) { items.push({ pri:2, name:b.name, kind:"new", text:`started — ${b.headline||b.branch||""}` }); continue; }
    if (g !== before.g) {
      if (g === "attn")         items.push({ pri:0, name:b.name, kind:"attn",    text:`now needs a decision${b.headline?` — ${b.headline}`:""}` });
      else if (g === "waiting") items.push({ pri:0, name:b.name, kind:"waiting", text:`paused for you${b.headline?` — ${b.headline}`:""}` });
      else if (g === "done")    items.push({ pri:1, name:b.name, kind:"done",    text:`finished${dnow?` (${dnow} lines changed)`:""}` });
      else if (g === "working") items.push({ pri:3, name:b.name, kind:"working", text:`back to work` });
    } else if (dnow > before.d) {
      items.push({ pri:3, name:b.name, kind:"prog", text:`+${dnow-before.d} more lines changed` });
    }
  }
  for (const name in snap.m) if (!now[name]) items.push({ pri:2, name, kind:"gone", text:`left the board (destroyed / merged)` });
  if (!items.length) return;
  items.sort((a,b) => a.pri-b.pri || a.name.localeCompare(b.name));
  renderAwayOverlay(items, snap.t);
}
function renderAwayOverlay(items, sinceT) {
  let ov = document.getElementById("away");
  if (!ov) { ov = document.createElement("div"); ov.id = "away"; document.body.append(ov);
    ov.addEventListener("click", e => { if (e.target === ov) ov.classList.remove("open"); }); }
  const mins = Math.max(1, Math.round((Date.now()-sinceT)/60000));
  ov.innerHTML = `<div class="away-card"><div class="away-head"><b>While you were away</b>`
    + `<span class="sub">~${mins}m · ${items.length} update${items.length>1?"s":""}</span><span class="x">✕</span></div>`
    + `<div class="away-list"></div></div>`;
  const list = ov.querySelector(".away-list");
  for (const it of items) {
    const row = document.createElement("div"); row.className = `away-row k-${it.kind}`;
    row.innerHTML = `<span class="ad"></span><span class="nm">${esc(it.name)}</span><span class="tx">${esc(it.text)}</span>`;
    row.addEventListener("click", () => { ov.classList.remove("open"); openSession(it.name); });
    list.append(row);
  }
  ov.querySelector(".x").addEventListener("click", () => ov.classList.remove("open"));
  ov.classList.add("open");
}

// ---------- command palette ----------
const pal = document.getElementById("pal"), palInput = document.getElementById("pal-input"), palList = document.getElementById("pal-list");
let palItems = [], palSel = 0;
function commands() {
  const acts = [
    { sec:"Actions", label:"New box…", run:openNewBox, meta:"⌘N" },
    { sec:"Actions", label:"Add a repo…", run:addRepoPrompt },
    { sec:"Actions", label:"Settings…", run:() => openSettings("repos") },
    { sec:"Actions", label:"Keyboard shortcuts…", run:() => openSettings("keys"), meta:"?" },
    { sec:"Actions", label:"Open mailbox", run:openMailbox },
    { sec:"Actions", label:"Review pull requests…", run:() => openReview() },
    { sec:"Actions", label:"Repo write access — approve a box's push", run:openGitq },
    // Named for the credential, not the pane, because that is the word someone arrives with. The
    // panel above answers "a box is asking"; this answers "where do I put my token", and looking
    // for the second in the first is exactly the trip that sent this work here.
    { sec:"Actions", label:"GitHub tokens & the App — set up how boxes authenticate", run:() => openSettings("github") },
    { sec:"Actions", label:"Package requests from boxes", run:openSubq },
    { sec:"Actions", label:"Broadcast to all boxes…", run:()=>{ openMailbox(); setTimeout(()=>{ document.getElementById("mbx-to").value="broadcast"; document.getElementById("mbx-body").focus(); },50); } },
    { sec:"Actions", label:"Jump to next box that needs you", run:nextNeedsYou, meta:"]" },
    { sec:"Actions", label:(alertsOn?"Disable":"Enable")+" desktop alerts", run:()=>document.getElementById("alerts").click() },
    { sec:"Actions", label:(voiceOn?"Silence":"Speak")+" boxes that need you", run:()=>document.getElementById("voice").click() },
    { sec:"Actions", label:"Read what needs me", run:sayInbox },
    { sec:"Actions", label:"Load by box — who is using the CPU, memory and disk", run:showLoad },
    // Click-to-talk for the same commands the held key takes. `continuous:false` ends the
    // recognition on a pause by itself, so this needs no second click to stop it.
    { sec:"Actions", label:"Listen for a command", run:listen, meta:"hold ⌥ right" },
    { sec:"Actions", label:"Reconnect live stream", run:connect },
  ];
  if (sessions.size) acts.unshift({ sec:"Actions", label:"Close all terminal sessions", run:()=>[...sessions.keys()].forEach(closeSession) });
  const bx = [];
  for (const b of boxes) {
    bx.push({ sec:"Boxes", label:b.name, meta:labelOf(b.state), state:b.state, run:()=>openTerminal(b.name) });
    bx.push({ sec:"Boxes", label:`Diff: ${b.name}`, meta:diffBadge(b.diff)?`${b.diff.ins}+ ${b.diff.del}−`:"", state:b.state, run:()=>openDiff(b.name) });
  }
  return [...bx, ...acts];
}
function fuzzy(q, s) { q = q.toLowerCase(); s = s.toLowerCase(); let i = 0; for (const ch of s) if (ch === q[i]) i++; return i === q.length; }
function openPalette() { pal.classList.add("open"); palInput.value = ""; renderPal(""); palInput.focus(); }
function closePalette() { pal.classList.remove("open"); }
function renderPal(q) {
  palItems = commands().filter(c => !q || fuzzy(q, c.label)); palSel = 0;
  let html = "", lastSec = null;
  palItems.forEach((c, i) => {
    if (c.sec !== lastSec) { html += `<div class="pal-sec">${c.sec}</div>`; lastSec = c.sec; }
    const col = c.state ? (groupOf(c.state)) : null;
    const dot = `<span class="di" style="background:${col?`var(--${col==='attn'?'attn':col==='waiting'?'waiting':col==='done'?'done':col==='working'?'working':'stale'})`:'transparent'}"></span>`;
    html += `<div class="pal-item${i===0?' on':''}" data-i="${i}">${dot}<span class="lbl">${esc(c.label)}</span>${c.meta?`<span class="meta">${esc(c.meta)}</span>`:""}</div>`;
  });
  palList.innerHTML = html || `<div class="pal-sec">no matches</div>`;
  palList.querySelectorAll(".pal-item").forEach(el => {
    el.addEventListener("mousemove", () => setPalSel(+el.dataset.i));
    el.addEventListener("click", runPal);
  });
}
function setPalSel(i) { palSel = i; palList.querySelectorAll(".pal-item").forEach(el => el.classList.toggle("on", +el.dataset.i === i)); }
function runPal() { const c = palItems[palSel]; if (c) { closePalette(); c.run(); } }
palInput.addEventListener("input", () => renderPal(palInput.value.trim()));
palInput.addEventListener("keydown", e => {
  if (e.key === "ArrowDown") { e.preventDefault(); setPalSel(Math.min(palSel+1, palItems.length-1)); palList.querySelector(".on")?.scrollIntoView({block:"nearest"}); }
  else if (e.key === "ArrowUp") { e.preventDefault(); setPalSel(Math.max(palSel-1,0)); palList.querySelector(".on")?.scrollIntoView({block:"nearest"}); }
  else if (e.key === "Enter") { e.preventDefault(); runPal(); }
  else if (e.key === "Escape") { e.preventDefault(); closePalette(); }
});

// ---------- mailbox (cross-box hand-offs + broadcast) ----------
const mbx = document.getElementById("mbx");
let mbxPoll = null;   // live-refresh the message list while the panel is open (no daemon push exists)
function openMailbox() {
  mbx.classList.add("open"); fillRecipients(); loadMailbox();
  if (!mbxPoll) mbxPoll = setInterval(loadMailbox, 5000);
}
function closeMailbox() {
  mbx.classList.remove("open");
  if (mbxPoll) { clearInterval(mbxPoll); mbxPoll = null; }
}
function fillRecipients() {
  const sel = document.getElementById("mbx-to"), cur = sel.value;
  sel.innerHTML = `<option value="broadcast">📢 broadcast — all boxes</option>`
    + boxes.map(b => `<option value="${esc(b.name)}">${esc(b.name)}</option>`).join("");
  if (cur) sel.value = cur;
}
function loadMailbox() {
  const list = document.getElementById("mbx-list");
  fetch("/api/mailbox").then(r => r.json()).then(msgs => {
    if (!msgs.length) { list.innerHTML = `<div class="note">no messages yet — send a hand-off or broadcast below</div>`; return; }
    list.innerHTML = msgs.map(m => `
      <div class="msg">
        <div class="mh"><span class="who">${esc(m.from||"?")}</span><span class="arrow">→</span>
          <span class="to">${m.to==="broadcast"?"📢 all":esc(m.to)}</span>
          <span class="kind">${esc(m.kind||"note")}</span>
          <span class="mt">${esc((m.ts||"").replace("T"," ").replace("Z",""))}</span></div>
        <div class="mb">${esc(m.body||"")}</div>
        ${(m.seenBy&&m.seenBy.length)?`<div class="seen">seen by ${m.seenBy.map(esc).join(", ")}</div>`:""}
      </div>`).join("");
  }).catch(() => { list.innerHTML = `<div class="note">could not load mailbox</div>`; });
}
function sendMail() {
  const to = document.getElementById("mbx-to").value, kind = document.getElementById("mbx-kind").value;
  const ta = document.getElementById("mbx-body"), body = ta.value.trim();
  const hint = document.getElementById("mbx-hint");
  if (!body) { ta.focus(); return; }
  hint.textContent = "sending…";
  fetch("/api/mailbox", { method:"POST", headers:{"content-type":"application/json"}, body:JSON.stringify({ to, body, kind }) })
    .then(r => { if (!r.ok) throw 0; ta.value = ""; hint.textContent = `sent to ${to==="broadcast"?"all boxes":to}`; loadMailbox(); setTimeout(()=>hint.textContent="",2500); })
    .catch(() => hint.textContent = "send failed");
}
document.getElementById("mbx-send").addEventListener("click", sendMail);
document.getElementById("mbx-body").addEventListener("keydown", e => {
  if (e.key === "Enter" && (e.metaKey||e.ctrlKey)) { e.preventDefault(); sendMail(); }
  e.stopPropagation();
});

// ---------- package requests (a box asks, the fleet's owner decides) ----------
//
// A box cannot install a system package and never will be able to; the sandbox around it can, once,
// for everyone in it. So `sudo apt-get install X` inside a box files a request instead of failing,
// and this is where it is answered. Approving installs it for every box in the fleet — which is the
// reason it is a decision at all rather than something a box just does.
// Announced-once bookkeeping, in the shape the standing-debt work settled on: remember what has
// been said rather than what the last snapshot looked like, or a request that stays pending gets
// re-announced on every poll for as long as it waits.
const subqAnnounced = new Set();
let subqPrimed = false;
function openSubq() { const o = document.getElementById("subq"); o.classList.add("open"); o.setAttribute("aria-hidden","false"); loadSubq(); }
function closeSubq() { const o = document.getElementById("subq"); o.classList.remove("open"); o.setAttribute("aria-hidden","true"); }

// `installed`/`failed` land on the request itself, so the panel needs no progress channel of its
// own — approving flips it to `approved` and a later poll shows how it ended.
// What each card was rendered from, keyed by id. The decision is sent back from HERE, not looked
// up again server-side: the queue lives in the sandbox and every box can write it, so between this
// render and the click on it — seconds to minutes — the box that asked can change what its request
// says. Approving a re-read approves whatever it says *then*. This is the thing that was seen.
//
// Keyed by box AND id (ISO-7). An id is chosen by the box that filed it, and every box can read
// every other box's queue, so a box can file the id it watched a neighbour use. Keyed by id alone,
// the card rendered last owned the entry, and pressing one box's button sent the other's request.
// A box name holds no slash, so the id is everything after the first one.
const subqShown = new Map();

// Any approved apt or npm package runs maintainer or lifecycle scripts as root, for the whole
// fleet, and a remembered one runs them again at every launch. That is what the button does, and
// the card says so rather than leaving it to be inferred from "Approve & install".
function subqCard(r) {
  const pending = r.state === "pending";
  const log = (r.log || "").trim();
  const key = (r.box || "") + "/" + r.id;
  if (pending) subqShown.set(key, { box: r.box || "", kind: r.kind || "", packages: r.packages || [] });
  return `
    <div class="msg">
      <div class="sq-pkgs">${esc((r.packages || []).join(" "))}</div>
      <div class="sq-meta">${esc(r.kind || "?")} · asked by <b>${esc(r.box || "?")}</b> · ${esc((r.asked || "").replace("T", " ").replace("Z", ""))}</div>
      <div class="sq-row">
        <span class="sq-state ${esc(r.state || "")}">${esc(r.state || "?")}</span>
        ${pending ? `
          <label><input type="checkbox" checked id="sq-rem-${esc(key)}" /> remember for rebuilt sandboxes</label>
          <button class="kbtn primary" onclick="decideSubq(${esc(JSON.stringify(key))}, true)">Approve &amp; install</button>
          <button class="kbtn ghost" onclick="decideSubq(${esc(JSON.stringify(key))}, false)">Deny</button>
          <span class="sq-meta">runs as root for the whole fleet</span>` : ""}
        ${r.state === "approved" ? `<span class="sq-meta">installing… apt can take a few minutes</span>` : ""}
        ${r.state === "installed" && r.remember ? `<span class="sq-meta">recorded — a rebuilt sandbox reinstalls it</span>` : ""}
      </div>
      ${log ? `<div class="sq-log">${esc(log)}</div>` : ""}
    </div>`;
}

function loadSubq() {
  const list = document.getElementById("subq-list");
  return fetch("/api/fleet/substrate").then(r => r.json()).then(reqs => {
    if (!reqs.length) {
      list.innerHTML = `<div class="note">no requests — a box asks by running the install it wanted, e.g. <code>sudo apt-get install libnss3</code></div>`;
      return;
    }
    // Newest first here, though the API returns oldest-first: the thing needing a decision is
    // usually the thing that just happened.
    list.innerHTML = reqs.slice().reverse().map(subqCard).join("");
  }).catch(() => { list.innerHTML = `<div class="note">could not load package requests</div>`; });
}

function decideSubq(key, approve) {
  const id = key.slice(key.indexOf("/") + 1);
  const rem = document.getElementById("sq-rem-" + key);
  // Unticked means approve this once without recording it, so a rebuilt sandbox comes back without
  // it. Denials never record, whatever the box happens to be showing.
  const remember = approve ? !!(rem && rem.checked) : false;
  const shown = subqShown.get(key);
  if (!shown) { toast("that request is no longer on screen — reopen the panel"); return; }
  fetch("/api/fleet/substrate/" + encodeURIComponent(id), {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ approve, remember, ...shown }),
  }).then(r => { if (!r.ok) throw 0; loadSubq(); })
    .catch(() => toast("could not record that decision"));
}

// The badge, so a request does not wait unseen for someone to open the panel.
function paintSubqBadge(n) {
  const btn = document.getElementById("subqbtn");
  if (!btn) return;
  const had = btn.querySelector(".badge");
  if (had) had.remove();
  if (n > 0) {
    const b = document.createElement("span");
    b.className = "badge";
    b.textContent = String(n);
    btn.appendChild(b);
  }
  btn.title = n > 0 ? `${n} package request${n === 1 ? "" : "s"} waiting on you` : "Package requests from boxes";
}

// Returns its promise, so a caller — the test, or anything that wants to poll and then act — can
// wait for the round trip instead of guessing how many ticks it took.
function pollSubq() {
  return fetch("/api/fleet/substrate").then(r => r.json()).then(reqs => {
    const pending = reqs.filter(r => r.state === "pending");
    paintSubqBadge(pending.length);
    // The first poll only seeds: a page reload is not news, and announcing the whole backlog every
    // time the cockpit is opened is exactly the nagging this channel was fixed to stop.
    const fresh = pending.filter(r => !subqAnnounced.has(r.id));
    pending.forEach(r => subqAnnounced.add(r.id));
    if (subqPrimed && fresh.length && alertsOn) {
      const what = fresh.map(r => (r.packages || []).join(" ")).join("; ");
      // The tag is the asking box when there is exactly one, so clicking the notification opens it —
      // `pushNote` treats a tag that names a box as somewhere to go.
      pushNote(
        fresh.length === 1 ? `${fresh[0].box} wants ${what} installed for the fleet` : `${fresh.length} boxes want packages: ${what}`,
        fresh.length === 1 ? fresh[0].box : "substrate",
      );
    }
    subqPrimed = true;
    if (document.getElementById("subq").classList.contains("open")) loadSubq();
  }).catch(() => {});
}

// ---------- repo write access (a box asking to push somewhere that is not its own) ----------
//
// The credential is the boundary, not this panel: a box holds a token scoped to one repository, so
// a push anywhere else is refused by GitHub whether or not anyone ever opens this. What the panel
// decides is whether that stops being true for one box and one repo, for a while.
const gitqAnnounced = new Set();
// The last credential list the panel drew, so "Replace token" can prefill without a second fetch.
let gitqCreds = [];
let gitqPrimed = false;
function openGitq() { const o = document.getElementById("gitq"); o.classList.add("open"); o.setAttribute("aria-hidden","false"); loadGitq(); }
function closeGitq() { const o = document.getElementById("gitq"); o.classList.remove("open"); o.setAttribute("aria-hidden","true"); }

// What each card was rendered from, keyed by id — sent back with the decision instead of being
// looked up again server-side. The queue lives in the sandbox and every box can write it, so a
// request re-read at click time can name a different repo AND a different box. The box is the part
// that surprises: the minted installation token is written into the box the grant names, so a swap
// puts a live write credential wherever the requester likes.
//
// Keyed by box AND id, for `subqShown`'s reason — and here the swap it closes is the one that puts
// a token in the wrong box: two cards sharing an id, and Grant on the trusted one sent the other.
const gitqShown = new Map();

function gitqCard(r) {
  const pending = r.state === "pending";
  const reason = (r.reason || "").trim();
  const key = (r.box || "") + "/" + r.id;
  if (pending) gitqShown.set(key, { box: r.box || "", repo: r.repo || "" });
  return `
    <div class="msg">
      <div class="sq-pkgs">${esc(r.repo || "?")}</div>
      <div class="sq-meta">asked by <b>${esc(r.box || "?")}</b> · ${esc((r.asked || "").replace("T", " ").replace("Z", ""))}</div>
      ${reason ? `<div class="sq-log">${esc(reason)}</div>` : ""}
      <div class="sq-row">
        <span class="sq-state ${esc(r.state || "")}">${esc(r.state || "?")}</span>
        ${pending ? `
          <label>for <input type="number" min="1" value="24" style="width:4em" id="gq-h-${esc(key)}" /> hours
            <input type="checkbox" id="gq-keep-${esc(key)}" /> keep indefinitely</label>
          <button class="kbtn primary" onclick="decideGitq(${esc(JSON.stringify(key))}, true)">Grant write</button>
          <button class="kbtn ghost" onclick="decideGitq(${esc(JSON.stringify(key))}, false)">Deny</button>` : ""}
      </div>
    </div>`;
}

function gitqGrantRow(g) {
  // An expired grant is shown rather than hidden: "this box had access until Tuesday" is the answer
  // to a question this list exists to answer, and a row that silently vanishes answers nothing.
  const when = g.expires ? esc(g.expires.replace("T", " ").replace("Z", "").slice(0, 16)) : "no expiry";
  return `
    <div class="msg">
      <div class="sq-pkgs">${esc(g.box)} → ${esc(g.repo)}</div>
      <div class="sq-row">
        <span class="sq-state ${g.live ? "approved" : ""}">${g.live ? "live" : "expired"}</span>
        <span class="sq-meta">${when}</span>
        ${g.live ? `<button class="kbtn ghost" onclick="revokeGitq(${esc(JSON.stringify(g.box))},${esc(JSON.stringify(g.repo))})">Revoke</button>` : ""}
      </div>
    </div>`;
}

// A stored token covers exactly one repository, and the row says so rather than leaving it to be
// discovered: a token covering three would hand all three to any box that receives it, because the
// helper that chooses runs inside the box as the agent's own uid.
function gitCredRow(c) {
  const state = c.problem ? "refused" : (c.has_token ? "ready" : "incomplete");
  return `
    <div class="set-cred" data-cred="${esc(c.id)}">
      <span class="sq-state ${c.problem || !c.has_token ? "" : "approved"}">${state}</span>
      <span class="cslug">${esc(c.repo || (c.repos || []).join(", ") || "?")}</span>
      <span class="cwhy">${c.problem ? `unusable — ${esc(c.problem)}` : (c.has_token ? "" : "no token stored")}</span>
      <button type="button" class="kbtn ghost" onclick="editGitCred(${esc(JSON.stringify(c.id))})">${c.has_token ? "Replace token" : "Add token"}</button>
      <button type="button" class="kbtn ghost" onclick="removeGitCred(${esc(JSON.stringify(c.id))})">Forget</button>
    </div>`;
}

// The pane's own status line. No single control can give this answer: the scope switch can be on
// while nothing is scoped, which is precisely the state worth naming — and the one a switch drawn
// "on" would otherwise misreport.
function renderGitState(d) {
  const el = document.getElementById("set-gitstate");
  if (!el) return;
  const n = (d.credentials || []).filter(c => !c.problem && c.has_token).length;
  const how = [d.app_ready ? `App ${d.app_id || ""}`.trim() : "", n ? `${n} repository token${n === 1 ? "" : "s"}` : ""].filter(Boolean).join(" · ");
  if (!d.ready) {
    el.className = "set-state warn";
    // The App's problem is worth naming only when there *is* an App — "no App configured" would be
    // repeating the sentence above it, whereas "the key is not at <path>" is the whole answer to a
    // half-configured fleet, and the state someone is most likely to be stuck in.
    const half = d.app_id && d.app_problem ? ` <b>${esc(d.app_problem)}</b>` : "";
    // Two different fleets used to read as one sentence. With the account token seeded by default,
    // "not scoped" always meant "every box holds your whole account" — so that is what this said.
    // Now that the account token is chosen rather than assumed, an unconfigured fleet has *no* way to
    // push, and telling someone their boxes hold a credential they never picked would be worse than
    // saying nothing: they would go looking for the wrong problem when a push fails.
    el.innerHTML = d.account_token
      ? `<b>Not scoped.</b> Every box holds this account's <code>gh</code> token — whatever it reaches, read and write. `
        + `Add a GitHub App or a repository token below to narrow that.${half}`
      : `<b>No credential chosen.</b> Boxes read public repos anonymously and <b>cannot push anywhere</b>. `
        + `Pick one of the three below — an App is the one that needs no per-repo upkeep.${half}`;
    return;
  }
  const on = document.getElementById("set-gitscope");
  if (on && !on.checked) {
    el.className = "set-state warn";
    el.innerHTML = `<b>Ready, but switched off.</b> ${esc(how)} can issue tokens; until the switch below is on, boxes keep the fleet-wide credential.`;
    return;
  }
  el.className = "set-state on";
  el.innerHTML = `<b>Scoped.</b> A box writes only its own repo and asks for any other. ${esc(how)}. Takes effect at each box's next start.`;
}

// One fetch answers both sections and the repo cards, so it is done once and shared rather than
// three times from three renderers.
let gitReadPatSet = false;
// The last payload, kept so the status line can repaint when the switch moves without spending a
// round trip to learn what has not changed.
let gitLast = { credentials: [], ready: false };
function loadGitCreds() {
  return fetch("/api/fleet/git-grants").then(r => r.json()).then(d => {
    gitLast = d;
    gitqCreds = d.credentials || [];
    gitReadPatSet = !!d.read_pat_set;
    const el = document.getElementById("set-gitcreds");
    if (el) {
      el.innerHTML = gitqCreds.length ? gitqCreds.map(gitCredRow).join("")
        : `<div class="set-desc">None stored. On the App path you need none — it mints a per-repo token on its own. Add one here for a repo the App cannot reach, or if you would rather not run an App at all.</div>`;
    }
    const rp = document.getElementById("set-readpat");
    if (rp) rp.placeholder = gitReadPatSet ? "a token is stored — paste to replace it" : "github_pat_… (read)";
    // Folded because it is the second path, not because it is unimportant — so a fleet that is
    // *running* on stored tokens must find them open. Folding away the credentials in use would
    // hide the thing the status line is talking about.
    const alt = document.getElementById("set-gitalt");
    if (alt && (gitqCreds.length || gitReadPatSet)) alt.open = true;
    renderGitState(d);
    // The repo cards show each repo's own token, so they follow whatever just changed here.
    renderRepoList();
    return d;
  }).catch(() => {});
}

function saveReadPat() {
  const el = document.getElementById("set-readpat");
  const token = (el.value || "").trim();
  if (!token) { toast("paste a token first — Forget is how you clear one"); return Promise.resolve(); }
  return putReadPat(token).then(() => { el.value = ""; toast("stored the read token"); });
}
function clearReadPat() {
  if (gitReadPatSet && !confirm("Forget the read-only token? Private repos this fleet does not have an App on stop being readable.")) return Promise.resolve();
  return putReadPat("").then(() => { document.getElementById("set-readpat").value = ""; toast("forgot the read token"); });
}
function putReadPat(token) {
  return fetch("/api/fleet/git-read-token", {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ token }),
  }).then(r => { if (!r.ok) return r.text().then(t => { throw new Error(t); }); return loadGitCreds(); })
    .catch(e => toast(String(e.message || "could not store that token")));
}

// Rotating a token is the common edit — they expire, and a compromised one has to be replaced in a
// hurry. Retyping the repository to do it invites a typo that stores a *second* credential rather
// than replacing the first, which then quietly keeps working.
function editGitCred(id) {
  const c = (gitqCreds || []).find(x => x.id === id);
  if (!c) return;
  document.getElementById("set-c-repo").value = c.repo || (c.repos || [])[0] || "";
  const t = document.getElementById("set-c-token");
  t.value = "";
  t.placeholder = c.has_token ? "paste the new token — the old one is replaced" : "github_pat_…";
  t.focus();
  t.scrollIntoView({ block: "nearest" });
}

function loadGitq() {
  const list = document.getElementById("gitq-list");
  return fetch("/api/fleet/git-grants").then(r => r.json()).then(d => {
    const reqs = d.requests || [], grants = d.grants || [], creds = d.credentials || [];
    gitqCreds = creds;
    let html = "";
    // Said first and plainly: with no way to issue a write token nothing is scoped, so a reader
    // seeing an empty panel knows whether that means "nobody asked" or "not switched on yet".
    // Keyed on `ready` rather than the App alone — someone using only their own tokens is set up.
    if (!d.ready) {
      html += `<div class="note">nothing is scoped yet — every box keeps the credential it already had. `
        + `<a href="#" onclick="closeGitq();openSettings('github');return false">Settings → GitHub &amp; keys</a> is where an App or a repository token goes.`
        + `${d.app_id && d.app_problem ? ` <b>${esc(d.app_problem)}</b>` : ""}</div>`;
    }
    html += reqs.length
      ? reqs.slice().reverse().map(gitqCard).join("")
      : `<div class="note">no requests — a box asks with <code>box-session.sh --request-write &lt;box&gt; &lt;owner/name&gt; "&lt;why&gt;"</code></div>`;
    if (grants.length) {
      html += `<div class="mbx-head" style="margin-top:10px"><b>Grants</b></div>` + grants.map(gitqGrantRow).join("");
    }
    // Credentials are not configured here. This panel is an inbox — it carries a badge, it opens
    // because a box asked for something, and one click answers. Setting up a token is the opposite
    // kind of act: deliberate, done once, and looked for in Settings, which is where it now lives.
    // The link is here because this is the panel that tells you nothing is scoped yet.
    if (d.ready) {
      html += `<div class="note" style="margin-top:10px">Tokens, the GitHub App and the scope switch live in
        <a href="#" onclick="closeGitq();openSettings('github');return false">Settings → GitHub &amp; keys</a>${
          creds.length ? ` — ${creds.length} repository token${creds.length === 1 ? "" : "s"} stored` : ""}.</div>`;
    }
    list.innerHTML = html;
  }).catch(() => { list.innerHTML = `<div class="note">could not load write requests</div>`; });
}

// Spends a real round trip per repo, so it is a button and never a poll. Reports per repo, because
// "it works" is not a fleet-wide fact — an App installed on three of your four repos is the single
// most likely half-configured state, and the one a fleet-wide yes/no would hide.
function probeGit() {
  const btn = document.getElementById("gq-probe");
  const out = document.getElementById("gq-probe-out");
  const rows = document.getElementById("gq-probe-rows");
  btn.disabled = true; out.textContent = "asking GitHub…"; rows.innerHTML = "";
  return fetch("/api/fleet/git-probe", { method: "POST" }).then(r => r.json()).then(list => {
    const bad = list.filter(r => !r.ok).length;
    out.textContent = bad ? `${bad} of ${list.length} cannot be written` : `all ${list.length} can be written`;
    rows.innerHTML = list.map(r => `
      <div class="sq-row">
        <span class="sq-state ${r.ok ? "approved" : ""}">${r.ok ? "ok" : "failed"}</span>
        <span class="sq-pkgs">${esc(r.repo)}</span>
        <span class="sq-meta">${esc(r.detail)}</span>
      </div>`).join("");
  }).catch(() => { out.textContent = "could not run the test"; })
    .finally(() => { btn.disabled = false; });
}

// The id is derived from the repo rather than asked for: it is only a filename, and one token per
// repo means the repo already identifies it. Re-storing the same repo therefore *replaces* its
// token, which is what rotating one should do — and is why nothing anywhere asks for an id.
const credId = repo => repo.toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 64);
// Which stored credential covers `owner/name`, if any. Case-insensitive, because GitHub is.
const credFor = slug => (gitqCreds || []).find(c => (c.repo || "").toLowerCase() === (slug || "").toLowerCase());

// The network call alone, with no form attached — three places store a token now (this pane, a
// repo's own card, and the add-repo dialog) and only one of them has the inputs below.
function storeGitCred(repo, token) {
  return fetch("/api/fleet/git-credentials", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ id: credId(repo), label: repo, repos: [repo], token }),
  }).then(r => { if (!r.ok) return r.text().then(t => { throw new Error(t); }); return loadGitCreds(); });
}

// Returns its promise, so a caller — the test, or anything wanting to store then act — can wait
// for the round trip instead of guessing how many ticks the clear took.
function addGitCred() {
  const repo = (document.getElementById("set-c-repo").value || "").trim();
  const token = (document.getElementById("set-c-token").value || "").trim();
  if (!repo || !token) { toast("a repository and a token are both needed"); return Promise.resolve(); }
  return storeGitCred(repo, token).then(() => {
    document.getElementById("set-c-token").value = "";
    document.getElementById("set-c-repo").value = "";
  }).catch(e => toast(String(e.message || "could not store that token")));
}

function removeGitCred(id) {
  return fetch("/api/fleet/git-credentials/" + encodeURIComponent(id), { method: "DELETE" })
    .then(r => { if (!r.ok) throw 0; return loadGitCreds(); })
    .catch(() => toast("could not forget that token"));
}

function decideGitq(key, approve) {
  const id = key.slice(key.indexOf("/") + 1);
  const keep = document.getElementById("gq-keep-" + key);
  const hrs = document.getElementById("gq-h-" + key);
  // 0 means "never expires" to the API. Ticking keep is the deliberate act; the number is ignored
  // when it is set, so the two controls cannot disagree about what was granted.
  const hours = approve ? (keep && keep.checked ? 0 : Math.max(1, parseInt(hrs && hrs.value, 10) || 24)) : 24;
  const shown = gitqShown.get(key);
  if (!shown) { toast("that request is no longer on screen — reopen the panel"); return; }
  fetch("/api/fleet/git-grants/" + encodeURIComponent(id), {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ approve, hours, ...shown }),
  // **A refusal says why, and the row is redrawn either way** (SKEIN-1034). The server answers a
  // second answer to one request with a sentence — "request r1 is already granted — a request is
  // answered once; to take back a grant, revoke it" — and that sentence is the only way the person
  // pressing learns the row they pressed on was stale. The reload is what makes the row show the
  // real state, so the button that was just refused goes.
  }).then(r => r.ok ? loadGitq() : r.text().then(why => {
    toast(why.trim() || "could not record that decision");
    return loadGitq();
  })).catch(() => toast("could not record that decision"));
}

function revokeGitq(box, repo) {
  fetch("/api/fleet/git-grants/" + encodeURIComponent(box) + "/" + encodeURIComponent(repo), { method: "DELETE" })
    .then(r => { if (!r.ok) throw 0; loadGitq(); })
    .catch(() => toast("could not revoke that grant"));
}

function paintGitqBadge(n) {
  const btn = document.getElementById("gitqbtn");
  if (!btn) return;
  const had = btn.querySelector(".badge");
  if (had) had.remove();
  if (n > 0) {
    const b = document.createElement("span");
    b.className = "badge";
    b.textContent = String(n);
    btn.appendChild(b);
  }
  // Says what the panel is *for*, since it is now only that: boxes asking. Credentials moved to
  // Settings, and a tooltip still implying they live here would keep sending people to the badge.
  btn.title = n > 0 ? `${n} write request${n === 1 ? "" : "s"} waiting on you` : "Repo write access — requests from boxes";
}

// Returns its promise, so a caller can wait for the round trip rather than guess at ticks.
function pollGitq() {
  return fetch("/api/fleet/git-grants").then(r => r.json()).then(d => {
    const pending = (d.requests || []).filter(r => r.state === "pending");
    paintGitqBadge(pending.length);
    const fresh = pending.filter(r => !gitqAnnounced.has(r.id));
    pending.forEach(r => gitqAnnounced.add(r.id));
    // The first poll only seeds: reopening the cockpit is not news.
    if (gitqPrimed && fresh.length && alertsOn) {
      pushNote(
        fresh.length === 1
          ? `${fresh[0].box} wants to write ${fresh[0].repo}`
          : `${fresh.length} boxes want write access`,
        fresh.length === 1 ? fresh[0].box : "gitgate",
      );
    }
    gitqPrimed = true;
    if (document.getElementById("gitq").classList.contains("open")) loadGitq();
  }).catch(() => {});
}

// ---------- launch a new box ----------
const newbox = document.getElementById("newbox"), nbBranch = document.getElementById("nb-branch");
const nbRepo = document.getElementById("nb-repo"), nbRepoLbl = document.getElementById("nb-repo-lbl");
const nbAgent = document.getElementById("nb-agent");
let repos = [];
let settings = {};
function fillRuntimeSelects() {
  for (const id of ["nb-agent", "ar-agent", "set-agent"]) {
    const select = document.getElementById(id);
    const current = select.value;
    select.innerHTML = runtimes.map(runtime =>
      `<option value="${esc(runtime.id)}">${esc(runtime.label)}</option>`
    ).join("");
    const wanted = current || settings.default_agent;
    if (wanted && runtimes.some(runtime => runtime.id === wanted)) select.value = wanted;
  }
}
function loadRuntimes() {
  if (DEMO) {
    runtimes = [{id:"claude",label:"Claude",adapted_statusline:false},{id:"codex",label:"Codex",adapted_statusline:true}];
    fillRuntimeSelects();
    return Promise.resolve(runtimes);
  }
  return fetch("/api/runtimes").then(r => r.json()).then(values => {
    runtimes = Array.isArray(values) ? values : [];
    fillRuntimeSelects();
    return runtimes;
  }).catch(() => []);
}
// Load skein's app settings.
function loadSettings() {
  return fetch("/api/settings").then(r => r.json()).then(s => { settings = s || settings; }).catch(() => {});
}
// The health report, rendered where it can be read.
//
// Every detail in it used to live in the banner's `title` attribute: uncopyable, invisible on a
// phone (which this product ships a layout for), gone on scroll. Meanwhile `skein doctor` prints
// the same facts beautifully — and is CLI-only, in a product whose stated goal is that the cockpit
// is the only surface you touch. Same report, same ✓/✗/! vocabulary, now reachable from the banner
// that reports the fault.
function renderDiagnostics() {
  const el = document.getElementById("set-diag");
  if (!el) return;
  const h = lastHealth;
  if (!h) { el.innerHTML = `<div class="note">no report yet</div>`; return; }
  const CHECKS = [
    ["registry", "registry"], ["sbx", "sbx"], ["git", "git"], ["gh", "gh"],
    ["probes", "box probes"], ["mailbox", "mailbox"], ["memory", "fleet memory"],
    ["disk", "fleet disk"],
    // "token life" directly under "github scope": the same credential asked two questions — what it
    // can reach, and whether it will still reach it next month (SKEIN-928).
    ["gitgate", "github scope"], ["token_expiry", "token life"],
    // "proxy credential" third in the same group, because it is the same credential asked the
    // question the other two cannot: not what skein's token may reach and not how long it lasts,
    // but whether the token skein holds is the one GitHub is actually answering (SKEIN-548). It is
    // the substrate's answer rather than skein's, which is why skein can only report it.
    ["proxy_injection", "proxy credential"],
    ["warden", "host warden"], ["cover", "box isolation"],
    ["ai", "ai enrichment"],
  ];
  // Three states, not two. `unknown` is "skein could not tell", which is neither a pass nor a
  // fault — reporting it as ✗ sent people to reinstall a working tool because `sbx ls` timed out
  // once. It gets the ! this panel already uses for "look at this, nothing is broken".
  const MARK = { satisfied: ["ok", "✓"], unsatisfied: ["bad", "✗"], unknown: ["warn", "!"] };
  const rows = CHECKS.filter(([k]) => h[k]).map(([k, label]) => {
    const [cls, mark] = MARK[h[k].level] || MARK.unknown;
    // The way out, under the fault it clears, and selectable: it is usually a command, and a
    // command you cannot copy is a command you retype wrongly.
    const fix = h[k].fix
      ? `<div class="dg-fix">→ <code>${esc(h[k].fix)}</code>${h[k].destructive
          ? ` <span class="dg-danger">destroys something — copy it and run it yourself</span>` : ""}</div>`
      : "";
    return `
    <div class="dg-row ${cls}">
      <span class="dg-mark">${mark}</span>
      <span class="dg-name">${label}</span>
      <span class="dg-detail">${esc(h[k].detail || "")}${fix}</span>
    </div>`;
  }).join("");
  const list = (names, what) => names?.length
    ? `<div class="dg-row bad"><span class="dg-mark">!</span><span class="dg-name">${what}</span>
       <span class="dg-detail">${esc(names.join(", "))}</span></div>` : "";
  // Containers and cgroups no box owns. It is not one of CHECKS, because it is never a fault: ✓, or
  // `!` with what was found, and never ✗. Every command in it is only shown for copying, with no
  // button, because the owner declined one and the server never runs these itself.
  const u = h.unowned;
  const unowned = u ? `
    <div class="dg-row ${u.clear ? "ok" : "warn"}">
      <span class="dg-mark">${u.clear ? "✓" : "!"}</span>
      <span class="dg-name">unowned containers</span>
      <span class="dg-detail">${(u.said || []).map(s => `<div>${esc(s.text)}</div>`
        + (s.offers || []).map(o => `<div class="dg-fix">→ ${esc(o.lead)} <code>${esc(o.command)}</code>${o.destructive
          ? ` <span class="dg-danger">destroys something — copy it and run it yourself</span>` : ""}</div>`).join("")).join("")}</span>
    </div>` : "";
  el.innerHTML = rows
    + unowned
    + list(h.dark_boxes, "no signals")
    + list(h.stale_boxes, "stale sessions")
    + `<div class="dg-foot">the same report <code>skein doctor</code> prints, which also probes
       bwrap, tmux and the fleet's mounts from inside the sandbox.</div>`;
  const pill = document.getElementById("set-diagn");
  if (pill) {
    // The pill counts FAULTS. An unknown is visible in the list above and does not add a number
    // to a badge that people read as "things I must fix".
    const bad = CHECKS.filter(([k]) => h[k] && h[k].level === "unsatisfied").length;
    pill.textContent = bad || "";
  }
}

