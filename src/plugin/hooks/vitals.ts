// What the band above the prompt reads, and the words of its vitals and running-work lines
// (SKEIN-1440, step 2). The readers are small and each answers null where skein wrote nothing fresh
// to believe, so the docked pane can draw from the same ones.
//
// * `readResources`: `$SKEIN_STATE/signals/resources.json` (`Signals` in `src/resource_signal.rs`),
//   written every minute: the disk and skein's ask, memory, CPU, and versions waiting for a restart.
// * `readSpend`: `signals/usage.json`, written once an hour by skein-server (SKEIN-1384).
// * `readJobsView`: `signals/jobs.json` (`View` in `src/jobs.rs`), rewritten at least every 20 s.
//
// Nothing here computes a figure skein did not write. The one thing the band keeps of its own is
// the CPU a minute at a time, so the sparkline has an hour to draw from (`sampled`).

import type { EngineInterface as Engine } from 'claude-code'

import type {
  SkeinAgentRun,
  SkeinBand,
  SkeinBandJob,
  SkeinBandJobs,
  SkeinCpuSample,
  SkeinResources,
  SkeinUpdate,
} from '../types'

// skein writes the signal file every 60 s, and the plugin's command hooks believe nothing in one
// older than five minutes (`STALE_AFTER` in `bin/skein-resources`): neither does the band.
export const STALE_AFTER_MS = 300_000

// skein-server reads the fleet's usage once an hour (`USAGE_MAX_AGE` in skein-server's
// `health.rs`) and writes the box's spend from each reading. The band trusts that file for the hour
// and ten minutes more, so a reading that takes a while does not blank the line.
export const SPEND_STALE_AFTER_MS = 70 * 60_000

// skein rewrites the jobs view at least every 20 s (`VIEW_FRESH` in `src/jobs.rs`), and the wrapper
// takes 30 s unwritten as skein not scheduling: a view older than that has no row.
export const JOBS_STALE_AFTER_MS = 30_000

/** The sparkline's hour, and how many of its minutes the band draws. */
const HISTORY_MINUTES = 60
const SPARK_CELLS = 8

const G = 1024 * 1024 * 1024
const MIB = 1024 * 1024

// ------------------------------------------------------------------------------------------------
// Reading
// ------------------------------------------------------------------------------------------------

/** The signal file, while it is fresh: null when it is missing, unreadable or over five minutes old. */
export async function readResources($: Engine): Promise<(SkeinResources & { at: number }) | null> {
  const state = await $.env.get('SKEIN_STATE')
  if (!state) return null
  const parsed = await parsedFile($, `${state}/signals/resources.json`)
  if (!isRecord(parsed) || typeof parsed.at !== 'string') return null
  const at = Date.parse(parsed.at)
  const now = await $.clock.now()
  if (Number.isNaN(at) || now - at > STALE_AFTER_MS) return null
  const disk = isRecord(parsed.disk) ? parsed.disk : {}
  const ask = (Array.isArray(parsed.asks) ? parsed.asks : []).find(
    a => isRecord(a) && a.kind === 'disk' && typeof a.crossing_id === 'string' && a.crossing_id !== '',
  )
  // Memory is read when the cgroup could be: a tick that could not carries the last figures forward
  // with the time they were read, and those are believed no longer than the file itself.
  const mem = isRecord(parsed.memory) ? parsed.memory : {}
  const memAt = typeof mem.read_at === 'string' ? Date.parse(mem.read_at) : at
  const anon = numberOr(mem.anon, null)
  const memory =
    anon === null || Number.isNaN(memAt) || now - memAt > STALE_AFTER_MS
      ? null
      : { bytes: anon, limit: numberOr(mem.high, null) ?? numberOr(mem.max, null) }
  const cpu = isRecord(parsed.cpu) ? numberOr(parsed.cpu.cores, null) : null
  const pids = isRecord(parsed.pids) ? { current: numberOr(parsed.pids.current, null), max: numberOr(parsed.pids.max, null) } : null
  // What else skein asks of the box than disk, by kind (`memory`, `pids`): the agent's to act on.
  const asked = (Array.isArray(parsed.asks) ? parsed.asks : [])
    .filter(isRecord)
    .map(a => a.kind)
    .filter((k): k is string => typeof k === 'string' && k !== '' && k !== 'disk')
  const updates: SkeinUpdate[] = (Array.isArray(parsed.updates) ? parsed.updates : [])
    .filter(isRecord)
    .filter(u => typeof u.component === 'string' && typeof u.new === 'string')
    .map(u => ({
      component: String(u.component),
      have: typeof u.have === 'string' ? u.have : '',
      new: String(u.new),
      restartRequested: u.restart_requested === true,
    }))

  return {
    at,
    disk: {
      mb: numberOr(disk.mb, null),
      limit_mb: numberOr(disk.limit_mb, null),
      ask: isRecord(ask)
        ? {
            id: String(ask.crossing_id),
            amount_mb: numberOr(ask.amount, 0) ?? 0,
            live_mb: numberOr(ask.live_mb, 0) ?? 0,
            why: typeof ask.why === 'string' ? ask.why : '',
            held: typeof ask.hold === 'string' && ask.hold !== '',
          }
        : null,
    },
    memory,
    cpu,
    pids,
    asked,
    updates,
  }
}

