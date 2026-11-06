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
// **An idle job's cores are lent** (SKEIN-1469): a running row with `idle_secs` says so in its
// figures, and the pool says how many of the granted cores are lent, as the queue counted them.
//
// **The CPU figures may be averaged** over the last 1, 5 or 15 minutes or a job's whole run, by the
// owner's pick; "now" is the rate between the queue's last two readings, two seconds apart, which
// swings with a bursty build. A job's `using` and the machine's busy cores follow the same pick. A
// span not covered yet, because the job or skein's watching of it is younger than that, shows the
// longest that is, and says which. A waiting job's reason is the queue's own words for why it
// waits, from its last reading, and is never averaged.
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

// What the figures may be averaged over, keyed as `jobs::FleetJob`'s `using_avg` and
// `jobs::FleetView`'s `busy_avg` are, with `now` first: the default.
export const JOB_OVER = [["now", "now"], ["60", "1 min"], ["300", "5 min"], ["900", "15 min"], ["run", "whole run"]];

// `over` if it is one of JOB_OVER's, else "now".
export function jobsOver(over) {
  return JOB_OVER.some(([k]) => k === over) ? over : "now";
}

// The pick, as a labelled select with `over` chosen.
export function jobsOverHtml(over) {
  const o = jobsOver(over);
  const opts = JOB_OVER.map(([k, label]) => `<option value="${k}"${k === o ? " selected" : ""}>${label}</option>`).join("");
  return `<label class="jb-over">average over <select data-jobs-over>${opts}</select></label>`;
}

const jobsNum = x => typeof x === "number" && Number.isFinite(x);

// A running job's cores used over `over`, and what to say beside them when that is not the span
// picked: `{ cores, note }`, or null when it was never read. A span its readings do not cover yet
// gives its whole run so far, which is all of the time it has run.
export function jobUsing(j, over = "now") {
  const o = jobsOver(over);
  const avg = j && j.using_avg && typeof j.using_avg === "object" ? j.using_avg : {};
  if (o !== "now" && jobsNum(avg[o])) return { cores: avg[o], note: "" };
  if (o !== "now" && jobsNum(avg.run)) return { cores: avg.run, note: ` (all ${jobSpan(j.secs)} so far)` };
  if (!jobsNum(j && j.using)) return null;
  return { cores: j.using, note: o === "now" ? "" : " (now)" };
}

// The whole machine's busy cores over `over`, as `jobUsing` gives a job's. The machine has no run
// of its own, so "whole run" and a span not covered yet give the longest span that is.
export function machineBusy(v, over = "now") {
  const o = jobsOver(over);
  const avg = v && v.busy_avg && typeof v.busy_avg === "object" ? v.busy_avg : {};
  if (o !== "now") {
    if (jobsNum(avg[o])) return { cores: avg[o], note: "" };
    const upto = o === "run" ? Infinity : Number(o);
    const longest = JOB_OVER.filter(([k]) => k !== "now" && k !== "run" && Number(k) < upto && jobsNum(avg[k])).pop();
    if (longest) return { cores: avg[longest[0]], note: ` (last ${longest[1]})` };
  }
  if (!jobsNum(v && v.busy)) return null;
  return { cores: v.busy, note: o === "now" ? "" : " (now)" };
}

const cores = n => (n === 1 ? "1 core" : `${n | 0} cores`);
const gb = mib => {
  const s = (Number(mib) / 1024).toFixed(1);
  return s.endsWith(".0") ? s.slice(0, -2) : s;
};

// One row's figures, in words: what it asked for and has, its memory, and how long. Its CPU is
// averaged over `over` (JOB_OVER).
export function jobFigures(j, over = "now") {
  const running = j.state === "granted";
  const parts = [];
  if (running) {
    parts.push(j.granted_cores && j.granted_cores !== j.cores
      ? `asked ${cores(j.cores)}, granted ${j.granted_cores}` : `${cores(j.granted_cores || j.cores)}`);
    const u = jobUsing(j, over);
    if (u) parts.push(`using ${u.cores.toFixed(1)}${u.note}`);
    if (typeof j.idle_secs === "number") parts.push(`idle ${jobSpan(j.idle_secs)}, its cores lent`);
    parts.push(j.mem_measured ? `${gb(j.mem_mib)} GB` : `~${gb(j.mem_mib)} GB`);
    parts.push(`running ${jobSpan(j.secs)}`);
  } else {
    parts.push(j.min && j.min < j.cores ? `asks ${j.min}–${j.cores} cores` : `asks ${cores(j.cores)}`);
    parts.push(`needs ~${gb(j.mem_mib)} GB`);
    parts.push(`waiting ${jobSpan(j.secs)}`);
  }
  return parts.join(" · ");
}

// The list, as HTML. `box` narrows it to one box's jobs ("" for all); `over` is what the CPU figures
// are averaged over (JOB_OVER).
export function jobsHtml(view, box = "", over = "now") {
  const v = view && typeof view === "object" ? view : {};
  const all = Array.isArray(v.jobs) ? v.jobs : [];
  const jobs = box ? all.filter(j => j.box === box) : all;
  const lent = v.lent | 0 ? ` · <b>${v.lent | 0}</b> lent` : "";
  const m = machineBusy(v, over);
  const busy = m ? ` · machine busy <b>${m.cores.toFixed(1)}</b>${m.note}` : "";
  const pool = `<div class="jb-pool"><b>${v.cores | 0}</b> cores · <b>${v.granted | 0}</b> granted${lent} · <b>${v.free | 0}</b> free${busy}</div>`;
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
      + `<div class="sq-meta jb-fig">${jobsEsc(jobFigures(j, over))}</div>`
      + (j.reason ? `<div class="sq-log jb-why">${jobsEsc(j.reason)}</div>` : "")
      + `<div class="sq-row"><button type="button" class="kbtn ghost" data-stop data-box="${jobsEsc(j.box)}" data-id="${jobsEsc(j.id)}"`
      + ` title="${running ? "Kill its processes. The agent is told you stopped it." : "Take it out of the queue. The agent is told you stopped it."}">Stop</button>`
      + `<span class="sq-meta sq-sending" hidden>stopping…</span></div>`
      + `</div>`;
  });
  return pool + stale + scope + rows.join("");
}
