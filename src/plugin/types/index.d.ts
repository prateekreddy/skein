// skein's $.state contract: every value the hooks module keeps for a session, by key.
//
// One place for the types. A feature file imports its types from here, never declares its own,
// and adds its key below when it first keeps a value. Self-contained on purpose: no import and no
// reference, as the engine asks of a contract.
//
// A value outlives a hot reload, so code meets what an older version of itself stored. Change a
// value's fields and its `atom` takes a new `shape` tag in the same commit.

/** What `/skein` answers with: who this session is, read once when it starts. */
export type SkeinIdentity = {
  /** The box's name from `$SKEIN_BOX`, or null when the session is not in a skein box. */
  box: string | null
  /** The plugin's own version, from its `.claude-plugin/plugin.json`. */
  version: string
}

/** One tracker item this session claimed and has not yet completed or released, as skein saw it. */
export type SkeinLease = {
  /** The tracker's id for the item, as `claim` took it. */
  workItemId: string
  /** The item's short name (`SKEIN-1364`), from the claim's briefing; null when not seen. */
  readableId: string | null
  /** The item's title, from the claim's briefing; null when not seen. */
  title: string | null
  /** The subagent whose call claimed it, or null for the main conversation or unknown. */
  agentId: string | null
  /** When the tracker last said the lease runs out, in ms since the epoch; null when not said. */
  expiresAt: number | null
  /** How long one renewal keeps the lease, in ms, from the tracker's own answer; null when not said. */
  renewsForMs: number | null
  /** The main-conversation turn count at the last call that named this item. */
  touchedTurn: number
  /** The `touchedTurn` the box was last reminded of for an untouched lease, or null. */
  remindedIdleAt: number | null
  /** The expiry the box was last warned about, or null. */
  remindedExpiryAt: number | null
}

/** What this session holds in the tracker, kept from the tracker tools' own answers (SKEIN-1366). */
export type SkeinLeases = {
  /** The leases held now, oldest claim first. */
  items: SkeinLease[]
  /** Main-conversation turns completed so far in this session. */
  turns: number
  /** True once any tracker tool answered in this session: there is a tracker to ask. */
  hasTracker: boolean
}

/** skein's guidance as the system prompt carries it (SKEIN-1365). */
export type SkeinGuide = {
  /** The text added to the prompt's environment section; empty when there is none to add. */
  section: string
  /** The shared-home rules, for a box whose instruction files do not already carry them. */
  sharedHome: string
  /** The heading the shared-home rules open with, which says an instruction file has them. */
  sharedHomeHeading: string
}

/**
 * What the attention feature last wrote for the cockpit (`hooks/attention.ts`): the moment the box
 * needs its person, as the engine saw it. `loaded` until the first event of the session.
 */
export type SkeinAttentionState =
  | 'loaded'
  | 'working'
  | 'permission'
  | 'question'
  | 'waiting'
  | 'interrupted'
  | 'refused'
  | 'error'
  | 'ended'

/** The highest rate-limit mark a window has crossed, as it was when it crossed. */
export type SkeinAttentionLimit = {
  /** The window: `five_hour`, `seven_day`, or a gateway's `spend_limit`. */
  kind: string
  /** The mark crossed, one of the feature's `LIMIT_MARKS`. */
  mark: number
  percentUsed: number
  /** When the window resets, ISO 8601, or empty when the engine did not say. */
  resetsAt: string
}

/** The attention feature's session: where it writes, and what it last wrote. */
export type SkeinAttention = {
  /** `<store>/status/<box>.mod.json`, from `probe/box-self.sh`. */
  file: string
  box: string
  /** The plugin's own version, so the cockpit can say which mod wrote it. */
  version: string
  session: string
  state: SkeinAttentionState
  /** When `state` was entered, ISO 8601; empty only between a change and its stamp. */
  at: string
  /** The tool asked about, or the session's end reason; empty otherwise. */
  detail: string
  /** The last turn's `reason` (`answer`, `aborted`, `refusal`, `error`), or empty. */
  stop: string
  /** The subagent whose call is waiting, or empty for the main loop. */
  agent: string
  /** The tool call a person is being asked about, so its return is seen to settle it. */
  asking: string
  /**
   * The last call `tool.check` put to a person, not yet shown to them: a mode's classifier may still
   * settle it. The permission dialog's notification names no call, so this names it.
   */
  asked: { id: string; tool: string; agent: string } | null
  /** The last mark crossed, kept until a higher one. */
  limit: SkeinAttentionLimit | null
  /** The highest mark each window has reached, by window kind. */
  marks: Record<string, number>
}

