// The agents panel, through the whole module: `claude plugin test src/plugin` loads the plugin
// from its folder, so these go through register.tsx. Each `on` here stands for the engine; the
// box it reads (a repository, its worktrees, `/proc`) is made up below.

import { expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On, TurnUsage } from 'claude-code'

const SURFACES = ['terminal', 'desktop'] as const

const PANE = {
  plugin: 'skein',
  component: 'Pane',
  requestId: 'skein-agents',
  props: {
    title: 'Agents',
    isFocused: false,
    bodyColumns: 100,
    placement: 'dock',
    scroll: { offset: 0, bodyRows: 40 },
    view: {},
  },
} as const

const TYPED = {
  command: 'agents-info',
  args: '',
  origin: { kind: 'composer' },
  presentation: { isFullscreen: true, columns: 160 },
} as const

const LOCK = '/var/tmp/skein-lanes-heavy.lock'

/** A box: files by path, folders by path, links by path, and what a tool answers, by tool. */
type Box = {
  answers?: Record<string, string>
  files?: Record<string, string>
  dirs?: Record<string, string[]>
  links?: Record<string, string>
  env?: Record<string, string>
}

function engine(on: On, box: Box = {}): { opened: string[]; panes: Set<string> } {
  const opened: string[] = []
  const panes = new Set<string>()
  // `/skein` reads the box's name as the session starts; this file is not about it.
  mock.env(on, box.env ?? {})
  on('command.register', (_, e) => ({ value: { command: e.name } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('ui.open', (_, e) => {
    opened.push(e.id)
    panes.add(e.id)

    return { value: { isPlaced: true } }
  })
  on('ui.close', (_, e) => {
    panes.delete(e.id)

    return { value: undefined }
  })
  on('ui.panes', () => ({
    value: [...panes].map(id => ({ id, title: id, isShown: true, isFocused: false, isPlaced: true })),
  }))
  on('fs.read', (_, e) => {
    const text = box.files?.[e.path]
    if (text === undefined) throw new Error(`ENOENT ${e.path}`)

    return { value: text }
  })
  on('fs.list', (_, e) => {
    const names = box.dirs?.[e.path ?? '']
    if (names === undefined) throw new Error(`ENOENT ${e.path}`)

    return { value: names.map(name => ({ name, kind: 'other' as const, size: 0, mtimeMs: 0, isLink: false })) }
  })
  on('fs.stat', (_, e) => {
    const realPath = box.links?.[e.path]
    if (realPath === undefined) throw new Error(`ENOENT ${e.path}`)

    return { value: { kind: 'other' as const, size: 0, mtimeMs: 0, isLink: true, realPath } }
  })
  on('agent.spawn', (_, e) => ({ model: e.model ?? 'claude-opus-5-5', agentId: `agent-${e.tool_use_id}` }))
  on('tool.call', (_, e) => ({ result: {}, text: box.answers?.[String(e.tool)] ?? '' }))

  return { opened, panes }
}

async function spawn($: Engine, id: string, prompt: string, model?: string): Promise<void> {
  await $.agent.spawn({
    tool_use_id: id,
    prompt,
    description: `task ${id}`,
    subagentType: 'general-purpose',
    provider: { plugin: 'engine', tier: 'core' },
    parentModel: 'claude-opus-5-5',
    background: true,
    fork: false,
    ...(model ? { model } : {}),
  })
}

function stepWith(on: On, usage: TurnUsage): void {
  on('turn.step', async function* (_, e) {
    return { turnId: e.turnId, index: e.index, answer: '', toolUses: [], stopReason: 'end_turn' as const, usage }
  })
}

async function step($: Engine, agentId: string, model: string): Promise<void> {
  const stream = $.turn.step({ turnId: 't1', index: 0, model, messageCount: 1, agentId })
  for await (const _ of stream) {
    // Nothing streams from the stand-in; the result is what counts.
  }
}

const USAGE = {
  input_tokens: 1000,
  output_tokens: 2000,
  cache_read_input_tokens: 100_000,
  cache_creation_input_tokens: 10_000,
}

const start = ($: Engine) =>
  $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })

test('a subagent is listed with its model, context, tokens, cost, time and status, on both surfaces', async ($, on) => {
  const clock = mock.clock(on, { now: 1_000_000 })
  engine(on)
  stepWith(on, { ...USAGE, model: 'claude-opus-5-5' })

  await start($)
  await spawn($, 'u1', 'look around')
  await step($, 'agent-u1', 'claude-opus-5-5')
  await clock.advance(65_000)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...PANE, surface })
    expect((await ui.find({ type: 'Text', text: /task u1/ }))?.text).toBe('task u1')
    expect((await ui.find({ type: 'Text', text: /general-purpose/ }))?.text).toBe(
      '  general-purpose · Opus 5.5 · running',
    )
    // 1,000 in at $4, 2,000 out at $20, 100,000 read at $0.20 and 10,000 written at $5 a million.
    expect((await ui.find({ type: 'Text', text: /ctx \d+%/ }))?.text).toMatch(
      /ctx 11% · 113k · 113k tokens · ≈\$0\.11 · 1:05$/,
    )
    expect((await ui.find({ type: 'Text', text: /estimate/ }))?.text).toBe(
      'Cost is an estimate: cache writes are priced at the 5-minute rate, so 1-hour cache writes read low.',
    )
    await ui.unmount()
  }
})

