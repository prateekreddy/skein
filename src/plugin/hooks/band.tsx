// What skein needs from this box, or offers it, drawn above the prompt: the disk skein asks the box
// to free, the requests and questions the box is waiting on, and its unread mail. A pane holds the
// details, with a Clean button on each item of skein's disk list (SKEIN-1362).
//
// Nothing here is new authority. Every figure is read from what skein already writes or answers,
// through the entry points the agent itself has:
//
// * the disk figures and skein's ask from the signal file, `$SKEIN_STATE/signals/resources.json`;
// * the disk list, and every removal, from `skein-mcp clean [PATH...]`, the one implementation the
//   `skein_clean` MCP tool also runs;
// * the requests from `skein-mcp requests`, the `skein_requests` tool's own function;
// * the unread count from `mailbox.sh unread`, which marks nothing: mail is delivered by the
//   turn-boundary hooks alone.
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

import type { SkeinBand, SkeinCleanRow, SkeinRequestRow, SkeinSharedCache } from '../types'

const band = atom({ plugin: 'skein', key: 'band' } as const, null, { shape: 'band-1' })
const cleanSaid = atom({ plugin: 'skein', key: 'cleanSaid' } as const, null, {
  shape: 'clean-said-1',
})

const PANE = 'skein'

// skein writes the signal file every 60 s, and the plugin's command hooks believe nothing in one
// older than five minutes (`STALE_AFTER` in `bin/skein-resources`): neither does the band.
const STALE_AFTER_MS = 300_000

// How often the band reads again. skein lists the box's disk every five minutes and writes the
// signal file every minute; a request answered shows within a minute.
const LOOK_EVERY_MS = 60_000

// The disk list's longest rows in the pane; `skein_clean` lists every one.
const TOP_ROWS = 8

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

/** Setup, called from the one `session.start` hook `register.tsx` owns (SKEIN-1387): the `clean`
 * tool, and, in a session a person watches, the looks that keep the band current. */
export async function onStart($: Engine, e: SessionStartInput): Promise<void> {
  ticker?.cancel()
  ticker = null
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
  $.clock.after(0, () => void look($))
  ticker = $.clock.every(LOOK_EVERY_MS, () => void look($))
}

/** After each turn of the main loop (`register.tsx`'s one `turn.complete` hook calls it): the
 * agent may have asked for something or cleaned, so the band looks again, after the turn. */
export async function onTurnComplete($: Engine, e: TurnCompleteInput): Promise<void> {
  if (e.agentId) return
  $.clock.after(0, () => void look($))
}

/** `/skein <args>`, handed over by `identity.ts`, which owns the command's hook. */
export async function subcommand($: Engine, args: string): Promise<CommandRunResult> {
  const sub = args.trim().split(/\s+/)[0]
  if (sub === 'status') {
    await look($)

    return { text: status(await read($, band)) }
  }
  if (sub === 'requests') {
    await look($)

    return { text: requestLines(await read($, band)) }
  }
  if (sub === 'clean') {
    await look($)
    const opened = await $.ui.open({ id: PANE, title: 'skein', focus: true })

    return {
      text: opened.isPlaced
        ? 'the list of what this box can remove is open.'
        : 'the list could not be opened here. skein_clean lists the same items.',
    }
  }

  return { text: `there is no /skein ${sub}. Try /skein status, /skein clean or /skein requests.` }
}

