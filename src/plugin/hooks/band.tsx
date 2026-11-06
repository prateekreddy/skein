// What skein needs from this box, or offers it, drawn above the prompt (SKEIN-1362, SKEIN-1440):
//
// * what waits on the person, in amber, each worded as what to do: the questions and requests
//   waiting for an answer, and versions that wait for a restart. Each new one also raises one
//   toast, since a collapsed band shows nothing;
// * the disk skein asks the box to free, dim: clearing it is the agent's, and `/skein clean` is the
//   person's override (SKEIN-1460), so it is not counted as waiting, toasted, or a reason to open
//   the pane. A box more than a tenth over its share, one skein holds a command for, waits on the
//   person too, amber with Free disk (SKEIN-1475). A request for more allowance,
//   `skein_request_disk`, is the person's to answer, and waits with the other requests;
// * one dim line of vitals, always: disk, memory and CPU against the box's limits, and its spend,
//   each part drawn only while its signal is fresh (`vitals.ts`);
// * one line for each thing running now: each of the box's granted or waiting heavy jobs, and each
//   lane a subagent is working in, while it runs.
//
// The skein pane (`pane.tsx` draws it) holds the box, what waits on the person, the lanes, the
// queue, the spend, the items held and a short log; `/skein clean` opens it on the disk list, with a
// Clean button on each item. This file reads what it draws and decides when it opens: as the session starts, on `/skein`
// and `/skein clean`, and whenever something new waits on the person, but not again for what
// already waited when the person closed it.
//
// Nothing here is new authority. Every figure is read from what skein already writes or answers,
// through the entry points the agent itself has:
//
// * the disk figures and skein's ask from the signal file, `$SKEIN_STATE/signals/resources.json`;
// * the disk list, and every removal, from `skein-mcp clean [PATH...]`, the one implementation the
//   `skein_clean` MCP tool also runs;
// * the requests from `skein-mcp requests`, the `skein_requests` tool's own function;
// * the unread count from `mailbox.sh unread`, which marks nothing: mail is delivered by the
//   turn-boundary hooks alone;
// * the box's own spend today and over 7 days from `$SKEIN_STATE/signals/usage.json`, which
//   skein-server writes beside the resource signal once an hour (SKEIN-1384). The fleet runs on a
//   Claude login, so the dollars are an estimate at the API's prices, and `/skein status` says so;
// * the jobs from `$SKEIN_STATE/signals/jobs.json`, and the lanes at work from the agents panel's
//   own record of the session's subagents (`lanesWorking`), which reads `skein-mcp lane list`.
//
// The `clean` tool this file registers (`mcp__skein__clean`) is `skein-mcp clean` too, and answers
// what the MCP tool answers, which `tests/band.test.tsx` holds against `tests/clean-contract.ts`
// and `tests/plugin_tools.rs` holds the MCP tool to.

import { atom, read, update } from 'claude-code'
import type {
  CommandRunResult,
  EngineInterface as Engine,
  On,
  SessionStartInput,
  Timer,
  TurnCompleteInput,
} from 'claude-code'

import type {
  SkeinAgentRun,
  SkeinBand,
  SkeinBandJobs,
  SkeinCleanRow,
  SkeinCpuSample,
  SkeinDiskAsk,
  SkeinLaneRecord,
  SkeinPane,
  SkeinPaneLogEntry,
  SkeinPaneView,
  SkeinRequestRow,
  SkeinSharedCache,
  SkeinUpdate,
} from '../types'

import { laneRuns, lanesAtWork, lanesWorking } from './agents-panel'
import { boardLanes, stageWords } from './board'
import { lanesListed, ordinal } from './lanes'
import { size } from './tool-results'
import {
  cut,
  isRecord,
  lanesAtWorkNamed,
  numberOr,
  readJobsView,
  readResources,
  readSpend,
  sampled,
  utcTime,
  vitalsLine,
  workLines,
} from './vitals'

const band = atom({ plugin: 'skein', key: 'band' } as const, null, { shape: 'band-4' })
const bandJobs = atom({ plugin: 'skein', key: 'bandJobs' } as const, null, { shape: 'band-jobs-2' })
const cpuHistory = atom({ plugin: 'skein', key: 'cpuHistory' } as const, [], { shape: 'cpu-history-1' })
const toasted = atom({ plugin: 'skein', key: 'bandToasted' } as const, [], { shape: 'band-toasted-1' })
const cleanSaid = atom({ plugin: 'skein', key: 'cleanSaid' } as const, null, {
  shape: 'clean-said-1',
})
const paneView = atom({ plugin: 'skein', key: 'paneView' } as const, { view: 'board', lane: null } as SkeinPaneView, {
  shape: 'pane-view-1',
})
const pane = atom(
  { plugin: 'skein', key: 'pane' } as const,
  { seen: null, lanes: [], log: [], marks: {} } as SkeinPane,
  { shape: 'pane-1' },
)

export const PANE = 'skein'

// The log's lines the pane keeps.
const LOG_KEEP = 8

// How often the band reads again. skein lists the box's disk every five minutes and writes the
// signal file every minute; a request answered shows within a minute.
const LOOK_EVERY_MS = 60_000