/** This box's own spend, as skein-server last wrote it; null when it is missing or out of date. */
export async function readSpend($: Engine): Promise<SkeinBand['spend']> {
  const state = await $.env.get('SKEIN_STATE')
  if (!state) return null
  const parsed = await parsedFile($, `${state}/signals/usage.json`)
  if (!isRecord(parsed) || typeof parsed.at !== 'string') return null
  const at = Date.parse(parsed.at)
  if (Number.isNaN(at) || (await $.clock.now()) - at > SPEND_STALE_AFTER_MS) return null
  const today = isRecord(parsed.today) ? numberOr(parsed.today.cost, null) : null
  const week = isRecord(parsed.week) ? numberOr(parsed.week.cost, null) : null
  if (today === null || week === null) return null

  return { at: utcTime(at), today, week, unpriced: parsed.unpriced === true }
}

/**
 * The box's granted and waiting jobs from its jobs view, and who goes before its first waiting one;
 * null when there is no view to believe. A finished job is left out: it no longer runs.
 */
export async function readJobsView($: Engine): Promise<SkeinBandJobs | null> {
  const state = await $.env.get('SKEIN_STATE')
  if (!state) return null
  const parsed = await parsedFile($, `${state}/signals/jobs.json`)
  if (!isRecord(parsed) || typeof parsed.at !== 'string' || !Array.isArray(parsed.jobs)) return null
  const at = Date.parse(parsed.at)
  if (Number.isNaN(at) || (await $.clock.now()) - at > JOBS_STALE_AFTER_MS) return null
  const jobs: SkeinBandJob[] = parsed.jobs
    .filter(isRecord)
    .filter(j => j.state === 'granted' || j.state === 'waiting')
    .map(j => ({
      id: String(j.id ?? ''),
      command: String(j.command ?? ''),
      cwd: String(j.cwd ?? ''),
      state: String(j.state),
      cores: numberOr(j.cores, 0) ?? 0,
      granted_cores: numberOr(j.granted_cores, 0) ?? 0,
      position: numberOr(j.position, null),
      reason: typeof j.reason === 'string' ? j.reason : '',
    }))
  const ahead = (Array.isArray(parsed.ahead) ? parsed.ahead : [])
    .filter(isRecord)
    .filter(a => typeof a.box === 'string' && a.box !== '')
    .map(a => ({ box: String(a.box), command: String(a.command ?? a.class ?? ''), cores: numberOr(a.cores, 0) ?? 0 }))

  // The fleet's cores, what is granted of them, and how many jobs run, fleet-wide: the queue's slots.
  const fleet = { cores: numberOr(parsed.cores, 0) ?? 0, granted: numberOr(parsed.granted, 0) ?? 0, running: numberOr(parsed.running, 0) ?? 0 }

  return { jobs, ahead, fleet: fleet.cores > 0 ? fleet : null }
}

