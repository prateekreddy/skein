// Saved answers of skein's tools and the box's jobs view, for `tool-results.test.tsx`.
//
// Each is the shape its writer gives, field for field: `skein_resources` is the signal file
// (`Signals` in `src/resource_signal.rs`), `skein_top` and `skein_clean` are `top` and `clean` in
// `bin/skein-mcp`, `skein_requests` is `requests` there, and the jobs view is `View` in
// `src/jobs.rs`. The figures are a real box's on 2026-10-31, with its name and paths replaced.

export const HOME = '/home/agent'
const ROOT = '/fleet-root-example/web-main'

const G = 1024 * 1024 * 1024
const M = 1024 * 1024

/** `skein_resources` with nothing asked. */
export const RESOURCES = {
  at: '2026-10-31T17:49:36Z',
  disk: { mb: 5073, limit_mb: 10240, fleet: 'under', fleet_over_by_mb: 0 },
  cpu: { cores: 1.345610505727857, usage_usec: 155694909157 },
  memory: {
    anon: 2475343872,
    cache: 2007171072,
    high: 14021558272,
    max: 17845714944,
    throttled: 0,
    throttled_per_min: 0.0,
    read_at: '2026-10-31T17:49:36Z',
  },
  pids: { current: 58, max: 8192 },
  asks: [],
  cleared: {
    disk: "skein: the fleet's disk no longer needs anything from this box. Nothing more is asked.",
    memory: 'skein: this box is no longer being slowed for memory. Nothing more is asked.',
    pids: 'skein: this box is back to 58 processes. Nothing more is asked.',
  },
  clean: {
    at: '2026-10-31T17:47:36Z',
    box_root: ROOT,
    items: [
      { path: `${ROOT}/home/.cache/ms-playwright`, shown: '~/.cache/ms-playwright', kind: 'download cache', bytes: 1031127040, live: false },
      { path: `${ROOT}/home/.cargo/registry`, shown: '~/.cargo/registry', kind: 'download cache', bytes: 232701952, live: false },
      { path: `${ROOT}/home/.cache/skein/claude/s1`, shown: '~/.cache/skein/claude/s1', kind: 'scratchpad', bytes: 226979840, live: false },
      { path: `${ROOT}/home/.npm/_cacache`, shown: '~/.npm/_cacache', kind: 'download cache', bytes: 194813952, live: false },
      { path: `${ROOT}/tree/tests/ui/node_modules`, shown: 'web-main/tree/tests/ui/node_modules', kind: 'build output', bytes: 19120128, live: false },
      { path: `${ROOT}/tree/target`, shown: 'web-main/tree/target', kind: 'build output', bytes: 3 * G, live: true },
    ],
    repo_cache: {
      path: '/fleet-root-example/.skein/repos/web/cache',
      bytes: 106475520,
      items: [],
      others_running: 0,
      others_unknown: 0,
      none_running_at: '2026-10-31T17:49:36Z',
    },
  },
}

/** `skein_resources` while skein asks for disk and the box is slowed for memory. */
export const RESOURCES_ASKED = {
  ...RESOURCES,
  disk: { mb: 11468, limit_mb: 10240, fleet: 'over', fleet_over_by_mb: 4198 },
  memory: { ...RESOURCES.memory, throttled: 840, throttled_per_min: 12.4 },
  asks: [
    {
      kind: 'disk',
      why: 'allowance',
      amount: 1228,
      since: '2026-10-31T17:40:00Z',
      crossing_id: 'disk-1793468400000',
      hold: '',
      start: 'start line',
      remind: 'reminder',
      enter: 'skein: this box holds 11.2G of its 10.0G allowance. Clear 1.2G: skein_clean lists what can go.',
    },
  ],
  cleared: {},
  updates: [{ component: 'claude', have: '2.1.316', new: '2.1.318', applies: 'restart', restart_requested: false }],
}

