// skein's tool results drawn for the person (SKEIN-1440, step 1), through the whole module.
//
// Each view is drawn from a saved answer (`tool-fixtures.ts`, the shapes `bin/skein-mcp` and
// `src/jobs.rs` write) and read back as the rows a terminal shows. Nothing sits beneath the plugin:
// the `ui.render` hook each test adds stands for the engine's own drawing, so a result the plugin
// does not draw reads `engine`, and the `tool.call` hooks stand for the tools themselves.

import { expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On, RenderNode } from 'claude-code'

import * as F from './tool-fixtures'

const SURFACES = ['terminal', 'desktop'] as const
const NOW = Date.parse(F.JOBS_VIEW.at)
const STATE = '/fleet-root-example/.skein/state/web-main'

const name = (t: string) => `mcp__plugin_skein_skein__skein_${t}`

/** The text of a node and everything in it. */
function textOf(n: RenderNode): string {
  if (typeof n === 'string') return n
  const kids = 'children' in n && Array.isArray(n.children) ? (n.children as RenderNode[]) : []

  return kids.map(textOf).join('')
}

/** A drawn tree as the rows it shows: a Box's children one under another, a Text one row. */
function rows(n: RenderNode): string[] {
  if (typeof n === 'string') return [n]
  if (n.type === 'Box') {
    const kids = 'children' in n && Array.isArray(n.children) ? (n.children as RenderNode[]) : []
    // A row Box: its first child's text beside the rest, the rows under it indented to match.
    const isRow = 'props' in n && (n.props as Record<string, unknown> | undefined)?.flexDirection === 'row'
    if (isRow && kids.length === 2 && kids[0] !== undefined && kids[1] !== undefined) {
      const lead = textOf(kids[0])

      return rows(kids[1]).map((r, i) => `${i === 0 ? lead : ' '.repeat(lead.length)}${r}`)
    }

    return kids.flatMap(rows)
  }

  return [textOf(n)]
}

/** The engine beneath the plugin: its own drawing of any row reads `engine`, and what it was
 * handed is kept, to show the plugin handed it on unchanged. */
function engine(on: On) {
  const handed: unknown[] = []
  mock.env(on, { HOME: F.HOME, SKEIN_STATE: STATE, SKEIN_BOX: 'web-main' })
  on('ui.render', ($, e) => {
    handed.push(e.props)
    const { Text } = $.ui.resolve(e)

    return <Text>engine</Text>
  })

  return handed
}

type Surface = (typeof SURFACES)[number]

async function drawResult($: Engine, tool: string, output: unknown, over: { columns?: number; isErrored?: boolean; isExpanded?: boolean; surface?: Surface; id?: string } = {}) {
  const ui = await $.ui.mount({
    plugin: 'skein',
    surface: over.surface ?? 'terminal',
    component: 'ToolResult',
    requestId: over.id ?? 't1',
    props: { tool_use_id: over.id ?? 't1', tool, output, isErrored: over.isErrored ?? false, ...(over.isExpanded === undefined ? {} : { isExpanded: over.isExpanded }) },
    viewport: { columns: over.columns ?? 86, rows: 40 },
  })
  const drawn = await ui.drawn()

  return { ui, drawn, lines: rows(drawn) }
}

/** The colour, dimness and weight of the innermost run whose text matches. */
async function toneOf(ui: { findAll: (q: { type: 'Text'; text: RegExp }) => Promise<{ props: Record<string, unknown> }[]> }, text: RegExp) {
  const found = await ui.findAll({ type: 'Text', text })
  const p = found.at(-1)?.props ?? {}

  return { color: p.color, dim: p.dimColor === true, bold: p.bold === true }
}

// ------------------------------------------------------------------------------------------------
// The four views
// ------------------------------------------------------------------------------------------------

// What would make it fail: the view drawing the JSON again (the hook passing to `next`), a limit
// left out or put on another line, or sizes in another unit. Watched: with `resourcesLines`
// returning null, every row read `engine` and this failed.
test('skein_resources draws one line per limit, and says nothing is asked', async ($, on) => {
  engine(on)
  for (const surface of SURFACES) {
    const { ui, lines } = await drawResult($, name('resources'), F.mcp(F.RESOURCES), { surface })
    expect(lines).toEqual([
      'as of 17:49 UTC',
      'disk     4.95 GB of 10 GB   fleet disk fine',
      'cpu      1.3 cores',
      'memory   2.31 GB of 13.1 GB   not slowed',
      'procs    58 of 8192',
      'nothing asked of this box',
      'about 1.38 GB could be removed · /skein clean',
    ])
    expect((await toneOf(ui, /nothing asked/)).color).toBe('success')
    expect((await toneOf(ui, /fleet disk fine/)).dim).toBe(true)
    await ui.unmount()
  }
})

