// skein's guidance in the system prompt (SKEIN-1365), through the whole module: read from the
// copies the plugin ships, added to the environment section, asked again only when it changed, and
// the shared-home rules added only where the instruction files lack them.

import { expect, test } from 'claude-code/testing'

import { start, world } from './world'
import type { World } from './world'

// The shapes of the three sources, cut down: the kit spec, the trust decision, the guide writer.
const SPEC = [
  'name: skein',
  'memory: |',
  '  ## skein workspace',
  '',
  '  Use `skein_top` to see what this box holds.',
  'commands:',
  '  startup: []',
].join('\n')
const TRUST = [
  '# Repos are the trust boundary',
  '',
  "**Decision.** skein's security model has three levels.",
  '',
  '1. **A hard wall around the person.**',
  '',
  '**Date.** 2026-10-29.',
].join('\n')
const WRITER = [
  "begin='<!-- skein:shared-home:start -->'",
  "end='<!-- skein:shared-home:end -->'",
  "cat >>\"$tmp\" <<'GUIDE'",
  '<!-- skein:shared-home:start -->',
  '## Skein shared working files',
  '',
  '- `$HOME/shared` is durable.',
  '<!-- skein:shared-home:end -->',
  'GUIDE',
].join('\n')

function sources(w: World, spec = SPEC): void {
  w.files.set('guide/kit-spec.yaml', spec)
  w.files.set('guide/repos-are-the-trust-boundary.md', TRUST)
  w.files.set('guide/agent-guide.sh', WRITER)
}

const FACTS = { model: 'm', promptModel: 'm', surfaces: [], tools: [], outputStyle: null, traits: [] }
const ENV = { name: 'env_info_simple', text: 'Working directory: /work' } as const

test('the guide rides at the end of the environment section, from the shipped sources', async ($, on) => {
  const w = world(on)
  sources(w)
  on('prompt.section', (_, e) => ({ text: e.text }))

  await start($)
  const got = (await $.prompt.section(ENV)).text ?? ''

  expect(got.startsWith('Working directory: /work\n\n# skein box guide\n\n## skein workspace\n\nUse `skein_top`')).toBe(true)
  expect(got).toContain("## What separates boxes\n\nskein's security model has three levels.\n\n1. **A hard wall around the person.**")
  expect(got).toContain('`remember_rule` tool')
  expect(got).not.toContain('commands:')
  expect(got).not.toContain('**Date.**')
  // The shared-home rules are not here: they go in the first message, and only where needed.
  expect(got).not.toContain('Skein shared working files')

  const other = await $.prompt.section({ name: 'memory', text: 'memory as core made it' })
  expect(other.text).toBe('memory as core made it')
})

test('a guide that did not change is not asked again; one that did is, from the next turn', async ($, on) => {
  const w = world(on)
  sources(w)
  on('prompt.section', (_, e) => ({ text: e.text }))

  await start($)
  await $.prompt.section(ENV)
  await $.turn.start({ text: 'one', turnId: 't1' })
  await $.turn.start({ text: 'two', turnId: 't2' })
  expect(w.invalidated).toEqual([])

  sources(w, SPEC.replace('see what this box holds', 'see what this box is charged for'))
  await $.turn.start({ text: 'three', turnId: 't3' })
  expect(w.invalidated).toEqual(['prompt.section'])
  expect((await $.prompt.section(ENV)).text).toContain('see what this box is charged for')

  // A subagent's turn reads nothing.
  sources(w)
  await $.turn.start({ text: 'lane', turnId: 't4', agentId: 'lane-1' })
  expect(w.invalidated).toEqual(['prompt.section'])
})

test('the shared-home rules are added only where no instruction file carries them', async ($, on) => {
  const w = world(on)
  sources(w)
  on('prompt.context', (_, e) => ({ blocks: e.blocks }))

  await start($)
  const carried = await $.prompt.context({
    blocks: [{ name: 'claudeMd', text: 'Contents of ~/.claude/CLAUDE.md:\n## Skein shared working files\n- …' }],
  })
  expect(carried.blocks.map(b => b.name)).toEqual(['claudeMd'])

  const missing = await $.prompt.context({ blocks: [{ name: 'claudeMd', text: 'Contents of CLAUDE.md:\n# direction' }] })
  expect(missing.blocks.map(b => b.name)).toEqual(['claudeMd', 'skeinSharedHome'])
  expect(missing.blocks[1]?.text).toBe('## Skein shared working files\n\n- `$HOME/shared` is durable.')
})

test('a plugin with none of its sources adds nothing', async ($, on) => {
  world(on)
  on('prompt.section', (_, e) => ({ text: e.text }))
  on('prompt.context', (_, e) => ({ blocks: e.blocks }))

  await start($)
  expect((await $.prompt.section(ENV)).text).toBe(ENV.text)
  expect((await $.prompt.context({ blocks: [] })).blocks).toEqual([])
})

test('a prompt with no environment section gets the guide as a section of its own', async ($, on) => {
  const w = world(on)
  sources(w)
  const sections = (ids: string[]) => ids.map(id => ({ id, text: id, scope: 'session' as const }))
  let core = sections(['intro', 'memory'])
  on('prompt.compose', () => ({ sections: core }))

  await start($)
  const without = await $.prompt.compose(FACTS)
  expect(without.sections.map(s => s.id)).toEqual(['intro', 'memory', 'skein:guide'])
  expect(without.sections.at(-1)?.text).toContain('# skein box guide')

  core = sections(['intro', 'env_info_simple'])
  const withEnv = await $.prompt.compose(FACTS)
  expect(withEnv.sections.map(s => s.id)).toEqual(['intro', 'env_info_simple'])
})
