# skein's one package install. Every place skein installs a package runs these two functions:
# the fleet sandbox's substrate at each launch and an approved package request
# (src/fleet/substrate.rs, src/substrate.rs), a runtime update, a takeover's source box
# (src/takeover.rs), a per-VM box's startup kit (src/kit/skein-startup.sh) and bootstrap.sh.
#
# The Rust callers embed this file with include_str! and put their own lines after it. The two
# shell callers run before any skein binary exists, so they carry it byte for byte between the
# `>>> package-install.sh` and `<<< package-install.sh` marker lines instead, and
# `fleet::substrate`'s `every_package_install_runs_the_same_bytes` fails when either copy differs.
# Edit this file, then paste it into both.
#
# They used to be five copies, and they had drifted: two lock waits, three install timeouts,
# a retry in one, a log kept, deleted or thrown away, and npm run as root in all but one.
#
# POSIX sh, and definitions only: sourcing it runs nothing. Each function appends apt's or npm's
# own output to the LOG it is given, because that output is the only thing that says whether the
# mirror, the lock, sudo or the package name was the problem, and returns the tool's own status.
# What a failure MEANS (a failed launch, a refused request, a warning) is the caller's decision.

# skein_apt_install LOG PACKAGE...
skein_apt_install() {
  _skein_log=$1
  shift
  # A fresh sandbox or agent image runs its own first-boot apt, and apt refuses to run twice.
  # Outlast it rather than fail on a race: measured on a real rebuild, where the retry landed on
  # "Could not get lock ... held by process 281 (apt-get)". Two ways of seeing it, because each
  # misses a case: `fuser` is absent from images without psmisc, and between its update and its
  # install an image's own apt holds no lock at all while its process is still running. grep reads
  # all of ps rather than `-q`, which can quit early and fail the pipe under `pipefail`. An image
  # without `fuser`, `ps` or `grep` sees no lock through that one, and does not wait on it.
  _skein_waited=0
  while [ "$_skein_waited" -lt 120 ]; do
    if sudo -n fuser /var/lib/dpkg/lock-frontend /var/lib/apt/lists/lock >/dev/null 2>&1 \
      || ps -eo comm= 2>/dev/null | grep -E '^[[:space:]]*(apt|apt-get|dpkg)[[:space:]]*$' >/dev/null 2>&1; then
      sleep 3
      _skein_waited=$((_skein_waited + 3))
    else
      break
    fi
  done
  {
    _skein_bounded 120 sudo -n apt-get update -qq
    _skein_bounded 180 sudo -n apt-get install -y -qq "$@"
  } >>"$_skein_log" 2>&1 && return 0
  _skein_rc=$?
  # A timeout is not retried: the same mirror gets the same time again and the caller's own
  # deadline runs out first. Anything else, the lock race above most of all, gets one more install.
  # The index is already fetched, so it is not fetched again.
  [ "$_skein_rc" -eq 124 ] && return 124
  sleep 5
  _skein_bounded 180 sudo -n apt-get install -y -qq "$@" >>"$_skein_log" 2>&1
}

# Why these bounds, which every caller shares. `update` FIRST, every time: a fresh image ships an
# empty index, where install reports "Package 'tmux' has no installation candidate", which reads as
# a missing package and is a missing index. `;` rather than `&&` after it: one unreachable source
# fails `update` for the whole index, and the packages wanted may well be on the sources that
# answered. Install's status is the verdict.
#
# `sudo -n`: nothing here can answer a password prompt, and a prompt nobody answers waits out the
# whole timeout before failing.
#
# The numbers are set by the tightest deadline around them, not by the slowest mirror: at worst
# 120 waiting + 120 + 180 + 5 + 180 = 605s of apt, and the fleet's launch runs npm after it inside
# one 900s `sbx exec`, so npm has 240 of what is left. Longer bounds here are a deadline that fires
# on the caller's side instead, where it says nothing about apt. `fleet::start`'s
# `the_provisioning_budget_outlasts_the_script` reads them out of the startup kit's copy.

