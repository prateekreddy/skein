// One input, one answer, at both doors of skein's lane list (docs/decisions/skein-is-a-mod.md, "The
// shape a feature takes"): the `skein_lane` MCP tool and `skein-mcp lane list` on the command line,
// which the hooks module's agents panel reads (SKEIN-1374).
//
// `records` are the files in `$SKEIN_STATE/lanes/`, by lane name; `result` and `said` are what skein
// answers for them. `tests/plugin_lanes.rs` (`both_doors_of_lane_list_give_the_contracts_answer`)
// writes the records under a directory written here as `/fixture`, with the lane root at
// `/fixture/lanes`, and holds both doors to this answer. Then `agents-panel.test.tsx` hands the
// command line's answer to the panel and holds it to the lanes in it: lane `a` is drawn, and lane
// `b`, from a newer skein, is not. So a change at either door that the other does not share fails
// one of the two.
//
// Read by the Rust test as JSON: everything after `LANE_CONTRACT = ` must stay JSON.

export const LANE_CONTRACT = {
  "records": {
    "a": {
      "schema": 1,
      "name": "a",
      "item": "SKEIN-9",
      "worktree": "/fixture/lanes/a",
      "branch": "lane/a",
      "base": "master",
      "base_sha": "0123456789abcdef0123456789abcdef01234567",
      "target": "/fixture/lanes/a/target",
      "rules": "/fixture/rules.md",
      "brief": "/fixture/state/lanes/a.brief.md",
      "git_dir": "/fixture/repo/.git",
      "phase": "created",
      "created": "2026-10-30T10:00:00Z",
      "updated": "2026-10-30T10:00:00Z"
    },
    "b": {
      "schema": 2,
      "name": "b",
      "item": "SKEIN-10",
      "worktree": "/fixture/lanes/b",
      "branch": "lane/b",
      "phase": "reported"
    }
  },
  "result": {
    "root": "/fixture/lanes",
    "lanes": [
      {
        "schema": 1,
        "name": "a",
        "item": "SKEIN-9",
        "worktree": "/fixture/lanes/a",
        "branch": "lane/a",
        "base": "master",
        "base_sha": "0123456789abcdef0123456789abcdef01234567",
        "target": "/fixture/lanes/a/target",
        "rules": "/fixture/rules.md",
        "brief": "/fixture/state/lanes/a.brief.md",
        "git_dir": "/fixture/repo/.git",
        "phase": "created",
        "created": "2026-10-30T10:00:00Z",
        "updated": "2026-10-30T10:00:00Z"
      }
    ],
    "refused": [{ "name": "b", "error": "newer-schema", "schema": 2 }]
  },
  "said": "skein: left out lane b, recorded by a newer skein (schema 2; this one reads 1)."
}