/** One item on skein's disk list for this box, as `skein-mcp clean` lists it (SKEIN-1356). */
export type SkeinCleanRow = {
  /** The fleet's path, which `skein-mcp clean` takes. */
  path: string
  /** Where the agent sees it: `$HOME/…` for the box's home. */
  in_box: string
  /** `download cache`, `build output`, `scratchpad`, or `shared download cache`. */
  kind: string
  bytes: number
  /** `shared` for the repo's shared download cache, which is not counted against this box. */
  where: 'box' | 'shared'
  safe_now: boolean
  why_not: string | null
}

/** The repo's shared download cache, on a row of its own. */
export type SkeinSharedCache = {
  path: string
  bytes: number
  safe_to_clean_now: boolean
  why_not: string | null
}

/** One of this box's requests or questions, with its answer's state, as `skein-mcp requests` reads it. */
export type SkeinRequestRow = {
  queue: 'package' | 'write' | 'question' | 'disk'
  id: string
  asked: string
  /** `waiting` until the owner's inbox answers it. */
  state: string
  /** What was asked, in a few words. */
  what: string
}

/** skein's disk ask, as the band reads it from the signal file. `id` is the ask's crossing, new at
 * each one, which the band raises one toast for. */
export type SkeinDiskAsk = { id: string; amount_mb: number; live_mb: number; why: string }

/** A version rolled out since the box started that a restart would give it (`Update` in
 * `src/resource_signal.rs`). `restartRequested`: the box has already asked for the restart. */
export type SkeinUpdate = { component: string; have: string; new: string; restartRequested: boolean }

/** What the band reads of `signals/resources.json` while it is fresh (SKEIN-1440). */
export type SkeinResources = {
  disk: {
    mb: number | null
    limit_mb: number | null
    ask: SkeinDiskAsk | null
  }
  /** Anonymous memory against `memory.high` (or `memory.max`); null when its reading is old. */
  memory: { bytes: number; limit: number | null } | null
  /** Cores in use, averaged since skein's previous reading; null when skein had none. */
  cpu: number | null
  /** Processes in the box against its limit (`PidsLevel`); null when the file has none. */
  pids: { current: number | null; max: number | null } | null
  /** What else skein asks of the box than disk, by kind (`memory`, `pids`); empty for none. */
  asked: string[]
  /** Versions a restart would give the box. */
  updates: SkeinUpdate[]
}

/** One minute of the box's CPU, for the band's sparkline: the minute since the epoch, and cores. */
export type SkeinCpuSample = { minute: number; cores: number }

/** One of the box's own jobs as the band keeps it: the view's job less what changes every write. */
export type SkeinBandJob = Omit<SkeinViewJob, 'waited_secs'>

/** The box's jobs view as the band keeps it while it is fresh: its granted and waiting jobs, and
 * every waiting job of another box that goes before its first waiting one, named with its box. */
export type SkeinBandJobs = {
  jobs: SkeinBandJob[]
  ahead: { box: string; command: string; cores: number }[]
  /** The fleet's cores, those granted fleet-wide, and the jobs running fleet-wide; null when the
   * view says no cores. */
  fleet: { cores: number; granted: number; running: number } | null
}

/** What the band and the pane draw from, read on a timer and after a Clean (SKEIN-1362). */
export type SkeinBand = {
  /** From the signal file; null when there is none to believe. */
  disk: {
    mb: number | null
    limit_mb: number | null
    /** skein's disk ask of this box, when it stands: what it can clear now, and what is in use
     * now, which no ask counts (SKEIN-1391). */
    ask: SkeinDiskAsk | null
  } | null
  /** skein's disk list; null before skein has listed it. */
  clean: {
    rows: SkeinCleanRow[]
    removable_now_bytes: number
    shared: SkeinSharedCache | null
  } | null
  /** null outside a box. */
  requests: SkeinRequestRow[] | null
  /** Unread mail; null when the mailbox could not be read. */
  mail: number | null
  /** Memory, CPU and updates from the signal file; null as `disk` is. */
  memory: SkeinResources['memory']
  cpu: number | null
  pids: SkeinResources['pids']
  asked: string[]
  updates: SkeinUpdate[]
  /**
   * This box's own spend from `signals/usage.json`, in dollars at the API's prices; null when the
   * file is missing or more than 70 minutes old. `at`: when skein read it, `HH:MM` in UTC.
   * `unpriced`: a model in the week has no price.
   */
  spend: { at: string; today: number; week: number; unpriced: boolean } | null
}

