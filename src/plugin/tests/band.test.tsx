// The band above the prompt, its pane, its `/skein` subcommands and its `clean` tool, through the
// whole module (`claude plugin test src/plugin` loads it from register.tsx).
//
// Nothing sits beneath the plugin: each `on` here stands for the engine and the box, answering the
// signal file through `fs.read` and `skein-mcp` and `mailbox.sh` through `process.run`, with what
// those answer in a box.

import { expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On } from 'claude-code'

import { CLEAN_CONTRACT } from './clean-contract'

const NOW = Date.parse('2026-10-30T12:00:00Z')
const STATE = '/fleet-root-example/.skein/state/web-main'
const SURFACES = ['terminal', 'desktop'] as const

const G = 1024 * 1024 * 1024
const M = 1024 * 1024

const PIP = '/fleet-root-example/web-main/home/.cache/pip'
const PAD = '/fleet-root-example/web-main/home/.cache/skein/claude/x'
const SHARED = '/fleet-root-example/.skein/repos/web/cache'

/** A signal file as skein writes it, `ago` seconds old, with a disk ask or none; `over` replaces
 * fields of the ask. */
function signal(ago: number, ask: boolean, over: Record<string, unknown> = {}): string {
  const at = new Date(NOW - ago * 1000).toISOString().replace(/\.\d+Z$/, 'Z')

  return JSON.stringify({
    at,
    disk: { mb: 11468, limit_mb: 10240, fleet: 'under', fleet_over_by_mb: 0 },
    asks: ask
      ? [
          {
            kind: 'disk',
            why: 'allowance',
            amount: 1228,
            since: at,
            crossing_id: 'c-1',
            hold: '',
            start: 'start',
            remind: 'remind',
            enter: 'enter',
            ...over,
          },
        ]
      : [],
  })
}

/** This box's own spend as skein-server writes it, `ago` seconds old. */
function usage(ago: number, today: number, week: number, unpriced = false): string {
  const at = new Date(NOW - ago * 1000).toISOString().replace(/\.\d+Z$/, 'Z')
  const window = (cost: number) => ({ cost, input: 1, output: 2, cache_read: 3, cache_write: 4 })

  return JSON.stringify({ at, prices_as_of: '2026-10-30', today: window(today), week: window(week), unpriced })
}

/** What `skein-mcp clean` with no paths prints: the list, as `skein_clean` answers it. */
const LISTED = {
  result: {
    listed_at: '2026-10-30T11:58:00Z',
    items: [
      {
        path: PIP,
        in_box: '/home/agent/.cache/pip',
        kind: 'download cache',
        bytes: 3.2 * G,
        where: 'box',
        safe_now: true,
        why_not: null,
      },
      {
        path: PAD,
        in_box: '/home/agent/.cache/skein/claude/x',
        kind: 'scratchpad',
        bytes: 100 * M,
        where: 'box',
        safe_now: false,
        why_not: 'a scratchpad may belong to a session still running in this box',
      },
      {
        path: `${SHARED}/pip`,
        in_box: `${SHARED}/pip`,
        kind: 'shared download cache',
        bytes: 500 * M,
        where: 'shared',
        safe_now: true,
        why_not: null,
      },
    ],
    removable_now_bytes: 3.2 * G + 500 * M,
    shared_repo_cache: {
      path: SHARED,
      bytes: 500 * M,
      shared_with: 'every box of this repo',
      counted_against_this_box: false,
      safe_to_clean_now: true,
      why_not: null,
    },
  },
  said: '',
}

/** What `skein-mcp requests` prints: one waiting package request and one granted write. */
function requests(waiting: number, questions: readonly Record<string, unknown>[] = []) {
  const rows = [
    { queue: 'write', id: '8', asked: '2026-10-30T09:00:00Z', repo: 'acme/site', state: 'granted' },
  ]
  for (let i = 0; i < waiting; i++) {
    rows.push({ queue: 'package', id: `p${i}`, asked: '2026-10-30T10:00:00Z', kind: 'apt', packages: ['jq'], state: 'waiting' } as never)
  }
  rows.push(...(questions as never[]))

  return { result: { box: 'web-main', requests: rows }, said: '' }
}

