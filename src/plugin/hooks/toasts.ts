// A toast for the moment a coordinator otherwise polls for (SKEIN-1440, step 4): CI's verdict on a
// `ci/<sha>` branch this box pushed, one toast per verdict.
//
// The flow it watches is docs/decisions/master-moves-only-to-a-green-commit.md: a merge is pushed to
// `ci/<short-sha>`, CI runs there, and `master` moves once it passes. So:
//
// * **Which branches.** The ones this box pushed, and only those: a `refs/remotes/origin/ci/*` ref
//   whose reflog says `update by push`. A push leaves that ref behind (origin's fetch refspec maps
//   it); a fetch of another box's `ci/` branch leaves one too, but its reflog says `fetch`, and a
//   `push --delete` once `master` has moved takes the ref and its reflog away. The reflogs are read
//   as files, `<git dir>/logs/refs/remotes/origin/ci/<name>`, so looking runs nothing.
// * **When it asks GitHub.** Only while such a branch has no verdict yet, once a minute, with `gh run
//   list` on the box's own repo (origin's URL). A branch whose runs all ended is not asked again,
//   nor one pushed more than `GIVE_UP_MS` ago that never got a run. A box where gh is not signed in
//   (`gh` exits 4) stays silent for the rest of the session.
// * **Once.** Every verdict toasted is kept in `$.store` by its runs' ids and attempts, so a reload,
//   a resumed session or a second session of the box does not toast it again; a re-run is a new
//   attempt, and a verdict of its own.
//
// The check is started at the session's start, after a Bash call that pushed, and at each turn's
// end (a reload loses the module's timer, and the turn's end is when the box next looks up); each
// start reads only local files, and the timer stops once nothing is waiting. A session nobody
// watches (`-p`) toasts nothing and asks nothing.
//
// A lane handed back for verifying has no toast: a lane record carries one phase, `created`
// (`LANE_PHASE_CREATED` in `bin/skein-mcp`), and nothing records that a lane ended.

import type { EngineInterface, SessionStartInput, Timer, ToolCallInput, TurnCompleteInput } from 'claude-code'

import { commonGitDir } from './lanes'

type Engine = EngineInterface

/** How often GitHub is asked while a `ci/` branch waits for its verdict. */
export const POLL_MS = 60_000

/** A pushed branch with no run this long after its push is not asked about again. */
export const GIVE_UP_MS = 3 * 60 * 60 * 1000

/** How long a CI toast stays: longer than the default, it being the one line the person waits for. */
export const TOAST_MS = 12_000

/** `$.store` keys: the verdicts toasted, by their runs; the pushes whose verdict is in. */
export const TOASTED_KEY = 'ci.toasted'
export const DONE_KEY = 'ci.done'

/** The verdicts and pushes kept: far more than one box pushes in the store's life. */
const KEEP = 200

/** gh's exit status when it is not signed in. */
const GH_NO_AUTH = 4

let poller: Timer | null = null
let ghSilent = false
// Set at the session's start; a reload keeps the session it was, and `-p` never reloads.
let isWatched = true

/** A `ci/` branch this box pushed: its name, its commit, and when it was pushed (ms). */
export type Pushed = { branch: string; sha: string; pushedAt: number }

/** One workflow run on a branch, as `gh run list --json` gives it. */
export type Run = { id: number; attempt: number; status: string; conclusion: string; headSha: string; workflow: string }

// ------------------------------------------------------------------------------------------------
// The parts register.tsx calls
// ------------------------------------------------------------------------------------------------

export async function onStart($: Engine, e: SessionStartInput): Promise<void> {
  ghSilent = false
  isWatched = e.isInteractive
  stop()
  await kick($)
}

/** After a Bash call: one that pushed may have pushed a `ci/` branch. */
export async function onToolCall($: Engine, e: ToolCallInput): Promise<void> {
  if (e.tool !== 'Bash' || typeof e.command !== 'string' || !/\bgit\b[^\n]*\bpush\b/.test(e.command)) return
  await kick($)
}

export async function onTurnComplete($: Engine, _e: TurnCompleteInput): Promise<void> {
  if (poller === null) await kick($)
}