const TMUX =
  '/usr/bin/tmux -S /fleet-root-example/web-main/session.sock new-session -d -s skein-shell -- sh -c printf "\\n%s\\n\\n" "$0"; exec "$@" web-main is a box.'

/** `skein_top` by cpu, six of sixteen processes, one with no parent. */
export const TOP_CPU = {
  by: 'cpu',
  processes_in_box: 16,
  parentless_in_box: 1,
  processes: [
    { pid: 2177076, ppid: 2177074, rss_bytes: 921862144, cpu_seconds: 1788.6, age_seconds: 20872, parentless: false, command: 'claude --name web-main --plugin-dir /fleet-root-example/.skein/plugin --continue' },
    { pid: 2176811, ppid: 1, rss_bytes: 6537216, cpu_seconds: 39.5, age_seconds: 20872, parentless: true, command: TMUX },
    { pid: 3787624, ppid: 2177076, rss_bytes: 3424256, cpu_seconds: 2.3, age_seconds: 14535, parentless: false, command: '/bin/bash -c source /home/agent/.claude/shell-snapshots/snapshot-bash.sh 2>/dev/null || true' },
    { pid: 2177900, ppid: 2177899, rss_bytes: 12599296, cpu_seconds: 1.8, age_seconds: 20869, parentless: false, command: 'python3 /fleet-root-example/.skein/plugin/bin/skein-resources monitor' },
    { pid: 1437768, ppid: 2177076, rss_bytes: 3424256, cpu_seconds: 1.3, age_seconds: 5445, parentless: false, command: 'bash' },
    { pid: 1431763, ppid: 2177076, rss_bytes: 3407872, cpu_seconds: 0.3, age_seconds: 45, parentless: false, command: 'sync-monitor' },
  ],
}

/** `skein_top` by memory. */
export const TOP_MEMORY = { ...TOP_CPU, by: 'memory', processes: [...TOP_CPU.processes].sort((a, b) => b.rss_bytes - a.rss_bytes).slice(0, 3) }

/** `skein_top` by disk (`top_disk_all` in `bin/skein-mcp`). */
export const TOP_DISK = {
  by: 'disk',
  checkout: `${ROOT}/tree`,
  dirs: [
    { path: `${ROOT}/tree/target`, bytes: 3 * G },
    { path: `${ROOT}/tree/tests`, bytes: 40 * M },
    { path: `${ROOT}/tree/src`, bytes: 12 * M },
  ],
  charged_mb: 5073,
  limit_mb: 10240,
  cleanable: {
    listed_at: '2026-10-31T17:47:36Z',
    bytes_by_kind: { 'download cache': 1458642944, scratchpad: 226979840, 'build output': 19120128 },
    items: [],
    how: 'skein_clean removes them',
  },
  shared_repo_cache: {
    path: '/fleet-root-example/.skein/repos/web/cache',
    bytes: 0,
    what: "the repo's old download cache on the host",
    shared_with: 'every box of this repo',
    counted_against_this_box: false,
    safe_to_clean_now: true,
    why_not: null,
  },
}

const PAD = 'a scratchpad may belong to a session still running in this box'

const item = (inBox: string, kind: string, bytes: number, why: string | null = null) => ({
  path: inBox.replace(HOME, `${ROOT}/home`),
  in_box: inBox,
  kind,
  bytes,
  where: 'box',
  safe_now: why === null,
  why_not: why,
})