// What would make it fail: an ask drawn after the limits, or not in amber, or in words of the
// mod's own rather than skein's. Watched: with the asks pushed after the procs row, the second row
// read `disk …` and this failed.
test("an ask from skein comes first, in amber and in skein's own words", async ($, on) => {
  engine(on)
  const { ui, lines } = await drawResult($, name('resources'), F.mcp(F.RESOURCES_ASKED))
  expect(lines).toEqual([
    'as of 17:49 UTC',
    '⚑ skein: this box holds 11.2G of its 10.0G allowance. Clear 1.2G: skein_clean lists what can go.',
    'disk     11.2 GB of 10 GB   fleet disk over its line by 4.1 GB',
    'cpu      1.3 cores',
    'memory   2.31 GB of 13.1 GB   slowed 12 times a minute',
    'procs    58 of 8192',
    'a restart would bring claude 2.1.318',
    'about 1.38 GB could be removed · /skein clean',
  ])
  expect(await toneOf(ui, /^⚑ skein: this box holds/)).toEqual({ color: 'warning', dim: false, bold: true })
  expect((await toneOf(ui, /^11\.2 GB of 10 GB$/)).color).toBe('warning')
  expect((await toneOf(ui, /slowed 12/)).color).toBe('warning')
  await ui.unmount()
})

// What would make it fail: a parentless process left unmarked, sizes or times not right-aligned,
// or a command running past the width the engine gives. Watched: with the `no parent` run left
// out, the tmux row ended in its command and this failed.
test('skein_top draws a table, marks the process with no parent, and cuts commands to the width', async ($, on) => {
  engine(on)
  const { ui, lines } = await drawResult($, name('top'), F.mcp(F.TOP_CPU))
  expect(lines).toEqual([
    'by cpu · 16 processes in this box',
    '    cpu      mem   age  command',
    '  1789s   879 MB    5h  claude --name web-main --plugin-dir /fleet-root-example…',
    '    40s     6 MB    5h  /usr/bin/tmux -S /fleet-root-example/web-main… no parent',
    '   2.3s     3 MB    4h  /bin/bash -c source /home/agent/.claude/shell-snapshots…',
    '   1.8s    12 MB    5h  python3 /fleet-root-example/.skein/plugin/bin/skein-res…',
    '   1.3s     3 MB    1h  bash',
    '   0.3s     3 MB   45s  sync-monitor',
    '1 process has no parent',
  ])
  expect((await toneOf(ui, /^ no parent$/)).color).toBe('warning')
  await ui.unmount()

  // Narrower: every row still fits the room the engine leaves after its `  ⎿  `.
  const narrow = await drawResult($, name('top'), F.mcp(F.TOP_CPU), { columns: 60 })
  expect(narrow.lines.slice(2, 4)).toEqual([
    '  1789s   879 MB    5h  claude --name web-main --plug…',
    '    40s     6 MB    5h  /usr/bin/tmux -S /f… no parent',
  ])
  expect(Math.max(...narrow.lines.map(l => l.length))).toBeLessThanOrEqual(54)
  await narrow.ui.unmount()
})

test('skein_top by memory is the same table, and by disk lists the largest and the removable by kind', async ($, on) => {
  engine(on)
  const memory = await drawResult($, name('top'), F.mcp(F.TOP_MEMORY))
  expect(memory.lines).toEqual([
    'by memory · 16 processes in this box',
    '    cpu      mem   age  command',
    '  1789s   879 MB    5h  claude --name web-main --plugin-dir /fleet-root-example…',
    '   1.8s    12 MB    5h  python3 /fleet-root-example/.skein/plugin/bin/skein-res…',
    '    40s     6 MB    5h  /usr/bin/tmux -S /fleet-root-example/web-main… no parent',
    '1 process has no parent',
  ])
  await memory.ui.unmount()

  const disk = await drawResult($, name('top'), F.mcp(F.TOP_DISK))
  expect(disk.lines).toEqual([
    'by disk · this box holds 4.95 GB of 10 GB',
    'largest in /fleet-root-example/web-main/tree',
    '     3 GB  target',
    '    40 MB  tests',
    '    12 MB  src',
    'removable and made again, by kind · skein_clean removes them',
    '  1.36 GB  download cache',
    '   216 MB  scratchpad',
    '    18 MB  build output',
    'shared repo cache · empty · not counted here',
  ])
  await disk.ui.unmount()
})

