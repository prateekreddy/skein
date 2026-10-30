// The moment a box needs its person, told by the box's own engine (SKEIN-1360).
//
// The cockpit has read this from outside: hook edges that nothing clears when a prompt is answered,
// and a screen observer that once read a spinner frozen on one frame as a box at work (SKEIN-321).
// From inside the engine the same moments are events: a tool call asked of a person, a question
// opened, a turn ended and why, a rate-limit window crossing a mark. This feature writes each one
// to one file, `<store>/status/<box>.mod.json`, beside the files the command hooks write.
//
// The file is recorded state with one writer, never authoritative (docs/architecture.md §2.1): the
// cockpit prefers it while it is fresh and falls back to its own reading otherwise. Fresh is the
// `seen` heartbeat, rewritten every HEARTBEAT_MS while the session lives, so a session that died
// stops being believed within the cockpit's window instead of saying "working" for ever: the
// heartbeat is what makes this a level signal rather than one more edge.
//
// One writer: the box's interactive session alone. A `claude -p` run inside the box loads the same
// plugin, and turn state keyed by box but written by every session is the bug that once wrote
// seventeen spurious `ended`s in 114 seconds (§2.2), so a session that is not interactive writes
// nothing. Where the file goes and under which box's name is decided by `probe/box-self.sh`, the
// one answer every script skein ships into a box uses (SKEIN-1174), never worked out again here.

import { atom, read, update } from 'claude-code'
import type {
  EngineInterface,
  On,
  SessionEndInput,
  SessionMeasureInput,
  SessionStartInput,
  Timer,
  ToolCallInput,
  TurnCompleteInput,
  TurnStartInput,
} from 'claude-code'

import type { SkeinAttention, SkeinAttentionLimit, SkeinAttentionState } from '../types'
import { pluginVersion } from './identity'

/** How often the file is rewritten while nothing happens. The cockpit trusts it for three of these. */
export const HEARTBEAT_MS = 30_000

/** The rate-limit marks, in percent used. Crossing one upward is written; falling back is not. */
export const LIMIT_MARKS = [75, 90, 100] as const

/** The file's shape tag. The cockpit reads only shapes it knows (`signals::ENGINE_SHAPE`). */
export const SIGNAL_SHAPE = 'attention-1'

const attention = atom({ plugin: 'skein', key: 'attention' } as const, null, {
  shape: 'attention-1',
})

// Prints the store and the box's name, one per line, through box-self.sh's two functions, or
// prints nothing and fails where either is unknown. `$1` the helper, `$2` the session's directory.
const WHERE = [
  '. "$1" 2>/dev/null || exit 1',
  'root="$(git -C "$2" rev-parse --show-toplevel 2>/dev/null || printf %s "$2")"',
  'store="$(skein_box_store "$root")" || exit 1',
  'name="$(skein_box_name)" || exit 1',
  'printf "%s\\n%s\\n" "$store" "$name"',
].join('\n')

// The heartbeat of this environment. A hot reload drops the old environment's timers with it, and
// a second `session.start` in one environment (a resume) cancels the first before starting its own.
let heartbeat: Timer | null = null

type Change = (was: SkeinAttention) => SkeinAttention | null

// Its parts of the events several features watch, which `register.tsx` hooks once, after `next`.

/** As the session starts: find the box, say the mod is loaded, and start the heartbeat. */
export async function onStart($: EngineInterface, e: SessionStartInput): Promise<void> {
  // Interactive sessions only: see "One writer" above.
  if (!e.isInteractive) return
  heartbeat?.cancel()
  heartbeat = null

  let where: string[] = []
  try {
    const dir = (await $.env.get('CLAUDE_PROJECT_DIR')) || e.cwd
    const ran = await $.process.run(
      ['bash', '-c', WHERE, 'skein-attention', `${$.plugin.root}/probe/box-self.sh`, dir],
      { timeoutMs: 10_000 },
    )
    if (ran.exitCode === 0) where = ran.stdout.split('\n').filter(line => line !== '')
  } catch {
    // No bash, no helper, or it hung: not a box skein can file a signal for.
  }
  if (where.length !== 2) return
  const [store, box] = where as [string, string]

  const version = await pluginVersion(() =>
    $.fs.read(`${$.plugin.root}/.claude-plugin/plugin.json`),
  )
  const now = await stamp($)
  const started: SkeinAttention = {
    file: `${store}/status/${box}.mod.json`,
    box,
    version,
    session: await $.session.id(),
    state: 'loaded',
    at: now,
    detail: '',
    stop: '',
    agent: '',
    asking: '',
    limit: null,
    marks: {},
  }
  await update($, attention, () => started)
  await write($, started, now)

  heartbeat = $.clock.every(HEARTBEAT_MS, () => {
    void beat()
  })
  async function beat(): Promise<void> {
    const held = await read($, attention)
    if (held !== null) await write($, held, await stamp($))
  }
}

/** A turn of the main loop began. A subagent's turn is not the box's. */
export async function onTurnStart($: EngineInterface, e: TurnStartInput): Promise<void> {
  if (e.agentId === undefined) await change($, was => moved(was, 'working', { detail: '', stop: '' }))
}

/**
 * A tool call returned, whoever answered it: the permission it was asked about, or the question
 * it put, is settled. Every other call changes nothing.
 */
