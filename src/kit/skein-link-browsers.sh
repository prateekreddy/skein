#!/usr/bin/env bash
# skein-link-browsers.sh — link the fleet's Playwright browsers into this box's own browsers
# directory (SKEIN-1341, SKEIN-1355). Run inside a box, as the box: `bash <this file>`.
#
# The ONE copy of this logic, with three callers that must not drift apart:
#   * the kit, as every box starts (`skein-startup.sh`, which runs this file from beside itself);
#   * skein, right after a fill publishes a browser, through its crossing into each running box
#     (`fleet::browsers::link_running_boxes`);
#   * the box's own agent, whenever it likes: after a browser request is answered `installed`,
#     `skein_requests` names this command.
#
# The store is skein's, read-only in a box, beside this file under the fleet root's `.skein`; the
# directory linked into is the one the launcher names as PLAYWRIGHT_BROWSERS_PATH. A link the box
# planted can only mislead the box itself, because this runs in the box's namespace and never from
# outside it. A real file or directory is the box's own download and is left alone, and so is any
# link at an entry's name, whoever made it; a link into the store whose entry is gone is removed, so
# Playwright downloads that version privately instead of failing on it. Only names shaped like a
# Playwright browser directory, `<name>-<revision>` in lower case, are linked: a name in the store
# becomes a path in every box. With no store here (a per-VM sandbox) it does nothing.
#
# brace-free, like the kit beside it. Safe to run any number of times, and it always exits 0.
set -u
skein_browsers="$(cd "$(dirname "$0")" 2>/dev/null && pwd)/browsers"
skein_pw="$HOME/.cache/ms-playwright"
[ -d "$skein_browsers" ] && mkdir -p "$skein_pw" 2>/dev/null || exit 0
for skein_link in "$skein_pw"/*; do
  [ -L "$skein_link" ] || continue
  case "$(readlink "$skein_link")" in
    "$skein_browsers"/*) [ -e "$skein_link" ] || rm -f "$skein_link" ;;
  esac
done
for skein_entry in "$skein_browsers"/*; do
  [ -d "$skein_entry" ] || continue
  skein_name="$(basename "$skein_entry")"
  case "$skein_name" in [a-z]*-[0-9]*) ;; *) continue ;; esac
  case "$skein_name" in *[!a-z0-9_-]*) continue ;; esac
  [ -e "$skein_pw/$skein_name" ] || [ -L "$skein_pw/$skein_name" ] ||
    ln -s "$skein_entry" "$skein_pw/$skein_name" 2>/dev/null || true
done
exit 0