// What would make it fail: a kept item counted as safe (the scratchpads, or the build that is
// running), sizes left-aligned, or the reason for keeping one put in words other than skein's.
// Watched: with the `safe_now` filter dropped, the header read 4.6 GB and this failed.
test('skein_clean draws what can go now, sizes first, apart from what is kept and why', async ($, on) => {
  engine(on)
  for (const surface of SURFACES) {
    const { ui, lines } = await drawResult($, name('clean'), F.mcp(F.CLEAN), { surface })
    expect(lines).toEqual([
      '1.38 GB safe to remove now',
      '  983 MB  ~/.cache/ms-playwright                download cache',
      '  222 MB  ~/.cargo/registry                     download cache',
      '  186 MB  ~/.npm/_cacache                       download cache',
      '   18 MB  /workspace/web/tests/ui/node_modules  build output',
      'kept · 3 scratchpads, 220 MB: a scratchpad may belong to a session still running in this box',
      'kept · 1 build output, 3 GB: a build is running in it now',
      'shared repo cache · empty · not counted here',
      '/skein clean opens the list to remove them · ctrl+o shows all 8',
    ])
    expect((await toneOf(ui, /safe to remove now/)).color).toBe('success')
    expect((await toneOf(ui, /^kept · 3 scratchpads/)).dim).toBe(true)
    await ui.unmount()
  }
})

test("the mod's own clean tool draws as skein_clean does, and a removal says what went and what did not", async ($, on) => {
  engine(on)
  // The mod's `clean` answers its blocks as the result (`mcpAnswer` in band.tsx).
  const own = await drawResult($, 'mcp__skein__clean', F.mcp(F.CLEAN_REMOVED).content)
  expect(own.lines).toEqual([
    'removed 1 · about 983 MB freed',
    '  ~/.cache/ms-playwright',
    '✗ ~/.cargo/registry: Permission denied',
    "this box's disk figure catches up within five minutes",
  ])
  expect((await toneOf(own.ui, /^✗ $/)).color).toBe('error')
  await own.ui.unmount()
})

test('skein_requests puts what waits for the owner first, in amber', async ($, on) => {
  engine(on)
  const { ui, lines } = await drawResult($, name('requests'), F.mcp(F.REQUESTS))
  expect(lines).toEqual([
    '⚑ package apt jq ripgrep · waiting for the owner',
    '  write acme/site · granted',
    '  question “Keep the old table?” · answered: yes, for one release',
  ])
  expect((await toneOf(ui, /^⚑ package/)).color).toBe('warning')
  await ui.unmount()
})

// The engine's own result for an MCP tool has not been seen in a live session from here: each way
// it could hold the blocks draws the same.
test('the answer is read whether the engine holds it as blocks, as MCP content, or as text', async ($, on) => {
  engine(on)
  const answer = F.mcp(F.REQUESTS)
  const shapes = [answer, answer.content, answer.content[0]?.text ?? '']
  const drawn = []
  for (const shape of shapes) {
    const { ui, lines } = await drawResult($, name('requests'), shape)
    drawn.push(lines[0])
    await ui.unmount()
  }
  expect(drawn).toEqual(Array(3).fill('⚑ package apt jq ripgrep · waiting for the owner'))
})

// ------------------------------------------------------------------------------------------------
// What falls back to the engine's own drawing
// ------------------------------------------------------------------------------------------------

// What would make it fail: an error or an unknown shape drawn as an empty or half view instead of
// the engine's, or the props handed on changed. Watched: with `resultJson` keeping an object that
// holds `error`, the restart that had nothing to apply (the whole signal, and an `error`) was drawn
// as the signal and this failed.
const FALLS_BACK: Array<[string, string, unknown, { isErrored?: boolean; isExpanded?: boolean }]> = [
  ['the call errored', name('resources'), F.mcp(F.RESOURCES), { isErrored: true }],
  ['the person expanded the row (ctrl+o)', name('resources'), F.mcp(F.RESOURCES), { isExpanded: true }],
  ['the answer is an error', name('top'), F.mcp({ by: 'cpu', error: 'not-in-a-box-cgroup' }), {}],
  // `skein_resources` with `restart: when-idle` and nothing rolled out: the signal and an error.
  ['the answer is the signal with an error', name('resources'), F.mcp({ ...F.RESOURCES, error: 'nothing-to-apply' }), {}],
  ['the answer is not JSON', name('clean'), 'skein: skein has not listed this box\'s disk yet.', {}],
  ['the JSON is a shape it does not know', name('resources'), F.mcp({ hello: 'world' }), {}],
  ['skein_top by something new', name('top'), F.mcp({ by: 'gpu', processes: [] }), {}],
  ['the tool is not skein\'s', 'mcp__other__skein_resources', F.mcp(F.RESOURCES), {}],
  ['the tool is skein\'s and not one drawn here', name('lane'), F.mcp({ lanes: [] }), {}],
  ['a Bash call that ran no skein-job', 'Bash', { stdout: 'ok\n', stderr: '', interrupted: false }, {}],
]