/**
 * The CPU history with one more reading: the minute of `atMs` set to `cores`, minutes over an hour
 * older than it dropped. A reading in a minute already kept replaces it.
 */
export function sampled(history: readonly SkeinCpuSample[], cores: number, atMs: number): SkeinCpuSample[] {
  const minute = Math.floor(atMs / 60_000)
  const kept = history.filter(s => s.minute !== minute && s.minute > minute - HISTORY_MINUTES && s.minute < minute)

  return [...kept, { minute, cores }]
}

// ------------------------------------------------------------------------------------------------
// The vitals line
// ------------------------------------------------------------------------------------------------

/** What the vitals line draws from: each part null where its signal is not fresh. */
export type Vitals = {
  disk: { mb: number; limit_mb: number | null } | null
  memory: SkeinResources['memory']
  /** The sparkline's minutes, oldest first; empty when the CPU now is not known. */
  cpu: readonly SkeinCpuSample[]
  spend: SkeinBand['spend']
}

/**
 * `skein  disk ▮▮▮▯▯▯▯▯▯▯ 2.7/10G  mem ▮▯▯▯▯ 1.1/14G  cpu ▁▂▂▃▂▁▁▂  ·  $31 today · $515 7d`, in at
 * most `columns` cells: the bars go first, then the week's spend, the sparkline and memory. A part
 * whose signal is not fresh is not drawn at all.
 */
export function vitalsLine(v: Vitals, columns: number): string {
  const disk = v.disk
  const diskFull = disk && (disk.limit_mb === null ? `disk ${gib(disk.mb * MIB)}G` : `disk ${bar(disk.mb, disk.limit_mb, 10)} ${pair(disk.mb * MIB, disk.limit_mb * MIB)}`)
  const diskShort = disk && (disk.limit_mb === null ? `disk ${gib(disk.mb * MIB)}G` : `disk ${pair(disk.mb * MIB, disk.limit_mb * MIB)}`)
  const mem = v.memory
  const memFull = mem && (mem.limit === null ? `mem ${gib(mem.bytes)}G` : `mem ${bar(mem.bytes, mem.limit, 5)} ${pair(mem.bytes, mem.limit)}`)
  const memShort = mem && (mem.limit === null ? `mem ${gib(mem.bytes)}G` : `mem ${pair(mem.bytes, mem.limit)}`)
  const cpu = v.cpu.length === 0 ? null : `cpu ${sparkline(v.cpu)}`
  const plus = v.spend?.unpriced ? '+' : ''
  const today = v.spend ? `${dollars(v.spend.today)}${plus} today` : null
  const week = v.spend ? `${dollars(v.spend.week)}${plus} 7d` : null

  const draw = (box: (string | null)[], spend: (string | null)[]) => {
    const left = box.filter((p): p is string => p !== null).join('  ')
    const right = spend.filter((p): p is string => p !== null).join(' · ')
    const parts = [left, right].filter(p => p !== '').join('  ·  ')

    return parts === '' ? 'skein  not measured lately' : `skein  ${parts}`
  }
  const tries = [
    draw([diskFull, memFull, cpu], [today, week]),
    draw([diskShort, memFull, cpu], [today, week]),
    draw([diskShort, memShort, cpu], [today, week]),
    draw([diskShort, memShort, cpu], [today]),
    draw([diskShort, memShort], [today]),
    draw([diskShort], [today]),
    draw([diskShort], []),
  ]

  return cut(tries.find(t => cells(t) <= columns) ?? (tries.at(-1) ?? ''), columns)
}

/** `▮▮▮▯▯▯▯▯▯▯`: `used` of `limit` in `n` segments, every one filled at or over the limit, and one
 * at least for any use, so a box using a little is not drawn as using none. */
export function bar(used: number, limit: number, n: number): string {
  const filled = limit <= 0 ? n : Math.max(used > 0 ? 1 : 0, Math.min(n, Math.round((used / limit) * n)))

  return '▮'.repeat(filled) + '▯'.repeat(n - filled)
}

