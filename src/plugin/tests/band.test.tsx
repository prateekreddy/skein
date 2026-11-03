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

/** A signal file as skein writes it, `ago` seconds before `now`, with a disk ask or none; `over`
 * replaces fields of the ask, and `more` fields of the file. */
function signal(ago: number, ask: boolean, over: Record<string, unknown> = {}, more: Record<string, unknown> = {}, now = NOW): string {
  const at = stamp(now - ago * 1000)

  return JSON.stringify({
    at,
    disk: { mb: 11468, limit_mb: 10240, fleet: 'under', fleet_over_by_mb: 0 },
    ...more,
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

const stamp = (ms: number) => new Date(ms).toISOString().replace(/\.\d+Z$/, 'Z')

/** The box's vitals as the signal file writes them, read `ago` seconds before `now`: 2.7 of 10 GB
 * of disk, 1.1 of 14 GB of memory, and `cores` of CPU. */
function vitals(cores: number | null, now = NOW, memAgo = 0): Record<string, unknown> {
  return {
    disk: { mb: 2765, limit_mb: 10240, fleet: 'under', fleet_over_by_mb: 0 },
    memory: { anon: 1.1 * G, cache: 0.2 * G, high: 14 * G, max: 16 * G, read_at: stamp(now - memAgo * 1000) },
    cpu: cores === null ? {} : { cores, usage_usec: 1 },
  }
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
  /** The box's environment; a skein box's when absent. */
  env?: Record<string, string>
  signal: string
  /** `signals/usage.json`; none when absent. */
  usage?: string
  mail: string
  waiting: number
  /** Questions to the person, as `skein-mcp requests` lists them. */
  questions?: readonly Record<string, unknown>[]
  /** `signals/jobs.json`; none when absent. */
  jobs?: string
  /** What `skein-mcp lane list` prints; it fails when absent. */
  lanes?: string
  /** What a `skein-mcp clean <paths>` run prints. */
  removal?: { result: Record<string, unknown>; said: string }
  /** Every argv the plugin ran, in order. */
  ran: string[][]
  opened: string[]
  /** Every toast raised, in order. */
  toasts: string[]
}

function world(over: Partial<World> = {}): World {
  return { signal: signal(30, false), mail: '0', waiting: 0, ran: [], opened: [], toasts: [], ...over }
}

const out = (stdout: string, exitCode = 0) => ({
  value: { exitCode, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false },
})

function engine(on: On, w: World) {
  const clock = mock.clock(on, { now: NOW })
  mock.env(on, w.env ?? { SKEIN_BOX: 'web-main', SKEIN_STATE: STATE })
  on('command.register', (_, e) => ({ value: { command: e.name } }))
  on('ui.toast', (_, e) => {
    w.toasts.push(e.text)

    return { value: undefined }
  })
  on('ui.panes', () => ({ value: [] }))
  on('agent.spawn', (_, e) => ({ model: 'claude-opus-5-5', agentId: `agent-${e.tool_use_id}` }))
  on('tool.call', () => ({ result: {}, text: '' }))
  on('tool.register', (_, e) => ({ value: { tool: `mcp__skein__${e.name}` } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('ui.open', (_, e) => {
    w.opened.push(e.id)

    return { value: { isPlaced: true } }
  })
  on('fs.read', (_, e) => {
    if (e.path === `${STATE}/signals/resources.json`) return { value: w.signal }
    if (e.path === `${STATE}/signals/usage.json` && w.usage !== undefined) return { value: w.usage }
    if (e.path === `${STATE}/signals/jobs.json` && w.jobs !== undefined) return { value: w.jobs }
    if (e.path.endsWith('/.claude-plugin/plugin.json')) return { value: '{"version":"9.8.7"}' }
    throw new Error(`no such file: ${e.path}`)
  })
  on('process.run', (_, e) => {
    const argv = [...e.argv]
    w.ran.push(argv)
    if (argv[1]?.endsWith('/probe/mailbox.sh') && argv[2] === 'unread') return out(`${w.mail}\n`)
    if (argv[1]?.endsWith('/bin/skein-mcp') && argv[2] === 'requests') return out(JSON.stringify(requests(w.waiting, w.questions)))
    if (argv[1]?.endsWith('/bin/skein-mcp') && argv[2] === 'lane' && w.lanes !== undefined) return out(w.lanes)
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

// What would make it fail: the band drawn from a session that is not in a skein box, where there
// is no signal of skein's to draw. Watched: with the `SKEIN_STATE` check taken out of the render
// hook, the band drew its own lines here and this failed.
test('outside a skein box the band is the engine\'s', async ($, on) => {
  // What the engine draws when the plugin passes the band on.
  on('ui.render', ($, e) => {
    const { Text } = $.ui.resolve(e)

    return <Text>engine</Text>
  })
  await started(on, world({ env: {} }), $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect((await ui.find({ type: 'Text' }))?.text).toBe('engine')
    expect(await ui.find({ type: 'Button' })).toBe(undefined)
    await ui.unmount()
  }
})

// What would make it fail: the disk ask drawn as something that waits on the person, amber with a
// button, as it was before SKEIN-1460. Watched: with `asked` pushing the disk ask again (amber,
// `Free disk`), the disk line's colour and the buttons failed.
test('the band says the disk ask in one dim line with nothing to press, and counts requests and mail', async ($, on) => {
  const w = world({ signal: signal(30, true), waiting: 2, mail: '3' })
  await started(on, w, $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    const disk = await ui.findAll({ type: 'Text', text: /skein asks this box/ })
    expect(disk.map(t => t.text.trim())).toEqual([
      'skein asks this box to free 1.2G · the agent clears it · /skein clean to do it yourself',
    ])
    expect(disk[0]?.props.dimColor).toBe(true)
    expect(disk[0]?.props.color).toBe(undefined)
    expect(disk[0]?.props.bold).toBe(undefined)
    expect((await ui.find({ type: 'Text', text: /waiting/ }))?.text.trim()).toBe(
      '2 requests are waiting for an answer.',
    )
    expect((await ui.find({ type: 'Text', text: /unread/ }))?.text.trim()).toBe(
      "✉ 3 unread messages, read at the agent's next turn.",
    )
    // The only button is the requests'; the band has none for the disk.
    expect((await ui.findAll({ type: 'Button' })).map(b => b.props.label)).toEqual(['Requests'])
    await ui.unmount()
  }

  // Nothing of the band's line for it says to clean when none of it can be cleared now.
  w.signal = signal(30, true, { amount: 0, live_mb: 3072 })
  await $.command.run(TYPED('status'))
  const none = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await none.find({ type: 'Text', text: /skein asks this box/ }))?.text.trim()).toBe(
    'skein asks this box to free disk, but none of it can be cleared now · /skein clean lists it',
  )
  await none.unmount()
})

// What would make it fail: askLine leaving out `live_mb` (the first line reads as today's), or
// saying "free 0M" when nothing can be cleared. Watched: with the `live` part of the line dropped,
// the first expectation failed.
const LIVE_CASES: Array<[string, Record<string, unknown>, string]> = [
  [
    'part of the box is in use',
    { amount: 1228, live_mb: 3072 },
    'skein asks this box to free 1.2G of disk: it is over its own allowance (3.0G more in use now).',
  ],
  [
    'all it could clear is in use',
    { amount: 0, live_mb: 3072 },
    'skein asks this box to free disk, but none of it can be cleared now: it is over its own allowance (3.0G in use now).',
  ],
  [
    'nothing skein lists can be cleared',
    { amount: 0 },
    'skein asks this box to free disk, but none of it can be cleared now: it is over its own allowance.',
  ],
]

// The band says the ask in a few dim words (SKEIN-1460); `/skein status` and `/skein clean` say all of it.
for (const [when, over, line] of LIVE_CASES) {
  test(`/skein status says what skein asks when ${when}`, async ($, on) => {
    await started(on, world({ signal: signal(30, true, over) }), $)
    const disk = ((await $.command.run(TYPED('status'))).text ?? '').split('\n')[1] ?? ''
    expect(disk.slice(disk.indexOf('skein asks'))).toBe(line)
  })
}

// What would make it fail: a signal file believed whatever its age, so a ten-minute-old ask or
// figure is drawn as now. Watched: with the age check in `readResources` removed, the ask's line and
// the disk figure were drawn and this failed.
test('a signal file skein stopped writing asks nothing, and the vitals line says nothing is measured', async ($, on) => {
  // Ten minutes old: skein is not looking, as the command hooks read it.
  await started(on, world({ signal: signal(600, true, {}, vitals(0.5)) }), $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect((await ui.findAll({ type: 'Text' })).map(t => t.text)).toEqual(['skein  not measured lately'])
    expect(await ui.find({ type: 'Button' })).toBe(undefined)
    await ui.unmount()
  }
})

// ------------------------------------------------------------------------------------------------
// The pane
// ------------------------------------------------------------------------------------------------

test('the pane offers Clean on what can go now, greys what cannot, and keeps the shared cache apart', async ($, on) => {
  await started(on, world({ signal: signal(30, true) }), $)
  // The list is drawn in `/skein clean`'s view only (SKEIN-1460).
  await $.command.run(TYPED('clean'))

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
  await $.command.run(TYPED('clean'))

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

// What would make it fail: a row out of line with the others, what needs the person not marked or
// not first, or a row drawn for figures skein did not write (no jobs view and no lanes here, so no
// row for either). Watched: with the loud rows no longer sorted first, the requests row came after
// the disk row in the second case and this failed.
test('/skein status draws aligned rows, what needs the person first', async ($, on) => {
  const w = world({ signal: signal(30, true), waiting: 1, mail: '2' })
  await started(on, w, $)

  expect((await $.command.run(TYPED('status'))).text).toBe(
    [
      'web-main · 12:00 UTC',
      '⚑ disk      11.2 GB / 10 GB   skein asks this box to free 1.2G of disk: it is over its own allowance.',
      '⚑ requests  1 waiting for the owner   /skein requests lists them',
      '  mail      2 unread   the agent reads them at its next turn',
      '  spend     not written in the last 70 minutes',
      '/skein clean · /skein requests',
    ].join('\n'),
  )

  // With no ask, the disk is a quiet row, and the request that waits goes above it.
  w.signal = signal(30, false)
  w.mail = '0'
  expect((await $.command.run(TYPED('status'))).text?.split('\n').slice(1, 4)).toEqual([
    '⚑ requests  1 waiting for the owner   /skein requests lists them',
    '  disk      11.2 GB / 10 GB   3.69 GB removable',
    '  mail      none unread',
  ])
})

// Fails if /skein status stops saying the dollars are an estimate at API prices, or if `+` is not
// added where something had no price, in the band or in /skein status. Watched: with the `+` taken
// out of `vitalsLine`, the band's second line failed.
test('the band and /skein status draw the spend, with + when something had no price', async ($, on) => {
  const w = world({ usage: usage(60, 1.2, 31.084) })
  await started(on, w, $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    const shown = await ui.find({ type: 'Text', text: /today/ })
    expect(shown?.text).toBe('skein  disk ▮▮▮▮▮▮▮▮▮▮ 11.2/10G  ·  $1.20 today · $31 7d')
    expect(shown?.props.dimColor).toBe(true)
    await ui.unmount()
  }
  expect((await $.command.run(TYPED('status'))).text?.split('\n').at(-2)).toBe(
    '  spend     ≈$1.20 today · ≈$31.08 this week   estimated at API prices as of 11:59 UTC',
  )

  w.usage = usage(60, 1.2, 140.5, true)
  const status = (await $.command.run(TYPED('status'))).text?.split('\n') ?? []
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await ui.find({ type: 'Text', text: /today/ }))?.text).toBe('skein  disk ▮▮▮▮▮▮▮▮▮▮ 11.2/10G  ·  $1.20+ today · $141+ 7d')
  await ui.unmount()
  expect(status.at(-2)).toBe(
    '  spend     ≈$1.20+ today · ≈$141+ this week   estimated at API prices as of 11:59 UTC; + leaves out a model skein has no price for',
  )
})

// skein reads the fleet's usage once an hour, and the band trusts the file for 70 minutes. Fails if
// the file is believed whatever its age, or for the resource signal's five minutes. Watched: with
// `readSpend` judging age by `STALE_AFTER_MS`, the 69-minute figure was not shown and this failed;
// with the age check removed, the 71-minute figure was shown and this failed.
test('spend is shown for 70 minutes after skein read it, and not after', async ($, on) => {
  const w = world({ usage: usage(69 * 60, 1.2, 8.4) })
  await started(on, w, $)
  expect((await $.command.run(TYPED('status'))).text?.split('\n').at(-2)).toBe(
    '  spend     ≈$1.20 today · ≈$8.40 this week   estimated at API prices as of 10:51 UTC',
  )

  w.usage = usage(71 * 60, 1.2, 8.4)
  await $.command.run(TYPED('status'))

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect((await ui.find({ type: 'Text' }))?.text).toBe('skein  disk ▮▮▮▮▮▮▮▮▮▮ 11.2/10G')
    expect(await ui.find({ type: 'Text', text: /today/ })).toBe(undefined)
    await ui.unmount()
  }
  expect((await $.command.run(TYPED('status'))).text?.split('\n').at(-2)).toBe('  spend     not written in the last 70 minutes')
})

/** The box's jobs view (`View` in `src/jobs.rs`), `ago` seconds old: one job running, two waiting. */
function jobsView(ago: number): string {
  const at = new Date(NOW - ago * 1000).toISOString().replace(/\.\d+Z$/, 'Z')
  const job = (id: string, state: string, position: number | null) => ({
    id, class: 'test', command: 'cargo test', cwd: '/w', state, cores: 4, min: 0,
    granted_cores: state === 'granted' ? 4 : 0, position, reason: '', waited_secs: 5,
  })

  return JSON.stringify({
    schema: 1, at, cores: 12, granted: 8, free: 4, running: 2,
    jobs: [job('j-1', 'granted', null), job('j-2', 'waiting', 5), job('j-3', 'waiting', 3), job('j-4', 'done', null)],
    ahead: [],
  })
}

/** What `skein-mcp lane list` prints for two lanes skein made. */
const LANES = JSON.stringify({
  result: {
    root: '/w/.worktrees',
    lanes: [
      { name: 'ledger', worktree: '/w/.worktrees/ledger', branch: 'fix/ledger', item: 'SKEIN-1', phase: 'created' },
      { name: 'spend', worktree: '/w/.worktrees/spend', branch: 'fix/spend', item: 'SKEIN-2', phase: 'created' },
    ],
    refused: [],
  },
  said: '',
})

// What would make it fail: the jobs row counting another box's jobs or a finished one, its place
// read from anything but the view, a view skein stopped writing believed, or a lanes row in a box
// with none. Watched: with the staleness check in `readJobs` removed, the 40-second-old view still
// drew a jobs row and this failed.
test('/skein status counts the jobs from the jobs view and the lanes skein made, and leaves out what is not written', async ($, on) => {
  const w = world({ jobs: jobsView(5), lanes: LANES })
  await started(on, w, $)

  const rows = (await $.command.run(TYPED('status'))).text?.split('\n') ?? []
  expect(rows.filter(r => /^ {2}(jobs|lanes) /.test(r))).toEqual([
    "  jobs      1 running · 2 waiting, 3rd in the fleet's queue",
    '  lanes     0 running · 2 in all',
  ])

  // A view 40 s old is skein not scheduling, and a box with no lanes has no lanes row.
  w.jobs = jobsView(40)
  w.lanes = undefined
  const later = (await $.command.run(TYPED('status'))).text?.split('\n') ?? []
  expect(later.filter(r => /^ {2}(jobs|lanes) /.test(r))).toEqual([])
})

const STATUS_ROW = (text: string, args = 'status') =>
  ({
    plugin: 'skein',
    component: 'CommandOutput',
    requestId: 'm1',
    props: { command: 'skein', args, text, isErrored: false },
  }) as const

// What would make it fail: the rows drawn without the amber on what needs the person, or a text of
// another shape drawn as rows. Watched: with `isLoud` read from the wrong prefix, the disk row was
// drawn plain and this failed.
test('the drawn /skein status puts what needs the person in amber, and any other text is the engine\'s', async ($, on) => {
  on('ui.render', ($, e) => {
    const { Text } = $.ui.resolve(e)

    return <Text>engine</Text>
  })
  await started(on, world({ signal: signal(30, true), mail: '2' }), $)
  const text = (await $.command.run(TYPED('status'))).text ?? ''

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...STATUS_ROW(text), surface })
    const loud = (await ui.findAll({ type: 'Text', text: /^⚑ disk/ })).at(-1)
    expect(loud?.props.color).toBe('warning')
    const quiet = (await ui.findAll({ type: 'Text', text: /^ {2}mail/ })).at(-1)
    expect(quiet?.props.color).toBe(undefined)
    expect((await ui.findAll({ type: 'Text', text: /^ {3}the agent reads them/ })).at(-1)?.props.dimColor).toBe(true)
    await ui.unmount()

    for (const other of [STATUS_ROW('this box has asked for nothing.', 'requests'), STATUS_ROW('one line only')]) {
      const plain = await $.ui.mount({ ...other, surface })
      expect((await plain.find({ type: 'Text' }))?.text).toBe('engine')
      await plain.unmount()
    }
  }
})

test('/skein requests lists each request with its state', async ($, on) => {
  await started(on, world({ waiting: 1 }), $)

  const ran = await $.command.run(TYPED('requests'))

  expect(ran.text).toBe(['this box asked for:', '  write acme/site: granted', '  package apt jq: waiting'].join('\n'))
})

// What would make it fail: the pane not opening as the session starts, or `/skein` alone not
// opening it (SKEIN-1440). Watched: with identity.ts's open taken out, the last expectation failed.
test('/skein clean opens the list, and /skein alone still names the box', async ($, on) => {
  const w = world()
  await started(on, w, $)
  // The pane opened by itself as the session started (SKEIN-1440).
  expect(w.opened).toEqual(['skein'])

  const clean = await $.command.run(TYPED('clean'))
  expect(clean.text).toBe('the list of what this box can remove is open.')
  expect(w.opened).toEqual(['skein', 'skein'])

  // `/skein` alone opens the pane too, and its answer is unchanged.
  expect((await $.command.run(TYPED(''))).text).toBe('version 9.8.7, in box web-main')
  expect(w.opened).toEqual(['skein', 'skein', 'skein'])
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
    '⚑ disk      11.2 GB / 10 GB   skein asks this box to free 1.2G of disk: it is over its own allowance.',
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

// What would make it fail: mail drawn in amber as if it waited on the person, or toasted.
test('unread mail is said in the band, dim, with no button to press and no toast', async ($, on) => {
  const w = world({ mail: '2' })
  await started(on, w, $)
  expect(w.toasts).toEqual([])

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect((await ui.find({ type: 'Text', text: /unread/ }))?.text.trim()).toBe(
      "✉ 2 unread messages, read at the agent's next turn.",
    )
    // Mail is the agent's to read: dim, never amber, never toasted.
    expect((await ui.find({ type: 'Text', text: /unread/ }))?.props.color).toBe(undefined)
    expect((await ui.find({ type: 'Text', text: /unread/ }))?.props.dimColor).toBe(true)
    expect(await ui.find({ type: 'Text', text: /⚑/ })).toBe(undefined)
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
    // 100 cells less the `⚑ `, "Waiting for your answer: ", " And 1 more question." and 11 for
    // the quotes and the button: 41 cells for the question and its `…`, cut after a whole word.
    expect((await ui.find({ type: 'Text', text: /Waiting for your answer/ }))?.text.trim()).toBe(
      'Waiting for your answer: “Should I drop the legacy importer now…” And 1 more question.',
    )
    expect(await ui.find({ type: 'Button', key: 'band:question' })).not.toBe(undefined)
    // Questions are not counted again among the requests.
    expect(await ui.find({ type: 'Text', text: /requests? (is|are) waiting/ })).toBe(undefined)
    await ui.unmount()

    // The pane keeps every question whole, worded as what to do, wrapped in its panel.
    const pane = await $.ui.mount({ ...PANE, surface })
    const waiting = (await pane.findAll({ type: 'Box' }))
      .filter(b => /^row:waiting:\d+$/.test(b.key ?? ''))
      .map(b => b.text.replace(/^│ /, '').replace(/\s*│$/, '').trim())
      .join(' ')
    expect(waiting).toContain(`⚑ Answer in the cockpit: “${LONG.replace(/\s+/g, ' ')}”`)
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

// ------------------------------------------------------------------------------------------------
// The band's three states: calm, busy, and waiting on you (SKEIN-1440)
// ------------------------------------------------------------------------------------------------

/** The band's lines as drawn, top to bottom: each row's text, a button's `[ label ]` left out. */
async function bandLines(ui: { find: (q: { type: string }) => Promise<{ type: string; children: unknown[] } | undefined> }): Promise<string[]> {
  const root = await ui.find({ type: 'Box' })
  if (root === undefined) {
    const only = await ui.find({ type: 'Text' })
    return only === undefined ? [] : [shown(only).trimEnd()]
  }

  return root.children.filter(c => typeof c !== 'string').map(c => shown(c).trimEnd())
}

/** What an element shows, a Button left out. */
function shown(node: unknown): string {
  if (typeof node === 'string') return node
  if (typeof node !== 'object' || node === null) return ''
  const el = node as { type?: string; children?: unknown; props?: { children?: unknown } }
  if (el.type === 'Button') return ''
  const kids = el.children ?? el.props?.children
  if (Array.isArray(kids)) return kids.map(shown).join('')

  return shown(kids)
}

// The CPU a minute at a time, for eight minutes: ▁▂▂▃▂▁▁▂ against one core, the least the line
// scales to.
const CPU = [0, 0.15, 0.15, 0.3, 0.15, 0, 0, 0.15]

/** A session eight minutes in, one look a minute, the signal file rewritten before each. */
async function eightMinutes(on: On, w: World, $: Engine, more: (minute: number) => Partial<World> = () => ({})) {
  const clock = engine(on, w)
  for (const [i, cores] of CPU.entries()) {
    const now = NOW + i * 60_000
    Object.assign(w, { signal: signal(10, false, {}, vitals(cores, now), now), ...more(i) })
    if (i === 0) {
      await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
      await clock.settle()
    } else {
      await clock.advance(60_000)
    }
  }

  return clock
}

const CALM = 'skein  disk ▮▮▮▯▯▯▯▯▯▯ 2.7/10G  mem ▮▯▯▯▯ 1.1/14G  cpu ▁▂▂▃▂▁▁▂  ·  $31 today · $515 7d'

// What would make it fail: the vitals line hidden while nothing happens (the owner's decision is
// that it always shows), a bar or figure from the wrong field, or the sparkline not kept a minute
// at a time. Watched: with `sampled` keeping only the latest minute, the sparkline was one cell
// and this failed.
test('calm: one dim line of vitals, with an eight-minute CPU sparkline', async ($, on) => {
  const w = world({ usage: usage(60, 31.2, 515.4) })
  await eightMinutes(on, w, $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect(await bandLines(ui)).toEqual([CALM])
    expect((await ui.find({ type: 'Text', text: /^skein {2}disk/ }))?.props.dimColor).toBe(true)
    expect(await ui.find({ type: 'Button' })).toBe(undefined)
    await ui.unmount()
  }
  expect(w.toasts).toEqual([])
})

/** A jobs view `ago` seconds old: a test in lane ledger running, and two clippy runs waiting, the
 * first behind another box's job and another repo's. */
function busyJobs(ago: number): string {
  const job = (id: string, command: string, cwd: string, state: string, position: number | null, reason = '') => ({
    id, class: 'test', command, cwd, state, cores: 4, min: 0,
    granted_cores: state === 'granted' ? 4 : 0, position, reason, waited_secs: 5,
  })

  return JSON.stringify({
    schema: 1, at: stamp(NOW + 7 * 60_000 - ago * 1000), cores: 12, granted: 8, free: 0, running: 2,
    jobs: [
      job('j-1', 'cargo test --test fleet_launch', '/w/.worktrees/ledger/src', 'granted', null),
      job('j-2', 'cargo clippy --all-targets', '/w', 'waiting', 2, 'waiting for 4 cores (0 free)'),
      job('j-3', 'cargo clippy', '/w', 'waiting', 4),
      job('j-4', 'cargo build', '/w', 'done', null),
    ],
    ahead: [{ box: 'api-main', id: 'x-1', class: 'test', command: 'cargo test', cores: 4 }],
  })
}

/** Two lanes at work: ledger, whose job is in the view, and spend, waiting at the heavy-work lock. */
async function twoLanesAtWork($: Engine) {
  for (const [id, name] of [['u1', 'ledger'], ['u2', 'spend']] as const) {
    await $.agent.spawn({
      tool_use_id: id,
      prompt: `Worktree: /w/.worktrees/${name}, on its branch.`,
      description: `fix the ${name}`,
      subagentType: 'general-purpose',
      provider: { plugin: 'engine', tier: 'core' },
      parentModel: 'claude-opus-5-5',
      background: true,
      fork: false,
    })
  }
}

// What would make it fail: a finished job drawn, a job's lane not named, the first waiting job not
// naming the other box it waits behind, a later one not counting what goes before it, a lane drawn
// twice (once for its job, once for itself), or a lane with no job left out. Watched: with the
// `busy` set in `workLines` not consulted, `lane ledger` had a line of its own and this failed;
// with `ahead` read for every waiting job, the second clippy line named api-main and this failed.
test('busy: a line for each running job and each lane at work, while it runs', async ($, on) => {
  const w = world({ lanes: LANES, usage: usage(60, 31.2, 515.4) })
  const clock = await eightMinutes(on, w, $, i => (i === 7 ? { jobs: busyJobs(5) } : {}))
  await twoLanesAtWork($)
  await clock.advance(10_000)

  const ui = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, bodyColumns: 120 } })
  expect(await bandLines(ui)).toEqual([
    CALM,
    '● cargo test --test fleet_launch · lane ledger · 4 cores',
    "◌ cargo clippy --all-targets · #2 in the fleet's queue · behind api-main cargo test · waiting for 4 cores (0 free)",
    "◌ cargo clippy · #4 in the fleet's queue · 3 ahead",
    '● lane spend · SKEIN-2 · fix the spend',
  ])
  await ui.unmount()

  // The view goes stale: skein is not scheduling, and no job is drawn as running. The lanes stay.
  w.jobs = busyJobs(45)
  await clock.advance(10_000)
  const later = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, bodyColumns: 120 } })
  expect(await bandLines(later)).toEqual([CALM, '● lane ledger · SKEIN-1 · fix the ledger', '● lane spend · SKEIN-2 · fix the spend'])
  await later.unmount()
})

// What would make it fail: what waits on the person drawn above the vitals, not in amber, or not
// worded as what to do; a toast missing for one of them, raised again on the next look, or not
// raised for one that comes later; or the disk ask, which is the agent's, among them. Watched: with
// `raiseToasts` toasting whatever it toasted before, the second look raised both again and this
// failed; with the update line left out of `asked`, the rows failed.
test('waiting on you: amber lines under the vitals, each worded as the action, and one toast per new thing', async ($, on) => {
  const updates = [{ component: 'agent-cli', have: 'claude@2.1.300', new: 'claude@2.1.316', applies: 'restart', restart_requested: false }]
  const w = world({
    signal: signal(30, true, {}, { ...vitals(0.15), updates }),
    usage: usage(60, 31.2, 515.4),
    questions: [question('q1', '2026-10-30T08:00:00Z', 'Keep the old table?')],
  })
  const clock = await started(on, w, $)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...BAND, surface, props: { ...BAND.props, bodyColumns: 120 } })
    const lines = await bandLines(ui)
    expect(lines).toEqual([
      'skein  disk ▮▮▮▯▯▯▯▯▯▯ 2.7/10G  mem ▮▯▯▯▯ 1.1/14G  cpu ▂  ·  $31 today · $515 7d',
      '⚑ Waiting for your answer: “Keep the old table?”',
      '⚑ A restart gives this box claude@2.1.316. Ask the agent to restart when idle, or skein does once the box is idle.',
      'skein asks this box to free 1.2G · the agent clears it · /skein clean to do it yourself',
    ])
    for (const text of [/Waiting for your answer/, /A restart gives/]) {
      expect((await ui.find({ type: 'Text', text }))?.props.color).toBe('warning')
    }
    expect((await ui.find({ type: 'Text', text: /skein asks/ }))?.props.dimColor).toBe(true)
    // The band has no key that cleans, and none for the disk.
    expect((await ui.findAll({ type: 'Button' })).map(b => b.props.label)).toEqual(['Read'])
    await ui.unmount()
  }
  expect(w.toasts).toEqual([
    'Waiting for your answer: “Keep the old table?”',
    'A restart gives this box claude@2.1.316. Ask the agent to restart when idle, or skein does once the box is idle.',
  ])

  // The next look raises nothing again; a new question raises one toast, for it alone.
  await clock.advance(60_000)
  expect(w.toasts.length).toBe(2)
  w.questions = [...(w.questions ?? []), question('q2', '2026-10-30T11:00:00Z', 'Merge now?')]
  await clock.advance(60_000)
  expect(w.toasts.slice(2)).toEqual(['Waiting for your answer: “Merge now?”'])

  // A restart the box already asked for waits on nobody.
  w.signal = signal(30, true, {}, { ...vitals(0.15), updates: [{ ...updates[0], restart_requested: true }] })
  await clock.advance(60_000)
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, bodyColumns: 120 } })
  expect(await ui.find({ type: 'Text', text: /A restart gives/ })).toBe(undefined)
  await ui.unmount()
})

