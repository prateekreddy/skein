// skein's own tool results drawn for the person at the box (SKEIN-1440, step 1): `skein_resources`,
// `skein_top`, `skein_clean` (and the mod's own `clean`, which answers the same) and
// `skein_requests` as a few aligned lines instead of the JSON, and a `skein-job run` under Bash with
// its place in the fleet's heavy-work queue while it waits, and what it used once it ends.
//
// **It draws and does nothing else.** Every hook here is a `ui.render` hook that reads the result
// the engine already holds, or a `tool.call` hook that passes the call on untouched and only
// watches the box's jobs view while it runs. What the model reads is the tool's own answer, the
// same with this file as without it; ctrl+o (`isExpanded`) draws the engine's own view, the JSON
// whole. A result that is an error, or that this file does not recognise, is drawn by the engine.
//
// What it reads, all of it written by skein or by the tool itself:
//
// * a tool's answer, the first text block of what `skein-mcp` returned (`call` in `bin/skein-mcp`:
//   the result as JSON, then the sentence for the agent);
// * a Bash call's own stdout and stderr, where `skein-job` ends with one line of what the job used
//   (`said_used` in `bin/skein-job`);
// * the box's jobs view, `$SKEIN_STATE/signals/jobs.json` (`View` in `src/jobs.rs`), read every two
//   seconds while a `skein-job run` call runs, and not otherwise.

import { atom, read, update } from 'claude-code'
import type { EngineInterface as Engine, On, RenderElement, Timer } from 'claude-code'

import type { SkeinJobCall, SkeinJobsView, SkeinViewJob } from '../types'

import { ordinal } from './lanes'

const watch = atom({ plugin: 'skein', key: 'jobWatch' } as const, null, { shape: 'job-watch-1' })

// skein writes the jobs view every two seconds (`LOOK_EVERY` in `src/jobs.rs`); reading it at the
// same pace shows a change of place within one tick.
const POLL_MS = 2_000

// A view skein has not rewritten for this long is skein not scheduling: the wrapper itself gives up
// on it after 30 s (`UNANSWERED_SECONDS` in `bin/skein-job`), and nothing is drawn from it.
const VIEW_STALE_MS = 30_000

// The calls kept for their end line: a session's last few `skein-job` runs.
const KEEP_CALLS = 20

// The rows a list shows before it says how many more there are.
const SHOWN_ROWS = 8

// The columns the engine's own `  ⎿  ` takes before a result's first cell.
const RESULT_INDENT = 6

// The one timer that reads the jobs view while a `skein-job` call runs.
let poller: Timer | null = null

// ------------------------------------------------------------------------------------------------
// Lines
// ------------------------------------------------------------------------------------------------

/** How a run of text is drawn: the theme's colour for a state, or dim for a detail. */
export type Tone = 'plain' | 'dim' | 'bold' | 'warning' | 'success' | 'error'

/** One run of text in one tone. */
export type Seg = { text: string; tone?: Tone }

/** One line: its runs, cut at the width unless it `wraps` (skein's own sentences, said whole). */
export type Line = { segs: Seg[]; wraps?: boolean }

const line = (...segs: Seg[]): Line => ({ segs })
const seg = (text: string, tone: Tone = 'plain'): Seg => ({ text, tone })

/** A line as plain text, as the tests compare it. */
export function plain(l: Line): string {
  return l.segs.map(s => s.text).join('')
}

/** The lines as one Box of Text rows, each run in its tone. */
export function drawLines($: Engine, e: Parameters<Engine['ui']['resolve']>[0], lines: readonly Line[]): RenderElement {
  const { Box, Text } = $.ui.resolve(e)

  return (
    <Box flexDirection="column">
      {lines.map(l => (
        <Text wrap={l.wraps ? 'wrap' : 'truncate-end'}>
          {l.segs.map(s => (
            <Text
              dimColor={s.tone === 'dim' ? true : undefined}
              bold={s.tone === 'bold' || s.tone === 'warning' || s.tone === 'error' ? true : undefined}
              color={s.tone === 'warning' || s.tone === 'success' || s.tone === 'error' ? s.tone : undefined}
            >
              {s.text}
            </Text>
          ))}
        </Text>
      ))}
    </Box>
  )
}

// ------------------------------------------------------------------------------------------------
// The hooks
// ------------------------------------------------------------------------------------------------