// How often the band reads the jobs view, which skein rewrites at least every 20 s: a job's line
// comes and goes within ten seconds of skein seeing it start or end. One file read, nothing run.
const JOBS_EVERY_MS = 10_000

// A removal of a large build directory takes a while; `$.process.run`'s ceiling is ten minutes.
const CLEAN_TIMEOUT_MS = 600_000

const CLEAN_DESCRIPTION =
  'What in this box can be removed and made again: its download caches, build output and ' +
  "scratchpads, and the repo's shared download cache, each with its size and whether it is safe " +
  'to remove now. Call it with no paths to see the list; pass paths from that list to remove ' +
  'them. The same as the skein_clean tool: nothing else is removed, a link is never followed, ' +
  'and config and logins are never on the list.'

// The one timer that keeps the band current. A resumed session raises `session.start` again, so a
// start cancels the timer the last one left before it starts its own: one look per interval.
let ticker: Timer | null = null
let jobsTicker: Timer | null = null

/** Setup, called from the one `session.start` hook `register.tsx` owns (SKEIN-1387): the `clean`
 * tool, and, in a session a person watches, the looks that keep the band current. */
export async function onStart($: Engine, e: SessionStartInput): Promise<void> {
  ticker?.cancel()
  ticker = null
  jobsTicker?.cancel()
  jobsTicker = null
  await $.tool.register({
    name: 'clean',
    description: CLEAN_DESCRIPTION,
    inputSchema: {
      type: 'object',
      properties: { paths: { type: 'array', items: { type: 'string' } } },
      additionalProperties: false,
    },
  })
  // A `claude -p` draws no band, so it looks only when asked: `/skein status` looks for itself.
  if (!e.isInteractive) return
  // The first look after the session starts rather than inside it: three small processes the
  // first prompt need not wait for.
  // A look that fails, or outlives the session, draws what the last one found.
  $.clock.after(0, () => void look($, true).catch(() => {}))
  ticker = $.clock.every(LOOK_EVERY_MS, () => void look($, true).catch(() => {}))
  jobsTicker = $.clock.every(JOBS_EVERY_MS, () => void lookAtJobs($).catch(() => {}))
}

/** After each turn of the main loop (`register.tsx`'s one `turn.complete` hook calls it): the
 * agent may have asked for something or cleaned, so the band looks again, after the turn. */
export async function onTurnComplete($: Engine, e: TurnCompleteInput): Promise<void> {
  if (e.agentId) return
  $.clock.after(0, () => void look($, true).catch(() => {}))
}

/** `/skein <args>`, handed over by `identity.ts`, which owns the command's hook. */
export async function subcommand($: Engine, args: string): Promise<CommandRunResult> {
  const sub = args.trim().split(/\s+/)[0]
  if (sub === 'status') {
    await look($)
    const [jobs, lanes, box, now] = await Promise.all([readJobs($), readLanes($), $.env.get('SKEIN_BOX'), $.clock.now()])

    return { text: status(await read($, band), { jobs, lanes, box: box || null, at: utcTime(now) }) }
  }
  if (sub === 'requests') {
    await look($)

    return { text: requestLines(await read($, band)) }
  }
  if (sub === 'clean') {
    await look($)
    const opened = await openSkein($, 'clean')

    return {
      text: opened.isPlaced
        ? 'the list of what this box can remove is open.'
        : 'the list could not be opened here. skein_clean lists the same items.',
    }
  }

  return { text: `there is no /skein ${sub}. Try /skein status, /skein clean or /skein requests.` }
}

/** What the skein pane draws from (`pane.tsx`): this file's record of the box, read in one go. */
export async function paneData($: Engine): Promise<{
  found: SkeinBand | null
  jobs: SkeinBandJobs | null
  history: SkeinCpuSample[]
  kept: SkeinPane
  view: SkeinPaneView
  said: string | null
}> {
  const [found, jobs, history, kept, view, said] = await Promise.all([
    read($, band),
    read($, bandJobs),
    read($, cpuHistory),
    read($, pane),
    read($, paneView),
    read($, cleanSaid),
  ])

  return { found, jobs, history: history ?? [], kept, view, said }
}

/** The pane's view, as the person moved it: a lane opened, or back to the panels. */
export async function showInPane($: Engine, view: SkeinPaneView): Promise<void> {
  await update($, paneView, () => view)
}

/**
 * Open the skein pane at `view` because the person asked (`/skein`, `/skein clean`, a band
 * button): placed at any width.
 */
export async function openSkein($: Engine, view: SkeinPaneView['view']): Promise<{ isPlaced: boolean }> {
  await update($, paneView, () => ({ view, lane: null }))
  try {
    return await $.ui.open({ id: PANE, title: 'skein', focus: true })
  } catch {
    return { isPlaced: false }
  }
}