type World = {
  signal: string
  /** `signals/usage.json`; none when absent. */
  usage?: string
  mail: string
  waiting: number
  /** Questions to the person, as `skein-mcp requests` lists them. */
  questions?: readonly Record<string, unknown>[]
  /** What a `skein-mcp clean <paths>` run prints. */
  removal?: { result: Record<string, unknown>; said: string }
  /** Every argv the plugin ran, in order. */
  ran: string[][]
  opened: string[]
}

function world(over: Partial<World> = {}): World {
  return { signal: signal(30, false), mail: '0', waiting: 0, ran: [], opened: [], ...over }
}

const out = (stdout: string, exitCode = 0) => ({
  value: { exitCode, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false },
})

function engine(on: On, w: World) {
  const clock = mock.clock(on, { now: NOW })
  mock.env(on, { SKEIN_BOX: 'web-main', SKEIN_STATE: STATE })
  on('command.register', (_, e) => ({ value: { command: e.name } }))
  on('tool.register', (_, e) => ({ value: { tool: `mcp__skein__${e.name}` } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('ui.open', (_, e) => {
    w.opened.push(e.id)

    return { value: { isPlaced: true } }
  })
  on('fs.read', (_, e) => {
    if (e.path === `${STATE}/signals/resources.json`) return { value: w.signal }
    if (e.path === `${STATE}/signals/usage.json` && w.usage !== undefined) return { value: w.usage }
    if (e.path.endsWith('/.claude-plugin/plugin.json')) return { value: '{"version":"9.8.7"}' }
    throw new Error(`no such file: ${e.path}`)
  })
  on('process.run', (_, e) => {
    const argv = [...e.argv]
    w.ran.push(argv)
    if (argv[1]?.endsWith('/probe/mailbox.sh') && argv[2] === 'unread') return out(`${w.mail}\n`)
    if (argv[1]?.endsWith('/bin/skein-mcp') && argv[2] === 'requests') return out(JSON.stringify(requests(w.waiting, w.questions)))
    if (argv[1]?.endsWith('/bin/skein-mcp') && argv[2] === 'clean') {
      if (argv.length === 3) return out(JSON.stringify(LISTED))
      const removal = w.removal ?? { result: { error: 'unexpected' }, said: '' }

      return out(JSON.stringify(removal), 'error' in removal.result ? 1 : 0)
    }

    return out('', 127)
  })

  return clock
}

async function started(on: On, w: World, $: Engine) {
  const clock = engine(on, w)
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  // The first look runs on a timer just after the start.
  await clock.settle()

  return clock
}

const BAND = {
  plugin: 'skein',
  component: 'AbovePrompt',
  requestId: 'above-prompt',
  props: {
    hasSurvey: false,
    isWorking: false,
    maxRows: 10,
    bodyColumns: 100,
    scroll: { offset: 0, bodyRows: 9 },
    view: {},
  },
} as const

const PANE = {
  plugin: 'skein',
  component: 'Pane',
  requestId: 'skein',
  props: {
    title: 'skein',
    isFocused: false,
    bodyColumns: 100,
    placement: 'dock',
    scroll: { offset: 0, bodyRows: 40 },
    view: {},
  },
} as const

const TYPED = (args: string) =>
  ({
    command: 'skein',
    args,
    origin: { kind: 'composer' },
    presentation: { isFullscreen: false, columns: 80 },
  }) as const

// ------------------------------------------------------------------------------------------------
// The band
// ------------------------------------------------------------------------------------------------

test('the band stays out of the way while skein asks nothing of the box', async ($, on) => {
  // What the engine draws when the plugin passes the band on.
  on('ui.render', ($, e) => {
    const { Text } = $.ui.resolve(e)

    return <Text>engine</Text>
  })
  await started(on, world(), $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect((await ui.find({ type: 'Text' }))?.text).toBe('engine')
    expect(await ui.find({ type: 'Button' })).toBe(undefined)
    await ui.unmount()
  }
})

test('the band says loudly that skein asks for disk, and counts requests and mail', async ($, on) => {
  const w = world({ signal: signal(30, true), waiting: 2, mail: '3' })
  await started(on, w, $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    const disk = await ui.find({ type: 'Text', text: /skein asks this box/ })
    expect(disk?.text.trim()).toBe(
      'skein asks this box to free 1.2G of disk: it is over its own allowance.',
    )
    expect(disk?.props.bold).toBe(true)
    expect(disk?.props.color).toBe('warning')
    expect((await ui.find({ type: 'Text', text: /waiting/ }))?.text.trim()).toBe(
      '2 requests are waiting for an answer.',
    )
    expect((await ui.find({ type: 'Text', text: /unread/ }))?.text.trim()).toBe(
      "3 unread messages, read at the agent's next turn.",
    )

    // Its button opens the pane with the list, and removes nothing by itself.
    const before = w.ran.length
    await ui.press({ key: 'band:disk' })
    expect(w.opened.at(-1)).toBe('skein')
    expect(w.ran.slice(before).filter(a => a[2] === 'clean' && a.length > 3)).toEqual([])
    await ui.unmount()
  }
})

// What would make it fail: askLine leaving out `live_mb` (the first line reads as today's), or
// saying "free 0M" when nothing can be cleared. Watched: with the `live` part of the line dropped,
// the first expectation failed.
const LIVE_CASES: Array<[string, Record<string, unknown>, string]> = [
  [
    'part of the box is in a running build',
    { amount: 1228, live_mb: 3072 },
    'skein asks this box to free 1.2G of disk: it is over its own allowance (3.0G more in a build running now).',
  ],
  [
    'all it could clear is in a running build',
    { amount: 0, live_mb: 3072 },
    'skein asks this box to free disk, but none of it can be cleared now: it is over its own allowance (3.0G in a build running now).',
  ],
  [
    'nothing skein lists can be cleared',
    { amount: 0 },
    'skein asks this box to free disk, but none of it can be cleared now: it is over its own allowance.',
  ],
]

for (const [when, over, line] of LIVE_CASES) {
  test(`the band says what skein asks when ${when}`, async ($, on) => {
    await started(on, world({ signal: signal(30, true, over) }), $)
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    expect((await ui.find({ type: 'Text', text: /skein asks this box/ }))?.text.trim()).toBe(line)
    await ui.unmount()
  })
}

test('a signal file skein stopped writing asks nothing', async ($, on) => {
  on('ui.render', ($, e) => {
    const { Text } = $.ui.resolve(e)

    return <Text>engine</Text>
  })
  // Ten minutes old: skein is not looking, as the command hooks read it.
  await started(on, world({ signal: signal(600, true) }), $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect((await ui.find({ type: 'Text' }))?.text).toBe('engine')
    await ui.unmount()
  }
})

// ------------------------------------------------------------------------------------------------
// The pane
// ------------------------------------------------------------------------------------------------

test('the pane offers Clean on what can go now, greys what cannot, and keeps the shared cache apart', async ($, on) => {
  await started(on, world({ signal: signal(30, true) }), $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...PANE, surface })
    expect((await ui.find({ type: 'Text', text: /holds/ }))?.text).toBe(
      'This box holds 11.2G of its 10.0G allowance. 3.7G can be removed now.',
    )
    expect(await ui.find({ type: 'Button', key: `clean:${PIP}` })).not.toBe(undefined)
    expect((await ui.find({ type: 'Text', text: /\.cache\/pip/ }))?.text.trim()).toBe(
      '/home/agent/.cache/pip, download cache, 3.2G',
    )

    // The scratchpad: no button, dimmed, and why.
    expect(await ui.find({ type: 'Button', key: `clean:${PAD}` })).toBe(undefined)
    const pad = await ui.find({ type: 'Text', text: /scratchpad/ })
    expect(pad?.props.dimColor).toBe(true)
    expect(pad?.text).toBe(
      '/home/agent/.cache/skein/claude/x, scratchpad, 100M: a scratchpad may belong to a session still running in this box',
    )

    // The shared cache on a row of its own, said not to count against this box.
    expect((await ui.find({ type: 'Text', text: /shared cache/ }))?.text.trim()).toBe(
      `The repo's shared cache, ${SHARED}, 500M. Not counted against this box.`,
    )
    expect(await ui.find({ type: 'Button', key: 'clean:shared' })).not.toBe(undefined)
    await ui.unmount()
  }
})

test('Clean runs skein-mcp clean on that item alone and shows what it said', async ($, on) => {
  const said = "skein: removed 1, about 3.2G. This box's disk figure catches up within five minutes."
  const w = world({
    signal: signal(30, true),
    removal: { result: { removed: ['/home/agent/.cache/pip'], freed_bytes: 3.2 * G, failed: [] }, said },
  })
  await started(on, w, $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...PANE, surface })
    const before = w.ran.length
    await ui.press({ key: `clean:${PIP}` })
    const removals = w.ran.slice(before).filter(a => a[2] === 'clean' && a.length > 3)
    expect(removals.length).toBe(1)
    expect(removals[0]?.[0]).toBe('python3')
    expect(removals[0]?.[1]?.endsWith('/bin/skein-mcp')).toBe(true)
    expect(removals[0]?.slice(2)).toEqual(['clean', PIP])
    expect((await ui.find({ type: 'Text', text: /removed 1/ }))?.text).toBe(said)
    await ui.unmount()

    // And the band says it too, until it is dismissed.
    const band = await $.ui.mount({ ...BAND, surface })
    expect((await band.find({ type: 'Text', text: /removed 1/ }))?.text.trim()).toBe(said)
    await band.unmount()
  }
})