/** Which of skein's tools a tool name is, by the name the engine gives a plugin's MCP server's tool
 * (`mcp__plugin_skein_skein__skein_top`) or a configured one's (`mcp__skein__skein_top`), and the
 * mod's own `clean` (`mcp__skein__clean`), which answers what `skein_clean` answers. */
export function skeinTool(name: string): 'resources' | 'top' | 'clean' | 'requests' | null {
  if (name === 'mcp__skein__clean') return 'clean'
  const m = /^mcp__(?:plugin_skein_)?skein__skein_(resources|top|clean|requests)$/.exec(name)

  return (m?.[1] as 'resources' | 'top' | 'clean' | 'requests' | undefined) ?? null
}

export function register(on: On): void {
  on('ui.render', { component: 'ToolResult' }, async ($, e, next) => {
    if (e.props.isExpanded === true) return next(e)
    const columns = Math.max(40, (e.viewport?.columns ?? 100) - RESULT_INDENT)
    if (e.props.tool === 'Bash') {
      if (!isRecord(e.props.output)) return next(e)
      const ended = jobEnded(`${e.props.output.stdout ?? ''}\n${e.props.output.stderr ?? ''}`)
      if (ended === null) return next(e)
      const call = (await read($, watch))?.calls.find(c => c.id === e.props.tool_use_id) ?? null
      const own = await next(e)
      const { Box } = $.ui.resolve(e)

      return (
        <Box flexDirection="column">
          {own}
          {drawLines($, e, [endLine(ended, call?.command ?? null, e.props.isErrored)])}
        </Box>
      )
    }
    const tool = skeinTool(e.props.tool)
    if (tool === null || e.props.isErrored) return next(e)
    const result = resultJson(e.props.output)
    if (result === null) return next(e)
    const home = (await $.env.get('HOME')) ?? ''
    const lines =
      tool === 'resources'
        ? resourcesLines(result)
        : tool === 'top'
          ? topLines(result, columns)
          : tool === 'clean'
            ? cleanLines(result, columns, home)
            : requestsLines(result, columns)

    return lines === null ? next(e) : drawLines($, e, lines)
  })

  on('ui.render', { component: 'ToolUse' }, async ($, e, next) => {
    if (e.props.tool !== 'Bash' || !e.props.isRunning || e.props.isExpanded === true) return next(e)
    const found = await read($, watch)
    const call = found?.calls.find(c => c.id === e.props.tool_use_id)
    if (!found || !call || call.jobId === null || found.view === null) return next(e)
    const job = found.view.jobs.find(j => j.id === call.jobId)
    if (job === undefined) return next(e)
    const columns = Math.max(40, (e.viewport?.columns ?? 100) - RESULT_INDENT)
    const lines = waitLines(found.view, job, columns)
    if (lines.length === 0) return next(e)
    const own = await next(e)
    const { Box } = $.ui.resolve(e)

    return (
      <Box flexDirection="column">
        {own}
        {drawLines($, e, lines)}
      </Box>
    )
  })

  // Watches a `skein-job run` while it runs; the call itself is passed on as it came, and its
  // answer is returned as the engine settled it.
  on('tool.call', { tool: 'Bash' }, async ($, e, next) => {
    const command = typeof e.command === 'string' ? jobCommand(e.command) : null
    if (command === null) return next(e)
    await quietly(begin($, e.tool_use_id, command))
    try {
      return await next(e)
    } finally {
      await quietly(end($, e.tool_use_id))
    }
  }).catch(($, e, next) => next(e))
}

// ------------------------------------------------------------------------------------------------
// The jobs view, while a skein-job call runs
// ------------------------------------------------------------------------------------------------

async function begin($: Engine, id: string, command: string): Promise<void> {
  const call: SkeinJobCall = { id, command, jobId: null, isRunning: true }
  await update($, watch, w => ({
    calls: [...(w?.calls ?? []).filter(c => c.id !== id), call].slice(-KEEP_CALLS),
    view: w?.view ?? null,
  }))
  await poll($)
  if (poller === null) poller = $.clock.every(POLL_MS, () => void quietly(poll($)))
}

async function end($: Engine, id: string): Promise<void> {
  await update($, watch, w => (w === null ? w : { ...w, calls: w.calls.map(c => (c.id === id ? { ...c, isRunning: false } : c)) }))
  const left = (await read($, watch))?.calls.some(c => c.isRunning) ?? false
  if (!left) {
    poller?.cancel()
    poller = null
  }
}

