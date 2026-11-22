// The one line skein draws above the prompt: how many things wait on the person, in a few words,
// and where to go for them. Nothing is drawn when nothing waits. Everything else about a box is the
// web cockpit's (owner's decision 9, SKEIN-1512).
//
// What waits on the person, each read from what skein already writes or answers:
//
// * the questions and requests waiting for an answer, from `skein-mcp requests`, the
//   `skein_requests` tool's own function;
// * a disk ask skein holds a command for, from `$SKEIN_STATE/signals/resources.json`. An ask it
//   does not hold for is the agent's to clear, so it is not counted (SKEIN-1475);
// * versions that wait for a restart, from the same file.
//
// `/skein clean` stays here because the cockpit removes build output and scratchpads only from a
// stopped box: this is the person's way to free disk in a running one. It runs `skein-mcp clean`,
// the one implementation the `skein_clean` MCP tool also runs.

import { atom, read, update } from 'claude-code'
import type { CommandRunResult, EngineInterface as Engine, On, SessionStartInput, Timer, TurnCompleteInput } from 'claude-code'

import type { SkeinBand } from '../types'

const band = atom({ plugin: 'skein', key: 'band' } as const, null, { shape: 'band-5' })

// skein writes the signal file every 60 s, and the plugin's command hooks believe nothing in one
// older than five minutes (`STALE_AFTER` in `bin/skein-resources`): neither does the band.
const STALE_AFTER_MS = 300_000

// How often the band reads again: a request answered shows within a minute.
const LOOK_EVERY_MS = 60_000

// A removal of a large build directory takes a while; `$.process.run`'s ceiling is ten minutes.
const CLEAN_TIMEOUT_MS = 600_000

const COCKPIT = 'the cockpit shows this box. /skein clean lists what it can remove.'

// A resumed session raises `session.start` again, so a start cancels the timer the last one left.
let ticker: Timer | null = null

/** As the session starts (`register.tsx`'s one `session.start` hook calls it): in a session a
 * person watches, the looks that keep the line current. A `claude -p` draws no band. */
export async function onStart($: Engine, e: SessionStartInput): Promise<void> {
  ticker?.cancel()
  ticker = null
  if (!e.isInteractive) return
  $.clock.after(0, () => void look($).catch(() => {}))
  ticker = $.clock.every(LOOK_EVERY_MS, () => void look($).catch(() => {}))
}

/** After each turn of the main loop: the agent may have asked for something. */
export async function onTurnComplete($: Engine, e: TurnCompleteInput): Promise<void> {
  if (e.agentId) return
  $.clock.after(0, () => void look($).catch(() => {}))
}

/** `/skein <args>`, handed over by `identity.ts`, which owns the command's hook. */
export async function subcommand($: Engine, args: string): Promise<CommandRunResult> {
  const trimmed = args.trim()
  const sub = trimmed.split(/\s+/)[0] ?? ''
  if (sub === 'clean') {
    // The rest is one path, whole: a path on skein's list may hold a space.
    const path = trimmed.slice(sub.length).trim()

    return { text: path === '' ? await cleanList($) : await cleanPath($, path) }
  }
  if (sub === 'status' || sub === 'requests') return { text: COCKPIT }

  return { text: `there is no /skein ${sub}. ${COCKPIT}` }
}

export function register(on: On): void {
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const found = await read($, band)
    if (e.props.hasSurvey || found === null || found.count === 0 || !(await $.env.get('SKEIN_STATE'))) return next(e)
    const { Text } = $.ui.resolve(e)

    return (
      <Text color="warning" wrap="truncate-end">
        {waitingLine(found)}
      </Text>
    )
  })
}

/** The line: the way to act comes before the detail, so a narrow terminal cuts only the detail. */
export function waitingLine(found: SkeinBand): string {
  return `⚑ ${found.count} waiting on you: open the cockpit (${found.what.join(', ')})`
}

async function look($: Engine): Promise<void> {
  const [signal, requests] = await Promise.all([readSignal($), readRequests($)])
  const found = waiting(signal, requests)
  await update($, band, () => found)
}