// ------------------------------------------------------------------------------------------------
// The clean tool: one answer with the skein_clean MCP tool
// ------------------------------------------------------------------------------------------------

test('the clean tool answers what the skein_clean MCP tool answers, case by case', async ($, on) => {
  const w = world()
  await started(on, w, $)

  for (const c of CLEAN_CONTRACT) {
    // What `skein-mcp clean <paths>` prints for this case, as `tests/plugin_tools.rs` holds it to.
    w.removal = { result: c.result, said: c.said }
    const before = w.ran.length
    const ran = await $.tool.call({ tool: 'mcp__skein__clean', paths: c.paths })

    expect(w.ran[before]?.slice(2)).toEqual(['clean', ...c.paths])
    const json = JSON.stringify(c.result, null, 1)
    if ('error' in c.result) {
      // An error is a deny, which the model reads as the tool's error result.
      expect(ran.deny).toBe(c.said === '' ? json : `${json}\n${c.said}`)
      continue
    }
    // Core holds a registered tool's result to a string or a list of blocks, and refuses any other
    // shape in a live session though this harness does not: MCP's `{ content, isError }` was refused
    // there on every call (SKEIN-1409).
    expect(Array.isArray(ran.result)).toBe(true)
    const blocks = ran.result as { type: string; text: string }[]
    expect(blocks.map(b => b.type)).toEqual(['text', 'text'])
    expect(JSON.parse(blocks[0]?.text ?? '')).toEqual(c.result)
    expect(blocks[1]?.text).toBe(c.said)
  }
})