# `timeout SECS COMMAND...` where the image has `timeout`, and the command unbounded where it does
# not. coreutils is essential on Debian, so that is rare, but bootstrap.sh runs on whatever image
# the sandbox was made from and a missing `timeout` would otherwise fail every install on it.
_skein_bounded() {
  if command -v timeout >/dev/null 2>&1; then
    timeout "$@"
  else
    shift
    "$@"
  fi
}

# skein_npm_install LOG PACKAGE...
#
# Into the prefix a box runs from, which is not the one `sudo npm` writes (SKEIN-968): root's
# global prefix is /usr/local, while every box's PATH leads with the npm-global prefix, owned by
# the sandbox's user. Installed as root, a runtime lands where no box looks, and `npm ls -g`,
# asked as that user, says it is missing at every launch. Unprivileged when the configured
# prefix is writable, `sudo -n` when it is not, as on a plain image with a root-owned prefix.
#
# **A box never sees a package half-installed** (SKEIN-1413, SKEIN-1439). `npm install -g` into
# the live prefix removes the package and writes it back, and that takes seconds even at the same
# version. Every box shares the prefix, so a box starting in that window found no `claude` and
# dropped to bash: on 2026-10-31 three boxes did. So npm installs into a prefix of its own,
# `lib/.skein-v.XXXXXX`, which this install keeps, and what a box reads is moved over to it by
# renames alone, each of which replaces a link in one step:
#
# 1. Each command, `bin/<cmd>`, becomes a link to the same command in the new prefix, made under a
#    temporary name and renamed over the old link. From here on the command runs the new package
#    and needs nothing else below.
# 2. Each package, `lib/node_modules/<name>`, becomes a link into the new prefix the same way, so
#    `npm ls -g` and a path written through the package still find it. Where it is still the real
#    directory npm made, a rename cannot put a link over it: `mv --exchange` swaps the two in one
#    step where mv and the file system have it, else the directory is moved aside and the link
#    renamed into its place. The package's own path is then missing for one rename, but no command
#    goes through it any more (step 1). coreutils before 9.5 has no `--exchange`, and the first
#    cut's remove-then-rename here left `claude` missing on such a machine (SKEIN-1439).
# 3. Only then is an old prefix removed: one this install's links replaced and no link still names.
#
# Node finds a package's dependencies inside the package itself, a global install's layout, and
# resolves a script through its links to where it really is, so a link to a link is enough.
# `--globalconfig` keeps the live prefix's npmrc, which npm would otherwise look for under the new
# prefix. Two installs at once each have their own prefix, and the last rename wins.
# No dollar-brace expansions: the kit's copy is read by sbx, which takes them for placeholders.
skein_npm_install() {
  _skein_log=$1
  shift
  _skein_prefix="$(npm config get prefix 2>/dev/null)"
  case "$_skein_prefix" in undefined | null) _skein_prefix="" ;; esac
  if [ -z "$_skein_prefix" ]; then
    _skein_bounded 240 sudo -n npm install -g "$@" >>"$_skein_log" 2>&1
    return
  fi
  _skein_as=""
  [ -w "$_skein_prefix" ] || _skein_as="sudo -n"
  _skein_nm="$_skein_prefix/lib/node_modules"
  $_skein_as mkdir -p "$_skein_nm" "$_skein_prefix/bin" >>"$_skein_log" 2>&1 || return 1
  _skein_v="$($_skein_as mktemp -d "$_skein_prefix/lib/.skein-v.XXXXXX" 2>>"$_skein_log")" || return 1
  _skein_id="$(basename "$_skein_v")"
  # mktemp makes it 0700, and every box reads it.
  $_skein_as chmod 755 "$_skein_v"
  _skein_bounded 240 $_skein_as npm install -g --prefix "$_skein_v" \
    --globalconfig "$_skein_prefix/etc/npmrc" "$@" >>"$_skein_log" 2>&1
  _skein_rc=$?
  if [ "$_skein_rc" -ne 0 ]; then
    $_skein_as rm -rf "$_skein_v"
    return "$_skein_rc"
  fi
  # What the renames replace, for step 3, and the real directories moved aside.
  _skein_was=""
  _skein_aside=""
  for _skein_cmd in $(cd "$_skein_v/bin" 2>/dev/null && for n in *; do
    { [ -e "$n" ] || [ -L "$n" ]; } && echo "$n"
  done); do
    _skein_live="$_skein_prefix/bin/$_skein_cmd"
    _skein_tmp="$_skein_prefix/bin/.$_skein_cmd.skein-new"
    _skein_was="$_skein_was $(readlink "$_skein_live" 2>/dev/null)"
    $_skein_as ln -sfn "../lib/$_skein_id/bin/$_skein_cmd" "$_skein_tmp" \
      && $_skein_as mv -f -T "$_skein_tmp" "$_skein_live" || _skein_rc=1
  done
  # Package names, `name` or `@scope/name`; npm's names hold no white space.
  for _skein_name in $(cd "$_skein_v/lib/node_modules" 2>/dev/null && for n in * @*/*; do
    case "$n" in @*/* | [!@]*) [ -d "$n" ] && echo "$n" ;; esac
  done); do
    _skein_live="$_skein_nm/$_skein_name"
    _skein_tmp="$(dirname "$_skein_live")/.$(basename "$_skein_live").skein-new"
    _skein_up=".."
    case "$_skein_name" in */*) _skein_up="../.." ;; esac
    $_skein_as mkdir -p "$(dirname "$_skein_live")"
    $_skein_as ln -sfn "$_skein_up/$_skein_id/lib/node_modules/$_skein_name" "$_skein_tmp" \
      || { _skein_rc=1; continue; }
    if [ -d "$_skein_live" ] && [ ! -L "$_skein_live" ]; then
      if $_skein_as mv -T --exchange "$_skein_tmp" "$_skein_live" 2>/dev/null; then
        _skein_aside="$_skein_aside $_skein_tmp"
      elif $_skein_as mv -T "$_skein_live" "$_skein_tmp.old" \
        && $_skein_as mv -f -T "$_skein_tmp" "$_skein_live"; then
        _skein_aside="$_skein_aside $_skein_tmp.old"
      else
        _skein_rc=1
      fi
    else
      _skein_was="$_skein_was $(readlink "$_skein_live" 2>/dev/null)"
      $_skein_as mv -f -T "$_skein_tmp" "$_skein_live" || _skein_rc=1
    fi
  done
  for _skein_d in $_skein_aside; do
    $_skein_as rm -rf "$_skein_d"
  done
  # Every link that still names a prefix, none of which is removed. Besides the ones this install
  # replaced, a prefix an hour old is a leftover: of a killed install, or of one whose package a
  # hand-run `npm install -g` has since put back as a directory.
  _skein_named="$(for l in "$_skein_prefix"/bin/* "$_skein_nm"/* "$_skein_nm"/@*/*; do
    [ -L "$l" ] && readlink "$l"
  done) "
  for _skein_d in $( {
    for t in $_skein_was; do
      echo "$t" | sed -n 's|^\(\.\./\)*\(lib/\)*\(\.skein-v\.[A-Za-z0-9]*\)/.*|\3|p'
    done
    cd "$_skein_prefix/lib" && find . -maxdepth 1 -name '.skein-v.*' -mmin +60 | sed 's|^\./||'
  } 2>/dev/null | sort -u); do
    case "$_skein_d" in "$_skein_id" | */* | *[!A-Za-z0-9.-]*) continue ;; esac
    case "$_skein_named" in *"/$_skein_d/"*) ;; *) $_skein_as rm -rf "$_skein_prefix/lib/$_skein_d" ;; esac
  done
  return "$_skein_rc"
}
