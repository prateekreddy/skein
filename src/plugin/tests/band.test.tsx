// The one line above the prompt and `/skein clean`, through the whole module (`claude plugin test
// src/plugin` loads it from register.tsx).
//
// Nothing sits beneath the plugin: each `on` here stands for the engine and the box, answering the
// signal file through `fs.read` and `skein-mcp` through `process.run`, with what those answer in a
// box.

import { expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On } from 'claude-code'

import { CLEAN_CONTRACT } from './clean-contract'

const NOW = Date.parse('2026-10-30T12:00:00Z')
const STATE = '/fleet-root-example/.skein/state/web-main'
const G = 1024 * 1024 * 1024
const M = 1024 * 1024
const stamp = (ms: number) => new Date(ms).toISOString().replace(/\.\d+Z$/, 'Z')

/** A signal file `ago` seconds old, with these asks and updates. */
const signal = (ago: number, asks: unknown[] = [], updates: unknown[] = []) =>
  JSON.stringify({ at: stamp(NOW - ago * 1000), asks, updates })

const diskAsk = (hold: string, amount = 1228) => ({ kind: 'disk', crossing_id: 'c-1', amount, hold })

const question = (id: string) => ({ queue: 'question', id, state: 'waiting', question: 'Which one?' })
const pkg = (id: string, state = 'waiting') => ({ queue: 'package', id, state, kind: 'apt', packages: ['jq'] })

const LISTED = {
  result: {
    items: [
      { path: '/f/small', in_box: '/home/agent/.cache/small', kind: 'download cache', bytes: 100 * M, safe_now: true },
      { path: '/f/big', in_box: '/home/agent/.cache/big dir', kind: 'build output', bytes: 3 * G, safe_now: true },
      { path: '/f/pad', in_box: '/home/agent/.cache/pad', kind: 'scratchpad', bytes: 9 * G, safe_now: false, why_not: 'in use' },
    ],
    removable_now_bytes: 3 * G + 100 * M,
  },
  said: '',
}

type World = {
  env?: Record<string, string>
  signal: string
  requests: Record<string, unknown>[]
  listed?: unknown
  removal?: { result: Record<string, unknown>; said: string }
  ran: string[][]
}

const world = (over: Partial<World> = {}): World => ({ signal: signal(30), requests: [], ran: [], ...over })

const out = (stdout: string, exitCode = 0) => ({
  value: { exitCode, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false },
})

