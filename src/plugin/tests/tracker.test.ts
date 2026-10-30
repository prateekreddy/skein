// Tracker discipline (SKEIN-1366), through the whole module: what a session holds is heard from
// the tracker tools' answers, and a turn that ends with a quiet or expiring claim says so, once,
// and never releases anything.

import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

import { claimed, ENDED, start, WATCH, world } from './world'
import type { World } from './world'

const CLAIM = 'mcp__plugin_sync_sync__claim'
const COMPLETE = 'mcp__plugin_sync_sync__complete'
const NOW = Date.parse('2026-10-30T12:00:00Z')

/** The tracker beneath the plugin: `claim` answers a lease made at `at`, the rest answer `ok`. */
function tracker(on: On, w: World, at: () => number): void {
  on('tool.call', (_, e) => {
    w.tools.push(String(e.tool))
    const text =
      e.tool === CLAIM ? claimed(String((e as Record<string, unknown>).workItemId), 'SKEIN-7', 'Keep the lights on', at()) : '{"ok":true}'

    return { result: { content: [{ type: 'text', text }] }, text }
  })
}

test('a claim left untouched for three turns is mentioned once, and never released', async ($, on) => {
  const w = world(on)
  const clock = mock.clock(on, { now: NOW })
  tracker(on, w, () => clock.now())
  const rows = mock.session(on)
  w.stats.set(WATCH, NOW)

  await start($)
  await $.tool.call({ tool: CLAIM, projectId: 'p', workItemId: 'item-7' })
  await $.turn.complete(ENDED)
  await $.turn.complete(ENDED)
  expect(rows.appended()).toHaveLength(0)

  await $.turn.complete(ENDED)
  const said = rows.appended().map(r => JSON.stringify(r.message.content))
  expect(said).toHaveLength(1)
  expect(said[0]).toContain('you still hold SKEIN-7 in the tracker, and no tracker call has named it for 3 turns')
  expect(said[0]).toContain('skein does not release anything for you')
  expect(w.status.at(-1)).toBe('skein: SKEIN-7 untouched for 3 turns')

  await $.turn.complete(ENDED)
  expect(rows.appended()).toHaveLength(1)
  expect(w.status.at(-1)).toBe('skein: SKEIN-7 untouched for 4 turns')
  expect(w.tools.filter(t => /release|complete/.test(t))).toEqual([])
})

test('a tracker call that names the item counts as work on it', async ($, on) => {
  const w = world(on)
  const clock = mock.clock(on, { now: NOW })
  tracker(on, w, () => clock.now())
  const rows = mock.session(on)
  w.stats.set(WATCH, NOW)

  await start($)
  await $.tool.call({ tool: CLAIM, projectId: 'p', workItemId: 'item-7' })
  await $.turn.complete(ENDED)
  await $.turn.complete(ENDED)
  await $.tool.call({ tool: 'mcp__plugin_sync_sync__plane_comments', workItemId: 'item-7', body: 'progress' })
  await $.turn.complete(ENDED)

  expect(rows.appended()).toHaveLength(0)
  expect(w.status.at(-1)).toBe(undefined)
})

test('a completed item is no longer held, and is not mentioned', async ($, on) => {
  const w = world(on)
  const clock = mock.clock(on, { now: NOW })
  tracker(on, w, () => clock.now())
  const rows = mock.session(on)
  w.stats.set(WATCH, NOW)

  await start($)
  await $.tool.call({ tool: CLAIM, projectId: 'p', workItemId: 'item-7' })
  await $.tool.call({ tool: COMPLETE, workItemId: 'item-7', epoch: 1, evidence: 'abc123' })
  for (let i = 0; i < 4; i++) await $.turn.complete(ENDED)

  expect(rows.appended()).toHaveLength(0)
})

