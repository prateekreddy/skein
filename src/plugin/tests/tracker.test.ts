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
