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
      agents: Shaped<SkeinAgentRun[]>
      agentsPanel: Shaped<SkeinAgentsPanel>
      agentsNow: number
    }
  }
}
