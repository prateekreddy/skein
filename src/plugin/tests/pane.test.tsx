// The skein pane (SKEIN-1440, step 3), through the whole module: `claude plugin test src/plugin`
// loads the plugin from its folder, so these go through register.tsx.
//
// Nothing sits beneath the plugin: each `on` here stands for the engine and the box. The box is
// made up below: its signal files, its lanes as `skein-mcp lane list` prints them, its requests,
// and what the tracker answers a claim.

import { expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On } from 'claude-code'

const NOW = Date.parse('2026-10-30T12:00:00Z')
const STATE = '/fleet-root-example/.skein/state/web-main'
const LANES = '/lanes-example'
const REPO = '/repo-example'
const SURFACES = ['terminal', 'desktop'] as const
const G = 1024 * 1024 * 1024
const MIN = 60_000

const stamp = (ms: number) => new Date(ms).toISOString().replace(/\.\d+Z$/, 'Z')

/** The box as the tests change it: the clock's time the files are written at, and what is in them. */
type Box = {
  now: number
  cores: number
  ask: boolean
  questions: Record<string, unknown>[]
  lanes: { name: string; item: string }[]
  jobs: Record<string, unknown>[]
  spend: boolean
  mail: number
  /** Whether `$.ui.open` places a pane opened unasked: false stands for a terminal too narrow. */
  placed: boolean
  opened: string[]
  panes: Map<string, boolean>
  /**
   * The repository the session runs in, at REPO, for whether a lane's work landed: the worktrees
   * still on disk, its refs by name, and the `tip>onto` pairs git says are in one history. Null
   * for a session outside one.
   */
  git: { trees: Set<string>; refs: Map<string, string>; onMaster: Set<string>; asked: string[] } | null
}

function box(over: Partial<Box> = {}): Box {
  return {
    now: NOW,
    cores: 0.4,
    ask: false,
    questions: [],
    lanes: [],
    jobs: [],
    spend: false,
    mail: 0,
    placed: true,
    opened: [],
    panes: new Map(),
    git: null,
    ...over,
  }
}

const out = (stdout: string, exitCode = 0) => ({
  value: { exitCode, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false },
})

/** A lease as the tracker's `claim` answers it: claimed `ago` ms before NOW, for ten minutes. */
const claimed = (id: string, readable: string, title: string, ago: number) =>
  JSON.stringify({
    lease: { workItemId: id, epoch: 1, state: 'held', claimedAt: stamp(NOW - ago), heartbeatAt: stamp(NOW - ago), expiresAt: stamp(NOW - ago + 10 * MIN) },
    briefing: { workItemId: id, readableId: readable, title },
  })