export function register(on: On): void {
  on('tool.call', { tool: 'mcp__skein__clean' }, async ($, e) => {
    const paths = 'paths' in e ? e.paths : undefined
    // The MCP tool's own refusal, word for word: a list of anything but paths never reaches the
    // command line, which can only carry strings.
    if (paths !== undefined && !(Array.isArray(paths) && paths.every(p => typeof p === 'string' && p !== ''))) {
      return mcpAnswer({ error: 'paths-must-be-a-list-of-paths' }, '')
    }
    const ran = await runClean($, paths ?? [])
    if (ran.result !== null && 'removed' in ran.result) void look($).catch(() => {})

    return mcpAnswer(ran.result ?? { error: 'clean-did-not-answer' }, ran.said)
  }).catch(() => mcpAnswer({ error: 'clean-did-not-answer' }, ''))

  // The vitals line first and always, so the eye learns where it sits; then what waits on the
  // person, in amber; then a line for each thing running now, the disk skein asks for, and unread
  // mail, dim. Where the band has fewer rows than that, one line: the first thing that waits on the
  // person, or the vitals, with a count of what is left out. The disk ask never takes that line:
  // it is the agent's to clear.
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const found = await read($, band)
    // Outside a skein box there is nothing of skein's to draw.
    if (e.props.hasSurvey || found === null || !(await $.env.get('SKEIN_STATE'))) return next(e)
    const said = await read($, cleanSaid)
    const columns = e.props.bodyColumns
    // `⚑ ` takes two cells before each line that waits on the person.
    const asks = asked(found, columns - 2)
    const history = (await read($, cpuHistory)) ?? []
    const vitals = vitalsLine(
      {
        disk: found.disk !== null && found.disk.mb !== null ? { mb: found.disk.mb, limit_mb: found.disk.limit_mb } : null,
        memory: found.memory,
        cpu: found.cpu === null ? [] : history,
        spend: found.spend,
      },
      columns,
    )
    const work = workLines(await read($, bandJobs), lanesAtWorkNamed(await lanesWorking($)), columns)

    const { Box, Button, Text } = $.ui.resolve(e)
    // Free disk opens the pane on the disk list; Read and Requests on what waits.
    const open = (ask: Ask) => () => void openSkein($, ask.key === 'disk' ? 'clean' : 'board')
    const askRow = (ask: Ask, text = ask.text) => (
      <Box>
        <Text color="warning">{LOUD}</Text>
        <Text color="warning" wrap="truncate-end">
          {text}{' '}
        </Text>
        {ask.button !== undefined && <Button key={`band:${ask.key}`} label={ask.button} onPress={open(ask)} />}
      </Box>
    )
    // A held ask waits on the person, amber, in `asks`; one the agent is only told is dim.
    const disk = found.disk?.ask && !found.disk.ask.held ? quietAskLine(found.disk.ask) : null
    const mail = mailNote(found.mail ?? 0)
    // The pane opened by itself but this terminal could not place it: the band says where it is.
    const unplaced = (await paneWaitsUnplaced($)) ? UNPLACED : null
    const rows = asks.length + 1 + work.length + (disk === null ? 0 : 1) + (mail === null ? 0 : 1) + (said === null ? 0 : 1) + (unplaced === null ? 0 : 1)
    if (rows > Math.max(1, e.props.maxRows)) {
      const first = asks[0]
      const more = rows - 1
      const left = `· ${more} more`
      if (first !== undefined) return askRow(first, `${cut(first.text, columns - 2 - left.length - 1 - (first.button ? first.button.length + 5 : 0))} ${left}`)

      return <Text dimColor>{`${cut(vitals, columns - left.length - 1)} ${left}`}</Text>
    }

    return (
      <Box flexDirection="column">
        <Text dimColor wrap="truncate-end">{vitals}</Text>
        {asks.map(ask => askRow(ask))}
        {work.map(w => (
          <Text color={w.isWaiting ? undefined : 'suggestion'} dimColor={w.isWaiting ? true : undefined} wrap="truncate-end">
            {w.text}
          </Text>
        ))}
        {disk !== null && <Text dimColor wrap="truncate-end">{disk}</Text>}
        {mail !== null && <Text dimColor wrap="truncate-end">{mail}</Text>}
        {unplaced !== null && <Text dimColor wrap="truncate-end">{unplaced}</Text>}
        {said !== null && (
          <Box>
            <Text dimColor>{said} </Text>
            <Button key="band:said" label="OK" role="dismiss" onPress={() => update($, cleanSaid, () => null)} />
          </Box>
        )}
      </Box>
    )
  })

  // `/skein status` as rows, what needs the person in amber: drawn from the row's own text, which
  // is what the transcript keeps, so a row scrolled back to reads as it did when it was said.
  on('ui.render', { component: 'CommandOutput' }, async ($, e, next) => {
    if (e.props.command !== 'skein' || e.props.isErrored || e.props.args.trim().split(/\s+/)[0] !== 'status') return next(e)
    const rows = statusDrawn(e.props.text)
    if (rows === null) return next(e)
    const { Box, Text } = $.ui.resolve(e)

    return (
      <Box flexDirection="column">
        {rows.map(r =>
          r.kind === 'row' ? (
            <Text>
              <Text bold={r.isLoud ? true : undefined} color={r.isLoud ? 'warning' : undefined}>
                {r.head}
              </Text>
              <Text color={r.isLoud ? 'warning' : undefined}>{r.value}</Text>
              {r.detail !== '' && <Text dimColor>{r.detail}</Text>}
            </Text>
          ) : (
            <Text dimColor={r.kind === 'dim' ? true : undefined}>{r.text}</Text>
          ),
        )}
      </Box>
    )
  })
}

// ------------------------------------------------------------------------------------------------
// Reading
// ------------------------------------------------------------------------------------------------

