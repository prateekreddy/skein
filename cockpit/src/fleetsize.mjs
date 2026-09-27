// The size the fleet sandbox has, measured, for the Fleet pane (the owner, 2026-09-27, R1).
//
// The measured sandbox is the truth. `fleet_memory`, `fleet_cpus` and `fleet_disk` in config.json
// are only what the NEXT create asks sbx for, and sbx fixes a sandbox's size when it is made, so the
// two differ on any fleet whose settings were edited since. The pane shows both, each labelled for
// what it is, and nothing here compares them: a difference is what editing the next size means, not
// a fault.
//
// `r` is `/api/fleet/resources` (`FleetResources`, MiB throughout); `fmt` is the page's mebibyte
// formatter, passed in because these modules are concatenated without imports.
export function sandboxSize(r, fmt) {
  if (!r || !r.mem_total) return "";
  return [
    r.cpus ? `${r.cpus} CPUs` : "",
    `${fmt(r.mem_total)} memory`,
    r.disk_total ? `${fmt(r.disk_total)} disk` : "",
  ].filter(Boolean).join(" · ");
}
