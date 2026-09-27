// The note beside a Settings control that an environment variable is holding right now.
//
// Said only while it is true (the owner, 2026-09-27). `held` is the server's answer — which fields
// a variable holds, and which variable (`config::held_by_env`) — so a note that is always there,
// true or not, cannot come back: nothing held, nothing said.
//
// A switch can only be held OFF, because an environment variable may only turn things off
// (`config::env_holds_off`); the review model is a value, so any value set holds it.
const SWITCHES = ["pr_workflows", "ai_enrichment", "review_summaries", "box_plugin"];

export function heldNote(field, held) {
  const name = held && held[field];
  if (!name || !/^[A-Z0-9_]+$/.test(name)) return "";
  return `${SWITCHES.includes(field) ? "held off by" : "held by"} <code>$${name}</code>`;
}
