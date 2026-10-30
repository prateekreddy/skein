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

declare module 'claude-code' {
  interface PluginState {
    skein: {
      identity: Shaped<SkeinIdentity | null>
      attention: Shaped<SkeinAttention | null>
    }
  }
}