/** What waits on the person, counted, with its few words: the loudest first. */
export function waiting(signal: Record<string, unknown> | null, requests: readonly Record<string, unknown>[]): SkeinBand {
  const what: string[] = []
  let count = 0
  const ask = (Array.isArray(signal?.asks) ? signal.asks : []).find(
    a => isRecord(a) && a.kind === 'disk' && typeof a.crossing_id === 'string' && a.crossing_id !== '',
  )
  if (isRecord(ask) && typeof ask.hold === 'string' && ask.hold !== '') {
    count += 1
    what.push(typeof ask.amount === 'number' && ask.amount > 0 ? `free ${human(ask.amount * MIB)}` : 'free disk')
  }
  const open = requests.filter(r => (r.state ?? 'waiting') === 'waiting')
  const questions = open.filter(r => r.queue === 'question').length
  if (questions > 0) {
    count += questions
    what.push(questions === 1 ? 'a question' : `${questions} questions`)
  }
  for (const queue of ['package', 'write', 'disk']) {
    const n = open.filter(r => r.queue === queue).length
    if (n > 0) {
      count += n
      what.push(n === 1 ? `a ${queue} request` : `${n} ${queue} requests`)
    }
  }
  // Any number of versions is one restart.
  const updates = Array.isArray(signal?.updates) ? signal.updates : []
  if (updates.some(u => isRecord(u) && typeof u.component === 'string' && typeof u.new === 'string' && u.restart_requested !== true)) {
    count += 1
    what.push('a restart')
  }

  return { count, what }
}

/** The signal file, while it is fresh: null when it is missing, unreadable or over five minutes old. */
async function readSignal($: Engine): Promise<Record<string, unknown> | null> {
  const state = await $.env.get('SKEIN_STATE')
  if (!state) return null
  try {
    const parsed: unknown = JSON.parse(await $.fs.read(`${state}/signals/resources.json`))
    if (!isRecord(parsed) || typeof parsed.at !== 'string') return null
    const at = Date.parse(parsed.at)

    return Number.isNaN(at) || (await $.clock.now()) - at > STALE_AFTER_MS ? null : parsed
  } catch {
    return null
  }
}

async function readRequests($: Engine): Promise<Record<string, unknown>[]> {
  const ran = await skeinMcp($, ['requests'], 30_000)
  if (ran.result === null || 'error' in ran.result || !Array.isArray(ran.result.requests)) return []

  return ran.result.requests.filter(isRecord)
}

/** `/skein clean`: what can go now, largest first, and how to remove one. */
async function cleanList($: Engine): Promise<string> {
  const ran = await skeinMcp($, ['clean'], 30_000)
  if (ran.result === null) return 'skein could not list this box: skein-mcp did not answer.'
  if (!Array.isArray(ran.result.items)) return ran.said || `skein could not list this box: ${String(ran.result.error ?? 'no list')}.`
  const rows = ran.result.items.filter(isRecord)
  const now = rows.filter(r => r.safe_now === true).sort((a, b) => bytes(b) - bytes(a))
  const later = rows.length - now.length
  const notNow = later === 0 ? [] : [`${later} more cannot be removed now.`]
  if (now.length === 0) return ['nothing on this box can be removed now.', ...notNow].join('\n')

  return [
    `this box can remove ${human(now.reduce((n, r) => n + bytes(r), 0))} now:`,
    ...now.map(r => `  ${human(bytes(r)).padStart(6)}  ${String(r.in_box ?? r.path ?? '')}  (${String(r.kind ?? '')})`),
    ...notNow,
    '/skein clean PATH removes one.',
  ].join('\n')
}

/** `/skein clean PATH`: skein's own sentence about the removal, whichever way it went. */
async function cleanPath($: Engine, path: string): Promise<string> {
  const ran = await skeinMcp($, ['clean', path], CLEAN_TIMEOUT_MS)
  void look($).catch(() => {})
  if (ran.said !== '') return ran.said
  const error = ran.result !== null && typeof ran.result.error === 'string' ? ran.result.error : 'skein-mcp did not answer'

  return `skein could not clean: ${error}.`
}

type Answer = { result: Record<string, unknown> | null; said: string }

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

const MIB = 1024 * 1024

const bytes = (r: Record<string, unknown>) => (typeof r.bytes === 'number' && Number.isFinite(r.bytes) ? r.bytes : 0)

/** Bytes as `skein-mcp` prints them: `3.2G`, or `120M` under a gigabyte. */
function human(n: number): string {
  const mib = n / MIB
  if (mib >= 1024) return `${(mib / 1024).toFixed(1)}G`

  return n ? `${Math.max(1, Math.round(mib))}M` : '0M'
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}
