// The attention signal, through the whole module: each event a box's person is needed on writes
// the state the cockpit reads (`signals::engine_signal` in skein's Rust), and nothing else does.
//
// Nothing sits beneath the plugin under test: each `on` here stands for the engine, the box's
// filesystem and `probe/box-self.sh` included. `writes` is every file the plugin wrote, in order.

import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'
import type { MockClock } from 'claude-code/testing'

const MANIFEST = JSON.stringify({ name: 'skein', version: '9.8.7' })
const STORE = '/store/data'
const BOX = 'web-main'
const FILE = `${STORE}/status/${BOX}.mod.json`

type Written = { path: string; signal: Record<string, unknown> }

type Box = {
  clock: MockClock
  writes: Written[]
  ran: (readonly string[])[]
  /** The last signal written to the box's file, parsed. */
  last: () => Record<string, unknown>
  /** The states written, in order. */
  states: () => unknown[]
}

// An engine and a box: the manifest, the helper's answer (`where`, or a refusal), and a record of
// what was written. `null` for `where` is a session that is not in a box skein knows.
function engine(on: On, where: string | null = `${STORE}\n${BOX}\n`): Box {
  const writes: Written[] = []
  const ran: (readonly string[])[] = []
  on('command.register', (_, e) => ({ value: { command: e.name } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('session.id', () => ({ value: 'session-1' }))
  on('fs.read', () => ({ value: MANIFEST }))
  on('fs.write', (_, e) => {
    writes.push({ path: e.path, signal: JSON.parse(e.text) as Record<string, unknown> })
    return { value: undefined }
  })
  on('process.run', (_, e) => {
    ran.push(e.argv)
    return {
      value: {
        exitCode: where === null ? 1 : 0,
        stdout: where ?? '',
        stderr: '',
        isStdoutTruncated: false,
        isStderrTruncated: false,
      },
    }
  })
  on('turn.start', (_, e) => ({ turnId: e.turnId }))
  on('turn.complete', () => ({ text: '' }))
  on('session.measure', (_, e) => ({ changed: e.changed }))
  on('session.end', (_, e) => ({ sessionId: e.sessionId }))
  // No settings hook beneath: the notification goes on with nothing added.
  on('classic.Notification', () => ({}))
  mock.env(on, { SKEIN_BOX: BOX })
  const clock = mock.clock(on, { now: NOON })
  const last = () => {
    const w = writes.at(-1)
    if (w === undefined) throw new Error('nothing was written')
    return w.signal
  }

  return { clock, writes, ran, last, states: () => writes.map(w => w.signal.state) }
}

const NOON = Date.UTC(2026, 9, 30, 12, 0, 0)

const START = { cwd: '/boxes/web-main/tree', surface: 'terminal', isInteractive: true } as const

// The notification the engine raises as the permission dialog opens in front of the person.
const SHOWN = {
  message: 'Claude needs your permission to use Bash',
  notification_type: 'permission_prompt',
} as const

const TURN_END = { answer: '', durationMs: 5, isAborted: false, turnId: 't1' } as const

test('an interactive session in a box says the mod is loaded, with its version', async ($, on) => {
  const box = engine(on)

  await $.session.start(START)

  // The one answer to "which store, which box" is box-self.sh's, so the plugin asks it.
  expect(box.ran.length).toBe(1)
  expect(box.ran[0]?.[0]).toBe('bash')
  expect(String(box.ran[0]?.[4]).endsWith('/probe/box-self.sh')).toBe(true)
  expect(box.ran[0]?.[5]).toBe('/boxes/web-main/tree')
  expect(box.writes.map(w => w.path)).toEqual([FILE])
  expect(box.last()).toEqual({
    box: BOX,
    from: 'mod',
    shape: 'attention-1',
    version: '9.8.7',
    session: 'session-1',
    state: 'loaded',
    at: '2026-10-30T12:00:00.000Z',
    seen: '2026-10-30T12:00:00.000Z',
    detail: '',
    stop: '',
    agent: '',
    limit: null,
  })
})

test('a session that is not interactive writes nothing: one writer per box', async ($, on) => {
  const box = engine(on)

  await $.session.start({ ...START, isInteractive: false })
  await $.turn.start({ text: 'hi', turnId: 't1' })
  await $.turn.complete({ ...TURN_END, reason: 'answer' })

  expect(box.ran.length).toBe(0)
  expect(box.writes.length).toBe(0)
})

test('a session outside a box skein knows writes nothing', async ($, on) => {
  const box = engine(on, null)

  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't1' })

  expect(box.writes.length).toBe(0)
})

test('a turn is working, and its end says why it ended', async ($, on) => {
  const box = engine(on)
  await $.session.start(START)

  await $.turn.start({ text: 'hi', turnId: 't1' })
  expect(box.last().state).toBe('working')

  await $.turn.complete({ ...TURN_END, reason: 'answer' })
  expect(box.last().state).toBe('waiting')
  expect(box.last().stop).toBe('answer')

  await $.turn.start({ text: 'again', turnId: 't2' })
  await $.turn.complete({ ...TURN_END, turnId: 't2', isAborted: true, reason: 'aborted' })
  expect(box.last().state).toBe('interrupted')
  expect(box.last().stop).toBe('aborted')

  await $.turn.complete({ ...TURN_END, turnId: 't3', reason: 'error' })
  expect(box.last().state).toBe('error')

  await $.turn.complete({
    ...TURN_END,
    turnId: 't4',
    reason: 'refusal',
    refusal: { category: null, explanation: null },
  })
  expect(box.last().state).toBe('refused')
  expect(box.last().stop).toBe('refusal')
})

test("a subagent's turn is not the box's turn", async ($, on) => {
  const box = engine(on)
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't1' })
  const before = box.writes.length

  await $.turn.start({ text: 'sub', turnId: 's1', agentId: 'a1' })
  await $.turn.complete({ ...TURN_END, turnId: 's1', agentId: 'a1', reason: 'answer' })

  expect(box.writes.length).toBe(before)
  expect(box.last().state).toBe('working')
})