// What would make it fail: the disk ask counted again among what waits on the person, so a new ask
// toasts and opens the pane (SKEIN-1460); or a request for more allowance, which the person does
// answer, not counted, or read as a package request. Watched: with the disk ask put back first in
// `waitingThings`, the toasts and the opens failed; with `readRequests` reading the `disk` queue as
// `package`, the toast said "A package request waits for an answer: " and this failed.
test("a disk ask is the agent's: no toast, no pane opened; a request for more allowance waits on the person", async ($, on) => {
  const w = world({ signal: signal(30, false, {}, vitals(0.15)) })
  const clock = await started(on, w, $)
  // The first look opens the pane, as it does for every session; the person then closes it.
  expect(w.opened).toEqual(['skein'])

  w.signal = signal(30, true, {}, vitals(0.15), NOW + 60_000)
  await clock.advance(60_000)
  expect(w.toasts).toEqual([])
  expect(w.opened).toEqual(['skein'])
  const quiet = await $.ui.mount({ ...BAND, surface: 'terminal' })
  // The ask was read: its dim line is there, and nothing of it is amber.
  expect(await quiet.find({ type: 'Text', text: /the agent clears it/ })).not.toBe(undefined)
  expect((await quiet.findAll({ type: 'Text' })).filter(t => t.props.color === 'warning')).toEqual([])
  await quiet.unmount()

  w.questions = [{ queue: 'disk', id: 'd-1', asked: '2026-10-30T11:30:00Z', size: '20g', why: 'the release build needs room', state: 'waiting' }]
  w.signal = signal(30, true, {}, vitals(0.15), NOW + 120_000)
  await clock.advance(60_000)
  expect(w.toasts).toEqual(['A disk request waits for an answer: 20g (the release build needs room)'])
  expect(w.opened).toEqual(['skein', 'skein'])
  const asked = await $.ui.mount({ ...BAND, surface: 'terminal' })
  const line = await asked.find({ type: 'Text', text: /request is waiting/ })
  expect(line?.text.trim()).toBe('1 request is waiting for an answer.')
  expect(line?.props.color).toBe('warning')
  expect((await asked.findAll({ type: 'Button' })).map(b => b.props.label)).toEqual(['Requests'])
  await asked.unmount()
  expect((await $.command.run(TYPED('requests'))).text).toBe(
    ['this box asked for:', '  write acme/site: granted', '  disk 20g (the release build needs room): waiting'].join('\n'),
  )
})