/** `2.7/10G`, or in M under a gigabyte's limit. */
function pair(used: number, limit: number): string {
  if (limit < G) return `${Math.round(used / MIB)}/${Math.round(limit / MIB)}M`
  const whole = Math.abs(limit / G - Math.round(limit / G)) < 0.05

  return `${gib(used)}/${whole ? Math.round(limit / G) : gib(limit)}G`
}

const gib = (n: number) => (n / G).toFixed(1)

const SPARKS = '▁▂▃▄▅▆▇█'

/**
 * The last `cells` minutes (the band's eight unless asked), from the first one kept: one cell a
 * minute, a minute with no reading left blank rather than filled in, each cell's height against the
 * busiest minute shown (at least one core, so an idle box draws low).
 */
export function sparkline(history: readonly SkeinCpuSample[], cells = SPARK_CELLS): string {
  const last = history.at(-1)
  if (last === undefined) return ''
  const first = Math.max(history[0]?.minute ?? last.minute, last.minute - cells + 1)
  const shown = history.filter(s => s.minute >= first)
  const top = Math.max(1, ...shown.map(s => s.cores))
  const byMinute = new Map(shown.map(s => [s.minute, s.cores]))
  let out = ''
  for (let m = first; m <= last.minute; m++) {
    const c = byMinute.get(m)
    out += c === undefined ? ' ' : (SPARKS[Math.min(SPARKS.length - 1, Math.floor((c / top) * (SPARKS.length - 1) + 0.5))] ?? '▁')
  }

  return out
}

// ------------------------------------------------------------------------------------------------
// Running-work lines
// ------------------------------------------------------------------------------------------------

/** One line of work running now: `●` granted or a lane at work, `◌` waiting in the queue. */
export type WorkLine = { key: string; isWaiting: boolean; text: string }

/** A lane at work, as the band names it. */
export type LaneAtWork = {
  name: string
  worktree: string
  item: string | null
  description: string
  lock: 'waiting' | 'holding' | null
}

/** The session's subagents working in a lane, as the band names them. */
export function lanesAtWorkNamed(runs: readonly SkeinAgentRun[]): LaneAtWork[] {
  return runs.flatMap(r =>
    r.lane === null
      ? []
      : [
          {
            name: r.lane.worktree.replace(/\/+$/, '').split('/').at(-1) ?? r.lane.worktree,
            worktree: r.lane.worktree.replace(/\/+$/, ''),
            item: r.lane.item?.id ?? r.lane.recordedItem,
            description: r.description,
            lock: r.lane.lock,
          },
        ],
  )
}

/**
 * One line per job of the box that is granted or waiting, and one per lane at work with no job of
 * its own in the view, each cut to `columns` cells: the job's lane is named on its line.
 *
 * A waiting job says its place and what it waits behind. The box's first waiting job is the one the
 * view names what goes before (`ahead`, each with its box); a later one says how many go before it.
 */