/**
 * Read everything the band draws, and keep it for the session: the CPU reading joins its history,
 * and, where `isWatched` (a look on the band's own timer or after a turn, never `/skein status`'s),
 * each new thing that waits on the person raises one toast.
 */
async function look($: Engine, isWatched = false): Promise<void> {
  const [resources, clean, requests, mail, spend] = await Promise.all([
    readResources($),
    readClean($),
    readRequests($),
    readMail($),
    readSpend($),
  ])
  const found: SkeinBand = {
    disk: resources?.disk ?? null,
    clean,
    requests,
    mail,
    spend,
    memory: resources?.memory ?? null,
    cpu: resources?.cpu ?? null,
    pids: resources?.pids ?? null,
    asked: resources?.asked ?? [],
    updates: resources?.updates ?? [],
  }
  await update($, band, () => found)
  if (resources !== null && resources.cpu !== null) {
    const { at, cpu } = resources
    await update($, cpuHistory, h => sampled(h ?? [], cpu, at))
  }
  await lookAtJobs($)
  if (isWatched) {
    await raiseToasts($, found)
    await followLook($, found)
  }
}

/**
 * After a look in a session a person watches: the lanes again, a log line for each change, and the
 * pane opened by itself on the first look and whenever something new waits on the person. What
 * waited at the last look never opens it again, so a person who closed it sees it again only for a
 * new thing, however many looks pass.
 */
async function followLook($: Engine, found: SkeinBand): Promise<void> {
  const [listed, runs, jobs, now] = await Promise.all([listLanes($), laneRuns($), read($, bandJobs), $.clock.now()])
  const was = await read($, pane)
  const lanes: SkeinLaneRecord[] = listed ?? was.lanes
  const waiting = waitingThings(found)
  const keys = waiting.map(w => w.key)
  const marks = marksOf(waiting, lanes, runs, jobs)
  const isFirst = was.seen === null
  const isNew = keys.some(k => !(was.seen ?? []).includes(k))
  const entries = isFirst ? [] : logged(was.marks, marks, utcTime(now))
  await update($, pane, p => ({ ...p, seen: keys, lanes, marks, log: [...p.log, ...entries].slice(-LOG_KEEP) }))
  if (isFirst || isNew) {
    try {
      // Unasked: placed only where the terminal is wide enough, and otherwise waits undrawn,
      // which the band then says.
      await $.ui.open({ id: PANE, title: 'skein' })
    } catch {
      // No surface to open it on: the band and the toasts still say it.
    }
  }
}

/** True while the skein pane is open but undrawn: opened unasked on a terminal too narrow for it. */
async function paneWaitsUnplaced($: Engine): Promise<boolean> {
  try {
    return (await $.ui.panes()).some(p => p.id === PANE && !p.isPlaced)
  } catch {
    return false
  }
}

const UNPLACED = 'The skein pane waits for a wider terminal. /skein shows it here.'

/** What the log follows, each as `[who, what it is now, what to say once it is gone]`. */
function marksOf(
  waiting: readonly { key: string; short: string }[],
  lanes: readonly SkeinLaneRecord[],
  runs: readonly SkeinAgentRun[],
  jobs: SkeinBandJobs | null,
): Record<string, string> {
  const out: Record<string, string> = {}
  for (const w of waiting) out[`wait:${w.key}`] = JSON.stringify(['skein', `${w.short} waits on you`, `${w.short}: no longer waiting`])
  const board = boardLanes(lanes, runs, jobs)
  for (const l of board) out[`lane:${l.path}`] = JSON.stringify([l.name, stageWords(l), 'lane gone'])
  for (const j of jobs?.jobs ?? []) {
    const lane = board.find(l => j.cwd === l.path || j.cwd.startsWith(`${l.path}/`))
    const what = j.command.split(/\s+/).filter(w => w !== '').slice(0, 2).join(' ')
    out[`job:${j.id}`] = JSON.stringify([lane?.name ?? 'box', `${what} ${j.state === 'granted' ? 'running' : 'queued'}`, `${what} ended`])
  }

  return out
}

/** A log line for each mark that is new or changed since the last look, and each one gone. */
function logged(before: Record<string, string>, now: Record<string, string>, at: string): SkeinPaneLogEntry[] {
  const said = (mark: string, gone: boolean): SkeinPaneLogEntry | null => {
    try {
      const [who, what, after] = JSON.parse(mark) as [string, string, string]
      return { at, who, what: gone ? after : what }
    } catch {
      return null
    }
  }
  const out: (SkeinPaneLogEntry | null)[] = []
  for (const [k, v] of Object.entries(now)) if (before[k] !== v) out.push(said(v, false))
  for (const [k, v] of Object.entries(before)) if (!(k in now)) out.push(said(v, true))

  return out.filter((e): e is SkeinPaneLogEntry => e !== null)
}

/** The jobs view, kept only when what it says changed, so a rewrite that changes nothing draws nothing. */
async function lookAtJobs($: Engine): Promise<void> {
  const jobs = await readJobsView($)
  const kept = await read($, bandJobs)
  if (JSON.stringify(kept) !== JSON.stringify(jobs)) await update($, bandJobs, () => jobs)
}

/** One toast for each thing that waits on the person and had none yet; one gone is forgotten, so a
 * thing that comes back is new again. */
