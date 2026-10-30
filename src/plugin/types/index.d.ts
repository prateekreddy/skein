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
}

/** Where a subagent's run stands: working, answered, ended on an error or refusal, or interrupted. */
export type SkeinAgentStatus = 'running' | 'done' | 'failed' | 'stopped'

/** What skein knows of a subagent that works as a lane: in a git worktree of this repository. */
export type SkeinAgentLane = {
  /** The worktree it works in, as git records it under `.git/worktrees/`. */
  worktree: string
  /**
   * True once one of its own tool calls named the worktree. Until then it is the one its brief
   * named first, and its first tool call that names a worktree replaces it.
   */
  isSeenAtWork: boolean
  /** The worktree's branch, from its `HEAD`; null when detached or unreadable. */
  branch: string | null
  /** The tracker item it claimed, as the claim's own answer named it; null before a claim. */
  item: { id: string; title: string; isHeld: boolean } | null
  /** Its place at the shared heavy-work lock: waiting for it, holding it, or neither. */
  lock: 'waiting' | 'holding' | null
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
      band: Shaped<SkeinBand | null>
      /** What the last Clean said, `skein-mcp clean`'s own sentence. */
      cleanSaid: Shaped<string | null>
      agents: Shaped<SkeinAgentRun[]>
      agentsPanel: Shaped<SkeinAgentsPanel>
      agentsNow: number
    }
  }
}
