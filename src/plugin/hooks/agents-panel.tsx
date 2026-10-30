// The agents panel: a pane listing every subagent of this session with its model, context fill,
// tokens, estimated cost, elapsed time and status, and for a subagent that works as a lane, the
// tracker item it holds, its worktree and branch, and whether it waits on the heavy-work lock.
// It opens by itself when a subagent starts; `/agents-info` shows or hides it (SKEIN-1361).
//
// Adapted from savvy-progress 1.2.1 by johnnyvizz (https://github.com/johnnyvizz/claude-kit,
// plugins/savvy-progress, commit c871c153ead54ad9547cc717d0f3e5a676c73986), MIT licensed:
// Copyright (c) 2026 johnnyvizz. The licence's full text is in `agents-panel.LICENSE` beside this
// file, which ships with it. Kept from it: following each subagent through `agent.spawn`,
// `turn.step` and `turn.complete`, the once-a-second clock, the pane's rows, and the toggles.
// Left out: the savvy-flow progress bar and its `progress` and `step` tools, the rewrite of
// `savvy-*` agents' prompts, the planned-task rows, the drawings and the translations. Changed:
// cost comes from skein's one price table (`prices.ts`), and a model it does not know is said to
// have no price rather than priced as another; the lane facts are skein's own (`lanes.ts`).

import { atom, read, update } from 'claude-code'
import type {
  AgentSpawnInput,
  AgentSpawnResult,
  EngineInterface,
  On,
  SessionStartInput,
  ToolCallInput,
  ToolCallResult,
  TurnCompleteInput,
  TurnStepInput,
  TurnStepResult,
} from 'claude-code'

import type { SkeinAgentLane, SkeinAgentRun, SkeinAgentsPanel, SkeinAgentStatus } from '../types'
import { claimedItem, commonGitDir, heavyLock, lockOf, worktreeIn, worktrees } from './lanes'
import type { Files, Worktree } from './lanes'
import { costOf } from './prices'

const agents = atom({ plugin: 'skein', key: 'agents' } as const, [], { shape: 'agents-1' })
const panel = atom(
  { plugin: 'skein', key: 'agentsPanel' } as const,
  { isCompact: false, isDoneCollapsed: false, isClosedByPerson: false },
  { shape: 'agents-panel-1' },
)
const now = atom({ plugin: 'skein', key: 'agentsNow' } as const, 0)

const PANE = 'skein-agents'
const TITLE = 'Agents'
// A claim names the item; a release or a completion lets it go.
const SYNC_TOOL = /^mcp__.*sync.*__(claim|release|complete)$/
const MAX_RUNS = 200
// How often the heavy lock is looked at while a lane runs: `/proc/locks`, and the open files and
// folder of each process at a lock.
const LOCK_EVERY_MS = 5000

// The repository's git directory, found once when the session starts, and its worktrees, read
// again as each subagent starts. Not drawn from, so not in `$.state`: a reload finds them again.
let gitDir: string | null = null
let known: Worktree[] = []

// `register.tsx` hooks the events every feature may watch, once, and calls these in its order:
// `onStart` as the session starts, and for each subagent `onSpawn`, `onStep`, `onTurnComplete` and
// `onToolCall`. Only watched: each sees what `next` settled to, and changes nothing.

/** As the session starts: the command, the repository, and the two clocks. */
export async function onStart($: EngineInterface, e: SessionStartInput): Promise<void> {
  await $.command.register({
    name: 'agents-info',
    description: 'Show or hide the panel of subagents: model, context, tokens, cost, time and status',
  })
  gitDir = await commonGitDir(files($), e.cwd)

  // Ticks the running agents' clocks; quiet when nothing runs.
  $.clock.every(1000, () => {
    void (async () => {
      const list = await read($, agents)
      if (!list.some(a => a.status === 'running')) return
      const at = await $.clock.now()
      await update($, now, () => at)
    })()
  })
  $.clock.every(LOCK_EVERY_MS, () => void refreshLock($))
}

