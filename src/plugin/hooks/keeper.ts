// What a box must not forget across a compaction (SKEIN-1364): the tracker items it holds, its
// open requests to the owner, the lanes it runs, and the standing rules the owner gave it.
//
// A compaction replaces the conversation with a summary, and a summary keeps what the summarizer
// thought mattered. These four are facts the box can read again, so rather than hope they survive,
// the keeper reads them as the compaction lands and puts them after the summary, in one message.
//
// Where each comes from:
//   - held items: the tracker tools' own answers, kept by tracker.ts. Where the session claimed
//     something skein did not hear (before this module loaded), the message says to call `held`.
//   - open requests: `skein-mcp requests`, the one implementation `skein_requests` also answers from.
//   - lanes: the session's own running subagents, `$.agent.list()`.
//   - standing rules: a file the box owns, `~/.local/state/skein/standing-rules.md`, one rule per
//     line. `/skein-rule <rule>` and the `remember_rule` tool add to it; `/skein-rule` lists it.
//     `~/.local/state` is the box's own and outlives a restart (`src/box-session.sh`).

import type { EngineInterface, On } from 'claude-code'

import { heldNow, name } from './tracker'

type Engine = EngineInterface

/** The heading the kept message opens with, which also says where it came from. */
export const KEPT_HEADING = 'skein: what this box must not forget, read again as the conversation was compacted.'

/** The longest rule kept, and the most rules text re-read into a compaction. */
const RULE_MAX = 500
const RULES_MAX = 6000

export function register(on: On): void {
  // `claude plugin validate` refuses a second hook on one event without a matcher, and
  // identity.ts already holds `session.start`. Every session's `cwd` is absolute, so this matcher
  // selects every start: it is here to be a matcher, not to choose.
  on('session.start', { cwd: /^\// }, async ($, e, next) => {
    await $.command.register({
      name: 'skein-rule',
      description: 'Record a standing rule for this box, kept across compactions; with nothing after it, list them',
      argumentHint: '[rule]',
    })
    await $.tool.register({
      name: 'remember_rule',
      description:
        'Record a standing rule the owner gave this box, one that should hold from now on (for example ' +
        '"never push to main"). skein keeps every recorded rule in front of you after each compaction.',
      inputSchema: {
        type: 'object',
        properties: { rule: { type: 'string', description: 'The rule, in one line.' } },
        required: ['rule'],
      },
    })

    return next(e)
  })

  on('command.run', { command: 'skein-rule' }, async ($, e) => {
    const rule = e.args.trim()
    if (rule === '') {
      const rules = await readRules($)
      return {
        text: rules === '' ? 'no standing rules are recorded for this box.' : `standing rules for this box:\n${rules}`,
      }
    }
    return { text: await addRule($, rule) }
  })

  on('tool.call', { tool: 'mcp__skein__remember_rule' }, async ($, e) => {
    const rule = typeof e.rule === 'string' ? e.rule : ''

    return { result: await addRule($, rule) }
  })

  on('session.compact', async ($, e, next) => {
    const done = await next(e)
    if (e.trigger === 'precompute' || done.skip !== undefined) return done
    const kept = await keep($, e.agentId)
    if (kept === null) return done

    return { ...done, messages: [...done.messages, { role: 'user', text: kept, toolUses: [] }] }
  })
}

/** The message a compaction ends with, or null when there is nothing to keep. */
export async function keep($: Engine, agentId: string | undefined): Promise<string | null> {
  const parts: string[] = []

  const held = await heldNow($)
  const mine = held.items.filter(l => (agentId === undefined ? true : l.agentId === agentId))
  if (mine.length > 0) {
    const listed = mine.map(l => `- ${name(l)}${l.title !== null && l.readableId !== null ? `: ${l.title}` : ''} (${l.workItemId})`)
    parts.push(
      `Tracker items you hold, neither completed nor released:\n${listed.join('\n')}\n` +
        'Call held first to confirm them before you claim anything new.',
    )
  } else if (held.hasClaimed) {
    parts.push('This session has claimed tracker items. Call held first to see what you still hold before you claim anything new.')
  }

  const asked = await openRequests($)
  if (asked.length > 0) parts.push(`Your requests to the owner, still waiting for an answer:\n${asked.join('\n')}`)

  if (agentId === undefined) {
    const lanes = await runningLanes($)
    if (lanes.length > 0) parts.push(`Lanes you started that are still running:\n${lanes.join('\n')}`)
  }

  const rules = await readRules($)
  if (rules !== '') {
    const cut = rules.length > RULES_MAX ? `${rules.slice(0, RULES_MAX)}\n(more in ${await rulesPath($)})` : rules
    parts.push(`Standing rules the owner gave this box:\n${cut}`)
  }

  return parts.length === 0 ? null : [KEPT_HEADING, ...parts].join('\n\n')
}

/** The box's waiting requests, one line each, from `skein-mcp requests`; none when it cannot answer. */
async function openRequests($: Engine): Promise<string[]> {
  try {
    const ran = await $.process.run(['python3', `${$.plugin.root}/bin/skein-mcp`, 'requests'], { timeoutMs: 10000 })
    if (ran.exitCode !== 0) return []
    const parsed: unknown = JSON.parse(ran.stdout)
    const rows = (parsed as { result?: { requests?: unknown } }).result?.requests
    if (!Array.isArray(rows)) return []

    return rows
      .filter((r): r is Record<string, unknown> => typeof r === 'object' && r !== null && r.state === 'waiting')
      .map(r => `- ${String(r.queue)} ${String(r.id)}: ${what(r)}`)
  } catch {
    return []
  }
}

function what(r: Record<string, unknown>): string {
  if (r.queue === 'package') return `${String(r.kind ?? '')} ${Array.isArray(r.packages) ? r.packages.join(' ') : ''}`.trim()
  if (r.queue === 'write') return `${String(r.repo ?? '')}${r.reason ? `, ${String(r.reason)}` : ''}`
  return JSON.stringify(String(r.question ?? ''))
}

/** The main conversation's subagents that are still running. */
async function runningLanes($: Engine): Promise<string[]> {
  try {
    const agents = await $.agent.list()

    return agents
      .filter(a => a.status === 'running' && a.parentId === undefined)
      .map(a => `- ${a.name ?? a.description} (${a.type}, ${a.id})`)
  } catch {
    return []
  }
}

async function rulesPath($: Engine): Promise<string> {
  return `${(await $.env.get('HOME')) ?? ''}/.local/state/skein/standing-rules.md`
}

/** The recorded rules, as written; empty when there are none. */
export async function readRules($: Engine): Promise<string> {
  try {
    return (await $.fs.read(await rulesPath($))).trim()
  } catch {
    return ''
  }
}

/** Add one rule, and say what became of it. */
async function addRule($: Engine, given: string): Promise<string> {
  const rule = given.replace(/\s+/g, ' ').trim()
  if (rule === '') return 'nothing was recorded: the rule is empty.'
  if (rule.length > RULE_MAX) return `nothing was recorded: a rule is at most ${RULE_MAX} characters, and this one is ${rule.length}.`
  const path = await rulesPath($)
  const before = await readRules($)
  if (before.split('\n').includes(`- ${rule}`)) return `already recorded: ${rule}`
  await $.fs.write(path, `${before === '' ? '' : `${before}\n`}- ${rule}\n`)

  return `recorded. It is kept across compactions, in ${path}: ${rule}`
}