test('a claim nothing has renewed is warned about before it runs out', async ($, on) => {
  const w = world(on)
  const clock = mock.clock(on, { now: NOW })
  tracker(on, w, () => clock.now())
  const rows = mock.session(on)
  // The sync plugin last renewed it as it was claimed, and not since.
  w.stats.set(WATCH, NOW)

  await start($)
  await $.tool.call({ tool: CLAIM, projectId: 'p', workItemId: 'item-7' })
  await clock.advance(8 * 60 * 1000)
  await $.turn.complete(ENDED)

  const said = rows.appended().map(r => JSON.stringify(r.message.content))
  expect(said).toHaveLength(1)
  expect(said[0]).toContain('your claim on SKEIN-7 runs out in about 2 min, and nothing has renewed it since 12:00 UTC')
  expect(said[0]).toContain('Call held to see where you stand')
  expect(w.status.at(-1)).toBe("skein: SKEIN-7's claim runs out in about 2 min")
})

test('a claim the sync plugin keeps renewing is not warned about', async ($, on) => {
  const w = world(on)
  const clock = mock.clock(on, { now: NOW })
  tracker(on, w, () => clock.now())
  const rows = mock.session(on)
  w.stats.set(WATCH, NOW)

  await start($)
  await $.tool.call({ tool: CLAIM, projectId: 'p', workItemId: 'item-7' })
  await clock.advance(8 * 60 * 1000)
  // Renewed a minute ago: the credential file was rewritten then.
  w.stats.set(WATCH, NOW + 7 * 60 * 1000)
  await $.turn.complete(ENDED)

  expect(rows.appended()).toHaveLength(0)
})

test("a subagent's turn is not one of the box's turns", async ($, on) => {
  const w = world(on)
  const clock = mock.clock(on, { now: NOW })
  tracker(on, w, () => clock.now())
  const rows = mock.session(on)
  w.stats.set(WATCH, NOW)

  await start($)
  await $.tool.call({ tool: CLAIM, projectId: 'p', workItemId: 'item-7' })
  for (let i = 0; i < 4; i++) await $.turn.complete({ ...ENDED, agentId: 'lane-1' })

  expect(rows.appended()).toHaveLength(0)
})

// A claim made before skein's module loaded (SKEIN-1400): the session asks the tracker's `held`
// once itself, so the claim is known by its item, never by a hint to go and look.

const HELD = 'mcp__plugin_sync_sync__held'
const MINE = '9ed966bb-2e95-461d-8de1-883eb9763392'
const THEIRS = '5b14e31d-6ed0-4f5f-8b9c-8b7aca7603a3'
const WHY = 'mcp__plugin_sync_sync__why'
const PROJECT = '5b00388b-e528-4770-94f5-c0e9dccfcfb5'

/**
 * The tracker beneath the plugin, whose `held` answers one lease of this session's and one of
 * another's, and whose `why` names an item. `w.tools` gets each call, `why`'s with what it named.
 */
function holding(on: On, w: World): void {
  on('tool.call', (_, e) => {
    const input = e as Record<string, unknown>
    w.tools.push(e.tool === WHY ? `${WHY} ${String(input.projectId)} ${String(input.workItemId)}` : String(e.tool))
    const lease = (workItemId: string, thisSession: boolean) => ({
      workItemId,
      projectId: PROJECT,
      epoch: 1,
      state: 'held',
      claimedAt: new Date(NOW - 60000).toISOString(),
      heartbeatAt: new Date(NOW).toISOString(),
      expiresAt: new Date(NOW + 600000).toISOString(),
      thisSession,
    })
    const named = {
      item: {
        workItemId: String(input.workItemId),
        readableId: 'SKEIN-1400',
        title: 'The tracker ledger knows a claim by name',
      },
      claimable: false,
      reasons: [],
    }
    const text =
      e.tool === HELD
        ? JSON.stringify({ leases: [lease(MINE, true), lease(THEIRS, false)] })
        : e.tool === WHY
          ? JSON.stringify(named)
          : '{"ok":true}'

    return { result: { content: [{ type: 'text', text }] }, text }
  })
}