function engine(on: On, b: Box) {
  const clock = mock.clock(on, { now: NOW })
  mock.env(on, { HOME: '/home/agent', SKEIN_BOX: 'web-main', SKEIN_STATE: STATE })
  on('command.register', (_, e) => ({ value: { command: e.name } }))
  on('tool.register', (_, e) => ({ value: { tool: `mcp__skein__${e.name}` } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('turn.complete', (_, e) => ({ text: e.answer }))
  on('ui.toast', () => ({ value: undefined }))
  on('agent.spawn', (_, e) => ({ model: 'claude-opus-5-5', agentId: `agent-${e.tool_use_id}` }))
  on('tool.call', (_, e) => ({ result: {}, text: String(e.tool).endsWith('__claim') ? claimed('w-1', 'SKEIN-1409', 'Every box allows the read-only tracker tools', 3 * MIN) : '' }))
  // A person's own open (the plugin opens with `focus` only then) is placed at any width.
  on('ui.open', (_, e) => {
    b.opened.push(e.id)
    const isPlaced = b.placed || e.focus === true
    b.panes.set(e.id, isPlaced)

    return { value: isPlaced ? { isPlaced: true } : { isPlaced: false, reason: 'opened unasked under 144 columns (80 now)' } }
  })
  on('ui.panes', () => ({ value: [...b.panes].map(([id, isPlaced]) => ({ id, title: id, isShown: isPlaced, isFocused: false, isPlaced })) }))
  on('fs.read', (_, e) => {
    if (e.path === `${STATE}/signals/resources.json`) {
      return {
        value: JSON.stringify({
          at: stamp(b.now - 10_000),
          disk: { mb: 2765, limit_mb: 10240, fleet: 'under', fleet_over_by_mb: 0 },
          memory: { anon: 1.1 * G, high: 14 * G, max: 16 * G, read_at: stamp(b.now - 10_000) },
          cpu: { cores: b.cores, usage_usec: 1 },
          pids: { current: 50, max: 8192 },
          asks: b.ask ? [{ kind: 'disk', why: 'allowance', amount: 1228, since: stamp(NOW), crossing_id: 'c-1', hold: '' }] : [],
        }),
      }
    }
    if (e.path === `${STATE}/signals/jobs.json` && b.jobs.length > 0) {
      return {
        value: JSON.stringify({
          schema: 1,
          at: stamp(b.now - 5000),
          cores: 8,
          granted: 4,
          free: 4,
          running: 3,
          jobs: b.jobs,
          ahead: [{ box: 'other-box', id: 'x-1', class: 'build', command: 'cargo build', cores: 4 }],
        }),
      }
    }
    if (e.path === `${STATE}/signals/usage.json` && b.spend) {
      const window = (cost: number) => ({ cost, input: 1, output: 2, cache_read: 3, cache_write: 4 })
      return { value: JSON.stringify({ at: stamp(b.now - 60_000), today: window(31.34), week: window(515.4), unpriced: true }) }
    }
    const ref = b.git !== null && e.path.startsWith(`${REPO}/.git/refs/`) ? b.git.refs.get(e.path.slice(`${REPO}/.git/`.length)) : undefined
    if (ref !== undefined) return { value: `${ref}\n` }
    throw new Error(`no such file: ${e.path}`)
  })
  // The repository's git directory is a folder of its own; nothing else is listed.
  on('fs.list', (_, e) => {
    if (b.git !== null && e.path === `${REPO}/.git`) return { value: [] }
    throw new Error(`no such folder: ${e.path}`)
  })
  on('fs.stat', (_, e) => {
    if (b.git?.trees.has(e.path)) return { value: { kind: 'dir' as const, size: 0, mtimeMs: 0, isLink: false, realPath: e.path } }
    throw new Error(`ENOENT: ${e.path}`)
  })
  on('process.run', (_, e) => {
    const argv = [...e.argv]
    if (argv[0] === 'git' && argv[2] === 'merge-base' && b.git !== null) {
      const pair = `${argv[4]}>${argv[5]}`
      b.git.asked.push(pair)
      return out('', b.git.onMaster.has(pair) ? 0 : 1)
    }
    if (argv[1]?.endsWith('/probe/mailbox.sh')) return out(`${b.mail}\n`)
    if (argv[2] === 'requests') return out(JSON.stringify({ result: { box: 'web-main', requests: b.questions }, said: '' }))
    if (argv[2] === 'lane' && argv[3] === 'list') {
      const lanes = b.lanes.map(l => ({ name: l.name, item: l.item, worktree: `${LANES}/${l.name}`, branch: `lane/${l.name}`, phase: 'created' }))
      return out(JSON.stringify({ result: { lanes }, said: '' }))
    }
    if (argv[2] === 'clean') return out(JSON.stringify({ result: { items: [], removable_now_bytes: 0, shared_repo_cache: null }, said: '' }))

    return out('', 127)
  })

  return clock
}

/** A minute on: the box's files rewritten at the new time, then the band's look. */
async function minute(clock: { advance: (ms: number) => Promise<void> }, b: Box, cores = b.cores) {
  b.now += MIN
  b.cores = cores
  await clock.advance(MIN)
}

async function spawn($: Engine, id: string, prompt: string) {
  await $.agent.spawn({
    tool_use_id: id,
    prompt,
    description: `lane ${id}`,
    subagentType: 'general-purpose',
    provider: { plugin: 'engine', tier: 'core' },
    parentModel: 'claude-opus-5-5',
    background: true,
    fork: false,
  })
}

/**
 * A repository with three lanes skein made, each with its subagent: gone's and kept's done, busy's
 * still at work, a minute on. A test then merges what it merges, removing worktrees, branches and
 * skein's lane records, so the board knows those lanes only by the session's subagents.
 * `onMaster` are the `tip>onto` pairs git says are in one history from the start.
 */
async function mergedBox($: Engine, on: On, onMaster: string[] = []): Promise<{ b: Box; clock: { advance: (ms: number) => Promise<void> } }> {
  const trees = new Set([`${LANES}/gone`, `${LANES}/kept`, `${LANES}/busy`])
  const refs = new Map([
    ['refs/heads/master', 'a'.repeat(40)],
    ['refs/heads/lane/gone', 'b'.repeat(40)],
    ['refs/heads/lane/kept', 'c'.repeat(40)],
    ['refs/heads/lane/busy', 'd'.repeat(40)],
  ])
  const b = box({
    lanes: [
      { name: 'gone', item: 'SKEIN-1' },
      { name: 'kept', item: 'SKEIN-2' },
      { name: 'busy', item: 'SKEIN-3' },
    ],
    git: { trees, refs, onMaster: new Set(onMaster), asked: [] },
  })
  const clock = engine(on, b)
  await $.session.start({ cwd: REPO, surface: 'terminal', isInteractive: true })
  await clock.settle()
  for (const name of ['gone', 'kept', 'busy']) await spawn($, name, `Your worktree is ${LANES}/${name}, on its branch.`)
  for (const name of ['gone', 'kept']) {
    await $.turn.complete({ turnId: 't', agentId: `agent-${name}`, reason: 'answer', answer: 'done', durationMs: 1, isAborted: false })
  }
  await minute(clock, b)

  return { b, clock }
}

/** The lanes panel's lines of the docked pane. */
async function lanesDrawn($: Engine): Promise<string[]> {
  const ui = await $.ui.mount({ ...DOCKED, surface: 'terminal' })
  const drawn = (await lines(ui)).filter(l => /^(lanes · |. \d: |    )/.test(l))
  await ui.unmount()

  return drawn
}

const question = (id: string, text: string) => ({ queue: 'question', id, asked: '2026-10-30T11:00:00Z', question: text, options: [], state: 'waiting' })

/**
 * The full box: three lanes (inbox's subagent done, ledger's running its tests, spend's waiting in
 * the fleet's queue behind another box's build), a held lease, spend, and the CPU over five
 * minutes. A question arrives in the last minute.
 */
async function busyBox($: Engine, on: On): Promise<Box> {
  const b = box({
    lanes: [
      { name: 'inbox', item: 'SKEIN-1375' },
      { name: 'ledger', item: 'SKEIN-1404' },
      { name: 'spend', item: 'SKEIN-1410' },
    ],
    jobs: [
      { id: 'j-1', class: 'test', command: 'cargo test --test ledger', cwd: `${LANES}/ledger`, state: 'granted', cores: 2, granted_cores: 2, position: null, reason: '', waited_secs: 3 },
      { id: 'j-2', class: 'lint', command: 'cargo clippy', cwd: `${LANES}/spend`, state: 'waiting', cores: 2, granted_cores: 0, position: 2, reason: 'fleet at its cores', waited_secs: 40 },
    ],
    spend: true,
    mail: 2,
  })
  const clock = engine(on, b)
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await clock.settle()
  await spawn($, 'u1', `Your worktree is ${LANES}/inbox, on its branch.`)
  await spawn($, 'u2', `Your worktree is ${LANES}/ledger, on its branch.`)
  await spawn($, 'u3', `Your worktree is ${LANES}/spend, on its branch.`)
  await $.turn.complete({ turnId: 't', agentId: 'agent-u1', reason: 'answer', answer: 'done', durationMs: 1, isAborted: false })
  await $.tool.call({ tool: 'mcp__plugin_sync_sync__claim', projectId: 'p', workItemId: 'w-1' })
  for (const cores of [0.2, 0.6, 1.0, 1.3]) await minute(clock, b, cores)
  b.questions = [question('q1', 'Keep the old table?')]
  await minute(clock, b, 0.9)

  return b
}

/** The pane as drawn, a line a row: a Button as the terminal paints it, `1: label` when plain. */
async function lines(ui: { drawn: () => Promise<unknown> }): Promise<string[]> {
  const root = (await ui.drawn()) as Node
  const rows: string[] = []
  const walk = (n: Node) => {
    for (const c of kids(n)) {
      if (typeof c === 'string') continue
      if (c.type === 'Box' && !(String(c.props?.key ?? '').startsWith('row:')) && c.props?.flexDirection === 'column') walk(c)
      else rows.push(shown(c).trimEnd())
    }
  }
  walk(root)

  return rows
}

type Node = { type?: string; props?: Record<string, unknown>; children?: unknown }

const kids = (n: Node): (string | Node)[] => {
  const k = n.children ?? n.props?.children
  return (Array.isArray(k) ? k : k === undefined || k === null ? [] : [k]).flat() as (string | Node)[]
}

function shown(n: string | Node): string {
  if (typeof n === 'string') return n
  if (n.type === 'Button') {
    const label = String(n.props?.label ?? '')
    return n.props?.plain ? `${n.props?.hotkey ? `${String(n.props.hotkey)}: ` : ''}${label}` : `[ ${label} ]`
  }

  return kids(n).map(shown).join('')
}

const DOCKED = {
  plugin: 'skein',
  component: 'Pane',
  requestId: 'skein',
  props: { title: 'skein', isFocused: false, bodyColumns: 48, placement: 'dock', scroll: { offset: 0, bodyRows: 40 }, view: {} },
} as const

const INLINE = { ...DOCKED, props: { ...DOCKED.props, bodyColumns: 78, placement: 'inline' } } as const

const HEAD = 'SKEIN · web-main · 1 waiting on you'

const BOX_PANEL = [
  '╭ BOX ─────────────────────────────────────────╮',
  '│ disk ▮▮▮▯▯▯▯▯▯▯  2.7 / 10 G   27%            │',
  '│ mem  ▮▯▯▯▯▯▯▯▯▯  1.1 / 14 G    8%            │',
  '│ cpu  ▃▂▄▆█▆  0.9 cores · 6m                  │',
  '│ processes 50 / 8192 · nothing asked          │',
  "│ ✉ 2 unread, for the agent's next turn        │",
  '╰──────────────────────────────────────────────╯',
]

// What would make it fail: a panel left out or drawn from the wrong signal, the waiting panel not
// amber-framed with its count, the lanes not along the board, another box's job left unnamed, the
// lease's time left from the claim rather than the clock, or a line wider than the 48 cells.
// Watched: with `waitingPanel` left out of the docked panels, this failed where the amber frame
// should start, its ninth line.
test('docked at 48 columns: every panel in order, from the box as skein and the session wrote it', async ($, on) => {
  await busyBox($, on)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...DOCKED, surface })
    const drawn = await lines(ui)
    expect(drawn).toEqual([
      HEAD,
      ...BOX_PANEL,
      '╭ WAITING ON YOU ─────────────────────────── 1 ╮',
      '│ ⚑ Answer in the cockpit: “Keep the old       │',
      '│   table?”                                    │',
      '╰──────────────────────────────────────────────╯',
      'lanes · 3 · 2 at work               1-3 open one',
      '✓ 1: inbox  SKEIN-1375 ━━━━━━── agent done',
      '◐ 2: ledger SKEIN-1404 ━━━━━─── heavy work',
      '◌ 3: spend  SKEIN-1410 ━━━━━─── #2 in the queue',
      '    made › at work › heavy work › agent done',
      '    › merged',
      'queue · yours: 1 running, 1 waiting',
      'fleet ▣▣▣▣□□□□ 4/8 cores · 3 running',
      '● cargo test · lane ledger · 2 cores',
      '◌ cargo clippy · lane spend · #2',
      '  ahead, from other-box: cargo build · 4 cores',
      'spend · ≈$31.34+ today · ≈$515+ 7d',
      'estimated at API prices as of 12:04 UTC; +',
      'leaves out a model skein has no price for',
      'holding · 1 item',
      ' SKEIN-1409 lease ━━───────── 2m left',
      '   Every box allows the read-only tracker tools',
      '12:01 inbox    agent done',
      '12:05 skein    a question waits on you',
    ])
    expect(drawn.every(l => [...l].length <= 48)).toBe(true)
    // Waiting on the person is amber; the box's own frame is skein's.
    expect((await ui.find({ type: 'Text', text: /WAITING ON YOU/ }))?.props.color).toBe('warning')
    expect((await ui.find({ type: 'Text', text: /╭ BOX/ }))?.props.color).toBe('suggestion')
    // Mail is the agent's to read: dim, never amber.
    const mail = await ui.find({ type: 'Text', text: /unread/ })
    expect(mail?.props.dimColor).toBe(true)
    expect(mail?.props.color).toBe(undefined)
    await ui.unmount()
  }
})

