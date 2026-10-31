// The heavy-jobs list: every job waiting or running in the fleet, and a Stop on each (SKEIN-1422).
//
// **No store of its own.** The rows are `/api/fleet/jobs`'s, which is what the queue's last tick
// wrote (`jobs::fleet_view`): running jobs first, then waiting ones in the order they go. This only
// draws them.
//
// **The owner sees everything.** Every box is named, and every repo, and the whole command is on
// the row's hover beside its two-word form: the owner chose named over counted (2026-10-31). The
// command, the folder and the box's class are the box's own words, so every one is escaped.
//
// **A link can open it**: `/#jobs` for the whole fleet, `/#jobs-<box>` for one box's jobs, so a
// box's terminal pane can link to its own.

const jobsEsc = s =>
  String(s).replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "\"": "&quot;", "'": "&#39;" }[c]));

// `{ box: "" }` for `#jobs`, `{ box: "<name>" }` for `#jobs-<name>`, else null. A box name is
// letters, digits, `.`, `_` and `-`, not starting with `.` or `-` (`util::valid_name`), so anything
// else after `jobs-` is not a box and opens nothing.
export function jobsForHash(hash) {
  const h = String(hash || "").replace(/^#/, "");
  if (h === "jobs") return { box: "" };
  const m = /^jobs-([A-Za-z0-9_][A-Za-z0-9._-]{0,127})$/.exec(h);
  return m && !m[1].includes("..") ? { box: m[1] } : null;
}

// Seconds as a short span: `40s`, `3m`, `2h 5m`.
export function jobSpan(secs) {
  const s = Math.max(0, Math.floor(Number(secs) || 0));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60);
  return m ? `${h}h ${m}m` : `${h}h`;
}

const cores = n => (n === 1 ? "1 core" : `${n | 0} cores`);
const gb = mib => {
  const s = (Number(mib) / 1024).toFixed(1);
  return s.endsWith(".0") ? s.slice(0, -2) : s;
};

// One row's figures, in words: what it asked for and has, its memory, and how long.
export function jobFigures(j) {
  const running = j.state === "granted";
  const parts = [];
  if (running) {
    parts.push(j.granted_cores && j.granted_cores !== j.cores
      ? `asked ${cores(j.cores)}, granted ${j.granted_cores}` : `${cores(j.granted_cores || j.cores)}`);
    if (typeof j.using === "number") parts.push(`using ${j.using.toFixed(1)}`);
    parts.push(j.mem_measured ? `${gb(j.mem_mib)} GB` : `~${gb(j.mem_mib)} GB`);
    parts.push(`running ${jobSpan(j.secs)}`);
  } else {
    parts.push(j.min && j.min < j.cores ? `asks ${j.min}–${j.cores} cores` : `asks ${cores(j.cores)}`);
    parts.push(`needs ~${gb(j.mem_mib)} GB`);
    parts.push(`waiting ${jobSpan(j.secs)}`);
  }
  return parts.join(" · ");
}

// The list, as HTML. `box` narrows it to one box's jobs ("" for all).
export function jobsHtml(view, box = "") {
  const v = view && typeof view === "object" ? view : {};
  const all = Array.isArray(v.jobs) ? v.jobs : [];
  const jobs = box ? all.filter(j => j.box === box) : all;
  const pool = `<div class="jb-pool"><b>${v.cores | 0}</b> cores · <b>${v.granted | 0}</b> granted · <b>${v.free | 0}</b> free</div>`;
  const stale = v.running === false
    ? `<div class="note jb-stale">The queue is not running: skein has not looked at it for a while, so this may be out of date.</div>` : "";
  const scope = box
    ? `<div class="jb-scope">${jobsEsc(box)}'s jobs only · <a href="#jobs" class="lk" data-jobs-all>show every box</a></div>` : "";
  if (!jobs.length) {
    const none = box ? `${jobsEsc(box)} has nothing waiting or running.`
      : `Nothing is waiting or running. A box runs heavy work with <code>skein-job run -- &lt;command&gt;</code>.`;
    return pool + stale + scope + `<div class="note">${none}</div>`;
  }
  const rows = jobs.map(j => {
    const running = j.state === "granted";
    const key = `${j.box}/${j.id}`;
    const what = j.short || (j.command || "").split(" ").slice(0, 2).join(" ") || j.id;
    const where = running ? "running" : (j.position ? `waiting, ${j.position} in line` : "waiting");
    return `<div class="msg jb-row" data-card="${jobsEsc(key)}" data-state="${running ? "running" : "waiting"}">`
      + `<div class="sq-meta"><b>${jobsEsc(j.box)}</b> · ${jobsEsc(j.repo)} · ${jobsEsc(j.class)} · ${where}</div>`
      + `<div class="aq-body jb-cmd" title="${jobsEsc(j.command || "")}">${jobsEsc(what)}</div>`
      + `<div class="sq-meta jb-fig">${jobsEsc(jobFigures(j))}</div>`
      + (j.reason ? `<div class="sq-log jb-why">${jobsEsc(j.reason)}</div>` : "")
      + `<div class="sq-row"><button type="button" class="kbtn ghost" data-stop data-box="${jobsEsc(j.box)}" data-id="${jobsEsc(j.id)}"`
      + ` title="${running ? "Kill its processes. The agent is told you stopped it." : "Take it out of the queue. The agent is told you stopped it."}">Stop</button>`
      + `<span class="sq-meta sq-sending" hidden>stopping…</span></div>`
      + `</div>`;
  });
  return pool + stale + scope + rows.join("");
}