for (const [when, tool, output, over] of FALLS_BACK) {
  test(`the engine draws the result when ${when}, from the props it had`, async ($, on) => {
    const handed = engine(on)
    for (const surface of SURFACES) {
      const { ui, lines } = await drawResult($, tool, output, { ...over, surface })
      expect(lines).toEqual(['engine'])
      expect(handed.at(-1)).toEqual({
        tool_use_id: 't1',
        tool,
        output,
        isErrored: over.isErrored ?? false,
        ...(over.isExpanded === undefined ? {} : { isExpanded: over.isExpanded }),
      })
      await ui.unmount()
    }
  })
}

// ------------------------------------------------------------------------------------------------
// What the model reads
// ------------------------------------------------------------------------------------------------

// What would make it fail: a `tool.call` hook that rewrote a skein tool's answer or a skein-job's
// Bash result on its way to the model, or answered in its place. Watched: with the Bash hook
// returning `{ ...ran, stderr: '' }`, the Bash comparison failed.
test("the model reads each tool's answer exactly as the tool gave it", async ($, on) => {
  engine(on)
  mock.clock(on, { now: NOW })
  on('fs.read', () => ({ value: JSON.stringify(F.JOBS_VIEW) }))
  const answers: Record<string, unknown> = {
    [name('resources')]: F.mcp(F.RESOURCES),
    [name('top')]: F.mcp(F.TOP_CPU),
    [name('clean')]: F.mcp(F.CLEAN),
    [name('requests')]: F.mcp(F.REQUESTS),
    Bash: F.JOB_BASH_OUTPUT,
  }
  on('tool.call', (_, e) => ({ result: answers[e.tool] }))

  for (const tool of Object.keys(answers).filter(t => t !== 'Bash')) {
    const ran = await $.tool.call({ tool: tool as `mcp__${string}__${string}` })
    expect(ran.result).toEqual(answers[tool])
  }
  const ran = await $.tool.call({ tool: 'Bash', command: 'skein-job run --cores 4 -- cargo test --lib' })
  expect(ran.result).toEqual(F.JOB_BASH_OUTPUT)
})

// ------------------------------------------------------------------------------------------------
// A skein-job call while it waits, and when it ends
// ------------------------------------------------------------------------------------------------

const RUN = { tool: 'Bash', command: 'cd /workspace/web && skein-job run --cores 4 -- cargo test --lib 2>&1 | tail -20' } as const

/** Bash beneath the plugin: a `skein-job` call stays running until `release` is called, any
 * other answers at once. */
function slowBash(on: On) {
  let release = () => {}
  let reached = () => {}
  const isReached = new Promise<void>(r => (reached = r))
  on('tool.call', { tool: 'Bash' }, async (_, e) => {
    if (!String(e.command).includes('skein-job')) return { result: { stdout: '', stderr: '', interrupted: false } }
    reached()
    await new Promise<void>(r => (release = r))

    return { result: F.JOB_BASH_OUTPUT }
  })

  return { isReached, release: () => release() }
}

const TOOL_ROW = (tool_use_id: string, command: string, isRunning: boolean) => ({
  plugin: 'skein',
  surface: 'terminal' as const,
  component: 'ToolUse' as const,
  requestId: tool_use_id,
  props: { tool_use_id, tool: 'Bash', input: { command }, isRunning, isErrored: false, isInterrupted: false },
  viewport: { columns: 86, rows: 40 },
})

