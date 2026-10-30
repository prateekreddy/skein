// One input, one answer, at both doors of skein's clean (docs/decisions/skein-is-a-mod.md, "The
// shape a feature takes"): the `skein_clean` MCP tool and the hooks module's `clean` tool.
//
// Each case is a list of paths and the answer skein gives for it: the result, and the sentence for
// the agent after it. `tests/plugin_tools.rs` (`both_doors_of_clean_give_the_contracts_answer`)
// plants the fixture its other clean tests plant, under a directory written here as `/fixture`,
// and holds the MCP tool and `skein-mcp clean` on the command line to these answers. Then
// `band.test.tsx` hands the command line's answer to the hooks module's tool and holds it to the
// same. So a change at either door that the other does not share fails one of the two.
//
// Read by the Rust test as JSON: everything after `CLEAN_CONTRACT = ` must stay JSON.

export const CLEAN_CONTRACT = [
  {
    "paths": ["/fixture/fleet/web-main/home/.cache/pip"],
    "result": {
      "removed": ["/fixture/fleet/web-main/home/.cache/pip"],
      "freed_bytes": 4096,
      "failed": []
    },
    "said": "skein: removed 1, about 1M. This box's disk figure catches up within five minutes."
  },
  {
    "paths": ["/fixture/fleet/web-main/home/.cache/agent-notes"],
    "result": {
      "error": "not-on-the-list",
      "paths": ["/fixture/fleet/web-main/home/.cache/agent-notes"]
    },
    "said": "skein: removed nothing. Not on this box's list now: /fixture/fleet/web-main/home/.cache/agent-notes. Call skein_clean with no paths to see what can be removed."
  },
  {
    "paths": ["/fixture/fleet/web-main/home/.cache/skein/claude/claude-1000/-p/old"],
    "result": {
      "error": "not-safe-now",
      "paths": [
        {
          "path": "/fixture/fleet/web-main/home/.cache/skein/claude/claude-1000/-p/old",
          "why": "a scratchpad may belong to a session still running in this box"
        }
      ]
    },
    "said": "skein: removed nothing. /fixture/fleet/web-main/home/.cache/skein/claude/claude-1000/-p/old: a scratchpad may belong to a session still running in this box."
  }
]