/** What the skein pane shows: its panels, the clean list first, or one lane opened (`lane`, its
 * worktree). */
export type SkeinPaneView = { view: 'board' | 'clean' | 'lane'; lane: string | null }

/** A lane skein made, as `skein-mcp lane list` reports it (`Lane` in `hooks/lanes.ts`). */
export type SkeinLaneRecord = {
  name: string
  path: string
  branch: string | null
  item: string | null
  phase: string | null
  isMadeBySkein: boolean
}

/** One line of the pane's log: when, whose, and what changed. */
export type SkeinPaneLogEntry = { at: string; who: string; what: string }

/** The skein pane's own record for the session (SKEIN-1440). */
export type SkeinPane = {
  /**
   * What waited on the person at the last look, by key; null before the first look. The pane opens
   * by itself on the first look and for a key not in this list, so what already waited when the
   * person closed it never opens it again.
   */
  seen: string[] | null
  /** The lanes skein made in this box, read at each look. */
  lanes: SkeinLaneRecord[]
  /** The last few changes the pane saw, oldest first. */
  log: SkeinPaneLogEntry[]
  /** What each thing the log follows was at the last look, by key, so a change is seen once. */
  marks: Record<string, string>
}

/** Where a subagent's run stands: working, answered, ended on an error or refusal, or interrupted. */
export type SkeinAgentStatus = 'running' | 'done' | 'failed' | 'stopped'

/** What skein knows of a subagent that works as a lane: in a lane skein made, or a worktree made by hand. */
export type SkeinAgentLane = {
  /** The lane's worktree, as its record names it (`skein-mcp lane list`, SKEIN-1374) or git does. */
  worktree: string
  /**
   * True once one of its own tool calls named the worktree. Until then it is the one its brief
   * named first, and its first tool call that names a worktree replaces it.
   */
  isSeenAtWork: boolean
  /** The lane's branch, as its record names it. */
  branch: string | null
  /** The tracker item the lane was made for, as its record names it. */
  recordedItem: string | null
  /** The lane's phase, as its record names it: `created` until later phases write more. */
  phase: string | null
  /** False for a worktree made by hand, which skein found in git's records and has no record of. */
  isMadeBySkein: boolean
  /** The tracker item it claimed, as the claim's own answer named it; null before a claim. */
  item: { id: string; title: string; isHeld: boolean } | null
  /** Its place at the shared heavy-work lock: waiting for it, holding it, or neither. */
  lock: 'waiting' | 'holding' | null
  /**
   * Its place in the fleet's heavy-work queue (SKEIN-1388), from the box's jobs view: its own
   * waiting job, or, waiting at the lock, the box's job it waits behind. Absent or null when the
   * box has no jobs view or nothing of it waits.
   */
  queue?: SkeinLaneQueue | null
  /**
   * When the session first saw the lane's work landed (SKEIN-1456): its worktree gone, and its
   * branch deleted or already on master (`hasLanded` in `lanes.ts`). Absent or null until then.
   */
  landedAt?: number | null
}

/** A lane's place in the fleet's heavy-work queue, as skein's jobs view gives it. */
export type SkeinLaneQueue = {
  /** Its place among every waiting job in the fleet, from 1. */
  position: number
  /** Why it did not start on skein's last look, such as `repo at cap 2/2`; empty when none. */
  reason: string
  /** True when the waiting job is the lane's own; false when the lane waits behind the box's. */
  isOwn: boolean
}