export function register(on: On): void {
  on('tool.call', { tool: 'mcp__skein__clean' }, async ($, e) => {
    const paths = 'paths' in e ? e.paths : undefined
    // The MCP tool's own refusal, word for word: a list of anything but paths never reaches the
    // command line, which can only carry strings.
    if (paths !== undefined && !(Array.isArray(paths) && paths.every(p => typeof p === 'string' && p !== ''))) {
      return { result: mcpAnswer({ error: 'paths-must-be-a-list-of-paths' }, '') }
    }
    const ran = await runClean($, paths ?? [])
    if (ran.result !== null && 'removed' in ran.result) void look($)

    return { result: mcpAnswer(ran.result ?? { error: 'clean-did-not-answer' }, ran.said) }
  }).catch(() => ({ result: mcpAnswer({ error: 'clean-did-not-answer' }, '') }))

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const found = await read($, band)
    const said = await read($, cleanSaid)
    const asks = asked(found, e.props.bodyColumns)
    if (e.props.hasSurvey || (asks.length === 0 && said === null)) return next(e)

    const { Box, Button, Text } = $.ui.resolve(e)
    const open = () => void $.ui.open({ id: PANE, title: 'skein', focus: true })

    return (
      <Box flexDirection="column">
        {asks.map(ask => (
          <Box>
            <Text bold={ask.isLoud} color={ask.isLoud ? 'warning' : undefined}>
              {ask.text}{' '}
            </Text>
            {ask.button !== undefined && (
              <Button key={`band:${ask.key}`} label={ask.button} onPress={open} />
            )}
          </Box>
        ))}
        {said !== null && (
          <Box>
            <Text dimColor>{said} </Text>
            <Button key="band:said" label="OK" role="dismiss" onPress={() => update($, cleanSaid, () => null)} />
          </Box>
        )}
      </Box>
    )
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Button, Text } = $.ui.resolve(e)
    const found = await read($, band)
    const said = await read($, cleanSaid)
    const clean = found?.clean ?? null
    const own = (clean?.rows ?? []).filter(r => r.where === 'box')
    const shown = [...own].sort((a, b) => b.bytes - a.bytes).slice(0, TOP_ROWS)
    const shared = clean?.shared ?? null
    const sharedRows = (clean?.rows ?? []).filter(r => r.where === 'shared' && r.safe_now)
    const remove = (paths: string[]) => () => void cleanFromPane($, paths)
    const itemRow = (row: SkeinCleanRow) => {
      const what = `${row.in_box}, ${row.kind}, ${human(row.bytes)}`
      if (!row.safe_now) return <Text dimColor>{what}: {row.why_not ?? 'not removable now'}</Text>

      return (
        <Box>
          <Button key={`clean:${row.path}`} label="Clean" onPress={remove([row.path])} />
          <Text> {what}</Text>
        </Box>
      )
    }
    const sharedRow = (cache: SkeinSharedCache) => {
      const what = `The repo's shared cache, ${cache.path}, ${human(cache.bytes)}. Not counted against this box.`
      if (!cache.safe_to_clean_now || sharedRows.length === 0) {
        return <Text dimColor>{what} {cache.why_not ?? 'Nothing in it can be removed now.'}</Text>
      }

      return (
        <Box>
          <Button key="clean:shared" label="Clean" onPress={remove(sharedRows.map(r => r.path))} />
          <Text> {what}</Text>
        </Box>
      )
    }

    return (
      <Box flexDirection="column">
        <Text bold>Disk</Text>
        <Text>{diskLine(found)}</Text>
        {found?.disk?.ask && <Text bold color="warning">{askLine(found.disk.ask)}</Text>}
        {clean === null && <Text dimColor>skein has not listed what this box can remove yet.</Text>}
        {clean !== null && own.length === 0 && <Text dimColor>Nothing in this box is on skein's list.</Text>}
        {shown.map(itemRow)}
        {own.length > shown.length && (
          <Text dimColor>And {own.length - shown.length} more. skein_clean lists every one.</Text>
        )}
        {shared !== null && sharedRow(shared)}
        {said !== null && <Text>{said}</Text>}
        <Text bold>Requests</Text>
        {(found?.requests ?? []).length === 0 && <Text dimColor>This box has asked for nothing.</Text>}
        {(found?.requests ?? []).map(r => (
          <Text color={r.state === 'waiting' ? 'warning' : undefined}>
            {requestLine(r)}
          </Text>
        ))}
        <Text bold>Mail</Text>
        <Text>{mailLine(found?.mail ?? null)}</Text>
      </Box>
    )
  })
}

// ------------------------------------------------------------------------------------------------
// Reading
// ------------------------------------------------------------------------------------------------

/** Read everything the band draws, and keep it for the session. */
async function look($: Engine): Promise<void> {
  const [disk, clean, requests, mail] = await Promise.all([
    readDisk($),
    readClean($),
    readRequests($),
    readMail($),
  ])
  const found: SkeinBand = { disk, clean, requests, mail }
  await update($, band, () => found)
}

