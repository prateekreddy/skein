#!/bin/sh
# The credential git asks for, chosen by which repository it is asking about.
#
# Installed beside `box-session.sh` in the fleet root and wired in per box as
# `credential.helper`, with `credential.useHttpPath=true` — without that flag git sends only the
# host, every GitHub URL looks identical here, and the whole point (a different answer per
# repository) is impossible.
#
# Two credentials exist, and the difference between them is the boundary:
#
#   * $SKEIN_GH_READ — a fine-grained PAT, read-only, every repository. The default answer.
#   * $SKEIN_GIT_TOKENS/<repo> — a GitHub App installation token scoped to that ONE repository,
#     with write. Placed by the host for the box's own repo, and for any repo its owner has granted
#     in the cockpit. Refreshed before it expires; never minted here, because that would need the
#     App's private key, which no box will ever hold.
#
# So this helper hands over a write credential exactly where a token file exists, and nothing
# anywhere else. GitHub enforces that server-side, which makes it a real bound on the token — and
# not on the box: see the note at the foot of this file, and SKEIN-548.
#
# **Never fails loudly.** A helper that errors takes down every git operation in the box, including
# the reads that were always allowed. Every unexpected shape here exits 0 with no output, which git
# reads as "this helper has nothing" and moves on to the next one.

case "${1-}" in
  get) ;;
  # `store` and `erase` are deliberate no-ops. These tokens live for an hour and are replaced by the
  # host; letting git cache one would hand the box a credential that outlives the grant behind it.
  *) exit 0 ;;
esac

protocol=''
host=''
path=''
# git writes key=value lines and ends with a blank one. `read` returning nonzero on EOF is the
# normal end of input, not an error.
while IFS='=' read -r key value; do
  [ -n "$key" ] || break
  case "$key" in
    protocol) protocol="$value" ;;
    host) host="$value" ;;
    path) path="$value" ;;
  esac
done

[ "$protocol" = "https" ] || exit 0
[ "$host" = "github.com" ] || exit 0

# owner/name from the path git asked about, with the `.git` suffix and any leading slash removed.
path="${path#/}"
owner="${path%%/*}"
rest="${path#*/}"
name="${rest%%/*}"
name="${name%.git}"
case "$owner" in '' | *[!A-Za-z0-9._-]* | -*) exit 0 ;; esac
case "$name" in '' | *[!A-Za-z0-9._-]* | -*) exit 0 ;; esac

[ -n "${SKEIN_GIT_TOKENS-}" ] || exit 0

# Narrowest first. Each of these is a file the HOST placed; none is minted here, and there is no
# credential in the environment to fall back on.
#
#   <owner>%2F<name>  write, this repository alone. Matches `gitgate::token_file` — the slash is
#                     encoded so a repository name can never become a directory, nor address a file
#                     outside the box's own token directory.
#   read/<owner>      read-only across everything the App is installed on for that owner. One file
#                     per owner because an installation token belongs to exactly one installation,
#                     so a personal account and an org are two tokens, not one.
#   read/_any         the optional read-only PAT, for someone who wanted cross-repo reads without
#                     running an App. Absent in the ordinary setup.
token=''
for candidate in \
  "${SKEIN_GIT_TOKENS}/${owner}%2F${name}" \
  "${SKEIN_GIT_TOKENS}/read/${owner}" \
  "${SKEIN_GIT_TOKENS}/read/_any"
do
  if [ -r "$candidate" ]; then
    token="$(cat "$candidate" 2>/dev/null)"
    [ -n "$token" ] && break
  fi
done

# Nothing, rather than something that will not work. A repository with no token here is one this box
# has no credential for, and answering with a token scoped elsewhere would turn a clone that would
# have succeeded without one — every public repo — into a 403. Silence lets git fall through to
# whatever the network answers a request carrying no credential, which is still exactly right: the
# wrong token is a 403 where silence is not.
#
# **What silence is NOT is a boundary, and this comment used to say it was — SKEIN-548, open.**
# It read: "unauthenticated access, which is exactly right: public works, private-and-not-yours
# does not." Measured from inside a live box on 2026-09-07: the sandbox routes HTTP through a
# credential-injecting proxy, so `git ls-remote` against a private repository that is not this
# box's — `GH_TOKEN` unset, this helper answering nothing — lists refs. Staying silent narrows what
# this box's own credential can DO; it does not narrow what the box can REACH. Closing that needs
# the substrate, not this script.
[ -n "$token" ] || exit 0

printf 'username=x-access-token\n'
printf 'password=%s\n' "$token"