// What would make it fail: the row drawn from no view, from a view of another job, or the jobs
// ahead left unnamed; or the line not following the view as skein rewrites it. Watched: with the
// poller started at 20 s instead of 2 s, the redraw after two seconds still read `waiting` and
// this failed.
test('a skein-job call waiting for cores shows its place, the jobs ahead and the pool, live', async ($, on) => {
  engine(on)
  const clock = mock.clock(on, { now: NOW })
  let view: unknown = F.JOBS_VIEW
  const reads: string[] = []
  on('fs.read', (_, e) => {
    reads.push(e.path)

    return { value: JSON.stringify(view) }
  })
  const bash = slowBash(on)

  const call = $.tool.call({ ...RUN, tool_use_id: 'b1' } as never)
  await bash.isReached
  expect(reads).toEqual([`${STATE}/signals/jobs.json`])

  const ui = await $.ui.mount(TOOL_ROW('b1', RUN.command, true))
  expect(rows(await ui.drawn())).toEqual([
    'engine',
    '✻ waiting for 4 cores · 2nd · 1 m 4 s · waiting for 4 cores (2 free)',
    '  ahead: api-main · cargo build · 6 cores',
    '  pool 12 cores · 10 granted · 2 free',
  ])
  expect((await toneOf(ui, /^✻ waiting/)).color).toBe('warning')

  // skein grants it: the next read, two seconds on, says so.
  view = {
    ...F.JOBS_VIEW,
    at: new Date(NOW + 2000).toISOString().replace(/\.\d+Z$/, 'Z'),
    ahead: [],
    jobs: [{ ...F.JOBS_VIEW.jobs[0], state: 'granted', granted_cores: 4, position: null, reason: '', waited_secs: 66 }, F.JOBS_VIEW.jobs[1]],
  }
  await clock.advance(2000)
  expect(rows(await ui.drawn())).toEqual(['engine', '● running on 4 cores · waited 1 m 6 s'])
  await ui.unmount()

  bash.release()
  expect((await call).result).toEqual(F.JOB_BASH_OUTPUT)

  // Ended: the reads stop, and the result says what the job used, under the engine's own.
  const read = reads.length
  await clock.advance(10_000)
  expect(reads.length).toBe(read)
  const done = await drawResult($, 'Bash', F.JOB_BASH_OUTPUT, { id: 'b1' })
  expect(done.lines).toEqual(['engine', '✓ cargo test · 3 m 12 s · used 3.4 of 4 cores · peak 2.1 GB'])
  await done.ui.unmount()
})

const SPINNER = (requestId: string) => ({
  plugin: 'skein',
  surface: 'terminal' as const,
  component: 'Spinner' as const,
  requestId,
  props: { word: 'Sauteing', message: null, suffix: '…', mode: 'tool-use' as const },
  viewport: { columns: 86, rows: 40 },
})

// What would make it fail: the spinner left saying `Sauteing` while the job waits, an estimate
// shown that no data gives, the box ahead left unnamed, the words kept once the job is granted, or a
// subagent's wait drawn on another agent's spinner. Watched: with the Spinner hook passing
// `next(e)` unchanged, the first `handed` comparison read `message: null` and this failed.
test('while a skein-job call waits the spinner says so, and is the engine’s again once it runs', async ($, on) => {
  const handed = engine(on)
  const clock = mock.clock(on, { now: NOW })
  let view: unknown = { ...F.JOBS_VIEW, jobs: [{ ...F.JOBS_VIEW.jobs[0], cwd: '/workspace/web/.worktrees/ledger' }, F.JOBS_VIEW.jobs[1]] }
  on('fs.read', () => ({ value: JSON.stringify(view) }))
  const bash = slowBash(on)

  const before = await $.ui.mount(SPINNER('main'))
  await before.drawn()
  expect(handed.at(-1)).toMatchObject({ word: 'Sauteing', message: null, suffix: '…' })
  await before.unmount()

  const call = $.tool.call({ ...RUN, tool_use_id: 'b1' } as never)
  await bash.isReached
  const ui = await $.ui.mount(SPINNER('main'))
  await ui.drawn()
  expect(handed.at(-1)).toMatchObject({
    word: 'Sauteing',
    message: 'Waiting for a fleet slot · #2 · cargo test, lane ledger · 1 ahead: api-main cargo build',
    suffix: '',
  })

  // Granted: the spinner is the engine's again.
  view = { ...F.JOBS_VIEW, at: new Date(NOW + 2000).toISOString().replace(/\.\d+Z$/, 'Z'), ahead: [], jobs: [{ ...F.JOBS_VIEW.jobs[0], state: 'granted', granted_cores: 4, position: null }] }
  await clock.advance(2000)
  await ui.drawn()
  expect(handed.at(-1)).toMatchObject({ message: null, suffix: '…' })
  await ui.unmount()
  bash.release()
  await call
})