// ------------------------------------------------------------------------------------------------
// The check
// ------------------------------------------------------------------------------------------------

/** Looks once, and keeps looking every `POLL_MS` while a pushed branch waits for its verdict. */
async function kick($: Engine): Promise<void> {
  if (!isWatched) return
  const waiting = await check($)
  if (waiting && poller === null) {
    poller = $.clock.every(POLL_MS, () => {
      void check($)
        .then(still => {
          if (!still) stop()
        })
        .catch(() => {})
    })
  }
  if (!waiting) stop()
}

function stop(): void {
  poller?.cancel()
  poller = null
}

/**
 * Asks GitHub about every pushed branch still without a verdict, toasts each verdict that is in and
 * not yet toasted, and says whether any branch still waits.
 */
export async function check($: Engine): Promise<boolean> {
  if (ghSilent) return false
  const now = await $.clock.now()
  const done = new Set(await strings($, DONE_KEY))
  const pending = (await pushedBranches($)).filter(p => !done.has(doneKey(p)) && now - p.pushedAt < GIVE_UP_MS)
  if (pending.length === 0) return false
  const repo = await githubRepo($)
  if (repo === null) return false
  let waiting = false
  for (const p of pending) {
    const listed = await gh($, ['run', 'list', '--repo', repo, '--branch', p.branch, '--json', 'databaseId,attempt,status,conclusion,headSha,workflowName', '--limit', '20'])
    if (listed === null) {
      if (ghSilent) return false
      waiting = true
      continue
    }
    const runs = runsOf(listed).filter(r => p.sha.startsWith(r.headSha) || r.headSha.startsWith(p.sha))
    if (runs.length === 0 || runs.some(r => r.status !== 'completed')) {
      waiting = true
      continue
    }
    const key = verdictKey(runs)
    if (!(await strings($, TOASTED_KEY)).includes(key)) {
      const failed = runs.filter(r => !PASSED.has(r.conclusion))
      const jobs = failed.length === 0 ? [] : await failedJobs($, repo, failed)
      await remember($, TOASTED_KEY, key)
      $.ui.toast(verdictText(p.branch, runs, jobs), { timeoutMs: TOAST_MS })
    }
    await remember($, DONE_KEY, doneKey(p))
  }

  return waiting
}

/** The conclusions that let `master` move. */
const PASSED = new Set(['success', 'skipped', 'neutral'])

/** The toast for a branch whose runs all ended. */
export function verdictText(branch: string, runs: readonly Run[], failedJobNames: readonly string[]): string {
  const failed = runs.filter(r => !PASSED.has(r.conclusion))
  if (failed.length === 0) return `✓ CI passed on ${branch} · master can move`
  const names = failedJobNames.length > 0 ? failedJobNames : failed.map(r => (r.conclusion === 'failure' ? r.workflow : `${r.workflow} ${r.conclusion}`))
  const shown = names.slice(0, 3).join(', ')
  const more = names.length > 3 ? ` and ${names.length - 3} more` : ''

  return `✗ CI failed on ${branch} · ${shown}${more}`
}

/** A verdict's identity: its runs' ids and attempts, so a re-run is a verdict of its own. */
export function verdictKey(runs: readonly Run[]): string {
  return runs
    .map(r => `${r.id}.${r.attempt}`)
    .sort()
    .join(',')
}

const doneKey = (p: Pushed) => `${p.branch}@${p.sha}`

// ------------------------------------------------------------------------------------------------
// What git and gh say
// ------------------------------------------------------------------------------------------------

/** The `ci/` branches this box pushed and origin still has, by their remote-tracking refs' reflogs:
 * each line `<old> <new> <name> <<email>> <unix time> <zone>\t<why>`, the last one the ref's commit. */
