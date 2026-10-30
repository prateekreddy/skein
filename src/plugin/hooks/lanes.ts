// What skein knows of a subagent that works as a lane (SKEIN-1361, SKEIN-1374): the lanes skein
// made, as `skein-mcp lane list` reports them, and, for a worktree made by hand, git's own worktree
// records; `/proc/locks` and `/proc/<pid>/{cwd,fd}`; and the text of the subagent's own tool calls. Nothing here reaches the network or runs a process; the
// panel runs `skein-mcp` and hands its output to `lanesListed`.
//
// The agents panel (`agents-panel.tsx`) calls these; each takes the few `$.fs` calls it needs as
// a `Files`, so a test hands it a made-up box.

/** The `$.fs` calls this file uses. */
export type Files = {
  read: (path: string) => Promise<string>
  list: (path: string) => Promise<readonly { name: string }[]>
  realPath: (path: string) => Promise<string | null>
}

/**
 * A lane: one skein made (`skein-mcp lane create`), as `skein-mcp lane list` reports it, or a
 * worktree made by hand, from git's own records.
 */
export type Lane = {
  name: string
  /** Its worktree: `<lane root>/<name>`. */
  path: string
  branch: string | null
  /** The tracker item it was made for, as its record names it. */
  item: string | null
  /** Where it stands: `created` until later phases write more; null for one skein did not make. */
  phase: string | null
  /** False for a worktree made by hand, found only in git's records. */
  isMadeBySkein: boolean
}

/**
 * The lock lanes take before heavy work (builds, tests, browser runs) so they run one at a time.
 *
 * The coordinator's lane rules name it, not skein's code, which is why it is a constant here and
 * not a setting: a lane run `flock` on it, and a lane blocked there is waiting for CPU, not stuck.
 */
export const HEAVY_LOCK = '/var/tmp/skein-lanes-heavy.lock'

/**
 * The lanes in what `skein-mcp lane list` printed, `{ result: { lanes } }`.
 *
 * skein-mcp is the one reader of the lane records (`~/.local/state/skein/lanes/<name>.json`), so
 * a record it refused, one a newer skein wrote, is not among them. A row without a name or a
 * worktree is left out; none when the output is not a list.
 */
export function lanesListed(stdout: string): Lane[] {
  let parsed: unknown
  try {
    parsed = JSON.parse(stdout)
  } catch {
    return []
  }
  const result = isObject(parsed) && isObject(parsed.result) ? parsed.result : null
  const rows = result !== null && Array.isArray(result.lanes) ? (result.lanes as unknown[]) : []
  const found: Lane[] = []
  for (const row of rows) {
    if (!isObject(row) || typeof row.name !== 'string' || typeof row.worktree !== 'string' || row.worktree === '') continue
    found.push({
      name: row.name,
      path: row.worktree.replace(/\/+$/, ''),
      branch: typeof row.branch === 'string' ? row.branch : null,
      item: typeof row.item === 'string' ? row.item : null,
      phase: typeof row.phase === 'string' ? row.phase : null,
      isMadeBySkein: true,
    })
  }

  return found
}

/**
 * The repository's git directory shared by all its worktrees, from the session's folder.
 *
 * A main checkout's `.git` is that directory; a worktree's `.git` is a file naming its own, whose
 * `commondir` leads back to the shared one. Null outside a git checkout.
 */
export async function commonGitDir(fs: Files, cwd: string): Promise<string | null> {
  const dotGit = `${cwd.replace(/\/+$/, '')}/.git`
  try {
    const text = await fs.read(dotGit)
    const own = /^gitdir:\s*(.+)$/m.exec(text)?.[1]?.trim()
    if (!own) return null
    const common = (await fs.read(`${own}/commondir`).catch(() => '')).trim()
    if (!common) return own
    return common.startsWith('/') ? common : normalise(`${own}/${common}`)
  } catch {
    // `.git` is a directory (a main checkout), or there is none.
  }
  return fs.list(dotGit).then(
    () => dotGit,
    () => null,
  )
}

/**
 * Every worktree git records for the repository, from `<git dir>/worktrees/<name>/`, as lanes skein
 * did not make: no item and no phase. The fallback for lanes made by hand, which have no record.
 */
export async function worktrees(fs: Files, gitDir: string): Promise<Lane[]> {
  const names = await fs.list(`${gitDir}/worktrees`).catch(() => [])
  const found: Lane[] = []
  for (const { name } of names) {
    const record = `${gitDir}/worktrees/${name}`
    const gitdir = (await fs.read(`${record}/gitdir`).catch(() => '')).trim()
    if (!gitdir.endsWith('/.git')) continue
    const head = (await fs.read(`${record}/HEAD`).catch(() => '')).trim()
    const branch = /^ref:\s*refs\/heads\/(.+)$/.exec(head)?.[1] ?? null
    found.push({ name, path: gitdir.slice(0, -'/.git'.length), branch, item: null, phase: null, isMadeBySkein: false })
  }

  return found
}

