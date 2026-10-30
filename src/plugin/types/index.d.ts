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

declare module 'claude-code' {
  interface PluginState {
    skein: {
      identity: Shaped<SkeinIdentity | null>
      leases: Shaped<SkeinLeases | null>
      guide: Shaped<SkeinGuide | null>
    }
  }
}