// What would make it fail: the inline seat drawn as the docked panels, the summary past eight
// lines, or a lane in it that its number does not open. Watched: with `placement === 'inline'`
// taken out of the narrow test, the docked panels were drawn here and this failed.
test('narrow, above the prompt: the same parts a line each, eight at most', async ($, on) => {
  await busyBox($, on)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...INLINE, surface })
    expect(await lines(ui)).toEqual([
      'skein  disk 2.7/10G  mem 1.1/14G  cpu ▃▂▄▆█▆  ·  $31+ today · $515+ 7d',
      '⚑ 1 waiting on you · a question',
      '✓ 1: inbox  agent done',
      '◐ 2: ledger heavy work',
      '◌ 3: spend  #2 in the queue',
      'queue ▣▣▣▣□□□□ · yours: 1 running, #2',
      'holding SKEIN-1409 2m',
      'ctrl+x tab, then 1-3 to open a lane',
    ])
    expect((await ui.find({ type: 'Button', key: 'lane:2' }))?.props.hotkey).toBe('2')
    await ui.unmount()
  }
})

// What would make it fail: a number that opens no lane or the wrong one, a lane's view drawn from
// another lane's subagent or job, or no way back. Watched: with the press setting the lane's name
// rather than its worktree, the view never matched a lane, the panels stayed, and this failed.
test('a lane opened by its number: its item, where it stands, its agent and job, and back', async ($, on) => {
  await busyBox($, on)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ ...DOCKED, surface })
    await ui.press({ key: 'lane:2' })
    expect(await lines(ui)).toEqual([
      HEAD,
      '◐ ledger · SKEIN-1404 · lane/ledger',
      '  SKEIN-1404, not claimed yet',
      '  ━━━━━━━━━━────── heavy work',
      '  agent    running · Opus 5.5 · 5m · ≈$0.00',
      '  job      cargo test --test ledger',
      '  worktree /lanes-example/ledger',
      '  record   created',
      '  Its commits, tests and CI are not recorded for',
      '  a lane yet.',
      'b: Back',
    ])
    // No key removes anything: dropping a lane is the agent's, not a key's.
    expect((await ui.findAll({ type: 'Button' })).map(b => b.key)).toEqual(['pane:back'])
    await ui.press({ key: 'pane:back' })
    expect((await lines(ui))[1]).toBe(BOX_PANEL[0])
    await ui.unmount()
  }
})

