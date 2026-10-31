// The lanes board of the skein pane (SKEIN-1440, step 3): each lane of the box, and where it stands
// on the way from made to done, from what skein and the session already record. Nothing here reads
// or runs anything: `band.tsx` hands it the lanes `skein-mcp lane list` reports, the agents panel's
// record of the session's subagents, and the jobs view.
//
// A lane record says only `created` today (SKEIN-1454), so a lane's later phases (handed back,
// verified, CI, merged) are not drawn: the board goes as far as the session can see, which is the
// subagent working in the lane, its heavy work in the fleet's queue, and the subagent's answer.

import type { SkeinAgentRun, SkeinBandJob, SkeinBandJobs, SkeinLaneRecord } from '../types'

/** Where a lane stands, as far as the session sees it. */
export type Stage = 'made' | 'work' | 'queued' | 'heavy' | 'done' | 'failed' | 'stopped'

/** One lane on the board. */
export type BoardLane = {
  name: string
  /** Its worktree. */
  path: string
  branch: string | null
  /** The tracker item: the one its subagent claimed, else the one its record names. */
  item: string | null
  /** The claimed item's title, when the claim said one. */
  title: string | null
  /** False while the subagent holds the item no longer. */
  isHeld: boolean
  /** Its record's phase; null for a worktree made by hand. */
  phase: string | null
  isMadeBySkein: boolean
  stage: Stage
  /** The session's latest subagent in it, if any. */
  run: SkeinAgentRun | null
  /** Its heavy job in the jobs view, while one is granted or waits. */
  job: SkeinBandJob | null
}

/** The stages the board draws along, in order, and how far each one is along it. */
export const STAGES = ['made', 'at work', 'heavy work', 'agent done'] as const

const REACH: Record<Stage, number> = { made: 1, work: 2, queued: 3, heavy: 3, done: 4, failed: 4, stopped: 4 }

export const GLYPH: Record<Stage, string> = { made: '○', work: '●', queued: '◌', heavy: '◐', done: '✓', failed: '✗', stopped: '■' }

/**
 * The board: every lane skein made, then each worktree a subagent of this session worked in that
 * skein has no record of. A lane's subagent is the latest one seen in its worktree.
 */
export function boardLanes(records: readonly SkeinLaneRecord[], runs: readonly SkeinAgentRun[], view: SkeinBandJobs | null): BoardLane[] {
  const latest = new Map<string, SkeinAgentRun>()
  for (const r of runs) {
    if (r.lane === null) continue
    const was = latest.get(r.lane.worktree)
    if (was === undefined || r.startedAt >= was.startedAt) latest.set(r.lane.worktree, r)
  }
  const recorded = new Set(records.map(l => l.path))
  const byHand: SkeinLaneRecord[] = [...latest.values()]
    .filter(r => r.lane !== null && !recorded.has(r.lane.worktree))
    .map(r => ({
      name: r.lane?.worktree.split('/').at(-1) ?? '',
      path: r.lane?.worktree ?? '',
      branch: r.lane?.branch ?? null,
      item: r.lane?.recordedItem ?? null,
      phase: r.lane?.phase ?? null,
      isMadeBySkein: r.lane?.isMadeBySkein ?? false,
    }))

  return [...records, ...byHand].map(l => {
    const run = latest.get(l.path) ?? null
    const job = (view?.jobs ?? []).find(j => j.cwd === l.path || j.cwd.startsWith(`${l.path}/`)) ?? null
    const claimed = run?.lane?.item ?? null

    return {
      name: l.name,
      path: l.path,
      branch: l.branch,
      item: claimed?.id ?? l.item,
      title: claimed?.title ? claimed.title : null,
      isHeld: claimed?.isHeld ?? true,
      phase: l.phase,
      isMadeBySkein: l.isMadeBySkein,
      stage: stageOf(run, job),
      run,
      job,
    }
  })
}

function stageOf(run: SkeinAgentRun | null, job: SkeinBandJob | null): Stage {
  if (run !== null && run.status !== 'running') return run.status
  if (job !== null) return job.state === 'granted' ? 'heavy' : 'queued'
  if (run === null) return 'made'
  if (run.lane?.lock === 'holding') return 'heavy'
  if (run.lane?.lock === 'waiting') return 'queued'

  return 'work'
}

/** Where the lane stands, in a few words. */
export function stageWords(l: BoardLane): string {
  switch (l.stage) {
    case 'made':
      return 'no agent yet'
    case 'work':
      return 'at work'
    case 'queued':
      return l.job !== null && l.job.position !== null ? `#${l.job.position} in the queue` : 'waits for the lock'
    case 'heavy':
      return 'heavy work'
    case 'done':
      return 'agent done'
    case 'failed':
      return 'agent failed'
    case 'stopped':
      return 'agent stopped'
  }
}

/** `━━━━━━━━────`: how far along the board's stages the lane is, in `n` cells. */
export function pipeline(stage: Stage, n: number): string {
  const filled = Math.round((REACH[stage] / STAGES.length) * n)

  return '━'.repeat(filled) + '─'.repeat(n - filled)
}