// What would make it fail: a note for the agent read as something asked of the person, the way
// the disk ask is: `readResources` taking its ask from `notes` as well as `asks`, or listing a
// note's kind among what skein asks, would draw an amber line, a "Free disk" button, a toast, and
// "skein asks about disk-near" in the pane (SKEIN-1455).
test('a note for the agent is not drawn for the person: no amber line, no button, no toast', async ($, on) => {
  const NOTE = "skein: this box holds 8.6G of its 10.0G share of the fleet's disk; 5.0G of it can be removed now, before it goes over."
  const notes = [{ kind: 'disk-near', since: stamp(NOW - 30_000), crossing_id: 'disk-near-1', text: NOTE }]
  const near = { ...vitals(0.15), disk: { mb: 8806, limit_mb: 10240, fleet: 'under', fleet_over_by_mb: 0 }, notes }
  const w = world({ signal: signal(30, false, {}, near), usage: usage(60, 31.2, 515.4) })
  const clock = await started(on, w, $)
  await clock.advance(60_000)

  for (const surface of SURFACES) {
    const band = await $.ui.mount({ ...BAND, surface, props: { ...BAND.props, bodyColumns: 120 } })
    const lines = await bandLines(band)
    expect(lines.length).toBe(1)
    expect(lines.filter(l => l.startsWith('⚑'))).toEqual([])
    expect((await band.findAll({ type: 'Text' })).filter(t => t.props.color === 'warning')).toEqual([])
    expect(await band.find({ type: 'Button' })).toBe(undefined)
    await band.unmount()

    const pane = await $.ui.mount({ ...PANE, surface })
    expect((await pane.findAll({ type: 'Text' })).filter(t => t.props.color === 'warning')).toEqual([])
    expect(await pane.find({ type: 'Text', text: /nothing asked/ })).not.toBe(undefined)
    expect(await pane.find({ type: 'Text', text: /removed now, before it goes over/ })).toBe(undefined)
    await pane.unmount()
  }
  const status = (await $.command.run(TYPED('status'))).text ?? ''
  expect(status).not.toContain('⚑')
  expect(status).not.toContain('before it goes over')
  expect(w.toasts).toEqual([])
})