// What would make it fail: every running call's wait drawn on every spinner. Watched: with the
// `agentId` test in `spinnerText` made always true, lane-2's spinner said lane-1's wait and the
// `message: null` comparison failed.
test('a subagent’s waiting job shows on its own spinner, not on another’s', async ($, on) => {
  const handed = engine(on)
  mock.clock(on, { now: NOW })
  on('fs.read', () => ({ value: JSON.stringify(F.JOBS_VIEW) }))
  const bash = slowBash(on)

  const call = $.tool.call({ ...RUN, tool_use_id: 'b2', agentId: 'lane-1' } as never)
  await bash.isReached
  const other = await $.ui.mount(SPINNER('lane-2'))
  await other.drawn()
  expect(handed.at(-1)).toMatchObject({ message: null })
  await other.unmount()
  const own = await $.ui.mount(SPINNER('lane-1'))
  await own.drawn()
  expect(handed.at(-1)).toMatchObject({ message: 'Waiting for a fleet slot · #2 · cargo test · 1 ahead: api-main cargo build' })
  await own.unmount()
  bash.release()
  await call
})

test('a job that failed ends with a cross, and one from before this session without its command', async ($, on) => {
  engine(on)
  const ui = await $.ui.mount({
    plugin: 'skein',
    surface: 'terminal',
    component: 'ToolResult',
    props: { tool_use_id: 'old', tool: 'Bash', output: { ...F.JOB_BASH_OUTPUT, stderr: 'skein-job: used 1.3 avg, 45s\n' }, isErrored: true },
  })
  expect(rows(await ui.drawn())).toEqual(['engine', '✗ 45 s · used 1.3 cores'])
  expect((await toneOf(ui, /^✗ $/)).color).toBe('error')
  await ui.unmount()
})

// What would make it fail: any Bash call read as a job, or a view skein stopped writing believed.
// Watched: with the staleness check removed, the stale view drew `✻ waiting …` and this failed.
test('a Bash call with no skein-job, or a view skein stopped writing, is drawn by the engine alone', async ($, on) => {
  engine(on)
  mock.clock(on, { now: NOW + 31_000 })
  const reads: string[] = []
  on('fs.read', (_, e) => {
    reads.push(e.path)

    return { value: JSON.stringify(F.JOBS_VIEW) }
  })
  const bash = slowBash(on)

  await $.tool.call({ tool: 'Bash', command: 'cargo test', tool_use_id: 'plain' } as never)
  expect(reads).toEqual([])
  const plain = await $.ui.mount(TOOL_ROW('plain', 'cargo test', true))
  expect(rows(await plain.drawn())).toEqual(['engine'])
  await plain.unmount()

  const call = $.tool.call({ ...RUN, tool_use_id: 'stale' } as never)
  await bash.isReached
  const stale = await $.ui.mount(TOOL_ROW('stale', RUN.command, true))
  expect(rows(await stale.drawn())).toEqual(['engine'])
  await stale.unmount()
  bash.release()
  await call
})

// What would make it fail: the shell's own words (`2>&1 | tail`, `&& …`) or skein-job's options
// read as the command, so no job in the view ever matches; or a command matched by its first words
// alone, so a call is tied to another job. Watched: with the cut at shell
// operators removed, the first case read `cargo test --lib 2>&1 | tail -20` and this failed.
const COMMANDS: Array<[string, boolean]> = [
  ['cd /w && skein-job run --cores 4 -- cargo test --lib 2>&1 | tail -20', true],
  ["flock /var/tmp/x nice -n 19 skein-job run --class test -- 'cargo' test --lib; echo done", true],
  ['skein-job run -- cargo test --lib > out.txt', true],
  ['skein-job run -- cargo build', false],
  // A shorter command is another job: `cargo test` is not the view's `cargo test --lib`.
  ['skein-job run -- cargo test', false],
  ['echo skein-job run is how', false],
]

for (const [command, isDrawn] of COMMANDS) {
  test(`${isDrawn ? 'finds' : 'finds no'} job for: ${command}`, async ($, on) => {
    engine(on)
    mock.clock(on, { now: NOW })
    on('fs.read', () => ({ value: JSON.stringify(F.JOBS_VIEW) }))
    const bash = slowBash(on)
    const call = $.tool.call({ tool: 'Bash', command, tool_use_id: 'c1' } as never)
    if (/skein-job run .*--/.test(command)) await bash.isReached
    const ui = await $.ui.mount(TOOL_ROW('c1', command, true))
    expect(rows(await ui.drawn()).length > 1).toBe(isDrawn)
    await ui.unmount()
    bash.release()
    await call
  })
}