export async function pushedBranches($: Engine): Promise<Pushed[]> {
  const fs = { read: (p: string) => $.fs.read(p), list: (p: string) => $.fs.list(p), realPath: async () => null }
  const gitDir = await commonGitDir(fs, await $.session.cwd()).catch(() => null)
  if (gitDir === null) return []
  const dir = `${gitDir}/logs/refs/remotes/origin/ci`
  const found: Pushed[] = []
  for (const entry of await $.fs.list(dir).catch(() => [])) {
    if (entry.kind !== 'file') continue
    const text = await $.fs.read(`${dir}/${entry.name}`).catch(() => '')
    const lines = text.split('\n').filter(l => l.trim() !== '')
    const sha = /^[0-9a-f]+ ([0-9a-f]+) /.exec(lines.at(-1) ?? '')?.[1]
    const pushed = lines.map(l => /> (\d+) [+-]\d{4}\tupdate by push$/.exec(l)).find(m => m !== null)
    if (!sha || !pushed?.[1]) continue
    found.push({ branch: `ci/${entry.name}`, sha, pushedAt: Number(pushed[1]) * 1000 })
  }

  return found
}

/** `owner/name` of origin on GitHub; null where origin is not on GitHub. */
export async function githubRepo($: Engine): Promise<string | null> {
  const url = (await git($, ['remote', 'get-url', 'origin']))?.trim() ?? ''
  const m = /^(?:git@github\.com:|ssh:\/\/git@github\.com\/|https:\/\/github\.com\/)([^/\s]+)\/([^/\s]+?)(?:\.git)?\/?$/.exec(url)

  return m ? `${m[1]}/${m[2]}` : null
}

/** gh's runs, as this file reads them; a row it cannot read is left out. */
export function runsOf(json: string): Run[] {
  let rows: unknown
  try {
    rows = JSON.parse(json)
  } catch {
    return []
  }
  if (!Array.isArray(rows)) return []
  const out: Run[] = []
  for (const r of rows) {
    if (typeof r !== 'object' || r === null) continue
    const o = r as Record<string, unknown>
    if (typeof o.databaseId !== 'number' || typeof o.headSha !== 'string' || typeof o.status !== 'string') continue
    out.push({
      id: o.databaseId,
      attempt: typeof o.attempt === 'number' ? o.attempt : 1,
      status: o.status,
      conclusion: typeof o.conclusion === 'string' ? o.conclusion : '',
      headSha: o.headSha,
      workflow: typeof o.workflowName === 'string' ? o.workflowName : 'CI',
    })
  }

  return out
}

/** The names of the jobs that failed in the failed runs, in the order gh lists them. */
async function failedJobs($: Engine, repo: string, failed: readonly Run[]): Promise<string[]> {
  const names: string[] = []
  for (const r of failed) {
    const viewed = await gh($, ['run', 'view', String(r.id), '--repo', repo, '--json', 'jobs'])
    if (viewed === null) continue
    try {
      const jobs = (JSON.parse(viewed) as { jobs?: unknown }).jobs
      for (const j of Array.isArray(jobs) ? jobs : []) {
        const o = j as Record<string, unknown>
        if (typeof o.name === 'string' && typeof o.conclusion === 'string' && !PASSED.has(o.conclusion) && o.conclusion !== '') names.push(o.name)
      }
    } catch {
      // A view gh did not answer in JSON names no job; the run's name stands in.
    }
  }

  return names
}

async function git($: Engine, args: readonly string[]): Promise<string | null> {
  try {
    const ran = await $.process.run(['git', ...args], { timeoutMs: 10_000 })

    return ran.exitCode === 0 ? ran.stdout : null
  } catch {
    return null
  }
}

/** gh's stdout, or null; not signed in, it says nothing more this session. */
async function gh($: Engine, args: readonly string[]): Promise<string | null> {
  try {
    const ran = await $.process.run(['gh', ...args], { timeoutMs: 30_000 })
    if (ran.exitCode === GH_NO_AUTH) ghSilent = true

    return ran.exitCode === 0 ? ran.stdout : null
  } catch {
    return null
  }
}

async function strings($: Engine, key: string): Promise<string[]> {
  const v = await $.store.get(key)

  return Array.isArray(v) ? v.filter((s): s is string => typeof s === 'string') : []
}

async function remember($: Engine, key: string, value: string): Promise<void> {
  const had = await strings($, key)
  if (had.includes(value)) return
  await $.store.set(key, [...had, value].slice(-KEEP))
}