test('a model skein has no price for is said to have none, never priced as another', async ($, on) => {
  mock.clock(on, { now: 0 })
  engine(on)
  stepWith(on, { ...USAGE, model: 'claude-haiku-5-5' })

  await start($)
  await spawn($, 'u2', 'look around', 'claude-haiku-5-5')
  await step($, 'agent-u2', 'claude-haiku-5-5')

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...PANE, surface })
    expect((await ui.find({ type: 'Text', text: /ctx \d+%/ }))?.text).toMatch(/≈\$0\.00\+ ·/)
    expect((await ui.find({ type: 'Text', text: /no price/ }))?.text).toBe(
      '+ marks a model skein has no price for: the real cost is higher.',
    )
    await ui.unmount()
  }
})

test('the panel opens when a subagent starts, and stays shut once the person shut it', async ($, on) => {
  mock.clock(on, { now: 0 })
  const seen = engine(on)

  await start($)
  await spawn($, 'u3', 'first')
  expect(seen.opened).toEqual(['skein-agents'])

  const shut = await $.command.run(TYPED)
  expect(shut.text).toBe('Agents panel closed. It stays closed until you run /agents-info again.')
  await spawn($, 'u4', 'second')
  expect(seen.opened).toEqual(['skein-agents'])

  const opened = await $.command.run(TYPED)
  expect(opened.text).toBe('Agents panel opened.')
  await spawn($, 'u5', 'third')
  expect(seen.opened).toEqual(['skein-agents', 'skein-agents', 'skein-agents'])
})

// A repository at /repo with two worktrees, and a lane in the first one waiting on the heavy lock
// while a process in the second holds it.
const LANE_BOX: Box = {
  dirs: {
    '/repo/.git': ['worktrees'],
    '/repo/.git/worktrees': ['wt-a', 'wt-ab'],
    '/proc/41/fd': ['0', '3'],
    '/proc/42/fd': ['3'],
  },
  files: {
    '/repo/.git/worktrees/wt-a/gitdir': '/var/tmp/wt-a/.git\n',
    '/repo/.git/worktrees/wt-a/HEAD': 'ref: refs/heads/feat/a\n',
    '/repo/.git/worktrees/wt-ab/gitdir': '/var/tmp/wt-ab/.git\n',
    '/repo/.git/worktrees/wt-ab/HEAD': '0123456789abcdef0123456789abcdef01234567\n',
    '/proc/locks': [
      '1: POSIX  ADVISORY  READ 7 00:1d:99 128 128',
      '2: FLOCK  ADVISORY  WRITE 41 00:1d:7 0 EOF',
      '2: -> FLOCK  ADVISORY  WRITE 42 00:1d:7 0 EOF',
      '',
    ].join('\n'),
  },
  links: {
    '/proc/41/fd/0': '/dev/null',
    '/proc/41/fd/3': LOCK,
    '/proc/42/fd/3': LOCK,
    '/proc/41/cwd': '/var/tmp/wt-ab',
    '/proc/42/cwd': '/var/tmp/wt-a/src',
  },
}

const CLAIMED = JSON.stringify({
  lease: { workItemId: '6a1ef54e' },
  briefing: { readableId: 'SKEIN-9', title: 'Do the thing' },
})

