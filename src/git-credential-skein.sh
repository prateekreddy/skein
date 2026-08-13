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
# So a push succeeds exactly where a token file exists and is refused by GitHub everywhere else.
# The refusal is the real boundary — server-side, and true no matter what runs in the box.
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

token=''
# Must match `gitgate::token_file`: the slash is encoded so a repository name can never become a
# directory, nor address a file outside the box's own token directory.
file="${SKEIN_GIT_TOKENS-}/${owner}%2F${name}"
if [ -n "${SKEIN_GIT_TOKENS-}" ] && [ -r "$file" ]; then
  token="$(cat "$file" 2>/dev/null)"
fi
# Anything without a token of its own gets the read-only one, so cloning and fetching any repository
# keeps working exactly as before. A push with it comes back 403 from GitHub, which is the intended
# answer and the one `git`'s shim explains.
[ -n "$token" ] || token="${SKEIN_GH_READ-}"
[ -n "$token" ] || exit 0

printf 'username=x-access-token\n'
printf 'password=%s\n' "$token"