async function raiseToasts($: Engine, found: SkeinBand): Promise<void> {
  const waiting = waitingThings(found)
  const before = new Set((await read($, toasted)) ?? [])
  for (const w of waiting) if (!before.has(w.key)) $.ui.toast(w.toast, { timeoutMs: 8000 })
  const keys = waiting.map(w => w.key)
  if (JSON.stringify(keys) !== JSON.stringify([...before])) await update($, toasted, () => keys)
}

async function readClean($: Engine): Promise<SkeinBand['clean']> {
  const ran = await runClean($, [])
  const listed = ran.result
  if (listed === null || !Array.isArray(listed.items)) return null
  const rows: SkeinCleanRow[] = listed.items.filter(isRecord).map(it => ({
    path: String(it.path ?? ''),
    in_box: String(it.in_box ?? it.path ?? ''),
    kind: String(it.kind ?? ''),
    bytes: numberOr(it.bytes, 0) ?? 0,
    where: it.where === 'shared' ? 'shared' : 'box',
    safe_now: it.safe_now === true,
    why_not: typeof it.why_not === 'string' ? it.why_not : null,
  }))
  const rc = listed.shared_repo_cache
  const shared: SkeinSharedCache | null = isRecord(rc)
    ? {
        path: String(rc.path ?? ''),
        bytes: numberOr(rc.bytes, 0) ?? 0,
        safe_to_clean_now: rc.safe_to_clean_now === true,
        why_not: typeof rc.why_not === 'string' ? rc.why_not : null,
      }
    : null

  return { rows, removable_now_bytes: numberOr(listed.removable_now_bytes, 0) ?? 0, shared }
}

async function readRequests($: Engine): Promise<SkeinBand['requests']> {
  const ran = await skeinMcp($, ['requests'], 30_000)
  if (ran.result === null || !Array.isArray(ran.result.requests) || 'error' in ran.result) return null

  return ran.result.requests.filter(isRecord).map(r => {
    // `disk` is a request for more allowance (`skein_request_disk`), the owner's to answer.
    const queue = r.queue === 'write' || r.queue === 'question' || r.queue === 'disk' ? r.queue : 'package'
    const row: SkeinRequestRow = {
      queue,
      id: String(r.id ?? ''),
      asked: String(r.asked ?? ''),
      state: String(r.state ?? 'waiting'),
      what: whatWasAsked(queue, r),
    }

    return row
  })
}

/** The box's own jobs, from `$SKEIN_STATE/signals/jobs.json`; null where there is no view to believe. */
async function readJobs($: Engine): Promise<StatusJobs | null> {
  const view = await readJobsView($)
  if (view === null) return null
  const jobs = view.jobs
  const places = jobs
    .filter(j => j.state === 'waiting')
    .map(j => j.position)
    .filter((p): p is number => p !== null)

  return {
    running: jobs.filter(j => j.state === 'granted').length,
    waiting: jobs.filter(j => j.state === 'waiting').length,
    first: places.length === 0 ? null : Math.min(...places),
  }
}

/** The lanes skein made in this box (`skein-mcp lane list`, read as the agents panel reads it), and
 * how many of the session's subagents are working in a lane now. */
async function readLanes($: Engine): Promise<StatusLanes> {
  // No lanes said: the row counts only what the session saw.
  const made = (await listLanes($))?.length ?? 0
  const running = await lanesAtWork($)

  return { made, running }
}

/** The lanes skein made in this box, from `skein-mcp lane list` as the agents panel reads it; null
 * when it did not answer. */
async function listLanes($: Engine): Promise<SkeinLaneRecord[] | null> {
  try {
    const ran = await $.process.run(['python3', `${$.plugin.root}/bin/skein-mcp`, 'lane', 'list'], { timeoutMs: 10_000 })

    return ran.exitCode === 0 ? lanesListed(ran.stdout) : null
  } catch {
    return null
  }
}

async function readMail($: Engine): Promise<number | null> {
  try {
    const ran = await $.process.run(['bash', `${$.plugin.root}/probe/mailbox.sh`, 'unread'])
    const n = Number(ran.stdout.trim())

    return ran.exitCode === 0 && ran.stdout.trim() !== '' && Number.isInteger(n) ? n : null
  } catch {
    return null
  }
}

type Answer = { result: Record<string, unknown> | null; said: string }

/** `skein-mcp clean [PATH...]`: the list with no paths, a removal with them. */
function runClean($: Engine, paths: readonly string[]): Promise<Answer> {
  return skeinMcp($, ['clean', ...paths], paths.length === 0 ? 30_000 : CLEAN_TIMEOUT_MS)
}

/** One run of the plugin's own `skein-mcp` on the command line: `{ result, said }`. */
async function skeinMcp($: Engine, args: readonly string[], timeoutMs: number): Promise<Answer> {
  try {
    const ran = await $.process.run(['python3', `${$.plugin.root}/bin/skein-mcp`, ...args], { timeoutMs })
    const parsed: unknown = JSON.parse(ran.stdout)
    if (!isRecord(parsed) || !isRecord(parsed.result)) return { result: null, said: '' }

    return { result: parsed.result, said: typeof parsed.said === 'string' ? parsed.said : '' }
  } catch {
    return { result: null, said: '' }
  }
}

