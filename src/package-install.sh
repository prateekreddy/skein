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
skein_npm_install() {
  _skein_log=$1
  shift
  _skein_prefix="$(npm config get prefix 2>/dev/null)"
  case "$_skein_prefix" in undefined | null) _skein_prefix="" ;; esac
  if [ -n "$_skein_prefix" ] && [ -w "$_skein_prefix" ]; then
    _skein_bounded 240 npm install -g "$@" >>"$_skein_log" 2>&1
  else
    _skein_bounded 240 sudo -n npm install -g "$@" >>"$_skein_log" 2>&1
  fi
}
