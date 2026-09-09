// What `POST /api/fleet/save` came back with, as the one sentence above the list (SKEIN-680).
//
// **The partial case is the only one worth a function.** A save that copied seven boxes out of
// eight has to name the eighth: it is the box whose work is still only inside the sandbox, and it
// is the box a person is about to destroy. "7 boxes saved" is an aggregate the failure fits inside,
// which is the shape this exists to refuse — the same rule the CLI's report keeps, said once here
// so the two surfaces cannot drift into disagreeing about the same run.
//
// An entry is a box that made it out when its `error` is empty; the server sends one entry per box
// either way, and a 200 with failures in it is a real answer rather than a fault.
export function savedSummary(boxes) {
  const all = Array.isArray(boxes) ? boxes : [];
  // Not the same as a failure, and not a success either: nothing was asked to happen. Saying "0
  // boxes copied out. Nothing was destroyed" would read as reassurance about an act that did not run.
  if (!all.length) return "no box was copied out.";
  const failed = all.filter(b => b && b.error);
  const done = all.length - failed.length;
  if (!failed.length) {
    return `${done} box${done === 1 ? "" : "es"} copied out to the host. Nothing was stopped and nothing was destroyed.`;
  }
  const names = failed.map(b => (b && b.name) || "an unnamed box").join(", ");
  return `${done} of ${all.length} copied out. ${names} did not — that work is still only inside the sandbox, and a destroy would take it.`;
}
