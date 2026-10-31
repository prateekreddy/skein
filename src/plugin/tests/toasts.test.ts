// CI's verdict on a `ci/` branch this box pushed, toasted once (SKEIN-1440, step 4), through the
// whole module.
//
// Nothing here reaches GitHub or a fleet: the box's checkout is a few files in memory, and `git` and
// `gh` are answered by this file's own `process.run` hook from saved answers in the shapes `gh run
// list --json` and `gh run view --json jobs` print.

import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

const NOW = Date.parse('2026-10-31T12:00:00Z')
const PUSHED_AT = Math.floor(NOW / 1000) - 120
const CWD = '/boxes/web-main/tree'
const LOGS = `${CWD}/.git/logs/refs/remotes/origin/ci`
const SHA = '33d5db0f9a1b2c3d4e5f60718293a4b5c6d7e8f9'
const RED = '53a7bca7aa1b2c3d4e5f60718293a4b5c6d7e8f9'
const OTHER = 'aaaa0000aa1b2c3d4e5f60718293a4b5c6d7e8f9'
const REPO = 'example-owner/web'

const ZERO = '0'.repeat(40)
const pushedLog = (sha: string) => `${ZERO} ${sha} Box <box@example.com> ${PUSHED_AT} +0000\tupdate by push\n`
const fetchedLog = (sha: string) => `${ZERO} ${sha} Box <box@example.com> ${PUSHED_AT} +0000\tfetch: fast-forward\n`

const entry = (name: string) => ({ name, kind: 'file' as const, size: 84, mtimeMs: NOW, isLink: false })

type Box = {
  /** Reflog files under `LOGS`, by branch name after `ci/`. */
  logs: Map<string, string>
  /** What `gh run list --branch <b>` prints, by branch; a number is gh's exit status instead. */
  runs: Map<string, unknown[] | number>
  /** What `gh run view <id> --json jobs` prints, by run id. */
  jobs: Map<number, unknown[]>
  /** The argv of every `gh` run. */
  gh: string[][]
  toasts: string[]
}