// ------------------------------------------------------------------------------------------------
// Each figure only while its signal is fresh
// ------------------------------------------------------------------------------------------------

// What would make it fail: a part drawn from a reading skein did not make fresh, which is a figure
// invented. Watched: with the `read_at` check in `readResources` removed, the memory part was drawn
// from a ten-minute-old reading and the first case failed; with the CPU drawn from the history when
// the reading now has none, the second failed.
const STALE_CASES: Array<[string, Record<string, unknown>, string]> = [
  ['memory read ten minutes ago', vitals(0.15, NOW, 600), 'skein  disk ▮▮▮▯▯▯▯▯▯▯ 2.7/10G  cpu ▂'],
  ['no CPU reading', vitals(null), 'skein  disk ▮▮▮▯▯▯▯▯▯▯ 2.7/10G  mem ▮▯▯▯▯ 1.1/14G'],
  [
    'no disk figure',
    { ...vitals(0.15), disk: { mb: null, limit_mb: null, fleet: 'unknown', fleet_over_by_mb: 0 } },
    'skein  mem ▮▯▯▯▯ 1.1/14G  cpu ▂',
  ],
  ['no allowance', { ...vitals(0.15), disk: { mb: 2765, limit_mb: null } }, 'skein  disk 2.7G  mem ▮▯▯▯▯ 1.1/14G  cpu ▂'],
]