test('the clean tool refuses a list of anything but paths as the MCP tool does', async ($, on) => {
  const w = world()
  await started(on, w, $)
  const before = w.ran.length

  const ran = await $.tool.call({ tool: 'mcp__skein__clean', paths: [3] })

  expect(ran.result).toBeUndefined()
  expect(JSON.parse(ran.deny ?? '')).toEqual({ error: 'paths-must-be-a-list-of-paths' })
  expect(w.ran.length).toBe(before)
})

// ------------------------------------------------------------------------------------------------
// /skein
// ------------------------------------------------------------------------------------------------

test('/skein status says the disk, the ask, the requests and the mail', async ($, on) => {
  await started(on, world({ signal: signal(30, true), waiting: 1, mail: '2' }), $)

  const ran = await $.command.run(TYPED('status'))

  expect(ran.text).toBe(
    [
      'This box holds 11.2G of its 10.0G allowance. 3.7G can be removed now.',
      'skein asks this box to free 1.2G of disk: it is over its own allowance.',
      '1 request waiting. /skein requests lists them.',
      '2 unread. The agent reads them at the start of its next turn.',
      "skein has not written this box's spend in the last 70 minutes.",
    ].join('\n'),
  )
})

// Fails if the line stops saying the dollars are an estimate at API prices, or if `+` is not added
// where something had no price. Watched: with "estimated at API prices" taken out of `spendLine`,
// this failed on the band and on /skein status.
test('the band and /skein status say the spend is an estimate, with + when something had no price', async ($, on) => {
  const w = world({ usage: usage(60, 1.2, 31.084) })
  await started(on, w, $)
  const line = 'Spend, estimated at API prices, as of 11:59 UTC: today ≈$1.20 · 7 days ≈$31.08'

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    const shown = await ui.find({ type: 'Text', text: /Spend/ })
    expect(shown?.text).toBe(line)
    expect(shown?.props.dimColor).toBe(true)
    await ui.unmount()
  }
  expect((await $.command.run(TYPED('status'))).text?.split('\n').at(-1)).toBe(line)

  w.usage = usage(60, 1.2, 140.5, true)
  const status = (await $.command.run(TYPED('status'))).text?.split('\n') ?? []
  expect(status.slice(-2)).toEqual([
    'Spend, estimated at API prices, as of 11:59 UTC: today ≈$1.20+ · 7 days ≈$141+',
    '+ means some of it was on a model skein has no price for, so it is left out.',
  ])
})