/** A subagent started: its row, its lane if it has one, and the panel opened unless shut. */
export async function onSpawn($: EngineInterface, e: AgentSpawnInput, started: Extract<AgentSpawnResult, { deny?: undefined }>): Promise<void> {
  known = gitDir === null ? [] : await worktrees(files($), gitDir)
  const tree = (e.cwd ? worktreeIn(e.cwd, known) : null) ?? worktreeIn(e.prompt, known)
  const at = await $.clock.now()
  const run: SkeinAgentRun = {
    id: started.agentId ?? e.tool_use_id,
    agentId: started.agentId ?? null,
    type: e.subagentType,
    description: e.description,
    model: started.model,
    effort: null,
    status: 'running',
    startedAt: at,
    endedAt: null,
    contextTokens: 0,
    contextMax: windowOf(started.model),
    tokens: 0,
    costUsd: 0,
    isUnpriced: false,
    steps: 0,
    lane: tree === null ? null : laneAt(tree, false),
  }
  await update($, agents, list => [...list.filter(a => a.id !== run.id), run].slice(-MAX_RUNS))
  await update($, now, () => at)
  const p = await read($, panel)
  if (!p.isClosedByPerson) await openPane($)
  if (run.lane) void refreshLock($)
}

/** Each model request of a subagent: its context, tokens and cost. */
export async function onStep($: EngineInterface, e: TurnStepInput, result: TurnStepResult): Promise<void> {
  const agentId = e.agentId
  const usage = result.usage
  if (!agentId || !usage) return

  const model = usage.model || e.model
  const cost = costOf(model, usage)
  const prompt = usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens
  await update($, agents, list =>
    list.map((a): SkeinAgentRun =>
      a.agentId !== agentId
        ? a
        : {
            ...a,
            model,
            effort: typeof e.effort === 'string' ? e.effort : a.effort,
            status: 'running',
            endedAt: null,
            contextTokens: prompt + usage.output_tokens,
            contextMax: windowOf(model),
            tokens: a.tokens + prompt + usage.output_tokens,
            costUsd: a.costUsd + (cost ?? 0),
            isUnpriced: a.isUnpriced || cost === null,
            steps: a.steps + 1,
          },
    ),
  )
}

/** A subagent's turn ended: answered, interrupted, or failed. */
export async function onTurnComplete($: EngineInterface, e: TurnCompleteInput): Promise<void> {
  const agentId = e.agentId
  if (!agentId) return
  const at = await $.clock.now()
  const status: SkeinAgentStatus = e.reason === 'answer' ? 'done' : e.reason === 'aborted' ? 'stopped' : 'failed'
  const usage = e.usage
  await update($, agents, list =>
    list.map((a): SkeinAgentRun => {
      if (a.agentId !== agentId) return a
      // A run whose requests went unseen still gets the turn's own sum.
      if (a.steps > 0 || !usage) return { ...a, status, endedAt: at }
      const cost = costOf(usage.model || a.model, usage)

      return {
        ...a,
        status,
        endedAt: at,
        model: usage.model || a.model,
        tokens: usage.input_tokens + usage.output_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens,
        costUsd: cost ?? 0,
        isUnpriced: cost === null,
      }
    }),
  )
  await update($, now, () => at)
}

/** A subagent's own tool call: the item it claims, and the worktree it works in. */
export async function onToolCall($: EngineInterface, e: ToolCallInput, answered: ToolCallResult): Promise<void> {
  const agentId = e.agentId
  if (!agentId) return
  await learnFrom($, agentId, String(e.tool), JSON.stringify(e), answered.text)
}