async function readDisk($: Engine): Promise<SkeinBand['disk']> {
  const state = await $.env.get('SKEIN_STATE')
  if (!state) return null
  const parsed = await parsedFile($, `${state}/signals/resources.json`)
  if (!isRecord(parsed) || typeof parsed.at !== 'string') return null
  const at = Date.parse(parsed.at)
  if (Number.isNaN(at) || (await $.clock.now()) - at > STALE_AFTER_MS) return null
  const disk = isRecord(parsed.disk) ? parsed.disk : {}
  const ask = (Array.isArray(parsed.asks) ? parsed.asks : []).find(
    a => isRecord(a) && a.kind === 'disk' && typeof a.crossing_id === 'string' && a.crossing_id !== '',
  )

  return {
    mb: numberOr(disk.mb, null),
    limit_mb: numberOr(disk.limit_mb, null),
    ask: isRecord(ask)
      ? { amount_mb: numberOr(ask.amount, 0) ?? 0, why: typeof ask.why === 'string' ? ask.why : '' }
      : null,
  }
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
    const queue = r.queue === 'write' || r.queue === 'question' ? r.queue : 'package'
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

async function parsedFile($: Engine, path: string): Promise<unknown> {
  try {
    return JSON.parse(await $.fs.read(path))
  } catch {
    return null
  }
}

/** A tool's answer as the MCP tool gives it: the result, then the sentence for the agent. */
function mcpAnswer(result: Record<string, unknown>, said: string) {
  const content = [{ type: 'text' as const, text: JSON.stringify(result, null, 1) }]
  if (said !== '') content.push({ type: 'text' as const, text: said })

  return { content, isError: 'error' in result }
}

async function cleanFromPane($: Engine, paths: string[]): Promise<void> {
  const ran = await runClean($, paths)
  const error = ran.result !== null && typeof ran.result.error === 'string' ? ran.result.error : null
  const said = ran.said !== '' ? ran.said : `skein could not clean: ${error ?? 'skein-mcp did not answer'}.`
  await update($, cleanSaid, () => said)
  await look($)
}

// ------------------------------------------------------------------------------------------------
// Words
// ------------------------------------------------------------------------------------------------

// A line with no `button` has nothing for the person to press: mail is the agent's to read.
type Ask = { key: string; text: string; button?: string; isLoud: boolean }

/** What the band says, loudest first, in `columns` cells; empty when there is nothing to say. */
function asked(found: SkeinBand | null, columns: number): Ask[] {
  if (found === null) return []
  const out: Ask[] = []
  const ask = found.disk?.ask
  if (ask) out.push({ key: 'disk', text: askLine(ask), button: 'Free disk', isLoud: true })
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
      isLoud: false,
    })
  }
  const waiting = (found.requests ?? []).filter(r => r.state === 'waiting' && r.queue !== 'question').length
  if (waiting > 0) {
    out.push({
      key: 'requests',
      text: `${waiting} ${waiting === 1 ? 'request is' : 'requests are'} waiting for an answer.`,
      button: 'Requests',
      isLoud: false,
    })
  }
  const mail = found.mail ?? 0
  if (mail > 0) {
    out.push({
      key: 'mail',
      text: `${mail} unread ${mail === 1 ? 'message' : 'messages'}, read at the agent's next turn.`,
      isLoud: false,
    })
  }

  return out
}

function askLine(ask: { amount_mb: number; why: string }): string {
  const why =
    ask.why === 'allowance'
      ? 'it is over its own allowance'
      : ask.why === 'fleet-every-box'
        ? "the fleet's disk is over its line, and every box is asked"
        : "the fleet's disk is over its line"

  return `skein asks this box to free ${humanMb(ask.amount_mb)} of disk: ${why}.`
}

function diskLine(found: SkeinBand | null): string {
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

function mailLine(mail: number | null): string {
  if (mail === null) return "This box's mail could not be read."
  if (mail === 0) return 'No unread mail.'

  return `${mail} unread. The agent reads them at the start of its next turn.`
}

function status(found: SkeinBand | null): string {
  const lines = [diskLine(found)]
  if (found?.disk?.ask) lines.push(askLine(found.disk.ask))
  const waiting = (found?.requests ?? []).filter(r => r.state === 'waiting').length
  lines.push(
    found?.requests === null || found === null
      ? 'Not in a skein box, so no requests.'
      : `${waiting} ${waiting === 1 ? 'request' : 'requests'} waiting. /skein requests lists them.`,
  )
  lines.push(mailLine(found?.mail ?? null))
  lines.push("Spend is not shown: skein does not write it into the box yet.")

  // The engine heads the answer with `skein:`, so the first line reads as a sentence after it.
  return lines.join('\n')
}

// ------------------------------------------------------------------------------------------------
// Numbers
// ------------------------------------------------------------------------------------------------

/** Bytes as `skein-mcp` prints them: `3.2G`, or `120M` under a gigabyte. */
function human(n: number): string {
  const mib = n / (1024 * 1024)
  if (mib >= 1024) return `${(mib / 1024).toFixed(1)}G`

  return n ? `${Math.max(1, Math.round(mib))}M` : '0M'
}

function humanMb(mb: number): string {
  return human(mb * 1024 * 1024)
}

function numberOr<T>(v: unknown, fallback: T): number | T {
  return typeof v === 'number' && Number.isFinite(v) ? v : fallback
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}