// What would make it fail: a panel that draws its heading with nothing under it. The same box then
// gets a lane and a question, so each panel is seen present before its absence is believed.
// Watched: with the lanes panel's empty check removed, `lanes · 0 · 0 at work` was drawn here and
// this failed.
test('a panel with nothing to say is not drawn, and is drawn once it has', async ($, on) => {
  const b = box()
  const clock = engine(on, b)
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await clock.settle()

  const ui = await $.ui.mount({ ...DOCKED, surface: 'terminal' })
  expect(await lines(ui)).toEqual([
    'SKEIN · web-main · ok',
    '╭ BOX ─────────────────────────────────────────╮',
    '│ disk ▮▮▮▯▯▯▯▯▯▯  2.7 / 10 G   27%            │',
    '│ mem  ▮▯▯▯▯▯▯▯▯▯  1.1 / 14 G    8%            │',
    '│ cpu  ▄  0.4 cores · 1m                       │',
    '│ processes 50 / 8192 · nothing asked          │',
    '╰──────────────────────────────────────────────╯',
  ])
  await ui.unmount()

  b.lanes = [{ name: 'inbox', item: 'SKEIN-1375' }]
  b.questions = [question('q1', 'Keep the old table?')]
  await minute(clock, b)
  const after = await $.ui.mount({ ...DOCKED, surface: 'terminal' })
  const drawn = await lines(after)
  expect(drawn.filter(l => /^(╭ WAITING ON YOU|lanes · 1)/.test(l))).toEqual([
    '╭ WAITING ON YOU ─────────────────────────── 1 ╮',
    'lanes · 1 · 0 at work                 1 opens it',
  ])
  await after.unmount()
})