/** Reads the jobs view once, and ties each running call that has no job yet to its job. */
async function poll($: Engine): Promise<void> {
  const state = await $.env.get('SKEIN_STATE')
  const text = state ? await $.fs.read(`${state}/signals/jobs.json`).catch(() => null) : null
  const view = jobsView(text, await $.clock.now())
  await update($, watch, w => {
    const calls = w?.calls ?? []
    const taken = new Set(calls.map(c => c.jobId).filter(j => j !== null))
    const tied = calls.map(c => {
      if (!c.isRunning || c.jobId !== null || view === null) return c
      const job = jobFor(view, c.command, taken)
      if (job === null) return c
      taken.add(job.id)

      return { ...c, jobId: job.id }
    })
    const same = w !== null && JSON.stringify(w.view) === JSON.stringify(view) && JSON.stringify(w.calls) === JSON.stringify(tied)

    return same ? w : { calls: tied, view }
  })
}

/**
 * The command a Bash call runs under `skein-job run`, as skein's view spells a job's (its argv
 * joined by spaces); null when the call runs no `skein-job run`. Its options and the `--` go, and
 * so does anything the shell does after the command (`2>&1 | tail`, `&& …`).
 */
export function jobCommand(bash: string): string | null {
  const m = /(?:^|[\s;&|(])skein-job\s+run\b(.*)$/s.exec(bash)
  if (!m?.[1]) return null
  const after = m[1]
  const dashes = /(^|\s)--(\s|$)/.exec(after)
  if (dashes === null) return null
  const rest = after.slice(dashes.index + dashes[0].length)
  const cut = rest.split(/\s*(?:&&|\|\||[;|\n]|\d?>|<)/)[0] ?? ''
  const words = cut
    .trim()
    .split(/\s+/)
    .map(w => w.replace(/^(['"])(.*)\1$/, '$2'))
    .filter(w => w !== '')

  return words.length === 0 ? null : words.join(' ')
}

/** The job a call runs: the newest job of the box's that is waiting or running, has the call's
 * command, and is not another call's. The command is the whole of it, or, where skein cut one past
 * 300 characters and marked it `…`, what skein kept. */
function jobFor(view: SkeinJobsView, command: string, taken: ReadonlySet<string | null>): SkeinViewJob | null {
  const live = view.jobs.filter(j => (j.state === 'waiting' || j.state === 'granted') && !taken.has(j.id))
  const same = live.filter(j => j.command === command || (j.command.endsWith('…') && command.startsWith(j.command.slice(0, -1))))

  return [...same].sort((a, b) => a.waited_secs - b.waited_secs)[0] ?? null
}

/** The jobs view as skein writes it (`View` in `src/jobs.rs`); null when it is missing, not that
 * shape, or older than skein's own staleness line. */
export function jobsView(text: string | null, now: number): SkeinJobsView | null {
  if (text === null) return null
  let v: unknown
  try {
    v = JSON.parse(text)
  } catch {
    return null
  }
  if (!isRecord(v) || typeof v.at !== 'string' || !Array.isArray(v.jobs)) return null
  const at = Date.parse(v.at)
  if (Number.isNaN(at) || now - at > VIEW_STALE_MS) return null
  const jobs: SkeinViewJob[] = []
  for (const j of v.jobs) {
    if (!isRecord(j) || typeof j.id !== 'string' || typeof j.state !== 'string') continue
    jobs.push({
      id: j.id,
      command: str(j.command),
      cwd: str(j.cwd),
      state: j.state,
      cores: num(j.cores) ?? 0,
      granted_cores: num(j.granted_cores) ?? 0,
      position: num(j.position),
      reason: str(j.reason),
      waited_secs: num(j.waited_secs) ?? 0,
    })
  }
  const ahead = (Array.isArray(v.ahead) ? v.ahead : []).filter(isRecord).map(a => ({
    box: str(a.box),
    command: str(a.command),
    cores: num(a.cores) ?? 0,
  }))

  return { at: v.at, cores: num(v.cores) ?? 0, granted: num(v.granted) ?? 0, free: num(v.free) ?? 0, jobs, ahead }
}

/** What is drawn under a running `skein-job` call: its wait while it waits, its grant once it runs. */
export function waitLines(view: SkeinJobsView, job: SkeinViewJob, columns: number): Line[] {
  if (job.state === 'granted') {
    return [line(seg('● ', 'success'), seg(`running on ${cores(job.granted_cores)}`), seg(` · waited ${duration(job.waited_secs)}`, 'dim'))]
  }
  if (job.state !== 'waiting') return []
  const place = job.position === null ? 'waiting' : ordinal(job.position)
  const head = line(seg(`✻ waiting for ${cores(job.cores)} · ${place} · ${duration(job.waited_secs)}`, 'warning'))
  if (job.reason !== '') head.segs.push(seg(` · ${job.reason}`, 'dim'))
  const out = [head]
  const ahead = view.ahead.slice(0, 5)
  for (const a of ahead) {
    out.push(line(seg(cut(`  ahead: ${[a.box, a.command, cores(a.cores)].filter(s => s !== '').join(' · ')}`, columns), 'dim')))
  }
  if (view.ahead.length > ahead.length) out.push(line(seg(`  and ${view.ahead.length - ahead.length} more ahead`, 'dim')))
  out.push(line(seg(`  pool ${view.cores} cores · ${view.granted} granted · ${view.free} free`, 'dim')))

  return out
}

/** What `skein-job` said it used, from its last line (`said_used` in `bin/skein-job`): `asked 4
 * cores, used 1.3 avg (peak 3.8), 1.4 GB peak, 6m12s`. Null when the output holds no such line. */
export function jobEnded(output: string): { asked: number | null; used: number; peakGb: number | null; secs: number } | null {
  const re = /^skein-job: (?:asked (\d+) cores?, )?used (\d+(?:\.\d+)?) avg(?: \(peak \d+(?:\.\d+)?\))?(?:, (\d+(?:\.\d+)?) GB peak)?, (?:(\d+)h)?(?:(\d+)m)?(?:(\d+)s)?$/gm
  let last: RegExpExecArray | null = null
  for (let m = re.exec(output); m !== null; m = re.exec(output)) last = m
  if (last === null) return null
  const [, asked, used, peak, h, m, s] = last
  if (h === undefined && m === undefined && s === undefined) return null

  return {
    asked: asked === undefined ? null : Number(asked),
    used: Number(used),
    peakGb: peak === undefined ? null : Number(peak),
    secs: Number(h ?? 0) * 3600 + Number(m ?? 0) * 60 + Number(s ?? 0),
  }
}

/** The line under a finished `skein-job` call. */
export function endLine(ended: NonNullable<ReturnType<typeof jobEnded>>, command: string | null, isErrored: boolean): Line {
  const used = ended.asked === null ? `used ${ended.used.toFixed(1)} cores` : `used ${ended.used.toFixed(1)} of ${cores(ended.asked)}`
  const parts = [command === null ? null : shortCommand(command), duration(ended.secs), used, ended.peakGb === null ? null : `peak ${ended.peakGb.toFixed(1)} GB`]

  return line(isErrored ? seg('✗ ', 'error') : seg('✓ ', 'success'), seg(parts.filter(p => p !== null).join(' · ')))
}

/** A command in two words, as skein names a job to another box (`short_command` in
 * `src/jobs.rs`): its program and its first argument that is not a flag or a `KEY=value`. */
export function shortCommand(command: string): string {
  const words = command.split(/\s+/).filter(w => w !== '')
  const word = (w: string) => (w.split('/').pop() ?? '').slice(0, 40)
  const program = words[0] === undefined ? '' : word(words[0])
  const first = words.slice(1).find(w => !w.startsWith('-') && !w.includes('='))

  return first === undefined ? program : `${program} ${word(first)}`
}

// ------------------------------------------------------------------------------------------------
// The tools' answers
// ------------------------------------------------------------------------------------------------

/**
 * The JSON a skein tool answered, from the result the engine holds: its first text block, whether
 * the engine keeps the blocks as a list, as MCP's `{ content }`, or as one string. Null when there
 * is no text, the text is not a JSON object, or the object is an error.
 */
export function resultJson(output: unknown): Record<string, unknown> | null {
  const text = firstText(output)
  if (text === null) return null
  let parsed: unknown
  try {
    parsed = JSON.parse(text)
  } catch {
    return null
  }

  return isRecord(parsed) && !('error' in parsed) ? parsed : null
}

function firstText(output: unknown): string | null {
  if (typeof output === 'string') return output
  const blocks = Array.isArray(output) ? output : isRecord(output) && Array.isArray(output.content) ? output.content : null
  if (blocks === null) return null
  const first = blocks.find(b => isRecord(b) && b.type === 'text' && typeof b.text === 'string')

  return isRecord(first) && typeof first.text === 'string' ? first.text : null
}

/** `skein_resources`: one line per limit, anything skein asks first, in skein's own words. */
export function resourcesLines(r: Record<string, unknown>): Line[] | null {
  if (typeof r.at !== 'string' || !isRecord(r.disk)) return null
  const asks = (Array.isArray(r.asks) ? r.asks : []).filter(isRecord)
  const asked = (kind: string) => asks.some(a => a.kind === kind)
  const out: Line[] = []
  const at = Date.parse(r.at)
  out.push(line(seg(Number.isNaN(at) ? `as skein wrote it at ${r.at}` : `as of ${utcTime(at)} UTC`, 'dim')))
  for (const a of asks) {
    const words = [a.enter, a.remind, a.hold].find(w => typeof w === 'string' && w !== '')
    const said = typeof words === 'string' ? words : `skein asks this box about its ${str(a.kind) || 'resources'}.`
    out.push({ segs: [seg(`⚑ ${said}`, 'warning')], wraps: true })
  }

  const disk = r.disk
  const mb = num(disk.mb)
  const limit = num(disk.limit_mb)
  if (mb !== null) {
    const fleet =
      disk.fleet === 'under'
        ? seg('fleet disk fine', 'dim')
        : disk.fleet === 'over'
          ? seg(`fleet disk over its line by ${size((num(disk.fleet_over_by_mb) ?? 0) * MIB)}`, 'warning')
          : null
    const value = limit === null ? size(mb * MIB) : `${size(mb * MIB)} of ${size(limit * MIB)}`
    const over = limit !== null && mb > limit
    out.push(line(seg(label('disk')), seg(value, asked('disk') || over ? 'warning' : 'plain'), ...(fleet ? [seg('   '), fleet] : [])))
  }
  const cpu = isRecord(r.cpu) ? num(r.cpu.cores) : null
  if (cpu !== null) out.push(line(seg(label('cpu')), seg(`${cpu.toFixed(1)} cores`)))
  if (isRecord(r.memory)) {
    const anon = num(r.memory.anon)
    const high = num(r.memory.high)
    const rate = num(r.memory.throttled_per_min)
    if (anon !== null) {
      const value = high === null ? size(anon) : `${size(anon)} of ${size(high)}`
      const slowed = rate === null ? null : rate > 0 ? seg(`slowed ${Math.round(rate)} times a minute`, 'warning') : seg('not slowed', 'dim')
      out.push(line(seg(label('memory')), seg(value, asked('memory') ? 'warning' : 'plain'), ...(slowed ? [seg('   '), slowed] : [])))
    }
  }
  if (isRecord(r.pids)) {
    const current = num(r.pids.current)
    const max = num(r.pids.max)
    if (current !== null) out.push(line(seg(label('procs')), seg(max === null ? `${current}` : `${current} of ${max}`, asked('pids') ? 'warning' : 'plain')))
  }
  if (asks.length === 0) out.push(line(seg('nothing asked of this box', 'success')))
  for (const u of (Array.isArray(r.updates) ? r.updates : []).filter(isRecord)) {
    const what = [str(u.component), str(u.new)].filter(s => s !== '').join(' ')
    out.push(line(seg(u.restart_requested === true ? `restarts onto ${what} once its turn ends` : `a restart would bring ${what}`, 'dim')))
  }
  if (r.restart === 'when-idle') out.push(line(seg('restarts onto the new versions once this turn ends', 'dim')))
  // What `skein_clean` may remove at any time (`ANY_TIME` in `bin/skein-mcp`): download caches and
  // build output no build is running in. A figure from skein's last list, so it says "about".
  const items = isRecord(r.clean) && Array.isArray(r.clean.items) ? r.clean.items.filter(isRecord) : []
  const removable = items
    .filter(i => (i.kind === 'download cache' || i.kind === 'build output') && i.live !== true)
    .reduce((n, i) => n + (num(i.bytes) ?? 0), 0)
  if (removable > 0) out.push(line(seg(`about ${size(removable)} could be removed · /skein clean`, 'dim')))

  return out
}

/** `skein_top`: a table of processes, or by disk, what the box is charged for and its largest. */
export function topLines(r: Record<string, unknown>, columns: number): Line[] | null {
  if (r.by === 'disk') return topDiskLines(r, columns)
  if ((r.by !== 'cpu' && r.by !== 'memory') || !Array.isArray(r.processes)) return null
  const procs = r.processes.filter(isRecord)
  const inBox = num(r.processes_in_box) ?? procs.length
  const out: Line[] = [line(seg(`by ${r.by} · ${inBox} ${inBox === 1 ? 'process' : 'processes'} in this box`, 'dim'))]
  out.push(line(seg(`${'cpu'.padStart(7)}  ${'mem'.padStart(7)}  ${'age'.padStart(4)}  command`, 'dim')))
  // The figures take 7 + 2 + 7 + 2 + 4 + 2 cells; the command has the rest.
  const room = Math.max(10, columns - 24)
  for (const p of procs) {
    const isParentless = p.parentless === true
    const mark = ' no parent'
    const command = cut(str(p.command), isParentless ? room - mark.length : room)
    out.push(
      line(
        seg(`${seconds(num(p.cpu_seconds) ?? 0).padStart(7)}  ${size(num(p.rss_bytes) ?? 0).padStart(7)}  ${age(num(p.age_seconds) ?? 0).padStart(4)}  ${command}`),
        ...(isParentless ? [seg(mark, 'warning')] : []),
      ),
    )
  }
  const parentless = num(r.parentless_in_box) ?? 0
  if (parentless > 0) {
    out.push(line(seg(`${parentless} ${parentless === 1 ? 'process has' : 'processes have'} no parent`, 'dim')))
  }

  return out
}

function topDiskLines(r: Record<string, unknown>, columns: number): Line[] | null {
  if (!Array.isArray(r.dirs)) return null
  const out: Line[] = []
  const charged = num(r.charged_mb)
  const limit = num(r.limit_mb)
  if (charged !== null) {
    const over = limit !== null && charged > limit
    out.push(line(seg('by disk · this box holds ', 'dim'), seg(limit === null ? size(charged * MIB) : `${size(charged * MIB)} of ${size(limit * MIB)}`, over ? 'warning' : 'plain')))
  } else {
    out.push(line(seg('by disk', 'dim')))
  }
  const dirs = r.dirs.filter(isRecord)
  const checkout = str(r.checkout)
  if (dirs.length > 0) {
    out.push(line(seg(cut(`largest in ${checkout || 'the checkout'}`, columns), 'dim')))
    for (const d of dirs) {
      const path = str(d.path)
      const rel = checkout !== '' && path.startsWith(`${checkout}/`) ? path.slice(checkout.length + 1) : path
      out.push(line(seg(`${size(num(d.bytes) ?? 0).padStart(9)}  ${cutStart(rel, columns - 11)}`)))
    }
  }
  const cleanable = isRecord(r.cleanable) && isRecord(r.cleanable.bytes_by_kind) ? r.cleanable.bytes_by_kind : null
  if (cleanable !== null) {
    const kinds = Object.entries(cleanable)
      .map(([k, v]) => [k, num(v) ?? 0] as const)
      .sort((a, b) => b[1] - a[1])
    if (kinds.length > 0) {
      out.push(line(seg('removable and made again, by kind · skein_clean removes them', 'dim')))
      for (const [kind, bytes] of kinds) out.push(line(seg(`${size(bytes).padStart(9)}  ${kind}`)))
    }
  }
  out.push(...sharedLines(r))

  return out
}

/** The repo's shared cache and tools area, each on a line of its own: never counted against the box. */
function sharedLines(r: Record<string, unknown>): Line[] {
  const out: Line[] = []
  const rc = r.shared_repo_cache
  if (isRecord(rc)) {
    const bytes = num(rc.bytes) ?? 0
    const why = typeof rc.why_not === 'string' && bytes > 0 ? ` · ${rc.why_not}` : ''
    out.push(line(seg(`shared repo cache · ${bytes > 0 ? size(bytes) : 'empty'} · not counted here${why}`, 'dim')))
  }
  const rt = r.repo_tools
  if (isRecord(rt)) {
    const unused = Array.isArray(rt.unused) ? rt.unused.length : 0
    const bytes = num(rt.bytes) ?? 0
    const what = unused === 0 ? 'no unused versions' : `${unused} unused ${unused === 1 ? 'version' : 'versions'}`
    out.push(line(seg(`repo tools · ${size(bytes)} · ${what} · not counted here`, 'dim')))
  }

  return out
}

/** `skein_clean`: what can go now, sizes first, apart from what is kept and why; or what a removal
 * removed and what it could not. */
export function cleanLines(r: Record<string, unknown>, columns: number, home: string): Line[] | null {
  if (Array.isArray(r.removed)) return removedLines(r, columns, home)
  if (!Array.isArray(r.items)) return null
  const items = r.items.filter(isRecord).map(i => ({
    shown: shownPath(str(i.in_box) || str(i.path), home),
    kind: str(i.kind),
    bytes: num(i.bytes) ?? 0,
    where: str(i.where),
    safe: i.safe_now === true,
    why: typeof i.why_not === 'string' ? i.why_not : '',
  }))
  const own = items.filter(i => i.where === 'box')
  const safe = own.filter(i => i.safe).sort((a, b) => b.bytes - a.bytes)
  const total = safe.reduce((n, i) => n + i.bytes, 0)
  const out: Line[] = []
  out.push(line(safe.length === 0 ? seg('nothing in this box can be removed now', 'dim') : seg(`${size(total)} safe to remove now`, 'success')))
  const shown = safe.slice(0, SHOWN_ROWS)
  const pathWidth = Math.min(Math.max(0, ...shown.map(i => i.shown.length)), Math.max(16, columns - 30))
  for (const i of shown) {
    out.push(line(seg(`${size(i.bytes).padStart(8)}  ${cutStart(i.shown, pathWidth).padEnd(pathWidth)}  `), seg(i.kind, 'dim')))
  }
  if (safe.length > shown.length) {
    const rest = safe.slice(shown.length).reduce((n, i) => n + i.bytes, 0)
    out.push(line(seg(`${' '.repeat(8)}  and ${safe.length - shown.length} more, ${size(rest)}`, 'dim')))
  }
  // What is kept, a line for each reason: skein's own words for why it may not go now.
  const kept = new Map<string, { kind: string; n: number; bytes: number; why: string }>()
  for (const i of own.filter(i => !i.safe)) {
    const k = `${i.kind}\n${i.why}`
    const g = kept.get(k) ?? { kind: i.kind, n: 0, bytes: 0, why: i.why }
    g.n += 1
    g.bytes += i.bytes
    kept.set(k, g)
  }
  for (const g of kept.values()) {
    const what = `${g.n} ${g.n === 1 ? g.kind : plural(g.kind)}, ${size(g.bytes)}`
    out.push({ segs: [seg(`kept · ${what}${g.why ? `: ${g.why}` : ''}`, 'dim')], wraps: true })
  }
  out.push(...sharedLines(r))
  if (safe.length > 0) out.push(line(seg('/skein clean opens the list to remove them · ctrl+o shows all ', 'dim'), seg(`${items.length}`, 'dim')))

  return out
}

function removedLines(r: Record<string, unknown>, columns: number, home: string): Line[] {
  const removed = (r.removed as unknown[]).filter((p): p is string => typeof p === 'string')
  const failed = (Array.isArray(r.failed) ? r.failed : []).filter(isRecord)
  const out: Line[] = []
  const freed = num(r.freed_bytes) ?? 0
  out.push(line(seg(`removed ${removed.length} · about ${size(freed)} freed`, removed.length > 0 ? 'success' : 'dim')))
  for (const p of removed.slice(0, SHOWN_ROWS)) out.push(line(seg(`  ${cutStart(shownPath(p, home), columns - 2)}`, 'dim')))
  if (removed.length > SHOWN_ROWS) out.push(line(seg(`  and ${removed.length - SHOWN_ROWS} more`, 'dim')))
  for (const f of failed) {
    out.push({ segs: [seg('✗ ', 'error'), seg(`${shownPath(str(f.path), home)}: ${str(f.why)}`)], wraps: true })
  }
  if (removed.length > 0) out.push(line(seg("this box's disk figure catches up within five minutes", 'dim')))

  return out
}

/** `skein_requests`: what this box asked and the owner's answer, the ones still waiting first. */
export function requestsLines(r: Record<string, unknown>, columns: number): Line[] | null {
  if (!Array.isArray(r.requests)) return null
  const rows = r.requests.filter(isRecord)
  if (rows.length === 0) return [line(seg('this box has asked for nothing', 'dim'))]
  const waiting = rows.filter(q => q.state === 'waiting')
  const answered = rows.filter(q => q.state !== 'waiting')
  const out: Line[] = []
  for (const q of waiting) out.push(line(seg(cut(`⚑ ${requestWhat(q)} · waiting for the owner`, columns), 'warning')))
  for (const q of answered) {
    const answer = typeof q.answer === 'string' && q.answer !== '' && q.queue === 'question' ? `: ${oneLineOf(q.answer)}` : ''
    out.push(line(seg(cut(`  ${requestWhat(q)} · ${str(q.state)}${answer}`, columns))))
  }

  return out
}

function requestWhat(q: Record<string, unknown>): string {
  if (q.queue === 'package') return ['package', str(q.kind), Array.isArray(q.packages) ? q.packages.map(String).join(' ') : ''].filter(s => s !== '').join(' ')
  if (q.queue === 'write') return `write ${str(q.repo)}`
  if (q.queue === 'disk') return `disk ${str(q.size)}`.trim()
  if (q.queue === 'question') return `question “${oneLineOf(str(q.question))}”`

  return str(q.queue)
}

// ------------------------------------------------------------------------------------------------
// Words and numbers
// ------------------------------------------------------------------------------------------------

const MIB = 1024 * 1024

/** A row's label, padded so the values line up. */
const label = (name: string) => name.padEnd(9)

/** Bytes as a person reads them, in skein's units (1024): `717 MB`, `2.62 GB`, `10 GB`. */
export function size(bytes: number): string {
  if (bytes <= 0) return '0 MB'
  const mib = bytes / MIB
  if (mib < 1024) return `${Math.max(1, Math.round(mib))} MB`
  const gib = mib / 1024
  const fixed = gib < 10 ? gib.toFixed(2) : gib < 100 ? gib.toFixed(1) : gib.toFixed(0)

  return `${fixed.replace(/\.?0+$/, '')} GB`
}

/** `1789s`, or `2.3s` under ten seconds. */
function seconds(s: number): string {
  return s < 10 ? `${s.toFixed(1)}s` : `${Math.round(s)}s`
}

/** An age in its largest whole unit: `45s`, `12m`, `6h`, `2d`. */
function age(s: number): string {
  if (s < 60) return `${Math.floor(s)}s`
  if (s < 3600) return `${Math.floor(s / 60)}m`
  if (s < 86400) return `${Math.floor(s / 3600)}h`

  return `${Math.floor(s / 86400)}d`
}

/** A wait or a run: `45 s`, `1 m`, `3 m 12 s`, `2 h 5 m`. */
export function duration(secs: number): string {
  const s = Math.max(0, Math.round(secs))
  if (s < 60) return `${s} s`
  if (s < 3600) return s % 60 === 0 ? `${Math.floor(s / 60)} m` : `${Math.floor(s / 60)} m ${s % 60} s`
  const m = Math.floor((s % 3600) / 60)

  return m === 0 ? `${Math.floor(s / 3600)} h` : `${Math.floor(s / 3600)} h ${m} m`
}

const cores = (n: number) => `${n} ${n === 1 ? 'core' : 'cores'}`

function plural(kind: string): string {
  if (kind.endsWith('cache')) return `${kind}s`
  if (kind.endsWith('output')) return kind

  return `${kind}s`
}

/** A path under the box's home as `~/…`. */
function shownPath(path: string, home: string): string {
  const h = home.replace(/\/+$/, '')

  return h !== '' && (path === h || path.startsWith(`${h}/`)) ? `~${path.slice(h.length)}` : path
}

/** Cut at the end to `width` cells, with `…` where anything was left out. */
function cut(text: string, width: number): string {
  return text.length <= width ? text : `${text.slice(0, Math.max(1, width - 1))}…`
}

/** Cut at the start to `width` cells, so a path keeps its last parts. */
function cutStart(text: string, width: number): string {
  return text.length <= width ? text : `…${text.slice(text.length - Math.max(1, width - 1))}`
}

function oneLineOf(text: string): string {
  return (text.trim().split('\n')[0] ?? '').replace(/\s+/g, ' ')
}

/** `14:05`, the UTC time of `ms`. */
function utcTime(ms: number): string {
  const d = new Date(ms)
  const two = (n: number) => String(n).padStart(2, '0')

  return `${two(d.getUTCHours())}:${two(d.getUTCMinutes())}`
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

function num(v: unknown): number | null {
  return typeof v === 'number' && Number.isFinite(v) ? v : null
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

async function quietly(part: Promise<void>): Promise<void> {
  try {
    await part
  } catch {
    // Drawing is not the call's to fail.
  }
}
