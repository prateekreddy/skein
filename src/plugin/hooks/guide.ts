// skein's guidance, in the system prompt and kept current (SKEIN-1365).
//
// Three texts, each read from a copy of its one source that the plugin ships beside this module
// (`PLUGIN_FILES` in `src/runtime.rs` takes the bytes of the source itself, so nothing here is a
// second statement of it):
//
//   - the box guide: the `memory:` block of the kit, `src/kit/spec.yaml`;
//   - what separates boxes: the three levels of `docs/decisions/repos-are-the-trust-boundary.md`;
//   - the shared-home rules: the block `src/store/agent-guide.sh` writes into the runtime's
//     instruction file.
//
// Where they go, and why there:
//
//   - The guide and the trust levels ride at the end of the engine's `env_info_simple` section,
//     through `prompt.section`. The engine caches a section's answer and asks again only after
//     `$.ui.invalidate("prompt.section")`, so the text is part of the cached prompt and costs
//     nothing per turn until it changes. A plugin's own section added through `prompt.compose`
//     is composed once per session and never asked again (measured on 2.1.314), so it could not
//     follow a change; it is only the fallback for a prompt with no `env_info_simple`.
//   - The shared-home rules go in through `prompt.context`, as a block of the first message, and
//     only when the instruction files the session loaded do not already carry them: a Claude box's
//     kit writes them into `~/.claude/CLAUDE.md`, and a second copy would be paid for on every
//     request. Codex boxes read that file and nothing here.
//   - Nothing that changes from turn to turn is put in either. The resource asks do change, so
//     they stay where they are: `skein-resources` puts each standing ask beside the prompt it
//     concerns, after the cached prefix. The guide only says where they come from.
//
// The texts are read when the prompt is first built and again as each main-conversation turn
// ends; a change is invalidated there and reaches the model on the next turn.

import { atom, read, update } from 'claude-code'
import type { EngineInterface, On, TurnCompleteInput } from 'claude-code'

import type { SkeinGuide } from '../types'

type Engine = EngineInterface

export const guide = atom({ plugin: 'skein', key: 'guide' } as const, null, {
  shape: 'guide-1',
})

/** The engine section skein's guidance is added to. */
export const CARRIER = 'env_info_simple'

/** skein's own section, where a prompt has no `CARRIER`. */
export const OWN_SECTION = 'skein:guide'

/** The context block the shared-home rules go in, where no instruction file carries them. */
export const SHARED_HOME_BLOCK = 'skeinSharedHome'

const MARK_START = '<!-- skein:shared-home:start -->'
const MARK_END = '<!-- skein:shared-home:end -->'

/**
 * As a turn ends, from the one `turn.complete` hook register.tsx holds (SKEIN-1387): read the
 * texts again after a main-conversation turn, and invalidate the section when they changed, so
 * the change reaches the model on the next turn.
 */
export async function onTurnComplete($: Engine, e: TurnCompleteInput): Promise<void> {
  if (e.agentId === undefined && (await refresh($))) $.ui.invalidate('prompt.section')
}

export function register(on: On): void {
  on('prompt.section', { name: CARRIER }, async ($, e, next) => {
    const done = await next(e)
    const g = await current($)
    if (g.section === '') return done

    return { text: done.text === null ? g.section : `${done.text}\n\n${g.section}` }
  })

  on('prompt.compose', async ($, e, next) => {
    const done = await next(e)
    const g = await current($)
    if (g.section === '' || done.sections.some(s => s.id === CARRIER || s.id === OWN_SECTION)) return done

    return { sections: [...done.sections, { id: OWN_SECTION, text: g.section, scope: 'session' }] }
  })

  on('prompt.context', async ($, e, next) => {
    const done = await next(e)
    const g = await current($)
    if (g.sharedHome === '') return done
    const files = done.instructionFiles ?? e.instructionFiles
    const claudeMd = done.blocks.find(b => b.name === 'claudeMd')?.text ?? ''
    const carried =
      claudeMd.includes(g.sharedHomeHeading) || (files ?? []).some(f => f.content.includes(g.sharedHomeHeading))
    if (carried || done.blocks.some(b => b.name === SHARED_HOME_BLOCK)) return done

    return { ...done, blocks: [...done.blocks, { name: SHARED_HOME_BLOCK, text: g.sharedHome }] }
  })
}

/** The guidance as last read, reading it first when nothing has been. */
async function current($: Engine): Promise<SkeinGuide> {
  const kept = await read($, guide)
  if (kept !== null) return kept
  await refresh($)

  return (await read($, guide)) ?? (await compose($))
}

/** Read the texts again; true when they changed. */
async function refresh($: Engine): Promise<boolean> {
  const next = await compose($)
  const before = await read($, guide)
  if (before !== null && same(before, next)) return false
  await update($, guide, () => next)

  return before !== null
}

function same(a: SkeinGuide, b: SkeinGuide): boolean {
  return a.section === b.section && a.sharedHome === b.sharedHome && a.sharedHomeHeading === b.sharedHomeHeading
}

/** The guidance, from the copies the plugin ships; a part whose copy is missing is left out. */
export async function compose($: Engine): Promise<SkeinGuide> {
  const root = $.plugin.root
  const kit = kitMemory(await text($, `${root}/guide/kit-spec.yaml`))
  const trust = trustLevels(await text($, `${root}/guide/repos-are-the-trust-boundary.md`))
  const shared = sharedHome(await text($, `${root}/guide/agent-guide.sh`))

  const parts: string[] = []
  if (kit !== '') parts.push(`# skein box guide\n\n${kit}`)
  if (trust !== '') parts.push(`## What separates boxes\n\n${trust}`)
  if (parts.length > 0) {
    parts.push(
      '## What skein asks of this box\n\n' +
        'When skein needs this box to use less disk, memory or processes, it says so beside your prompt and ' +
        'may hold one command to tell you; `skein_resources` shows what it asks now. When the owner gives this ' +
        'box a rule that should hold from now on, record it with the `remember_rule` tool: skein keeps every ' +
        'recorded rule in front of you after each compaction.',
    )
  }

  return {
    section: parts.join('\n\n'),
    sharedHome: shared,
    sharedHomeHeading: shared.split('\n').find(l => l.startsWith('#')) ?? '',
  }
}

async function text($: Engine, path: string): Promise<string> {
  try {
    return await $.fs.read(path)
  } catch {
    return ''
  }
}

/** The kit's `memory: |` block, as the kit writes it: its lines, two spaces of indent removed. */
export function kitMemory(spec: string): string {
  const lines = spec.split('\n')
  const at = lines.findIndex(l => /^memory:\s*\|\s*$/.test(l))
  if (at < 0) return ''
  const body: string[] = []
  for (const l of lines.slice(at + 1)) {
    if (l.trim() !== '' && !l.startsWith('  ')) break
    body.push(l.slice(2))
  }

  return body.join('\n').trim()
}

/** The decision's three levels: from its `**Decision.**` line up to its `**Date.**`. */
export function trustLevels(record: string): string {
  const from = record.indexOf('**Decision.**')
  const to = record.indexOf('**Date.**')
  if (from < 0 || to <= from) return ''

  return record.slice(from + '**Decision.**'.length, to).trim()
}

/** The shared-home rules, between the markers the kit's guide writer puts round them. */
export function sharedHome(script: string): string {
  const from = script.lastIndexOf(MARK_START)
  const to = script.lastIndexOf(MARK_END)
  if (from < 0 || to <= from) return ''

  return script.slice(from + MARK_START.length, to).trim()
}