/** A tool's answer as the MCP tool gives it: the result, then the sentence for the agent. */
// What skein_clean answers, as a registered tool may: its blocks as the result, or, for an error,
// a deny carrying the same text, which the model reads as the tool's error. Not MCP's own
// `{ content, isError }`: core holds a registered tool's result to a string or a list of blocks,
// and refuses anything else in a live session, which `claude plugin test` does not (SKEIN-1409).
function mcpAnswer(result: Record<string, unknown>, said: string) {
  const content = [{ type: 'text' as const, text: JSON.stringify(result, null, 1) }]
  if (said !== '') content.push({ type: 'text' as const, text: said })
  if ('error' in result) return { deny: content.map(b => b.text).join('\n') }

  return { result: content }
}

export async function cleanFromPane($: Engine, paths: string[]): Promise<void> {
  const ran = await runClean($, paths)
  const error = ran.result !== null && typeof ran.result.error === 'string' ? ran.result.error : null
  const said = ran.said !== '' ? ran.said : `skein could not clean: ${error ?? 'skein-mcp did not answer'}.`
  await update($, cleanSaid, () => said)
  await look($)
}

// ------------------------------------------------------------------------------------------------
// Words
// ------------------------------------------------------------------------------------------------

// A line with no `button` has nothing for the person to press. Mail and a disk ask skein does not
// hold for are not among these: they are the agent's, so each is a dim line of its own, not amber
// and never toasted (`mailNote`, `quietAskLine`). A held disk ask is (SKEIN-1475).
type Ask = { key: string; text: string; button?: string }

/** What the band says, loudest first, in `columns` cells; empty when there is nothing to say. */
function asked(found: SkeinBand | null, columns: number): Ask[] {
  if (found === null) return []
  const out: Ask[] = []
  const ask = found.disk?.ask
  if (ask?.held) out.push({ key: 'disk', text: askLine(ask), button: 'Free disk' })
  // The oldest waiting question in its own words, since it has waited longest for the person; the
  // rest are counted. The pane has every question whole.
  const questions = (found.requests ?? [])
    .filter(r => r.state === 'waiting' && r.queue === 'question')
    .sort((a, b) => (a.asked < b.asked ? -1 : a.asked > b.asked ? 1 : 0))
  const oldest = questions[0]
  if (oldest !== undefined) {
    const more = questions.length - 1
    const after = more === 0 ? '' : ` And ${more} more ${more === 1 ? 'question' : 'questions'}.`
    const before = 'Waiting for your answer: '
    // The quotes, the button `[ Read ]` and the space before it take 11 cells.
    const room = columns - before.length - after.length - 11
    out.push({
      key: 'question',
      text: `${before}“${oneLine(oldest.what, room)}”${after}`,
      button: 'Read',
    })
  }
  const waiting = (found.requests ?? []).filter(r => r.state === 'waiting' && r.queue !== 'question').length
  if (waiting > 0) {
    out.push({
      key: 'requests',
      text: `${waiting} ${waiting === 1 ? 'request is' : 'requests are'} waiting for an answer.`,
      button: 'Requests',
    })
  }
  const pending = (found.updates ?? []).filter(u => !u.restartRequested)
  if (pending.length > 0) out.push({ key: 'update', text: updateLine(pending) })

  return out
}

/** Unread mail, for the person to know of: the agent reads it at its next turn. */
export function mailNote(mail: number): string | null {
  return mail > 0 ? `✉ ${mail} unread ${mail === 1 ? 'message' : 'messages'}, read at the agent's next turn.` : null
}

/** The versions a restart would give the box, as the action that takes them. */
export function updateLine(pending: readonly SkeinUpdate[]): string {
  const names = pending.map(u => (u.new === '' ? `${u.component} (the sandbox's own)` : u.new)).join(', ')

  return `A restart gives this box ${names}. Ask the agent to restart when idle, or skein does once the box is idle.`
}

/** One thing that waits on the person: a key that is new when the thing is (one toast each), the
 * toast's words, a few words for a short line, and the pane's line, worded as what to do. */
export type Waiting = { key: string; toast: string; short: string; action: string }

/** Each thing that waits on the person, loudest first. The disk skein asks the box to free is one
 * only when skein holds a command for it, more than a tenth over (SKEIN-1475); otherwise the agent
 * clears it (`quietAskLine`). A request for more allowance is, as a request. */
export function waitingThings(found: SkeinBand): Waiting[] {
  const out: Waiting[] = []
  const ask = found.disk?.ask
  if (ask?.held) {
    out.push({
      key: `disk:${ask.id}`,
      toast: askLine(ask),
      short: ask.amount_mb > 0 ? `free ${humanMb(ask.amount_mb)}` : 'free disk',
      action: askLine(ask),
    })
  }
  for (const r of found.requests ?? []) {
    if (r.state !== 'waiting') continue
    out.push(
      r.queue === 'question'
        ? {
            key: `question:${r.id}`,
            toast: `Waiting for your answer: “${oneLine(r.what, 60)}”`,
            short: 'a question',
            action: `Answer in the cockpit: “${r.what.trim()}”`,
          }
        : {
            key: `request:${r.queue}:${r.id}`,
            toast: `A ${r.queue} request waits for an answer: ${r.what}`,
            short: `a ${r.queue} request`,
            action: `Answer in the cockpit: a ${r.queue} request, ${r.what}.`,
          },
    )
  }
  for (const u of found.updates ?? []) {
    if (!u.restartRequested) {
      out.push({ key: `update:${u.component}:${u.new}`, toast: updateLine([u]), short: 'restart', action: updateLine([u]) })
    }
  }

  return out
}