test('a permission wait is written when its dialog opens, not when the verdict asks', async ($, on) => {
  const box = engine(on)
  on('tool.check', () => ({ decision: 'ask', reason: 'not allowed yet' }))
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't1' })

  const verdict = await $.tool.check({
    tool: 'Bash',
    input: { command: 'rm -rf build' },
    tool_use_id: 'u1',
    approver: 'person',
  })

  // Handed up unchanged, and not yet the person's: a classifier may still settle it.
  expect(verdict.decision).toBe('ask')
  expect(verdict.reason).toBe('not allowed yet')
  expect(box.states()).toEqual(['loaded', 'working'])

  await $.classic.Notification(SHOWN)
  expect(box.last().state).toBe('permission')
  expect(box.last().detail).toBe('Bash')
})

test('an ask that auto mode settles before any dialog opens never reads as a wait', async ($, on) => {
  const box = engine(on)
  // The person's check asks; the mode settles it (here, as a background loop does) and the tool
  // runs with no dialog, so no permission notification is raised.
  on('tool.check', (_, e) => ({ decision: e.approver === 'person' ? 'ask' : 'allow' }))
  on('tool.call', async (_, e) => {
    await $.tool.check({
      tool: 'Bash',
      input: { command: 'make' },
      tool_use_id: e.tool_use_id,
      approver: 'person',
    })
    return { result: { stdout: 'ok', stderr: '', interrupted: false } }
  })
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't1' })

  await $.tool.call({ tool: 'Bash', command: 'make' })
  await $.turn.complete({ ...TURN_END, reason: 'answer' })

  expect(box.states()).toEqual(['loaded', 'working', 'waiting'])
})

test('a dialog shown with no ask remembered is still a wait, settled by the next call back', async ($, on) => {
  const box = engine(on)
  on('tool.call', () => ({ result: { stdout: 'ok', stderr: '', interrupted: false } }))
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't1' })

  await $.classic.Notification(SHOWN)
  expect(box.last().state).toBe('permission')
  expect(box.last().detail).toBe('')

  await $.tool.call({ tool: 'Bash', command: 'make' })
  expect(box.last().state).toBe('working')
})

test('an ask nobody but a host answers, or an allow, is not a wait on the person', async ($, on) => {
  const box = engine(on)
  let decision: 'ask' | 'allow' = 'ask'
  on('tool.check', () => ({ decision }))
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't1' })

  await $.tool.check({ tool: 'Bash', input: { command: 'ls' }, tool_use_id: 'u1', approver: 'host' })
  await $.tool.check({ tool: 'Bash', input: { command: 'ls' }, tool_use_id: 'u2' })
  decision = 'allow'
  await $.tool.check({ tool: 'Bash', input: { command: 'ls' }, tool_use_id: 'u3', approver: 'person' })

  expect(box.states()).toEqual(['loaded', 'working'])
})

