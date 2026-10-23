# Finding the cockpit's doorway, reloading it, and keeping what it prints. One implementation for
# both of skein's ways in: `fleet::door_pid`, `fleet::reload_server` and `fleet::start_server`
# (src/fleet/server.rs) embed this file with include_str!, and bootstrap.sh's start-door.sh, which
# runs before any skein binary exists, carries it byte for byte between the `>>> door.sh` and
# `<<< door.sh` marker lines.
# `fleet::server`'s `the_doorway_is_found_one_way` fails when that copy differs. Edit this file,
# then paste it there.
#
# Reload used to be two mechanisms: Rust sent `pkill -USR1 -f <pattern>`, a command-line match
# the repository's own rules ban, and start-door.sh sent `kill -USR1` to the pid in the stamp.
#
# POSIX sh, and definitions only: sourcing it runs nothing.

# skein_stamped_door STAMP PORT DOORWAY
#
# Print the pid of the doorway holding PORT by its stamp, or fail. A pid is a number that gets
# reused, so it counts only when the process is alive AND is this doorway, which its command line
# says exactly: one argument equal to DOORWAY. A stamp left by a doorway that was killed names a
# dead pid and reads as no doorway, which is the honest answer.
skein_stamped_door() {
  read -r _door_pid _door_held <"$1" 2>/dev/null || return 1
  [ "$_door_held" = "$2" ] || return 1
  kill -0 "$_door_pid" 2>/dev/null || return 1
  tr '\0' '\n' <"/proc/$_door_pid/cmdline" 2>/dev/null | grep -qxF -- "$3" || return 1
  printf '%s\n' "$_door_pid"
}

# skein_supervised_door DOORWAY SOCKET...
#
# Print the pid of the doorway a cockpit supervisor runs, or fail: the child of the `skein-server`
# session's pane, on any SOCKET given, whose command line is DOORWAY. Asked by the process tree
# rather than by a pattern, so it cannot find anything a supervisor did not start.
skein_supervised_door() {
  _door_path=$1
  shift
  for _door_sock in "$@"; do
    _door_pane=$(tmux -S "$_door_sock" list-panes -t skein-server -F '#{pane_pid}' 2>/dev/null | head -n 1)
    [ -n "$_door_pane" ] || continue
    for _door_status in $(grep -l "^PPid:[[:space:]]*$_door_pane\$" /proc/[0-9]*/status 2>/dev/null); do
      _door_pid=${_door_status#/proc/}
      _door_pid=${_door_pid%/status}
      if tr '\0' '\n' <"/proc/$_door_pid/cmdline" 2>/dev/null | grep -qxF -- "$_door_path"; then
        printf '%s\n' "$_door_pid"
        return 0
      fi
    done
  done
  return 1
}

# skein_door_reload STAMP PORT DOORWAY SOCKET...
#
# Send the doorway SIGUSR1, which ends the server behind it and re-execs the doorway across the
# SAME listening socket, so the port is never free (architecture §9.4). Print the pid signalled,
# or fail when there was no doorway to signal, which tells the caller to start one.
#
# The stamp's doorway first. When the stamp says nothing, the supervisor's: a doorway that is alive
# but unstamped (the stamp deleted, or naming a pid that is not the doorway) is exactly the one a
# re-exec repairs, because the fresh doorway stamps again across the same descriptor (SKEIN-226).
skein_door_reload() {
  _door_stamp=$1
  _door_port=$2
  _door_way=$3
  shift 3
  _door_target=$(skein_stamped_door "$_door_stamp" "$_door_port" "$_door_way") \
    || _door_target=$(skein_supervised_door "$_door_way" "$@") \
    || return 1
  kill -USR1 "$_door_target" 2>/dev/null || return 1
  printf '%s\n' "$_door_target"
}

# skein_logged LOG MAX COMMAND [ARG...]
#
# Run COMMAND with everything it prints still printed, and also appended to LOG, which is moved to
# LOG.1 (replacing the one before) once it passes MAX bytes: two files, never more (SKEIN-1286).
# Both supervisors run the doorway through this, `fleet::start_server`'s and start-door.sh's, so a
# cockpit writes `$SKEIN_HOME/server.log` however it was started (SKEIN-1317).
#
# A separate reader, not a redirection in the server: the lines a server prints as it exits are the
# ones most worth keeping, and a reader in another process drains them after it has gone. Through a
# FIFO, not a pipe, so COMMAND stays the calling shell's own child: skein_supervised_door finds the
# doorway as the child of the pane's shell, and a pipeline would make it a grandchild. The reader
# is not waited for, because something the server started can hold its output open and the
# supervisor must not hang on that. If the FIFO cannot be made, COMMAND runs unlogged.
# `PYTHONUNBUFFERED`, because the doorway is python and would otherwise hold its lines back.
skein_logged() {
  _log_file=$1
  _log_max=$2
  shift 2
  if _log_dir=$(mktemp -d 2>/dev/null) && mkfifo "$_log_dir/out" 2>/dev/null; then
    {
      _log_n=$(($(cat "$_log_file" 2>/dev/null | wc -c) + 0))
      while IFS= read -r _log_line || [ -n "$_log_line" ]; do
        printf '%s\n' "$_log_line"
        printf '%s\n' "$_log_line" >>"$_log_file"
        _log_n=$((_log_n + ${#_log_line} + 1))
        if [ "$_log_n" -gt "$_log_max" ]; then
          mv -f "$_log_file" "$_log_file.1"
          _log_n=0
        fi
      done
    } <"$_log_dir/out" &
    PYTHONUNBUFFERED=1 "$@" >"$_log_dir/out" 2>&1
    rm -rf "$_log_dir"
  else
    "$@"
  fi
}