// What would make it fail: the pane opening on every look (the person who closed it is shown it
// again each minute), or not opening for a new thing. Watched: with `isNew` taken out of the open
// test (open on every look), the pane opened again a minute after it was closed and this failed.
test('the pane opens as the session starts and for a new thing waiting, never again for an old one', async ($, on) => {
  const b = box({ questions: [question('q1', 'Keep the old table?')] })
  const clock = engine(on, b)
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await clock.settle()
  expect(b.opened).toEqual(['skein'])

  // The person closes it while the question still waits (the engine drops the pane; nothing of the
  // plugin's need hear it): minutes pass, and it stays closed.
  b.panes.delete('skein')
  await minute(clock, b)
  await minute(clock, b)
  expect(b.opened).toEqual(['skein'])

  // Something new waits: it opens again, once.
  b.questions = [...b.questions, question('q2', 'Merge the ledger lane?')]
  await minute(clock, b)
  expect(b.opened).toEqual(['skein', 'skein'])
  await minute(clock, b)
  expect(b.opened).toEqual(['skein', 'skein'])
})

// What would make it fail: a pane that opened unasked on a narrow terminal and waits undrawn, with
// nothing anywhere saying so. Watched: with the band's line for it removed, nothing said where the
// pane went and this failed.
test('where the pane cannot be placed, the band says so, and /skein places it', async ($, on) => {
  const b = box({ placed: false })
  const clock = engine(on, b)
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await clock.settle()
  expect(b.opened).toEqual(['skein'])

  const said = 'The skein pane waits for a wider terminal. /skein shows it here.'
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await band.find({ type: 'Text', text: /wider terminal/ }))?.text).toBe(said)
  expect((await band.find({ type: 'Text', text: /wider terminal/ }))?.props.dimColor).toBe(true)
  await band.unmount()

  await $.command.run({ command: 'skein', args: '', origin: { kind: 'composer' }, presentation: { isFullscreen: false, columns: 80 } })
  expect(b.opened).toEqual(['skein', 'skein'])
  const placed = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await placed.find({ type: 'Text', text: /wider terminal/ })).toBe(undefined)
  await placed.unmount()
})

