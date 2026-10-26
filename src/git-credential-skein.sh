#!/bin/sh
# The credential git asks for, chosen by which repository it is asking about.
#
# Installed beside `box-session.sh` in the fleet root and wired in per box as
# `credential.helper`, with `credential.useHttpPath=true` — without that flag git sends only the
# host, every GitHub URL looks identical here, and the whole point (a different answer per
# repository) is impossible.
#
# Every credential this helper can hand over is a FILE the host placed under $SKEIN_GIT_TOKENS; none
# is read from the environment (there is no $SKEIN_GH_READ, and there never was one anything read).
# The difference between the files is the boundary:
#
#   * $SKEIN_GIT_TOKENS/<owner>%2F<name> — a GitHub App installation token (or a per-repo PAT)
#     scoped to that ONE repository, with write. Placed by the host for the box's own repo, and for
#     any repo its owner has granted in the cockpit. Refreshed before it expires; never minted here,
#     because that would need the App's private key, which no box will ever hold.
#   * $SKEIN_GIT_TOKENS/read/<owner> — a read-only installation token, everything the App is
#     installed on for that owner. One file per owner, because an installation token belongs to one
#     installation. The default answer for a repo with no write file.
#   * $SKEIN_GIT_TOKENS/read/_any — the optional cross-repo read-only PAT, for someone who wanted
#     reads without an App. Absent in the ordinary setup.
#
# The lookup itself is below (`for candidate in …`), narrowest first; this list is only what the
# files mean.
#
# So this helper hands over a write credential exactly where a token file exists, and nothing
# anywhere else. GitHub enforces that server-side, which makes it a real bound on the token — and
# not on the box: see the note at the foot of this file, and SKEIN-548.
#
# **Never fails loudly.** A helper that errors takes down every git operation in the box, including
# the reads that were always allowed. Every unexpected shape here exits 0 with no output, which git
# reads as "this helper has nothing" and moves on to the next one. The one thing it says is a line
# on stderr when a GitHub repository has no token here at all (below), and it still exits 0.

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
# **And for a scoped box, silence now IS a boundary again — SKEIN-548 closed for the git path.**
# This comment used to record the hole: the sandbox proxy terminates TLS for the GitHub hosts and
# answered a request carrying no credential as the ACCOUNT, so `git ls-remote` against a private repo
# that was not this box's — helper answering nothing — listed refs anyway. `src/box-session.sh` now
# puts the GitHub hosts in `NO_PROXY` for a scoped box, so git reaches GitHub DIRECT: the proxy never
# sees the request, and a private repo this box holds no token for is refused by GitHub itself (401/
# 404). Silence therefore falls through to a genuinely unauthenticated DIRECT request — public still
# clones, private-and-not-yours does not. What remains outside this helper's reach is a process that
# deliberately routes back through the proxy; the substrate's egress policy (SKEIN-926), not this
# script, is what answers that. A `fleet`-scoped box keeps the proxy on purpose and this note does
# not apply to it.
#
# **Silence on stdout, and one line on stderr saying why** (SKEIN-1339). git asks a helper only
# after GitHub has refused a request carrying no credential, so a public repository never reaches
# here, and git's own words for what follows — "could not read Username for
# 'https://github.com/…'" — name neither the cause nor the fix. A submodule from another repository
# is the usual way in. The line names only the repository git asked about, which git already
# printed; nothing about any other repository's token is said, nor whether one exists.
if [ -z "$token" ]; then
  printf 'skein: this box has no GitHub token for %s/%s, so git cannot sign in to it. To reach it, ask the person to add %s/%s as a repo in the cockpit, share a write token with it from its card, or store a read-only token under Settings → GitHub & keys.\n' \
    "$owner" "$name" "$owner" "$name" >&2
  exit 0
fi

printf 'username=x-access-token\n'
printf 'password=%s\n' "$token"