// What skein asks, and how much of the box is in use now: skein asks only for what can be cleared
// now, so a box whose space is all in use, by a build or any other process in it, is asked for none
// of it (SKEIN-1391, SKEIN-1462).
export function askLine(ask: SkeinDiskAsk): string {
  const why =
    ask.why === 'allowance'
      ? 'it is over its own allowance'
      : ask.why === 'fleet-every-box'
        ? "the fleet's disk is over its line, and every box is asked"
        : "the fleet's disk is over its line"

  if (ask.amount_mb <= 0) {
    const live = ask.live_mb > 0 ? ` (${humanMb(ask.live_mb)} in use now)` : ''
    return `skein asks this box to free disk, but none of it can be cleared now: ${why}${live}.`
  }
  const live = ask.live_mb > 0 ? ` (${humanMb(ask.live_mb)} more in use now)` : ''

  return `skein asks this box to free ${humanMb(ask.amount_mb)} of disk: ${why}${live}.`
}

/** The disk ask as the band and the pane's BOX say it, dim: the agent's to clear, and the person's
 * to override. `/skein clean` has the whole of it, in `askLine`'s words. */
export function quietAskLine(ask: SkeinDiskAsk): string {
  return ask.amount_mb > 0
    ? `skein asks this box to free ${humanMb(ask.amount_mb)} · the agent clears it · /skein clean to do it yourself`
    : 'skein asks this box to free disk, but none of it can be cleared now · /skein clean lists it'
}

export function diskLine(found: SkeinBand | null): string {
  const disk = found?.disk
  if (!disk || disk.mb === null) return 'skein has not measured this box lately.'
  const held = `This box holds ${humanMb(disk.mb)}`
  const free = found?.clean ? ` ${human(found.clean.removable_now_bytes)} can be removed now.` : ''

  return disk.limit_mb === null
    ? `${held}.${free}`
    : `${held} of its ${humanMb(disk.limit_mb)} allowance.${free}`
}

function whatWasAsked(queue: SkeinRequestRow['queue'], r: Record<string, unknown>): string {
  if (queue === 'package') {
    const packages = Array.isArray(r.packages) ? r.packages.map(String).join(' ') : ''

    return [typeof r.kind === 'string' ? r.kind : '', packages].filter(s => s !== '').join(' ')
  }
  if (queue === 'write') return String(r.repo ?? '')
  if (queue === 'disk') {
    const why = typeof r.why === 'string' && r.why.trim() !== '' ? ` (${r.why.trim()})` : ''

    return `${String(r.size ?? '').trim()}${why}`
  }
  // Whole: the pane and `/skein requests` show it as asked, and the band cuts its own copy.
  return String(r.question ?? '')
}

/**
 * A question cut to one line of at most `room` cells (never fewer than 20): its first line, its
 * runs of space made one, cut at the last space past half the room, or mid-word when a word fills
 * more than half, and `…` where anything was left out.
 */
function oneLine(text: string, room: number): string {
  const width = Math.max(20, room)
  const lines = text.trim().split('\n')
  const first = (lines[0] ?? '').replace(/\s+/g, ' ').trim()
  const isCut = lines.length > 1 || first.length > width
  if (!isCut) return first
  // One cell is the `…`'s. A space just past the kept cells counts: the word before it is whole.
  const head = first.slice(0, width - 1)
  const space = first.slice(0, width).lastIndexOf(' ')
  const kept = first.length <= width - 1 ? first : space > width / 2 ? first.slice(0, space) : head

  return `${kept.trimEnd()}…`
}

function requestLine(r: SkeinRequestRow): string {
  return `${r.queue} ${r.what}: ${r.state}`
}

function requestLines(found: SkeinBand | null): string {
  if (found === null || found.requests === null) return 'this session is not in a skein box, so it has no requests.'
  if (found.requests.length === 0) return 'this box has asked for nothing.'

  return ['this box asked for:', ...found.requests.map(r => `  ${requestLine(r)}`)].join('\n')
}

/** The box's own heavy jobs, as `/skein status` counts them. */
type StatusJobs = { running: number; waiting: number; first: number | null }

/** The box's lanes, as `/skein status` counts them. */
type StatusLanes = { made: number; running: number }

/** What `/skein status` reads beside the band. */
type StatusMore = { jobs: StatusJobs | null; lanes: StatusLanes; box: string | null; at: string }

// A row of `/skein status`: `⚑ ` before what needs the person, two spaces before the rest, the
// name padded to one column, then the value, then three spaces and a detail. `statusDrawn` reads
// the same shape back to colour it.
const MIB = 1024 * 1024
const LOUD = '⚑ '
const QUIET = '  '
const NAME = 10
const DETAIL = '   '

type Row = { name: string; value: string; detail?: string; isLoud: boolean }

const rowText = (r: Row) =>
  `${r.isLoud ? LOUD : QUIET}${r.name.padEnd(NAME)}${r.value}${r.detail ? `${DETAIL}${r.detail}` : ''}`