export function register(on: On): void {
  on('command.run', { command: 'agents-info' }, async ($, e) => {
    // `/agents-info open` opens the panel, or brings it forward, without toggling it.
    const isOpen = (await $.ui.panes()).some(p => p.id === PANE)
    if ((e.args ?? '').trim() === 'open' || !isOpen) {
      await openPane($)
      await update($, panel, p => ({ ...p, isClosedByPerson: false }))

      return { text: 'Agents panel opened.' }
    }
    await $.ui.close({ id: PANE })
    await update($, panel, p => ({ ...p, isClosedByPerson: true }))

    return { text: 'Agents panel closed. It stays closed until you run /agents-info again.' }
  })

  on('ui.close', { id: PANE }, async ($, e, next) => {
    const closed = await next(e)
    if (e.id === PANE && e.origin.kind === 'person') {
      await update($, panel, p => ({ ...p, isClosedByPerson: true }))
    }

    return closed
  }).catch(($, e, next) => next(e))

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Text, Button } = $.ui.resolve(e)
    const list = await read($, agents)
    const p: SkeinAgentsPanel = await read($, panel)
    const at = Math.max(await read($, now), ...list.map(a => a.startedAt), 0)

    const running = list.filter(a => a.status === 'running').reverse()
    const finished = list.filter(a => a.status !== 'running').reverse()
    const t = totals(list, at)
    const cols = Math.max(24, e.props.bodyColumns || 40)
    const barW = Math.max(6, Math.min(20, cols - 34))

    const toggleCompact = (
      <Button
        key="compact"
        label={p.isCompact ? 'Expand' : 'Collapse'}
        plain
        onPress={() => update($, panel, prev => ({ ...prev, isCompact: !prev.isCompact }))}
      />
    )
    const toggleDone = (
      <Button
        key="done"
        label={`${p.isDoneCollapsed ? '▸' : '▾'} Finished · ${finished.length}`}
        plain
        onPress={() => update($, panel, prev => ({ ...prev, isDoneCollapsed: !prev.isDoneCollapsed }))}
      />
    )

    const row = (a: SkeinAgentRun) => {
      const ctx = ctxOf(a)
      const meta = [a.type, modelName(a.model), a.effort].filter(Boolean).join(' · ')
      return (
        <Box key={a.id} flexDirection="column" marginBottom={1}>
          <Box flexDirection="row" gap={1}>
            <Text color={STATUS_COLOR[a.status]}>{STATUS_GLYPH[a.status]}</Text>
            <Text bold wrap="truncate-end">
              {a.description || a.type}
            </Text>
          </Box>
          <Text dimColor wrap="truncate-end">
            {'  '}
            {meta} · {STATUS_WORD[a.status]}
          </Text>
          <Text wrap="truncate-end">
            {'  '}
            <Text dimColor>{ctxBar(ctx, barW)}</Text>
            <Text dimColor>
              {' '}
              ctx {ctx}% · {fmtTokens(a.contextTokens)} · {fmtTokens(a.tokens)} tokens · {fmtCost(a.costUsd, a.isUnpriced)} ·{' '}
              {fmtTime(elapsed(a, at))}
            </Text>
          </Text>
          {a.lane ? laneLines(a.lane).map(line => (
            <Text key={line} dimColor wrap="truncate-end">
              {'  '}
              {line}
            </Text>
          )) : null}
        </Box>
      )
    }

    return (
      <Box flexDirection="column">
        <Box flexDirection="row" justifyContent="space-between">
          <Text dimColor>
            {fmtCost(t.cost, t.isUnpriced)} · {fmtTokens(t.tokens)} tokens · {fmtTime(t.time)}
          </Text>
          {toggleCompact}
        </Box>
        {list.length > 0 ? (
          <Text dimColor>Cost is an estimate: cache writes are priced at the 5-minute rate, so 1-hour cache writes read low.</Text>
        ) : null}
        {t.isUnpriced ? <Text dimColor>+ marks a model skein has no price for: the real cost is higher.</Text> : null}
        {p.isCompact ? (
          <Text wrap="truncate-end">
            {[...running, ...finished].map(a => (
              <Text key={a.id} color={STATUS_COLOR[a.status]}>
                {STATUS_GLYPH[a.status]}{' '}
              </Text>
            ))}
          </Text>
        ) : (
          <Box flexDirection="column" marginTop={1}>
            {list.length === 0 ? <Text dimColor>No subagents yet.</Text> : null}
            {running.length > 0 ? <Text dimColor>Running · {running.length}</Text> : null}
            {running.map(row)}
            {finished.length > 0 ? toggleDone : null}
            {p.isDoneCollapsed ? null : finished.map(row)}
          </Box>
        )}
      </Box>
    )
  })
}

// --- skein's lane facts

function files($: EngineInterface): Files {
  return {
    read: path => $.fs.read(path),
    list: path => $.fs.list(path),
    realPath: async path => {
      const stat = await $.fs.stat(path, { resolve: true }).catch(() => null)

      return stat?.realPath ?? null
    },
  }
}

const laneAt = (tree: Worktree, isSeenAtWork: boolean): SkeinAgentLane => ({
  worktree: tree.path,
  isSeenAtWork,
  branch: tree.branch,
  item: null,
  lock: null,
})

async function learnFrom($: EngineInterface, agentId: string, tool: string, call: string, answer: string | undefined): Promise<void> {
  const list = await read($, agents)
  const run = list.find(a => a.agentId === agentId)
  if (!run) return

  let lane = run.lane
  if (!lane?.isSeenAtWork) {
    const tree = worktreeIn(call, known)
    if (tree !== null) lane = tree.path === lane?.worktree ? { ...lane, isSeenAtWork: true } : laneAt(tree, true)
  }
  const sync = SYNC_TOOL.exec(tool)?.[1]
  if (lane !== null && sync === 'claim' && answer !== undefined) {
    const item = claimedItem(answer)
    if (item !== null) lane = { ...lane, item: { ...item, isHeld: true } }
  } else if (lane?.item && (sync === 'release' || sync === 'complete')) {
    lane = { ...lane, item: { ...lane.item, isHeld: false } }
  }
  if (lane === run.lane) return

  const next = lane
  await update($, agents, all => all.map(a => (a.agentId === agentId ? { ...a, lane: next } : a)))
}