/** The tools the engine lists, read again on every ask, and the permission verdict for each. */
function engine(
  on: On,
  listed: { names: string[] },
  decision: 'allow' | 'ask' | ((tool: string) => 'allow' | 'ask') = 'allow',
): string[] {
  const checked: string[] = []
  on('tool.list', () => ({ value: listed.names.map(name => ({ name, description: '', mcp: name.startsWith('mcp__') })) }))
  on('tool.check', (_, e) => {
    checked.push(e.tool)

    return { decision: typeof decision === 'function' ? decision(e.tool) : decision }
  })

  return checked
}

test('a claim made before skein loaded is named in the compaction, and the model never called held', async ($, on) => {
  const w = world(on)
  mock.clock(on, { now: NOW })
  on('session.compact', () => ({ messages: [{ role: 'user', text: 'summary', toolUses: [] }] }))
  holding(on, w)
  engine(on, { names: ['Read', HELD] })
  w.stats.set(WATCH, NOW)

  await start($)
  const done = await $.session.compact({ trigger: 'manual', messages: [{ role: 'user', text: 'long', toolUses: [] }] })
  const kept = done.messages?.at(-1)?.text ?? ''

  expect(w.tools).toEqual([HELD])
  expect(kept).toContain(`- 9ed966bb (${MINE})`)
  expect(kept).not.toContain(THEIRS)
  expect(kept).not.toContain('This session has claimed tracker items')
})

test('held is asked once a session, at the first turn end it is listed by', async ($, on) => {
  const w = world(on)
  mock.clock(on, { now: NOW })
  holding(on, w)
  const listed = { names: ['Read'] }
  engine(on, listed)
  w.stats.set(WATCH, NOW)

  await start($)
  await $.turn.complete(ENDED)
  expect(w.tools).toEqual([])

  listed.names.push(HELD)
  for (let i = 0; i < 3; i++) await $.turn.complete(ENDED)
  expect(w.tools).toEqual([HELD])
})

test('held is not asked where the permission check would put it to someone', async ($, on) => {
  const w = world(on)
  mock.clock(on, { now: NOW })
  holding(on, w)
  const checked = engine(on, { names: [HELD] }, 'ask')
  w.stats.set(WATCH, NOW)

  await start($)
  for (let i = 0; i < 3; i++) await $.turn.complete(ENDED)

  expect(checked).toEqual([HELD])
  expect(w.tools).toEqual([])
})

// Named, not numbered (SKEIN-1401): an item `held` names only by its id is looked up once with the
// tracker's read-only `why`, on the same permission rule, and keeps its id where that is not allowed.

const COMPACT = { trigger: 'manual', messages: [{ role: 'user', text: 'long', toolUses: [] }] } as const

test("a claim made before skein loaded is named by the tracker's why, once, in the compaction", async ($, on) => {
  const w = world(on)
  mock.clock(on, { now: NOW })
  on('session.compact', () => ({ messages: [{ role: 'user', text: 'summary', toolUses: [] }] }))
  holding(on, w)
  engine(on, { names: [HELD, WHY] })
  w.stats.set(WATCH, NOW)

  await start($)
  for (let i = 0; i < 2; i++) await $.turn.complete(ENDED)
  const kept = (await $.session.compact(COMPACT)).messages?.at(-1)?.text ?? ''

  expect(w.tools).toEqual([HELD, `${WHY} ${PROJECT} ${MINE}`])
  expect(kept).toContain(`- SKEIN-1400: The tracker ledger knows a claim by name (${MINE})`)
})

test('an item why may not be asked about keeps its id, and why is not called', async ($, on) => {
  const w = world(on)
  mock.clock(on, { now: NOW })
  on('session.compact', () => ({ messages: [{ role: 'user', text: 'summary', toolUses: [] }] }))
  holding(on, w)
  const checked = engine(on, { names: [HELD, WHY] }, tool => (tool === WHY ? 'ask' : 'allow'))
  w.stats.set(WATCH, NOW)

  await start($)
  const kept = (await $.session.compact(COMPACT)).messages?.at(-1)?.text ?? ''

  // held's query, held's call (a call is checked again as it runs), then why's query, which asks.
  expect(checked).toEqual([HELD, HELD, WHY])
  expect(w.tools).toEqual([HELD])
  expect(kept).toContain(`- 9ed966bb (${MINE})`)
})
