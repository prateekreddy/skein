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

/** What the one line above the prompt says: how many things wait on the person, and in what few
 * words, the loudest first (`hooks/band.tsx`). */
export type SkeinBand = { count: number; what: string[] }

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
    }
  }
}