/** `skein_clean` with no paths: four that can go, a running build and three scratchpads kept. */
export const CLEAN = {
  listed_at: '2026-10-31T17:47:36Z',
  items: [
    item(`${HOME}/.cache/ms-playwright`, 'download cache', 1031127040),
    item(`${HOME}/.cargo/registry`, 'download cache', 232701952),
    item(`${HOME}/.cache/skein/claude/s1`, 'scratchpad', 226979840, PAD),
    item(`${HOME}/.npm/_cacache`, 'download cache', 194813952),
    item(`${HOME}/.cache/skein/claude/s2`, 'scratchpad', 3842048, PAD),
    item(`${HOME}/.cache/skein/claude/s3`, 'scratchpad', 8192, PAD),
    item('/workspace/web/tests/ui/node_modules', 'build output', 19120128),
    item('/workspace/web/target', 'build output', 3 * G, 'a build is running in it now'),
  ],
  removable_now_bytes: 1477763072,
  shared_repo_cache: {
    path: '/fleet-root-example/.skein/repos/web/cache',
    bytes: 0,
    what: "the repo's old download cache on the host. Nothing uses it since the caches moved to the repo's tools area, so all of it can go",
    shared_with: 'every box of this repo',
    counted_against_this_box: false,
    safe_to_clean_now: true,
    why_not: null,
  },
}

/** `skein_clean` with paths: one removed, one that could not be. */
export const CLEAN_REMOVED = {
  removed: [`${HOME}/.cache/ms-playwright`],
  freed_bytes: 1031127040,
  failed: [{ path: `${HOME}/.cargo/registry`, why: 'Permission denied' }],
}

/** `skein_requests`: one package waiting, a write granted, a question answered. */
export const REQUESTS = {
  box: 'web-main',
  requests: [
    { queue: 'write', id: '8', asked: '2026-10-31T09:00:00Z', repo: 'acme/site', state: 'granted', answer: null },
    { queue: 'question', id: 'q1', asked: '2026-10-31T10:00:00Z', question: 'Keep the old table?', state: 'answered', answer: 'yes, for one release' },
    { queue: 'package', id: 'p1', asked: '2026-10-31T11:00:00Z', kind: 'apt', packages: ['jq', 'ripgrep'], state: 'waiting', answer: null },
  ],
}

/** The box's jobs view (`View` in `src/jobs.rs`): this box's `cargo test` waits 2nd behind
 * another repo's build, and its `cargo clippy` runs. */
export const JOBS_VIEW = {
  schema: 1,
  at: '2026-10-31T12:00:00Z',
  cores: 12,
  granted: 10,
  free: 2,
  running: 3,
  jobs: [
    { id: 'j-1a2b3c4d', class: 'test', command: 'cargo test --lib', cwd: '/workspace/web', state: 'waiting', cores: 4, min: 0, granted_cores: 0, position: 2, reason: 'waiting for 4 cores (2 free)', waited_secs: 64 },
    { id: 'j-5e6f7a8b', class: 'build', command: 'cargo clippy --all-targets', cwd: '/workspace/web', state: 'granted', cores: 4, min: 0, granted_cores: 4, position: null, reason: '', waited_secs: 3 },
  ],
  ahead: [{ box: 'api-main', id: 'j-99', class: 'build', command: 'cargo build', cores: 6 }],
}

/** What a Bash call that ran `skein-job run` holds once it ends: the command's own output, and
 * `skein-job`'s last line on stderr (`said_used` in `bin/skein-job`). */
export const JOB_BASH_OUTPUT = {
  stdout: 'test result: ok. 74 passed; 0 failed\n',
  stderr:
    "skein-job: 2nd in the fleet's heavy-work queue: waiting for 4 cores (2 free)\n" +
    'skein-job: granted 4 cores; starting\n' +
    'skein-job: asked 4 cores, used 3.4 avg (peak 3.9), 2.1 GB peak, 3m12s\n',
  interrupted: false,
}

/** A tool's answer as `bin/skein-mcp` returns it over MCP: the result as JSON, then its sentence. */
export function mcp(result: unknown, said = ''): { content: { type: 'text'; text: string }[]; isError: boolean } {
  const content = [{ type: 'text' as const, text: JSON.stringify(result, null, 1) }]
  if (said !== '') content.push({ type: 'text', text: said })

  return { content, isError: typeof result === 'object' && result !== null && 'error' in result }
}