async function refreshLock($: EngineInterface): Promise<void> {
  const list = await read($, agents)
  if (!list.some(a => a.status === 'running' && a.lane !== null)) return
  const at = await heavyLock(files($))
  const changed = list.some(a => a.lane !== null && a.lane.lock !== (a.status === 'running' ? lockOf(a.lane.worktree, at) : null))
  if (!changed) return

  await update($, agents, all =>
    all.map(a =>
      a.lane === null ? a : { ...a, lane: { ...a.lane, lock: a.status === 'running' ? lockOf(a.lane.worktree, at) : null } },
    ),
  )
}

function laneLines(lane: SkeinAgentLane): string[] {
  const item = lane.item === null ? 'no tracker item claimed' : `${lane.item.id}${lane.item.title ? ` ${lane.item.title}` : ''}${lane.item.isHeld ? '' : ' (no longer held)'}`
  const where = `${lane.worktree}${lane.branch ? ` on ${lane.branch}` : ''}`
  const lock = lane.lock === 'waiting' ? ' · waiting for the heavy-work lock' : lane.lock === 'holding' ? ' · holds the heavy-work lock' : ''

  return [item, `${where}${lock}`]
}

async function openPane($: EngineInterface): Promise<void> {
  const at = await $.clock.now()
  await update($, now, () => at)
  await $.ui.open({ id: PANE, title: TITLE })
}

// --- figures

const STATUS_GLYPH: Record<SkeinAgentStatus, string> = { running: '●', done: '✓', failed: '✗', stopped: '■' }
const STATUS_WORD: Record<SkeinAgentStatus, string> = { running: 'running', done: 'done', failed: 'failed', stopped: 'stopped' }
const STATUS_COLOR: Record<SkeinAgentStatus, string> = { running: 'cyan', done: 'green', failed: 'red', stopped: 'yellow' }

// Haiku 4.5 and the Claude 3 models have a 200K window; the rest, 1M.
const windowOf = (model: string): number => (/haiku-4|claude-3/i.test(model) ? 200_000 : 1_000_000)

const modelName = (id: string): string => {
  const m = /(fable|mythos|opus|sonnet|haiku)-(\d+)(?:-(\d{1,2})(?!\d))?/i.exec(id)
  const [, family = '', major = '', minor] = m ?? []
  if (!family) return id.replace(/^claude-/, '').replace(/\[.*\]$/, '') || '-'
  return `${family.charAt(0).toUpperCase()}${family.slice(1).toLowerCase()} ${major}${minor ? '.' + minor : ''}`
}

const ctxOf = (a: SkeinAgentRun): number => (a.contextMax ? Math.min(100, Math.round((a.contextTokens / a.contextMax) * 100)) : 0)

const ctxBar = (pct: number, width: number): string => {
  const filled = Math.round((width * pct) / 100)
  return '█'.repeat(filled) + '░'.repeat(Math.max(0, width - filled))
}

const fmtTokens = (n: number): string => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${Math.round(n / 1e3)}k` : `${Math.round(n)}`)

// `≈$0.42`, with a `+` when a model had no price, so the figure is a floor.
const fmtCost = (usd: number, isUnpriced: boolean): string => `≈$${usd < 10 ? usd.toFixed(2) : usd.toFixed(1)}${isUnpriced ? '+' : ''}`

const fmtTime = (ms: number): string => {
  const s = Math.max(0, Math.round(ms / 1000))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const ss = String(s % 60).padStart(2, '0')
  return h ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`
}

const elapsed = (a: SkeinAgentRun, at: number): number => (a.endedAt ?? Math.max(at, a.startedAt)) - a.startedAt

function totals(list: readonly SkeinAgentRun[], at: number) {
  const cost = list.reduce((s, a) => s + a.costUsd, 0)
  const tokens = list.reduce((s, a) => s + a.tokens, 0)
  const isUnpriced = list.some(a => a.isUnpriced)
  if (list.length === 0) return { cost, tokens, isUnpriced, time: 0 }
  const start = Math.min(...list.map(a => a.startedAt))
  const end = Math.max(...list.map(a => a.endedAt ?? Math.max(at, a.startedAt)))
  return { cost, tokens, isUnpriced, time: end - start }
}