for (const [when, more, line] of STALE_CASES) {
  test(`the vitals line leaves a part out when it has ${when}`, async ($, on) => {
    await started(on, world({ signal: signal(30, false, {}, more) }), $)
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    expect(await bandLines(ui)).toEqual([line])
    await ui.unmount()
  })
}

// What would make it fail: a minute with no fresh reading filled in, or the line's CPU drawn while
// the signal file is old. Watched: with `sparkline` drawing nothing for a minute with no reading,
// the gap closed up and this failed.
test('a minute skein wrote nothing fresh for is a gap in the sparkline, not a guess', async ($, on) => {
  const w = world()
  await eightMinutes(on, w, $, i => (i === 4 ? { signal: signal(600, false, {}, vitals(9, NOW + i * 60_000), NOW + i * 60_000) } : {}))
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await bandLines(ui)).toEqual(['skein  disk ▮▮▮▯▯▯▯▯▯▯ 2.7/10G  mem ▮▯▯▯▯ 1.1/14G  cpu ▁▂▂▃ ▁▁▂'])
  await ui.unmount()
})

// ------------------------------------------------------------------------------------------------
// The width and the rows it has
// ------------------------------------------------------------------------------------------------

// What would make it fail: a line wider than the band, which the terminal would wrap into a mess,
// or details cut before the bars. Watched: with `vitalsLine` returning its first try whatever its
// width, the 60-cell band's first line was 87 cells and this failed.
test('a narrow band cuts details, never wraps: bars first, then the week, then the CPU', async ($, on) => {
  const w = world({ lanes: LANES, usage: usage(60, 31.2, 515.4) })
  const clock = await eightMinutes(on, w, $, i => (i === 7 ? { jobs: busyJobs(5) } : {}))
  await twoLanesAtWork($)
  await clock.advance(10_000)

  const at = async (bodyColumns: number) => {
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, bodyColumns } })
    const lines = await bandLines(ui)
    await ui.unmount()

    return lines
  }
  const wide = await at(87)
  expect(wide[0]).toBe(CALM)
  const narrow = await at(60)
  expect(narrow).toEqual([
    'skein  disk 2.7/10G  mem 1.1/14G  cpu ▁▂▂▃▂▁▁▂  ·  $31 today',
    '● cargo test --test fleet_launch · lane ledger · 4 cores',
    "◌ cargo clippy · #2 in the fleet's queue · behind api-main",
    "◌ cargo clippy · #4 in the fleet's queue · 3 ahead",
    '● lane spend · SKEIN-2 · fix the spend',
  ])
  const thin = await at(32)
  expect(thin).toEqual([
    'skein  disk 2.7/10G',
    '● cargo test · lane ledger',
    '◌ cargo clippy · #2',
    '◌ cargo clippy · #4',
    '● lane spend · SKEIN-2',
  ])
  for (const line of [...narrow, ...thin.slice(0, 1)]) expect([...line].length).toBeLessThanOrEqual(60)
  for (const line of thin) expect([...line].length).toBeLessThanOrEqual(32)
})