export function workLines(view: SkeinBandJobs | null, lanes: readonly LaneAtWork[], columns: number): WorkLine[] {
  const out: WorkLine[] = []
  const jobs = [...(view?.jobs ?? [])].sort(
    (a, b) => Number(a.state === 'waiting') - Number(b.state === 'waiting') || (a.position ?? 0) - (b.position ?? 0),
  )
  const firstWaiting = jobs.find(j => j.state === 'waiting')
  const busy = new Set<string>()
  for (const j of jobs) {
    const lane = lanes.find(l => j.cwd === l.worktree || j.cwd.startsWith(`${l.worktree}/`))
    if (lane) busy.add(lane.worktree)
    const head = [j.command, twoWords(j.command)]
    const inLane = lane ? [`lane ${lane.name}`] : []
    if (j.state === 'granted') {
      const cores = j.granted_cores > 0 ? [`${j.granted_cores} ${j.granted_cores === 1 ? 'core' : 'cores'}`] : []
      out.push({ key: `job:${j.id}`, isWaiting: false, text: fit(`● `, [head, inLane, cores], columns) })
      continue
    }
    const place = j.position === null ? [] : [`#${j.position} in the fleet's queue`, `#${j.position}`]
    const ahead = view?.ahead ?? []
    const behind =
      j === firstWaiting
        ? ahead.length === 0
          ? []
          : [
              `behind ${ahead.map(a => `${a.box} ${a.command}`).join(', ')}`,
              ahead.length === 1 ? `behind ${ahead[0]?.box}` : `behind ${ahead[0]?.box} and ${ahead.length - 1} more`,
            ]
        : j.position !== null && j.position > 1
          ? [`${j.position - 1} ahead`]
          : []
    const reason = j.reason === '' ? [] : [j.reason]
    out.push({ key: `job:${j.id}`, isWaiting: true, text: fit(`◌ `, [head, inLane, place, behind, reason], columns) })
  }
  for (const l of lanes) {
    if (busy.has(l.worktree)) continue
    const lock = l.lock === 'waiting' ? ['waiting for the heavy-work lock'] : l.lock === 'holding' ? ['holding the heavy-work lock'] : []
    out.push({
      key: `lane:${l.worktree}`,
      isWaiting: l.lock === 'waiting',
      text: fit(`● `, [[`lane ${l.name}`], l.item ? [l.item] : [], l.description ? [l.description] : [], lock], columns),
    })
  }

  return out
}

/**
 * `mark` and the parts joined by ` · `, in `columns` cells. Each part is its words from longest to
 * shortest. The first part, what runs, takes its shortest words first; then the last part shrinks,
 * then goes, then the one before it. The first part is never left out: it is cut with `…` when
 * nothing else is left to give.
 */
export function fit(mark: string, parts: readonly (readonly string[])[], columns: number): string {
  const given = parts.filter(p => p.length > 0)
  const pick = given.map(() => 0)
  const draw = () =>
    mark +
    given
      .map((p, i) => ((pick[i] ?? 0) < p.length ? p[pick[i] ?? 0] : null))
      .filter((p): p is string => p !== null && p !== '')
      .join(' · ')
  for (;;) {
    const text = draw()
    if (cells(text) <= columns) return text
    const head = given[0]?.length ?? 0
    if ((pick[0] ?? 0) < head - 1) {
      pick[0] = head - 1
      continue
    }
    let i = given.length - 1
    // The first part keeps its shortest words; every other part may go.
    while (i >= 0 && (pick[i] ?? 0) >= (given[i]?.length ?? 0) - (i === 0 ? 1 : 0)) i--
    if (i < 0) return cut(text, columns)
    pick[i] = (pick[i] ?? 0) + 1
  }
}

function twoWords(command: string): string {
  return command.split(/\s+/).filter(w => w !== '').slice(0, 2).join(' ')
}

/** Cells a line takes: one a character, as every glyph the band draws is one cell wide. */
export function cells(text: string): number {
  return [...text].length
}

/** `text` in at most `columns` cells, `…` where it was cut. */
export function cut(text: string, columns: number): string {
  const chars = [...text]
  if (chars.length <= columns) return text

  return `${chars.slice(0, Math.max(0, columns - 1)).join('').trimEnd()}…`
}

// ------------------------------------------------------------------------------------------------
// Numbers and files
// ------------------------------------------------------------------------------------------------

/** `$31`, or `$4.20` under ten dollars. */
function dollars(n: number): string {
  return n < 10 ? `$${n.toFixed(2)}` : `$${Math.round(n).toLocaleString('en-US')}`
}

/** `14:05`, the UTC time of `ms`. */
export function utcTime(ms: number): string {
  const d = new Date(ms)
  const two = (n: number) => String(n).padStart(2, '0')

  return `${two(d.getUTCHours())}:${two(d.getUTCMinutes())}`
}

export async function parsedFile($: Engine, path: string): Promise<unknown> {
  try {
    return JSON.parse(await $.fs.read(path))
  } catch {
    return null
  }
}

export function numberOr<T>(v: unknown, fallback: T): number | T {
  return typeof v === 'number' && Number.isFinite(v) ? v : fallback
}

export function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}