// skein reads the fleet's usage once an hour, and the band trusts the file for 70 minutes. Fails if
// the file is believed whatever its age, or for the resource signal's five minutes. Watched: with
// `readSpend` judging age by `STALE_AFTER_MS`, the 69-minute figure was not shown and this failed;
// with the age check removed, the 71-minute figure was shown and this failed.
test('spend is shown for 70 minutes after skein read it, and not after', async ($, on) => {
  on('ui.render', ($, e) => {
    const { Text } = $.ui.resolve(e)

    return <Text>engine</Text>
  })
  const w = world({ usage: usage(69 * 60, 1.2, 8.4) })
  await started(on, w, $)
  expect((await $.command.run(TYPED('status'))).text?.split('\n').at(-1)).toBe(
    'Spend, estimated at API prices, as of 10:51 UTC: today ≈$1.20 · 7 days ≈$8.40',
  )

  w.usage = usage(71 * 60, 1.2, 8.4)
  await $.command.run(TYPED('status'))

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect((await ui.find({ type: 'Text' }))?.text).toBe('engine')
    expect(await ui.find({ type: 'Text', text: /Spend/ })).toBe(undefined)
    await ui.unmount()
  }
  expect((await $.command.run(TYPED('status'))).text?.split('\n').at(-1)).toBe(
    "skein has not written this box's spend in the last 70 minutes.",
  )
})

test('/skein requests lists each request with its state', async ($, on) => {
  await started(on, world({ waiting: 1 }), $)

  const ran = await $.command.run(TYPED('requests'))

  expect(ran.text).toBe(['this box asked for:', '  write acme/site: granted', '  package apt jq: waiting'].join('\n'))
})

test('/skein clean opens the list, and /skein alone still names the box', async ($, on) => {
  const w = world()
  await started(on, w, $)

  const clean = await $.command.run(TYPED('clean'))
  expect(clean.text).toBe('the list of what this box can remove is open.')
  expect(w.opened).toEqual(['skein'])

  expect((await $.command.run(TYPED(''))).text).toBe('version 9.8.7, in box web-main')
  expect((await $.command.run(TYPED('bogus'))).text).toBe(
    'there is no /skein bogus. Try /skein status, /skein clean or /skein requests.',
  )
})

// ------------------------------------------------------------------------------------------------
// When the band looks
// ------------------------------------------------------------------------------------------------

/** How many times the band has read the requests: one per look. */
const looks = (w: World) => w.ran.filter(a => a[2] === 'requests').length

test('a resumed session looks once an interval, not once per start', async ($, on) => {
  const w = world()
  const clock = engine(on, w)
  // A resume raises `session.start` again in the same module.
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await clock.settle()
  const before = looks(w)

  await clock.advance(60_000)
  expect(looks(w) - before).toBe(1)
  await clock.advance(60_000)
  expect(looks(w) - before).toBe(2)
})

test('a claude -p session runs nothing on a timer, and /skein status still answers', async ($, on) => {
  const w = world({ signal: signal(30, true) })
  const clock = engine(on, w)
  await $.session.start({ cwd: '/', surface: null, isInteractive: false })
  await clock.settle()
  await clock.advance(180_000)
  expect(w.ran).toEqual([])

  const ran = await $.command.run(TYPED('status'))
  expect(ran.text?.split('\n')[1]).toBe(
    'skein asks this box to free 1.2G of disk: it is over its own allowance.',
  )
})