/** One subagent of this session, as the agents panel draws it (SKEIN-1361). */
export type SkeinAgentRun = {
  /** The engine's agent id, or the Agent call's id when none was given. */
  id: string
  agentId: string | null
  /** The agent type: `general-purpose`, `Explore`, a plugin's. */
  type: string
  /** The Agent call's few words. */
  description: string
  model: string
  effort: string | null
  status: SkeinAgentStatus
  startedAt: number
  endedAt: number | null
  /** The last request's prompt and output: how full its context is. */
  contextTokens: number
  contextMax: number
  /** Every token of every request it made. */
  tokens: number
  /** Dollars, from `hooks/prices.ts`, of the requests whose model has a price. */
  costUsd: number
  /** True once a request was made on a model with no price: the cost is then a floor. */
  isUnpriced: boolean
  /** Requests seen. */
  steps: number
  lane: SkeinAgentLane | null
}

/** The agents panel's own settings for the session. */
export type SkeinAgentsPanel = {
  isCompact: boolean
  isDoneCollapsed: boolean
  /** The person closed the panel: a new subagent no longer opens it until they open it again. */
  isClosedByPerson: boolean
}

/** One of the box's own jobs in its jobs view, `$SKEIN_STATE/signals/jobs.json` (`ViewJob` in
 * `src/jobs.rs`), the fields the tool rows draw. */
export type SkeinViewJob = {
  id: string
  /** Its argv joined by spaces, cut at 300 characters. */
  command: string
  cwd: string
  /** `waiting`, `granted`, `done` or `cancelled`. */
  state: string
  /** The cores it asks for. */
  cores: number
  /** The cores it was granted; zero while it waits. */
  granted_cores: number
  /** Its place among every waiting job in the fleet, from 1; null once it is not waiting. */
  position: number | null
  /** Why it has not started, or, once it ended, what it used, in skein's words. */
  reason: string
  waited_secs: number
}

/** The box's jobs view as the tool rows read it (`View` in `src/jobs.rs`). */
export type SkeinJobsView = {
  /** When skein wrote it, RFC3339. */
  at: string
  /** The machine's cores, those granted to running jobs fleet-wide, and those free now. */
  cores: number
  granted: number
  free: number
  jobs: SkeinViewJob[]
  /** Every waiting job that goes before the box's first waiting one, named with its box. */
  ahead: { box: string; command: string; cores: number }[]
}

/** A Bash call that runs `skein-job run` (SKEIN-1440). */
export type SkeinJobCall = {
  /** The call's tool_use_id. */
  id: string
  /** The command under `skein-job run --`, as skein's view spells a job's. */
  command: string
  /** The job in the view that is this call's, once one is found; null before. */
  jobId: string | null
  isRunning: boolean
  /** The subagent whose call it is (`tool.call`'s `agentId`); null on the main loop. */
  agentId: string | null
}

/** What the tool rows draw a `skein-job` call from: the calls, and the jobs view while one ran. */
export type SkeinJobWatch = {
  /** The session's last few `skein-job` calls, oldest first. */
  calls: SkeinJobCall[]
  /** The view as last read; null when it was missing, unreadable or out of date. */
  view: SkeinJobsView | null
}

declare module 'claude-code' {
  interface PluginState {
    skein: {
      identity: Shaped<SkeinIdentity | null>
      leases: Shaped<SkeinLeases | null>
      /** True once this session asked the tracker's `held` itself (SKEIN-1400). */
      leasesSeeded: boolean
      guide: Shaped<SkeinGuide | null>
      attention: Shaped<SkeinAttention | null>
      band: Shaped<SkeinBand | null>
      /** The box's running and waiting jobs, read every ten seconds while the view is fresh. */
      bandJobs: Shaped<SkeinBandJobs | null>
      /** The box's CPU a minute at a time over the last hour, oldest first (SKEIN-1440). */
      cpuHistory: Shaped<SkeinCpuSample[]>
      /** What waits on the person that a toast was raised for, by key, while it still waits. */
      bandToasted: Shaped<string[]>
      /** What the last Clean said, `skein-mcp clean`'s own sentence. */
      cleanSaid: Shaped<string | null>
      /** What the skein pane shows now (SKEIN-1440). */
      paneView: Shaped<SkeinPaneView>
      /** The skein pane's record: whether the person closed it, what it opened for, its log. */
      pane: Shaped<SkeinPane>
      agents: Shaped<SkeinAgentRun[]>
      agentsPanel: Shaped<SkeinAgentsPanel>
      agentsNow: number
      /** The `skein-job` calls the tool rows draw (SKEIN-1440). */
      jobWatch: Shaped<SkeinJobWatch | null>
    }
  }
}