function engine(on: On, w: World) {
  const clock = mock.clock(on, { now: NOW })
  mock.env(on, w.env ?? { SKEIN_BOX: 'web-main', SKEIN_STATE: STATE })
  on('command.register', (_, e) => ({ value: { command: e.name } }))
  on('tool.register', (_, e) => ({ value: { tool: `mcp__skein__${e.name}` } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  // What the engine draws when the plugin passes the band on.
  on('ui.render', ($, e) => {
    const { Text } = $.ui.resolve(e)

    return <Text>engine</Text>
  })
  on('fs.read', (_, e) => {
    if (e.path === `${STATE}/signals/resources.json`) return { value: w.signal }
    if (e.path.endsWith('/.claude-plugin/plugin.json')) return { value: '{"version":"9.8.7"}' }
    throw new Error(`no such file: ${e.path}`)
  })
  on('process.run', (_, e) => {
    const argv = [...e.argv]
    w.ran.push(argv)
    if (argv[1]?.endsWith('/bin/skein-mcp') && argv[2] === 'requests') {
      return out(JSON.stringify({ result: { box: 'web-main', requests: w.requests }, said: '' }))
    }
    if (argv[1]?.endsWith('/bin/skein-mcp') && argv[2] === 'clean') {
      if (argv.length === 3) return out(JSON.stringify(w.listed ?? LISTED))
      const removal = w.removal ?? { result: { error: 'unexpected' }, said: '' }

      return out(JSON.stringify(removal), 'error' in removal.result ? 1 : 0)
    }

    return out('', 127)
  })

  return clock
}

async function started(on: On, w: World, $: Engine, isInteractive = true) {
  const clock = engine(on, w)
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive })
  await clock.settle()

  return clock
}

const BAND = {
  plugin: 'skein',
  component: 'AbovePrompt',
  requestId: 'above-prompt',
  surface: 'terminal',
  props: { hasSurvey: false, isWorking: false, maxRows: 10, bodyColumns: 100, scroll: { offset: 0, bodyRows: 9 }, view: {} },
} as const

const TYPED = (args: string) =>
  ({ command: 'skein', args, origin: { kind: 'composer' }, presentation: { isFullscreen: false, columns: 80 } }) as const

/** What the band draws: skein's line, or `engine` where skein passed it on and drew nothing. */
async function line($: Engine): Promise<string | undefined> {
  const ui = await $.ui.mount(BAND)
  const text = (await ui.find({ type: 'Text' }))?.text
  await ui.unmount()

  return text
}

// What would make it fail: the count or the words drawn from anything but what waits, such as a
// granted request counted. Watched: with the `state` filter taken out of `waiting`, the granted
// package request made this line say 5.
test('the band is one line: the count, where to go, and what waits in a few words', async ($, on) => {
  const w = world({
    signal: signal(30, [diskAsk('hold-1')], [{ component: 'claude-code', new: '2.1.320', restart_requested: false }]),
    requests: [question('q1'), pkg('p1'), pkg('p2', 'granted')],
  })
  await started(on, w, $)

  expect(await line($)).toBe('⚑ 4 waiting on you: open the cockpit (free 1.2G, a question, a package request, a restart)')
})

// What would make it fail: a line drawn when nothing waits. Watched: with `found.count === 0`
// taken out of the render hook, the band drew `0 waiting on you` and this failed.
test('nothing waiting draws nothing, and a disk ask the agent can clear is not waiting', async ($, on) => {
  const w = world({ signal: signal(30, [diskAsk('')]), requests: [pkg('p1', 'granted')] })
  await started(on, w, $)

  expect(await line($)).toBe('engine')
})

// What would make it fail: an old signal file believed. Watched: with the staleness check removed
// from `readSignal`, the five-minute-old hold was counted and this failed.
test('a signal file skein stopped writing asks nothing', async ($, on) => {
  const w = world({ signal: signal(301, [diskAsk('hold-1')]) })
  await started(on, w, $)

  expect(await line($)).toBe('engine')
})

// What would make it fail: a band drawn in a session outside a skein box.
test("outside a skein box the band is the engine's", async ($, on) => {
  const w = world({ env: { SKEIN_BOX: 'web-main' }, requests: [question('q1')] })
  await started(on, w, $)

  expect(await line($)).toBe('engine')
})

// What would make it fail: the look after a turn dropped, so a new request waits a minute unseen.
test('the line follows the requests after a turn of the main loop, not of a subagent', async ($, on) => {
  const w = world()
  // The engine, beneath: a turn's end answers with its text.
  on('turn.complete', (_, e) => ({ text: e.answer }))
  const clock = await started(on, w, $)
  expect(await line($)).toBe('engine')
  const turn = { reason: 'answer', answer: 'done', durationMs: 1, isAborted: false } as const

  w.requests = [question('q1')]
  await $.turn.complete({ ...turn, turnId: 't1', agentId: 'a1' })
  await clock.settle()
  expect(await line($)).toBe('engine')

  await $.turn.complete({ ...turn, turnId: 't2' })
  await clock.settle()
  expect(await line($)).toBe('⚑ 1 waiting on you: open the cockpit (a question)')
})

// What would make it fail: a second timer left by a resumed session.
test('a resumed session looks once an interval, not once per start', async ($, on) => {
  const w = world()
  const clock = await started(on, w, $)
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await clock.settle()
  const before = w.ran.filter(a => a[2] === 'requests').length

  await clock.advance(60_000)

  expect(w.ran.filter(a => a[2] === 'requests').length - before).toBe(1)
})

test('a claude -p session runs nothing on a timer', async ($, on) => {
  const w = world()
  const clock = await started(on, w, $, false)

  await clock.advance(120_000)

  expect(w.ran).toEqual([])
})

// What would make it fail: a row that cannot be removed offered as if it could, or the rows out of
// size order. Watched: with the `safe_now` filter taken out, `/home/agent/.cache/pad` was listed.
test('/skein clean lists what can go now, largest first, and counts the rest', async ($, on) => {
  await started(on, world(), $)

  const ran = await $.command.run(TYPED('clean'))

  expect(ran.text).toBe(
    [
      'this box can remove 3.1G now:',
      '    3.0G  /home/agent/.cache/big dir  (build output)',
      '    100M  /home/agent/.cache/small  (download cache)',
      '1 more cannot be removed now.',
      '/skein clean PATH removes one.',
    ].join('\n'),
  )
})

// What would make it fail: `/skein clean PATH` building its own sentence, running anything but the
// command line `tests/plugin_tools.rs` holds to the MCP tool's answer, or leaving skein-mcp's
// `skein: ` on a line the engine already heads with it, which a live session drew as
// `skein: skein: removed…`. Watched: with `unheaded` returning its argument, this failed.
test('/skein clean PATH says what skein-mcp clean said, case by case', async ($, on) => {
  const w = world()
  await started(on, w, $)

  for (const c of CLEAN_CONTRACT) {
    w.removal = { result: c.result, said: c.said }
    const before = w.ran.length
    const ran = await $.command.run(TYPED(`clean ${c.paths[0]}`))

    expect(w.ran[before]?.slice(2)).toEqual(['clean', c.paths[0]])
    expect(c.said.startsWith('skein: ')).toBe(true)
    expect(ran.text).toBe(c.said.slice('skein: '.length))
  }
})

// What would make it fail: splitting the argument on spaces, which would remove the wrong path or
// refuse a right one.
test('/skein clean takes a path with a space in it whole', async ($, on) => {
  const w = world({ removal: { result: { removed: ['x'], freed_bytes: 1, failed: [] }, said: 'skein: removed 1.' } })
  await started(on, w, $)

  await $.command.run(TYPED('clean /home/agent/.cache/big dir'))

  expect(w.ran.find(a => a[2] === 'clean')?.slice(2)).toEqual(['clean', '/home/agent/.cache/big dir'])
})

test('what the pane used to show points at the cockpit, and /skein alone names the box', async ($, on) => {
  await started(on, world(), $)

  for (const sub of ['status', 'requests']) {
    expect((await $.command.run(TYPED(sub))).text).toBe('the cockpit shows this box. /skein clean lists what it can remove.')
  }
  expect((await $.command.run(TYPED(''))).text).toBe('version 9.8.7, in box web-main')
})

// What would make it fail: the duplicate of `skein_clean` registered again. `tool.call` on a tool
// no hook answers falls to the engine, so the registration itself is what is read.
test('the module registers no clean tool of its own: skein_clean is the one', async ($, on) => {
  const registered: string[] = []
  on('tool.register', (_, e) => {
    registered.push(e.name)

    return { value: { tool: `mcp__skein__${e.name}` } }
  })
  mock.clock(on, { now: NOW })
  mock.env(on, { SKEIN_BOX: 'web-main', SKEIN_STATE: STATE })
  on('command.register', (_, e) => ({ value: { command: e.name } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('fs.read', () => ({ value: '{}' }))
  on('process.run', () => out('', 127))
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: false })

  expect(registered).toContain('remember_rule')
  expect(registered).not.toContain('clean')
})