test("the band looks again after the main loop's turn, not after a subagent's", async ($, on) => {
  const w = world()
  // The engine, beneath: a turn's end answers with its text.
  on('turn.complete', (_, e) => ({ text: e.answer }))
  const clock = await started(on, w, $)
  const before = looks(w)
  const turn = { reason: 'answer', answer: 'done', durationMs: 1, isAborted: false } as const

  await $.turn.complete({ ...turn, turnId: 't1', agentId: 'a1' })
  await clock.settle()
  expect(looks(w)).toBe(before)

  await $.turn.complete({ ...turn, turnId: 't2' })
  await clock.settle()
  expect(looks(w)).toBe(before + 1)
})

test('unread mail is said in the band with no button to press', async ($, on) => {
  await started(on, world({ mail: '2' }), $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect((await ui.find({ type: 'Text', text: /unread/ }))?.text.trim()).toBe(
      "2 unread messages, read at the agent's next turn.",
    )
    expect(await ui.find({ type: 'Button' })).toBe(undefined)
    await ui.unmount()
  }
})

// ------------------------------------------------------------------------------------------------
// A question waiting for the person
// ------------------------------------------------------------------------------------------------

const LONG =
  'Should I drop the legacy importer now that every repo has moved, or keep it until the next ' +
  'release so the two boxes still on it can finish?\nThe tests for it take four minutes.'

const question = (id: string, asked: string, text: string, state = 'waiting') => ({
  queue: 'question',
  id,
  asked,
  question: text,
  options: [],
  state,
})

test('the band shows the oldest waiting question in its own words, cut to one line', async ($, on) => {
  const w = world({
    questions: [
      question('q3', '2026-10-30T11:00:00Z', 'Is the newer one shown? It should not be.'),
      question('q1', '2026-10-30T08:00:00Z', LONG),
      question('q0', '2026-10-30T07:00:00Z', 'Answered long ago.', 'answered'),
    ],
  })
  await started(on, w, $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface, props: { ...BAND.props, bodyColumns: 100 } })
    // 100 cells less "Waiting for your answer: ", " And 1 more question." and 11 for the
    // quotes and the button: 43 cells for the question and its `…`, cut after a whole word.
    expect((await ui.find({ type: 'Text', text: /Waiting for your answer/ }))?.text.trim()).toBe(
      'Waiting for your answer: “Should I drop the legacy importer now that…” And 1 more question.',
    )
    expect(await ui.find({ type: 'Button', key: 'band:question' })).not.toBe(undefined)
    // Questions are not counted again among the requests.
    expect(await ui.find({ type: 'Text', text: /requests? (is|are) waiting/ })).toBe(undefined)
    await ui.unmount()

    // The pane keeps every question whole.
    const pane = await $.ui.mount({ ...PANE, surface })
    expect((await pane.find({ type: 'Text', text: /legacy importer/ }))?.text).toBe(`question ${LONG}: waiting`)
    await pane.unmount()
  }
})

test('a short one-line question is shown whole, and the cut never leaves under 20 cells', async ($, on) => {
  const w = world({ questions: [question('q1', '2026-10-30T08:00:00Z', 'Merge now?')] })
  await started(on, w, $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect((await ui.find({ type: 'Text', text: /Waiting for your answer/ }))?.text.trim()).toBe(
      'Waiting for your answer: “Merge now?”',
    )
    await ui.unmount()
  }
  w.questions = [question('q1', '2026-10-30T08:00:00Z', LONG)]
  await $.command.run(TYPED('status'))

  // 30 cells leave less than 20, so the question gets 20, `…` included.
  const narrow = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, bodyColumns: 30 } })
  expect((await narrow.find({ type: 'Text', text: /Waiting for your answer/ }))?.text.trim()).toBe(
    'Waiting for your answer: “Should I drop the…”',
  )
  await narrow.unmount()
})

test('a question with more lines than its first shows that line and `…`', async ($, on) => {
  const w = world({ questions: [question('q1', '2026-10-30T08:00:00Z', 'Merge now?\nCI is green.')] })
  await started(on, w, $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect((await ui.find({ type: 'Text', text: /Waiting for your answer/ }))?.text.trim()).toBe(
      'Waiting for your answer: “Merge now?…”',
    )
    await ui.unmount()
  }
})