// What would make it fail: a band taller than its rows, so the engine scrolls it and the thing that
// waits on the person may be out of view; or the disk ask, the agent's, taking the one line from the
// vitals. Watched: with the row count not compared to `maxRows`, the band drew three rows into two
// and this failed; with the disk ask put back first in `asked`, the first line was the ask's.
test('where the band has too few rows, it is one line: what waits on the person first', async ($, on) => {
  const w = world({ signal: signal(30, true, {}, vitals(0.15)), mail: '2' })
  await started(on, w, $)
  // The disk ask and the mail are the agent's: the line is the vitals', with what is left out counted.
  const asked = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, maxRows: 2 } })
  const only = await asked.findAll({ type: 'Text' })
  expect(only.map(t => t.text)).toEqual(['skein  disk ▮▮▮▯▯▯▯▯▯▯ 2.7/10G  mem ▮▯▯▯▯ 1.1/14G  cpu ▂ · 2 more'])
  expect(only[0]?.props.dimColor).toBe(true)
  await asked.unmount()

  // A question does wait on the person, and takes the line.
  w.questions = [question('q1', '2026-10-30T08:00:00Z', 'Keep the old table?')]
  await $.command.run(TYPED('status'))
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, maxRows: 2 } })
  expect((await ui.findAll({ type: 'Text' })).map(t => t.text).join('').trimEnd()).toBe(
    '⚑ Waiting for your answer: “Keep the old table?” · 3 more',
  )
  await ui.unmount()
  w.questions = []

  w.signal = signal(30, false, {}, vitals(0.15))
  w.mail = '0'
  w.jobs = busyJobs(5 + 7 * 60)
  await $.command.run(TYPED('status'))
  const calm = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, maxRows: 1 } })
  expect((await calm.findAll({ type: 'Text' })).map(t => t.text)).toEqual([
    'skein  disk ▮▮▮▯▯▯▯▯▯▯ 2.7/10G  mem ▮▯▯▯▯ 1.1/14G  cpu ▂ · 3 more',
  ])
  await calm.unmount()
})
