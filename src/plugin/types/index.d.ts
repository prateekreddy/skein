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

declare module 'claude-code' {
  interface PluginState {
    skein: {
      identity: Shaped<SkeinIdentity | null>
    }
  }
}