const BAND = {
  plugin: 'skein',
  component: 'AbovePrompt',
  requestId: 'above-prompt',
  props: { hasSurvey: false, isWorking: false, maxRows: 10, bodyColumns: 100, scroll: { offset: 0, bodyRows: 9 }, view: {} },
} as const

// What would make it fail: the board taking a subagent's answer as the lane's last word (the
// owner's 2026-11-01 look: four merged lanes drawn `agent done` for hours), a lane whose worktree
// is still there drawn as merged, a running lane looked at at all, or a merged lane kept for ever.
// Watched, each against this test: with `stageOf`'s merged line taken out, gone stayed
// `✓ agent done`; with `hasLanded`'s worktree check taken out, kept, merged with its worktree not
// yet removed, read merged too; with the running checks taken out of `refreshLanded` and
// `stageOf`, busy read merged while at work; with `laneRuns`'s hour taken out, gone was still on
// the board an hour on.
test('a lane whose work landed is drawn merged, then leaves the board an hour on; the others stay as they were', async ($, on) => {
  // kept is merged too, but its worktree is not removed yet: the coordinator is not done with it.
  const { b, clock } = await mergedBox($, on, [`${'c'.repeat(40)}>${'a'.repeat(40)}`])
  const git = b.git
  if (git === null) throw new Error('the box has a repository')
  expect(await lanesDrawn($)).toEqual([
    'lanes · 3 · 1 at work               1-3 open one',
    '✓ 1: gone SKEIN-1    ━━━━━━── agent done',
    '✓ 2: kept SKEIN-2    ━━━━━━── agent done',
    '● 3: busy SKEIN-3    ━━━───── at work',
    '    made › at work › heavy work › agent done',
    '    › merged',
  ])

  // gone is merged: its worktree removed, its branch deleted, its record with them. busy's
  // worktree and branch go too, but its subagent still runs, so nothing of it is looked at.
  for (const name of ['gone', 'busy']) {
    git.trees.delete(`${LANES}/${name}`)
    git.refs.delete(`refs/heads/lane/${name}`)
  }
  b.lanes = []
  await minute(clock, b)
  await minute(clock, b)
  expect(await lanesDrawn($)).toEqual([
    'lanes · 3 · 1 at work               1-3 open one',
    '◆ 1: gone SKEIN-1    ━━━━━━━━ merged',
    '✓ 2: kept SKEIN-2    ━━━━━━── agent done',
    '● 3: busy SKEIN-3    ━━━───── at work',
    '    made › at work › heavy work › agent done',
    '    › merged',
  ])

  for (let i = 0; i < 60; i++) await minute(clock, b)
  expect(await lanesDrawn($)).toEqual([
    'lanes · 2 · 1 at work               1-2 open one',
    '✓ 1: kept SKEIN-2    ━━━━━━── agent done',
    '● 2: busy SKEIN-3    ━━━───── at work',
    '    made › at work › heavy work › agent done',
    '    › merged',
  ])
})