// ------------------------------------------------------------------------------------------------
// A run of calls the transcript folds into one line
// ------------------------------------------------------------------------------------------------

type Call = { tool_use_id?: string; tool: string; input: unknown; output?: unknown }

const groupCall = (c: Call) => ({ isRunning: false, isErrored: false, isInterrupted: false, ...c })

async function drawGroup($: Engine, calls: Call[], isExpanded: boolean, id = 'grp') {
  const ui = await $.ui.mount({
    plugin: 'skein',
    surface: 'terminal',
    component: 'ToolGroup',
    requestId: id,
    props: { calls: calls.map(groupCall), isActive: false, isExpanded, isCallsOnly: true },
    viewport: { columns: 86, rows: 40 },
  })
  await ui.drawn()
  await ui.unmount()
}

async function drawRow($: Engine, c: Call) {
  const id = c.tool_use_id ?? 'r1'
  const ui = await $.ui.mount({
    plugin: 'skein',
    surface: 'terminal',
    component: 'ToolUse',
    requestId: id,
    props: { ...groupCall(c), tool_use_id: id, isExpanded: true },
    viewport: { columns: 86, rows: 40 },
  })
  const lines = rows(await ui.drawn())
  await ui.unmount()

  return lines
}

const READ = { tool: 'Read', input: { file_path: '/workspace/web/README.md' }, output: { type: 'text' } }
const JOB_INPUT = { command: 'skein-job run --cores 4 -- cargo test --lib' }

// What would make it fail: a group of skein's own calls left folded, so the person sees only the
// count line; a group's MCP calls matched by a name other than the tool_use block's own; or a group
// that mixes skein's calls with other tools, or holds a skein-job shell command, unfolded, which
// opens every call in it (the owner's 2026-11-02 complaint about clutter).
const GROUPS: Array<[string, Call[], boolean]> = [
  ['a skein tool among reads', [{ ...READ, tool_use_id: 'a1' }, { tool_use_id: 'a2', tool: name('resources'), input: {} }], false],
  ['two skein tools alone', [{ tool_use_id: 'a3', tool: name('resources'), input: {} }, { tool_use_id: 'a4', tool: name('top'), input: {} }], true],
  ["the mod's own clean", [{ tool_use_id: 'b1', tool: 'mcp__skein__clean', input: {} }], true],
  ['a waiting skein-job run', [{ tool_use_id: 'c1', tool: 'Bash', input: JOB_INPUT }], false],
  ['only reads and a plain shell command', [{ ...READ, tool_use_id: 'd1' }, { tool_use_id: 'd2', tool: 'Bash', input: { command: 'ls' } }], false],
  ["a skein tool this file does not draw", [{ tool_use_id: 'e1', tool: name('lane'), input: {} }], false],
  ["another server's tool of the same name", [{ tool_use_id: 'f1', tool: 'mcp__other__skein_resources', input: {} }], false],
  // A host that names no call's tool_use_id: unfolded, its rows could not be told from ctrl+o's.
  ['a skein tool with no tool_use_id', [{ tool: name('resources'), input: {} }], false],
]

for (const [what, calls, isUnfolded] of GROUPS) {
  test(`a folded group is ${isUnfolded ? 'unfolded' : 'left folded'}: ${what}`, async ($, on) => {
    const handed = engine(on)
    await drawGroup($, calls, false)
    expect((handed.at(-1) as { isExpanded: boolean }).isExpanded).toBe(isUnfolded)
  })
}

// What would make it fail: a row this file unfolded drawn as the engine's JSON (the hook treating
// every `isExpanded` row as ctrl+o's), or skein's lines drawn under the engine's own result rather
// than in its place. Watched: with the ToolUse hook passing every expanded row on, the row read
// `engine` alone and this failed.
test('a row of a group unfolded here draws skein’s lines in place of the JSON', async ($, on) => {
  const handed = engine(on)
  const call = { tool_use_id: 'u1', tool: name('resources'), input: {}, output: F.mcp(F.RESOURCES) }
  await drawGroup($, [call], false)
  expect(await drawRow($, call)).toEqual([
    'engine',
    '  ⎿  as of 17:49 UTC',
    '     disk     4.95 GB of 10 GB   fleet disk fine',
    '     cpu      1.3 cores',
    '     memory   2.31 GB of 13.1 GB   not slowed',
    '     procs    58 of 8192',
    '     nothing asked of this box',
    '     about 1.38 GB could be removed · /skein clean',
  ])
  // The engine's row is handed no result to draw, and the rest of its props as they were.
  const row = handed.at(-1) as Record<string, unknown>
  expect(row.output).toBe(undefined)
  expect(row.tool).toBe(name('resources'))
})