test('a permission wait ends when the call it asked about returns', async ($, on) => {
  const box = engine(on)
  // The kit raises a check of its own for the call, as a background loop does, with nobody to ask:
  // an ask there would refuse the call before it ran. Only the person's check asks.
  on('tool.check', (_, e) => ({ decision: e.approver === 'person' ? 'ask' : 'allow' }))
  // Core, for the call: the person is asked (the check, as a session raises it inside the call),
  // then the tool runs.
  on('tool.call', async (_, e) => {
    await $.tool.check({
      tool: 'Bash',
      input: { command: 'make' },
      tool_use_id: e.tool_use_id,
      approver: 'person',
    })
    await $.classic.Notification(SHOWN)
    return { result: { stdout: 'ok', stderr: '', interrupted: false } }
  })
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't1' })

  await $.tool.call({ tool: 'Bash', command: 'make' })

  expect(box.states()).toEqual(['loaded', 'working', 'permission', 'working'])
  expect(box.writes[2]?.signal.detail).toBe('Bash')
})

test('an AskUserQuestion is a question while it is open, and working once answered', async ($, on) => {
  const box = engine(on)
  let during: unknown = null
  on('tool.call', { tool: 'AskUserQuestion' }, () => {
    during = box.last().state
    return { result: { answers: {} } }
  })
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't1' })

  await $.tool.call({
    tool: 'AskUserQuestion',
    questions: [
      {
        question: 'Which one?',
        header: 'Pick',
        options: [
          { label: 'a', description: 'the first' },
          { label: 'b', description: 'the second' },
        ],
        multiSelect: false,
      },
    ],
  })

  expect(during).toBe('question')
  expect(box.last().state).toBe('working')
})

test('a rate-limit window crossing a mark is written once, and again after it resets', async ($, on) => {
  const box = engine(on)
  await $.session.start(START)
  const measure = (percentUsed: number) =>
    $.session.measure({
      context: { window: 200000 },
      rateLimits: [{ kind: 'five_hour', percentUsed, resetsAt: '2026-10-30T15:00:00Z' }],
      changed: ['rateLimits'],
    })

  await measure(40)
  expect(box.writes.length).toBe(1)

  await measure(91)
  expect(box.last().limit).toEqual({
    kind: 'five_hour',
    mark: 90,
    percentUsed: 91,
    resetsAt: '2026-10-30T15:00:00Z',
  })
  expect(box.last().state).toBe('loaded')
  const crossed = box.writes.length

  await measure(93)
  expect(box.writes.length).toBe(crossed)

  // The window reset: below every mark, so the next crossing is news again.
  await measure(5)
  await measure(80)
  expect(box.last().limit).toEqual({
    kind: 'five_hour',
    mark: 75,
    percentUsed: 80,
    resetsAt: '2026-10-30T15:00:00Z',
  })
})

test('the heartbeat rewrites the file with a new seen and the same state', async ($, on) => {
  const box = engine(on)
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't1' })
  await $.turn.complete({ ...TURN_END, reason: 'answer' })
  const settled = box.writes.length

  await box.clock.advance(30_000)

  expect(box.writes.length).toBe(settled + 1)
  expect(box.last().state).toBe('waiting')
  expect(box.last().at).toBe('2026-10-30T12:00:00.000Z')
  expect(box.last().seen).toBe('2026-10-30T12:00:30.000Z')
})

test('a /clear leaves the person at a prompt, and any other end is the end', async ($, on) => {
  const box = engine(on)
  const resume = { sessionId: 'session-1', command: 'claude --resume session-1' }
  await $.session.start(START)
  await $.turn.start({ text: 'hi', turnId: 't1' })

  await $.session.end({ reason: 'clear', sessionId: 'session-1', resume } as never)
  expect(box.last().state).toBe('waiting')
  expect(box.last().detail).toBe('clear')

  await $.session.end({ reason: 'prompt_input_exit', sessionId: 'session-1', resume } as never)
  expect(box.last().state).toBe('ended')
  expect(box.last().detail).toBe('prompt_input_exit')
})