// What would make it fail: a worktree removed with its branch kept read as merged whatever the
// branch holds, or git asked again each minute about the same two commits. Watched: with
// `hasLanded`'s ancestry check made to answer yes, kept read merged; with the `ancestry` cache
// taken out, git was asked about kept's pair again at every look and the count failed.
test('a lane whose worktree went but whose branch stayed is merged only once its tip is on master, asked of git once', async ($, on) => {
  const { b, clock } = await mergedBox($, on)
  const git = b.git
  if (git === null) throw new Error('the box has a repository')
  // Both worktrees removed, both branches kept; only gone's tip is on master.
  git.trees.delete(`${LANES}/gone`)
  git.trees.delete(`${LANES}/kept`)
  git.onMaster.add(`${'b'.repeat(40)}>${'a'.repeat(40)}`)
  b.lanes = []
  for (let i = 0; i < 3; i++) await minute(clock, b)

  expect((await lanesDrawn($)).slice(1, 3)).toEqual([
    '◆ 1: gone SKEIN-1    ━━━━━━━━ merged',
    '✓ 2: kept SKEIN-2    ━━━━━━── agent done',
  ])
  // Each pair once, however many looks: gone's settled at the first, kept's answer kept.
  expect(git.asked.sort()).toEqual([`${'b'.repeat(40)}>${'a'.repeat(40)}`, `${'c'.repeat(40)}>${'a'.repeat(40)}`])
})