// What would make it fail: the calls this file unfolded remembered for good, so ctrl+o's view of
// the same call (the engine's own expanded group) draws readable too and the JSON is out of reach;
// or a row expanded where no group was folded (fullscreen ctrl+o, `--verbose`) drawn readable.
// Watched: with `markUnfolded(…, false)` left out for the engine's own expanded group, the second
// row read skein's lines and this failed.
test('a row ctrl+o or --verbose expanded draws the engine’s raw view', async ($, on) => {
  const handed = engine(on)
  const call = { tool_use_id: 'x1', tool: name('resources'), input: {}, output: F.mcp(F.RESOURCES) }

  // Never seen in a folded group.
  expect(await drawRow($, { ...call, tool_use_id: 'x0' })).toEqual(['engine'])

  // Unfolded here, then drawn again by the engine expanded (the non-fullscreen ctrl+o transcript).
  await drawGroup($, [call], false)
  expect((await drawRow($, call)).length > 1).toBe(true)
  await drawGroup($, [call], true)
  expect((handed.at(-1) as { isExpanded: boolean }).isExpanded).toBe(true)
  expect(await drawRow($, call)).toEqual(['engine'])
  expect((handed.at(-1) as Record<string, unknown>).output).toEqual(F.mcp(F.RESOURCES))

  // Folded again once the person leaves ctrl+o: readable again.
  await drawGroup($, [call], false)
  expect((await drawRow($, call))[1]).toBe('  ⎿  as of 17:49 UTC')
})

// What would make it fail: an errored call, or an answer of a shape this file does not read, drawn
// as an empty view in an unfolded row instead of by the engine. Watched: with the `isErrored` check
// removed, the errored row was handed no output and this failed.
test('an unfolded row the file cannot read is the engine’s, from the props it had', async ($, on) => {
  const handed = engine(on)
  const cases: Call[] = [
    { tool_use_id: 'y1', tool: name('top'), input: {}, output: F.mcp({ by: 'cpu', error: 'not-in-a-box-cgroup' }) },
    { tool_use_id: 'y2', tool: name('resources'), input: {}, output: F.mcp({ hello: 'world' }) },
    { ...READ, tool_use_id: 'y3' },
  ]
  // An errored call whose output is an answer this file could read: the error is the engine's.
  const errored = F.mcp(F.RESOURCES)
  await drawGroup($, [...cases, { tool_use_id: 'y4', tool: name('resources'), input: {}, output: errored }], false)
  for (const c of cases) {
    expect(await drawRow($, c)).toEqual(['engine'])
    expect((handed.at(-1) as Record<string, unknown>).output).toEqual(c.output)
  }
  const ui = await $.ui.mount({
    plugin: 'skein',
    surface: 'terminal',
    component: 'ToolUse',
    requestId: 'y4',
    props: { tool_use_id: 'y4', tool: name('resources'), input: {}, isRunning: false, isErrored: true, isInterrupted: false, isExpanded: true, output: errored },
  })
  expect(rows(await ui.drawn())).toEqual(['engine'])
  expect((handed.at(-1) as Record<string, unknown>).output).toEqual(errored)
  await ui.unmount()
})

// What would make it fail: unfolding or drawing a group touching what the model reads. Watched:
// with the Bash `tool.call` hook clearing its result's `stderr`, the skein-job comparison failed.
test('unfolding a group leaves what the model read as the tool gave it', async ($, on) => {
  engine(on)
  mock.clock(on, { now: NOW })
  on('fs.read', () => ({ value: JSON.stringify(F.JOBS_VIEW) }))
  const answer = F.mcp(F.RESOURCES)
  on('tool.call', (_, e) => ({ result: e.tool === 'Bash' ? F.JOB_BASH_OUTPUT : answer }))
  const ran = await $.tool.call({ tool: name('resources') as `mcp__${string}__${string}`, tool_use_id: 'm1' } as never)
  const job = await $.tool.call({ tool: 'Bash', command: JOB_INPUT.command, tool_use_id: 'm2' } as never)
  const calls = [
    { tool_use_id: 'm1', tool: name('resources'), input: {}, output: ran.result },
    { tool_use_id: 'm2', tool: 'Bash', input: JOB_INPUT, output: job.result },
  ]
  await drawGroup($, calls, false)
  for (const c of calls) await drawRow($, c)
  expect(ran.result).toEqual(answer)
  expect(job.result).toEqual(F.JOB_BASH_OUTPUT)
})
