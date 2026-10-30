// The world beneath the plugin, for the tests of the features that read the box: its files, its
// processes, its subagents and its screen. Each test builds one, says what is in it, and reads back
// what the plugin did to it. Nothing sits beneath the plugin under test, so each `on` here stands
// for the engine.

import { mock } from 'claude-code/testing'
import type { AgentInfo, On } from 'claude-code'

export const HOME = '/home/agent'
export const SESSION = 'sess-1'
export const RULES = `${HOME}/.local/state/skein/standing-rules.md`
export const WATCH = `${HOME}/.claude/sync/${SESSION}.watch`

export type World = {
  /** Files by absolute path, or by a path's end for the plugin's own (its root is the kit's). */
  files: Map<string, string>
  /** Modification times by absolute path, for a stat. */
  stats: Map<string, number>
  /** What `skein-mcp requests` prints. */
  requests: string
  agents: AgentInfo[]
  /** Every `$.ui.status` text, in order. */
  status: (string | undefined)[]
  /** Every `$.ui.invalidate` event, in order. */
  invalidated: string[]
  /** Every tool the plugin's calls reached, and every one the model made. */
  tools: string[]
  /** The argv of every process run. */
  ran: string[][]
}

export function world(on: On, env: Record<string, string> = {}): World {
  const w: World = {
    files: new Map(),
    stats: new Map(),
    requests: JSON.stringify({ result: { box: 'web-main', requests: [] }, said: '' }),
    agents: [],
    status: [],
    invalidated: [],
    tools: [],
    ran: [],
  }
  const find = (path: string): string | undefined => {
    if (w.files.has(path)) return w.files.get(path)
    for (const [k, v] of w.files) if (!k.startsWith('/') && path.endsWith(`/${k}`)) return v

    return undefined
  }

  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('turn.start', (_, e) => ({ turnId: e.turnId }))
  on('turn.complete', (_, e) => ({ text: e.answer }))
  on('command.register', (_, e) => ({ value: { command: e.name } }))
  on('tool.register', (_, e) => ({ value: { tool: `mcp__skein__${e.name}` } }))
  on('session.id', () => ({ value: SESSION }))
  on('fs.read', (_, e) => {
    const got = find(e.path)
    if (got === undefined) throw new Error(`ENOENT: ${e.path}`)

    return { value: got }
  })
  on('fs.write', (_, e) => {
    w.files.set(e.path, e.text)

    return { value: undefined }
  })
  on('fs.stat', (_, e) => {
    const at = w.stats.get(e.path)
    if (at === undefined) throw new Error(`ENOENT: ${e.path}`)

    return { value: { kind: 'file', size: 84, mtimeMs: at, isLink: false } }
  })
  on('process.run', (_, e) => {
    w.ran.push([...e.argv])
    const asked = e.argv.at(-1) === 'requests'

    return {
      value: {
        exitCode: asked ? 0 : 2,
        stdout: asked ? w.requests : '',
        stderr: '',
        isStdoutTruncated: false,
        isStderrTruncated: false,
      },
    }
  })
  on('agent.list', () => ({ value: w.agents }))
  on('ui.status', (_, e) => {
    w.status.push(e.text)

    return { value: undefined }
  })
  on('ui.invalidate', (_, e) => {
    w.invalidated.push(e.event)

    return { value: undefined }
  })
  mock.env(on, { HOME, ...env })

  return w
}

export async function start($: { session: { start: (e: { cwd: string; surface: 'terminal'; isInteractive: boolean }) => Promise<unknown> } }): Promise<void> {
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
}

/** A lease as the tracker's `claim` answers it. */
export function claimed(workItemId: string, readableId: string, title: string, at: number, ttlMs = 600000): string {
  return JSON.stringify({
    lease: {
      workItemId,
      epoch: 1,
      state: 'held',
      claimedAt: new Date(at).toISOString(),
      heartbeatAt: new Date(at).toISOString(),
      expiresAt: new Date(at + ttlMs).toISOString(),
    },
    briefing: { workItemId, readableId, title },
  })
}

/** A main-conversation turn's end. */
export const ENDED = { answer: 'done', durationMs: 10, isAborted: false, turnId: 't', reason: 'answer' } as const
