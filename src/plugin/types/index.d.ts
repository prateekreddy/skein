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
  queue: 'package' | 'write' | 'question'
  id: string
  asked: string
  /** `waiting` until the owner's inbox answers it. */
  state: string
  /** What was asked, in a few words. */
  what: string
}

/** What the band and the pane draw from, read on a timer and after a Clean (SKEIN-1362). */
export type SkeinBand = {
  /** From the signal file; null when there is none to believe. */
  disk: {
    mb: number | null
    limit_mb: number | null
    /** skein's disk ask of this box, when it stands. */
    ask: { amount_mb: number; why: string } | null
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
  /**
   * This box's own spend from `signals/usage.json`, in dollars at the API's prices; null when the
   * file is missing or more than 70 minutes old. `at`: when skein read it, `HH:MM` in UTC.
   * `unpriced`: a model in the week has no price.
   */
  spend: { at: string; today: number; week: number; unpriced: boolean } | null
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
      /** What the last Clean said, `skein-mcp clean`'s own sentence. */
      cleanSaid: Shaped<string | null>
      agents: Shaped<SkeinAgentRun[]>
      agentsPanel: Shaped<SkeinAgentsPanel>
      agentsNow: number
    }
  }
}