/**
 * `/skein status`: one row per signal, the ones that need the person first. A row whose figures
 * skein does not write (no jobs view, no lanes) is left out rather than guessed.
 */
function status(found: SkeinBand | null, more: StatusMore): string {
  const rows: Row[] = []
  const disk = found?.disk ?? null
  const ask = disk?.ask ?? null
  const removable = found?.clean ? `${size(found.clean.removable_now_bytes)} removable` : undefined
  rows.push(
    disk === null || disk.mb === null
      ? { name: 'disk', value: 'not measured lately', isLoud: false }
      : {
          name: 'disk',
          value: disk.limit_mb === null ? size(disk.mb * MIB) : `${size(disk.mb * MIB)} / ${size(disk.limit_mb * MIB)}`,
          detail: ask ? askLine(ask) : removable,
          isLoud: ask !== null,
        },
  )
  const requests = found?.requests ?? null
  const waiting = (requests ?? []).filter(r => r.state === 'waiting').length
  rows.push(
    found === null || requests === null
      ? { name: 'requests', value: 'none: not in a skein box', isLoud: false }
      : waiting === 0
        ? { name: 'requests', value: 'none waiting', isLoud: false }
        : { name: 'requests', value: `${waiting} waiting for the owner`, detail: '/skein requests lists them', isLoud: true },
  )
  const mail = found?.mail ?? null
  rows.push({
    name: 'mail',
    value: mail === null ? 'could not be read' : mail === 0 ? 'none unread' : `${mail} unread`,
    detail: mail !== null && mail > 0 ? "the agent reads them at its next turn" : undefined,
    isLoud: false,
  })
  const jobs = more.jobs
  if (jobs !== null) {
    const parts = [
      jobs.running > 0 ? `${jobs.running} running` : jobs.waiting === 0 ? 'none running' : null,
      jobs.waiting > 0 ? `${jobs.waiting} waiting${jobs.first === null ? '' : `, ${ordinal(jobs.first)} in the fleet's queue`}` : null,
    ].filter((p): p is string => p !== null)
    rows.push({ name: 'jobs', value: parts.join(' · '), isLoud: false })
  }
  const lanes = more.lanes
  if (lanes.made > 0 || lanes.running > 0) {
    rows.push({ name: 'lanes', value: `${lanes.running} running · ${Math.max(lanes.made, lanes.running)} in all`, isLoud: false })
  }
  const spend = found?.spend ?? null
  if (spend === null) {
    rows.push({ name: 'spend', value: 'not written in the last 70 minutes', isLoud: false })
  } else {
    const more = spend.unpriced ? '+' : ''
    rows.push({
      name: 'spend',
      value: `≈${dollars(spend.today)}${more} today · ≈${dollars(spend.week)}${more} this week`,
      detail: spend.unpriced
        ? `estimated at API prices as of ${spend.at} UTC; + leaves out a model skein has no price for`
        : `estimated at API prices as of ${spend.at} UTC`,
      isLoud: false,
    })
  }

  // The engine heads the answer with `skein:`, so the first line names the box and the time after it.
  const head = `${more.box ?? 'not in a skein box'} · ${more.at} UTC`
  const sorted = [...rows.filter(r => r.isLoud), ...rows.filter(r => !r.isLoud)]

  return [head, ...sorted.map(rowText), '/skein clean · /skein requests'].join('\n')
}

type Drawn =
  | { kind: 'row'; head: string; value: string; detail: string; isLoud: boolean }
  | { kind: 'plain' | 'dim'; text: string }

/** The rows of a `/skein status` text, to colour; null when the text is not that shape. */
function statusDrawn(text: string): Drawn[] | null {
  const lines = text.split('\n')
  if (lines.length < 3 || !lines.slice(1, -1).every(l => l.startsWith(LOUD) || l.startsWith(QUIET))) return null
  const rows: Drawn[] = [{ kind: 'plain', text: lines[0] ?? '' }]
  for (const l of lines.slice(1, -1)) {
    const isLoud = l.startsWith(LOUD)
    const head = l.slice(0, LOUD.length + NAME)
    const rest = l.slice(head.length)
    const at = rest.indexOf(DETAIL)
    rows.push({
      kind: 'row',
      head,
      value: at < 0 ? rest : rest.slice(0, at),
      detail: at < 0 ? '' : rest.slice(at),
      isLoud,
    })
  }
  rows.push({ kind: 'dim', text: lines.at(-1) ?? '' })

  return rows
}

// ------------------------------------------------------------------------------------------------
// Numbers
// ------------------------------------------------------------------------------------------------

/** `$4.20`, or whole dollars from $100: `$1,234`. */
function dollars(n: number): string {
  return n < 100 ? `$${n.toFixed(2)}` : `$${Math.round(n).toLocaleString('en-US')}`
}

/** Bytes as `skein-mcp` prints them: `3.2G`, or `120M` under a gigabyte. */
export function human(n: number): string {
  const mib = n / (1024 * 1024)
  if (mib >= 1024) return `${(mib / 1024).toFixed(1)}G`

  return n ? `${Math.max(1, Math.round(mib))}M` : '0M'
}

export function humanMb(mb: number): string {
  return human(mb * 1024 * 1024)
}