/**
 * The lanes a subagent may work in: every lane skein made, then every other worktree git records,
 * marked as not made by skein. A worktree skein made is known by its record, not twice.
 */
export function withHandMade(made: readonly Lane[], recorded: readonly Lane[]): Lane[] {
  const ours = new Set(made.map(l => l.path))

  return [...made, ...recorded.filter(t => !ours.has(t.path))]
}

/**
 * The lane a subagent works in, from text it was given or wrote: the one whose worktree is named
 * first.
 *
 * A path counts only as a whole path segment, so `/var/tmp/wt-a` is not found in
 * `/var/tmp/wt-ab`; a longer worktree inside a shorter one wins where both match.
 */
export function worktreeIn<T extends { path: string }>(text: string, known: readonly T[]): T | null {
  let best: { at: number; tree: T } | null = null
  for (const tree of known) {
    let from = 0
    for (;;) {
      const at = text.indexOf(tree.path, from)
      if (at < 0) break
      const after = text[at + tree.path.length]
      const isWhole = after === undefined || !/[\w.-]/.test(after)
      if (isWhole) {
        const isBetter =
          best === null || at < best.at || (at === best.at && tree.path.length > best.tree.path.length)
        if (isBetter) best = { at, tree }
        break
      }
      from = at + 1
    }
  }

  return best?.tree ?? null
}

/**
 * The tracker item a claim answered with: its readable id and title.
 *
 * The sync server's `claim` answers JSON whose `briefing` carries `readableId` and `title`; this
 * reads those two and nothing else of it. Null when the answer was a refusal or says neither.
 */
export function claimedItem(answer: string): { id: string; title: string } | null {
  try {
    const parsed: unknown = JSON.parse(answer)
    const briefing = isObject(parsed) && isObject(parsed.briefing) ? parsed.briefing : null
    const id = briefing?.readableId
    const title = briefing?.title
    if (typeof id === 'string' && id !== '') return { id, title: typeof title === 'string' ? title : '' }
  } catch {
    // Not JSON: a refusal's words.
  }

  return null
}

/**
 * Which folders are holding the heavy lock and which are waiting for it, by the working folder of
 * each process at it.
 *
 * `/proc/locks` lists every `flock` held, and, on a line marked `->`, every one waiting, each by
 * process id and the file's device and inode. The lock file is found by a process at it having
 * the file open (`/proc/<pid>/fd/*`), and each process's folder by `/proc/<pid>/cwd`.
 */
export async function heavyLock(fs: Files, lock = HEAVY_LOCK): Promise<{ waiting: string[]; holding: string[] }> {
  const none = { waiting: [], holding: [] }
  const text = await fs.read('/proc/locks').catch(() => '')
  const entries: { pid: string; file: string; isWaiting: boolean }[] = []
  for (const line of text.split('\n')) {
    const m = /^\d+:\s+(->\s+)?FLOCK\s+\S+\s+\S+\s+(\d+)\s+(\S+)/.exec(line)
    if (m?.[2] && m[3]) entries.push({ pid: m[2], file: m[3], isWaiting: m[1] !== undefined })
  }
  if (entries.length === 0) return none

  // The lock's device and inode: the first file a process at a lock has open as the lock's path.
  let file: string | null = null
  for (const { pid, file: candidate } of entries) {
    if (await hasOpen(fs, pid, lock)) {
      file = candidate
      break
    }
  }
  if (file === null) return none

  const waiting: string[] = []
  const holding: string[] = []
  for (const entry of entries) {
    if (entry.file !== file) continue
    const cwd = await fs.realPath(`/proc/${entry.pid}/cwd`)
    if (cwd !== null) (entry.isWaiting ? waiting : holding).push(cwd)
  }

  return { waiting, holding }
}

/** Where a lane stands at the heavy lock, from the folders `heavyLock` found. */
export function lockOf(worktree: string, at: { waiting: readonly string[]; holding: readonly string[] }): 'waiting' | 'holding' | null {
  const isInside = (cwd: string) => cwd === worktree || cwd.startsWith(`${worktree}/`)
  if (at.holding.some(isInside)) return 'holding'
  if (at.waiting.some(isInside)) return 'waiting'

  return null
}

async function hasOpen(fs: Files, pid: string, path: string): Promise<boolean> {
  const fds = await fs.list(`/proc/${pid}/fd`).catch(() => [])
  for (const { name } of fds) {
    if ((await fs.realPath(`/proc/${pid}/fd/${name}`)) === path) return true
  }

  return false
}

function normalise(path: string): string {
  const out: string[] = []
  for (const part of path.split('/')) {
    if (part === '' || part === '.') continue
    if (part === '..') out.pop()
    else out.push(part)
  }

  return `/${out.join('/')}`
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null
}