export async function onToolCall($: EngineInterface, e: ToolCallInput): Promise<void> {
  await change($, was =>
    was.asking === e.tool_use_id && was.asking !== ''
      ? moved(was, 'working', { detail: '', asking: '', agent: '' })
      : null,
  )
}

/** A turn of the main loop ended, and why. */
export async function onTurnComplete($: EngineInterface, e: TurnCompleteInput): Promise<void> {
  if (e.agentId !== undefined) return
  const state: SkeinAttentionState =
    e.reason === 'answer'
      ? 'waiting'
      : e.reason === 'aborted'
        ? 'interrupted'
        : e.reason === 'refusal'
          ? 'refused'
          : 'error'
  await change($, was => moved(was, state, { stop: e.reason, asking: '', agent: '', detail: '' }))
}

/** The session's usage moved: a rate-limit window that crossed a mark is written. */
export async function onMeasure($: EngineInterface, e: SessionMeasureInput): Promise<void> {
  if (!e.changed.includes('rateLimits')) return
  await change($, was => {
    const marks = { ...was.marks }
    let crossed: SkeinAttentionLimit | null = null
    for (const window of e.rateLimits) {
      const mark = highestMark(window.percentUsed)
      const had = marks[window.kind] ?? 0
      // A window that fell below its mark has reset: the next crossing is news again.
      if (mark < had) marks[window.kind] = mark
      if (mark > had) {
        marks[window.kind] = mark
        if (crossed === null || mark > crossed.mark) {
          crossed = {
            kind: window.kind,
            mark,
            percentUsed: window.percentUsed,
            resetsAt: window.resetsAt ?? '',
          }
        }
      }
    }
    const same = JSON.stringify(marks) === JSON.stringify(was.marks)
    if (crossed === null && same) return null

    return { ...was, marks, limit: crossed ?? was.limit }
  })
}

/** The session ended. A /clear ends the conversation and not the process. */
export async function onEnd($: EngineInterface, e: SessionEndInput): Promise<void> {
  if (e.reason === 'clear') {
    // The person is at an empty prompt.
    await change($, was => moved(was, 'waiting', { detail: 'clear', stop: '', asking: '', agent: '' }))
    return
  }
  await change($, was => moved(was, 'ended', { detail: e.reason, stop: '', asking: '', agent: '' }))
  heartbeat?.cancel()
  heartbeat = null
}

// The hooks that are this feature's alone, each under a matcher of its own.
export function register(on: On): void {
  // A question put to the person opens as its call starts; `onToolCall` sees it answered.
  on('tool.call', { tool: 'AskUserQuestion' }, async ($, e, next) => {
    await change($, was =>
      moved(was, 'question', { detail: '', asking: e.tool_use_id, agent: e.agentId ?? '' }),
    )

    return next(e)
  })

  // The engine's verdict, read and handed up unchanged: this feature never decides a call. An ask
  // that a person answers is the box needing its person; one the mode's classifier or a host
  // answers is not, and `approver` says which before anything is shown. `onToolCall` sees the
  // call return.
  on('tool.check', { approver: 'person' }, async ($, e, next) => {
    const verdict = await next(e)
    if (verdict.decision === 'ask' && e.tool !== 'AskUserQuestion') {
      await change($, was =>
        moved(was, 'permission', {
          detail: e.tool,
          asking: e.tool_use_id ?? '',
          agent: e.agentId ?? '',
        }),
      )
    }

    return verdict
  })
}

/** The highest mark `percent` has reached, or 0 for none. */
export function highestMark(percent: number): number {
  let reached = 0
  for (const mark of LIMIT_MARKS) if (percent >= mark) reached = mark

  return reached
}

function moved(
  was: SkeinAttention,
  state: SkeinAttentionState,
  fields: Partial<Pick<SkeinAttention, 'detail' | 'stop' | 'asking' | 'agent'>>,
): SkeinAttention {
  return { ...was, ...fields, state, at: '' }
}

// Applies `fn` to what this session holds and writes the file when it changed anything. Nothing
// is held outside a box, or before `session.start` found one, and then nothing is written.
async function change($: EngineInterface, fn: Change): Promise<void> {
  const held = await read($, attention)
  if (held === null) return
  const next = fn(held)
  if (next === null) return
  const now = await stamp($)
  const stamped: SkeinAttention = { ...next, at: next.at === '' ? now : next.at }
  await update($, attention, () => stamped)
  await write($, stamped, now)
}

async function stamp($: EngineInterface): Promise<string> {
  return new Date(await $.clock.now()).toISOString()
}

// The file as the cockpit reads it (`signals::engine_signal`). A write that fails is dropped: the
// cockpit falls back to its own reading for a box whose file is missing or old, which is the point.
async function write($: EngineInterface, held: SkeinAttention, seen: string): Promise<void> {
  const body = {
    box: held.box,
    from: 'mod',
    shape: SIGNAL_SHAPE,
    version: held.version,
    session: held.session,
    state: held.state,
    at: held.at,
    seen,
    detail: held.detail,
    stop: held.stop,
    agent: held.agent,
    limit: held.limit,
  }
  try {
    await $.fs.write(held.file, `${JSON.stringify(body)}\n`)
  } catch {
    // Read-only, gone, or full: see above.
  }
}