test('a lane shows the item it claimed, its worktree and branch, and its wait for the heavy lock', async ($, on) => {
  const clock = mock.clock(on, { now: 0 })
  engine(on, { ...LANE_BOX, answers: { mcp__plugin_sync_sync__claim: CLAIMED } })

  await start($)
  await spawn($, 'u6', 'Worktree: /var/tmp/wt-ab is not yours.\nWorktree: /var/tmp/wt-a, on its branch.')
  // Its first tool call in a worktree settles which one is its own, over the brief's first mention.
  await $.tool.call({ tool: 'Bash', command: 'cd /var/tmp/wt-a && git status', agentId: 'agent-u6' })
  await $.tool.call({ tool: 'mcp__plugin_sync_sync__claim', agentId: 'agent-u6', projectId: 'p' })
  await clock.advance(5000)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...PANE, surface })
    expect((await ui.find({ type: 'Text', text: /SKEIN-9/ }))?.text).toBe('  SKEIN-9 Do the thing')
    expect((await ui.find({ type: 'Text', text: /\/var\/tmp\/wt-a/ }))?.text).toBe(
      '  /var/tmp/wt-a on feat/a · waiting for the heavy-work lock',
    )
    await ui.unmount()
  }
})

// The box's jobs view (SKEIN-1388), as skein writes it: one job of this box waiting third in the
// fleet's queue, from a folder inside `cwd`.
const jobsView = (cwd: string) => ({
  env: { SKEIN_STATE: '/state' },
  files: {
    ...LANE_BOX.files,
    '/state/signals/jobs.json': JSON.stringify({
      schema: 1,
      at: '2026-10-30T12:00:00Z',
      fleet_slots: 2,
      running: 2,
      repo_cap: 2,
      jobs: [{ id: 'j-1', class: 'test', command: 'cargo test', cwd, state: 'waiting', position: 3, reason: 'repo at cap 2/2', waited_secs: 40 }],
    }),
  },
})

async function laneLine($: Engine, on: On, box: Box): Promise<string | undefined> {
  const clock = mock.clock(on, { now: 0 })
  engine(on, box)
  await start($)
  await spawn($, 'u9', 'Worktree: /var/tmp/wt-a, on its branch.')
  await $.tool.call({ tool: 'Bash', command: 'cd /var/tmp/wt-a && git status', agentId: 'agent-u9' })
  await clock.advance(5000)
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  const text = (await ui.find({ type: 'Text', text: /\/var\/tmp\/wt-a / }))?.text
  await ui.unmount()

  return text
}

// What would make it fail: the lane line rendering the lock's own words while a jobs view says
// where the job stands (the sabotage that was run), or the position read from anything but the view.
test("a lane whose job waits in the fleet's queue shows its place and why, not the lock", async ($, on) => {
  expect(await laneLine($, on, { ...LANE_BOX, ...jobsView('/var/tmp/wt-a/src') })).toBe(
    "  /var/tmp/wt-a on feat/a · 3rd in the fleet's heavy-work queue (repo at cap 2/2)",
  )
})

test("a lane at the box's lock behind the box's queued job shows that job's place", async ($, on) => {
  expect(await laneLine($, on, { ...LANE_BOX, ...jobsView('/var/tmp/wt-ab') })).toBe(
    "  /var/tmp/wt-a on feat/a · waiting behind this box's heavy job, 3rd in the fleet's queue (repo at cap 2/2)",
  )
})

test('a lane line keeps the lock words where the box has no jobs view', async ($, on) => {
  expect(await laneLine($, on, { ...LANE_BOX, env: { SKEIN_STATE: '/state' } })).toBe('  /var/tmp/wt-a on feat/a · waiting for the heavy-work lock')
})

test('a lane that releases its item says it no longer holds it', async ($, on) => {
  mock.clock(on, { now: 0 })
  engine(on, { ...LANE_BOX, answers: { mcp__plugin_sync_sync__claim: CLAIMED } })

  await start($)
  await spawn($, 'u7', 'Worktree: /var/tmp/wt-a')
  await $.tool.call({ tool: 'mcp__plugin_sync_sync__claim', agentId: 'agent-u7', projectId: 'p' })
  await $.tool.call({ tool: 'mcp__plugin_sync_sync__release', agentId: 'agent-u7', projectId: 'p' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect((await ui.find({ type: 'Text', text: /SKEIN-9/ }))?.text).toBe('  SKEIN-9 Do the thing (no longer held)')
  await ui.unmount()
})

test('a subagent in no worktree shows no lane lines', async ($, on) => {
  mock.clock(on, { now: 0 })
  engine(on, LANE_BOX)

  await start($)
  await spawn($, 'u8', 'Read /var/tmp/wt-abc/notes and summarise.')

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: /\/var\/tmp\/wt-a/ })).toBe(undefined)
  expect(await ui.find({ type: 'Text', text: /tracker item/ })).toBe(undefined)
  await ui.unmount()
})
