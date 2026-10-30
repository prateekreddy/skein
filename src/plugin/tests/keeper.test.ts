// The compaction keeper (SKEIN-1364), through the whole module: a compaction ends with what the
// box must not forget, read as it lands, and the standing rules live in the box's own file.

import { expect, mock, test } from 'claude-code/testing'
import type { On, SessionMessage } from 'claude-code'

import { claimed, RULES, start, WATCH, world } from './world'
import type { World } from './world'

const CLAIM = 'mcp__plugin_sync_sync__claim'
const NOW = Date.parse('2026-10-30T12:00:00Z')
const SUMMARY: SessionMessage = { role: 'user', text: 'The conversation so far, summarized.', toolUses: [] }
const HISTORY = { trigger: 'manual' as const, messages: [{ role: 'user' as const, text: 'a long conversation', toolUses: [] }] }

/** The engine's compaction beneath the plugin: the whole conversation becomes one summary. */
function compactor(on: On): void {
  on('session.compact', () => ({ messages: [SUMMARY] }))
}

function tracker(on: On, w: World): void {
  on('tool.call', (_, e) => {
    w.tools.push(String(e.tool))
    const text = claimed(String((e as Record<string, unknown>).workItemId), 'SKEIN-7', 'Keep the lights on', NOW)

    return { result: { content: [{ type: 'text', text }] }, text }
  })
}

const TYPED = { args: '', origin: { kind: 'composer' }, presentation: { isFullscreen: false, columns: 80 } } as const

test('a compaction ends with the held items, waiting requests, running lanes and standing rules', async ($, on) => {
  const w = world(on)
  mock.clock(on, { now: NOW })
  compactor(on)
  tracker(on, w)
  w.files.set(RULES, '- never push to main\n- ask before deleting a branch\n')
  w.requests = JSON.stringify({
    result: {
      box: 'web-main',
      requests: [
        { queue: 'package', id: 'p-1', kind: 'apt', packages: ['libnss3'], state: 'waiting' },
        { queue: 'write', id: 'w-1', repo: 'thing/example', reason: 'a fix', state: 'granted' },
        { queue: 'question', id: 'q-1', question: 'Ship it today?', state: 'waiting' },
      ],
    },
    said: '',
  })
  w.agents = [
    { id: 'a1', description: 'needs-you lane', type: 'general-purpose', status: 'running', name: 'needs-you' },
    { id: 'a2', description: 'finished lane', type: 'general-purpose', status: 'completed' },
  ]

  await start($)
  await $.tool.call({ tool: CLAIM, projectId: 'p', workItemId: 'item-7' })
  const done = await $.session.compact(HISTORY)

  expect(done.messages?.[0]?.text).toBe(SUMMARY.text)
  const kept = done.messages?.at(-1)?.text ?? ''
  expect(done.messages).toHaveLength(2)
  expect(kept).toContain('- SKEIN-7: Keep the lights on (item-7)')
  expect(kept).toContain('Call held first')
  expect(kept).toContain('- package p-1: apt libnss3')
  expect(kept).toContain('- question q-1: "Ship it today?"')
  expect(kept).not.toContain('w-1')
  expect(kept).toContain('- needs-you (general-purpose, a1)')
  expect(kept).not.toContain('finished lane')
  expect(kept).toContain('- never push to main\n- ask before deleting a branch')
  expect(w.ran).toEqual([['python3', expect.stringMatching(/\/bin\/skein-mcp$/), 'requests']])
})

test('a session that claimed before skein heard it is told to call held first', async ($, on) => {
  const w = world(on)
  compactor(on)
  // The sync plugin has a credential for this session: something was claimed.
  w.stats.set(WATCH, NOW)

  await start($)
  const done = await $.session.compact(HISTORY)

  expect(done.messages?.at(-1)?.text).toContain(
    'This session has claimed tracker items. Call held first to see what you still hold',
  )
})

test('with nothing to keep, the compaction is left as the engine made it', async ($, on) => {
  world(on)
  compactor(on)

  await start($)
  const done = await $.session.compact(HISTORY)

  expect(done.messages).toEqual([SUMMARY])
})

test('/skein-rule records a rule in the box file, lists it, and the next compaction keeps it', async ($, on) => {
  const w = world(on)
  compactor(on)

  await start($)
  expect(w.registered).toEqual(expect.arrayContaining(['/skein-rule', 'mcp__skein__remember_rule']))
  const none = await $.command.run({ command: 'skein-rule', ...TYPED })
  expect(none.text).toBe('no standing rules are recorded for this box.')

  const said = await $.command.run({ command: 'skein-rule', ...TYPED, args: '  never push\n to main ' })
  expect(said.text).toContain('recorded.')
  expect(w.files.get(RULES)).toBe('- never push to main\n')

  const again = await $.command.run({ command: 'skein-rule', ...TYPED, args: 'never push to main' })
  expect(again.text).toBe('already recorded: never push to main')

  const listed = await $.command.run({ command: 'skein-rule', ...TYPED })
  expect(listed.text).toBe('standing rules for this box:\n- never push to main')

  const done = await $.session.compact(HISTORY)
  expect(done.messages?.at(-1)?.text).toContain('Standing rules the owner gave this box:\n- never push to main')
})

test('the remember_rule tool adds to the same file', async ($, on) => {
  const w = world(on)
  w.files.set(RULES, '- never push to main\n')

  await start($)
  const done = await $.tool.call({ tool: 'mcp__skein__remember_rule', rule: 'keep lanes under the lock' })

  expect(done.result).toContain('recorded.')
  expect(w.files.get(RULES)).toBe('- never push to main\n- keep lanes under the lock\n')
})