/** The box beneath the plugin: its checkout's files, git and gh, and the screen's toasts. */
function box(on: On, store: Record<string, unknown> = {}): Box {
  const b: Box = { logs: new Map(), runs: new Map(), jobs: new Map(), gh: [], toasts: [] }
  mock.env(on, { HOME: '/home/agent' })
  mock.store(on, store)
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('session.cwd', () => ({ value: CWD }))
  on('turn.complete', (_, e) => ({ text: e.answer }))
  on('fs.read', (_, e) => {
    const name = e.path.startsWith(`${LOGS}/`) ? e.path.slice(LOGS.length + 1) : null
    const got = name === null ? undefined : b.logs.get(name)
    if (got === undefined) throw new Error(`ENOENT: ${e.path}`)

    return { value: got }
  })
  on('fs.list', (_, e) => {
    if (e.path === `${CWD}/.git`) return { value: [entry('HEAD')] }
    if (e.path === LOGS) return { value: [...b.logs.keys()].map(entry) }
    throw new Error(`ENOENT: ${e.path}`)
  })
  on('process.run', (_, e) => {
    const argv = [...e.argv]
    const answer = (exitCode: number, stdout = '') => ({ value: { exitCode, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
    if (argv.join(' ') === 'git remote get-url origin') return answer(0, `git@github.com:${REPO}.git\n`)
    if (argv[0] !== 'gh') return answer(1)
    b.gh.push(argv)
    if (argv[1] === 'run' && argv[2] === 'list') {
      const listed = b.runs.get(argv[argv.indexOf('--branch') + 1] ?? '') ?? []

      return typeof listed === 'number' ? answer(listed) : answer(0, JSON.stringify(listed))
    }
    if (argv[1] === 'run' && argv[2] === 'view') return answer(0, JSON.stringify({ jobs: b.jobs.get(Number(argv[3])) ?? [] }))

    return answer(1)
  })
  on('ui.toast', (_, e) => {
    b.toasts.push(e.text)

    return { value: undefined }
  })

  return b
}

const run = (id: number, sha: string, status: string, conclusion = '') => ({ databaseId: id, attempt: 1, status, conclusion, headSha: sha, workflowName: 'CI' })
const START = { cwd: CWD, surface: 'terminal', isInteractive: true } as const
const listed = (b: Box) => b.gh.filter(a => a[2] === 'list').map(a => a[a.indexOf('--branch') + 1])

// What would make it fail: a branch another box pushed (its reflog says `fetch`) watched as this
// box's; GitHub asked again once the verdict is in, or toasted twice; or the verdict forgotten by
// a reload. Watched: with the two `remember` calls in `check` removed, the second session toasted
// `✓ CI passed` again and the last `toasts` comparison failed.
test('CI passing on a ci/ branch this box pushed is toasted once, and not again after a reload', async ($, on) => {
  const clock = mock.clock(on, { now: NOW })
  const store: Record<string, unknown> = {}
  const b = box(on, store)
  b.logs.set('33d5db0f', pushedLog(SHA))
  b.logs.set('aaaa0000', fetchedLog(OTHER))
  b.runs.set('ci/33d5db0f', [run(901, SHA, 'in_progress')])

  await $.session.start(START)
  expect(listed(b)).toEqual(['ci/33d5db0f'])
  expect(b.gh[0]).toEqual(['gh', 'run', 'list', '--repo', REPO, '--branch', 'ci/33d5db0f', '--json', 'databaseId,attempt,status,conclusion,headSha,workflowName', '--limit', '20'])
  expect(b.toasts).toEqual([])

  // A minute on, CI has passed.
  b.runs.set('ci/33d5db0f', [run(901, SHA, 'completed', 'success')])
  await clock.advance(60_000)
  expect(b.toasts).toEqual(['✓ CI passed on ci/33d5db0f · master can move'])

  // Nothing waits now: GitHub is not asked again, at the next tick or at a turn's end.
  const asked = b.gh.length
  await clock.advance(180_000)
  await $.turn.complete({ answer: '', durationMs: 5, isAborted: false, turnId: 't1', reason: 'answer' })
  expect(b.gh.length).toBe(asked)

  // A new session of the same box, with the same store: the verdict is not toasted again.
  await $.session.start(START)
  await clock.advance(60_000)
  expect(b.toasts).toEqual(['✓ CI passed on ci/33d5db0f · master can move'])
})

// What would make it fail: a failure toasted as a pass, or the jobs that passed named beside the
// one that failed. Watched: with the `!PASSED.has(o.conclusion)` test in `failedJobs` removed, the
// toast read `· check, coverage` and this failed.
test('CI failing names the jobs that failed', async ($, on) => {
  mock.clock(on, { now: NOW })
  const b = box(on)
  b.logs.set('53a7bca7', pushedLog(RED))
  b.runs.set('ci/53a7bca7', [run(902, RED, 'completed', 'failure')])
  b.jobs.set(902, [
    { name: 'check', conclusion: 'success' },
    { name: 'coverage', conclusion: 'failure' },
  ])

  await $.session.start(START)
  expect(b.toasts).toEqual(['✗ CI failed on ci/53a7bca7 · coverage'])
})

// What would make it fail: gh asked about every push, not only one to a `ci/` branch; or a push
// not noticed until the next session. Watched: with `onToolCall`'s `kick` removed, nothing was
// asked after the push and the `listed` comparison failed.
test('a push from a Bash call starts the watch', async ($, on) => {
  mock.clock(on, { now: NOW })
  const b = box(on)
  on('tool.call', { tool: 'Bash' }, () => ({ result: { stdout: '', stderr: '', interrupted: false } }))

  await $.session.start(START)
  expect(b.gh).toEqual([])

  b.logs.set('33d5db0f', pushedLog(SHA))
  b.runs.set('ci/33d5db0f', [run(901, SHA, 'queued')])
  await $.tool.call({ tool: 'Bash', command: 'git push origin HEAD:refs/heads/ci/33d5db0f' })
  expect(listed(b)).toEqual(['ci/33d5db0f'])
  expect(b.toasts).toEqual([])
})

// What would make it fail: a box with no gh sign-in asking every minute for ever. Watched: with
// `ghSilent = true` removed from `gh`, the tick asked again and the count comparison failed.
test('a box where gh is not signed in stays silent and stops asking', async ($, on) => {
  const clock = mock.clock(on, { now: NOW })
  const b = box(on)
  b.logs.set('33d5db0f', pushedLog(SHA))
  b.runs.set('ci/33d5db0f', 4)

  await $.session.start(START)
  expect(b.gh.length).toBe(1)
  await clock.advance(300_000)
  expect(b.gh.length).toBe(1)
  expect(b.toasts).toEqual([])
})

// What would make it fail: a `-p` session asking GitHub, or toasting where nobody sees it.
// Watched: with the `isWatched` check in `kick` removed, gh was asked and this failed.
test('a session nobody watches asks nothing', async ($, on) => {
  mock.clock(on, { now: NOW })
  const b = box(on)
  b.logs.set('33d5db0f', pushedLog(SHA))
  b.runs.set('ci/33d5db0f', [run(901, SHA, 'completed', 'success')])

  await $.session.start({ ...START, isInteractive: false })
  expect(b.gh).toEqual([])
  expect(b.toasts).toEqual([])
})
