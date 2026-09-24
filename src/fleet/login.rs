//! What a login is and which copy is best: the launcher's merge, healing a dead login from a
//! live one, and sharing the fleet's login with every box.

use super::*;

/// Putting one login into one file — the ONE implementation of it, shared by both scripts that do.
///
/// **Two spellings of a merge is how the grants got destroyed.** `heal_logins_script` merges, and
/// its own comment claimed "the same rule the launcher's own credential sync already enforces";
/// [`share_login_script`] — the path an interactive `skein login` runs, and the one the owner
/// reports as the only one that works — did a whole-file `cp`. So every login handed every box the
/// sandbox's `mcpOAuth` and destroyed the box's own, which is its identity at a work-tracking
/// gateway and which SURVIVES a logout, that being exactly why `rank` ignores those grants when
/// judging a login (SKEIN-489). A shared string cannot drift; two of them did.
///
/// Interpolated into a `format!`, so it carries no `{}` of its own to escape — which is the second
/// reason it is here rather than inline.
const LOGIN_MERGE_PY: &str = r#"
KEYS = ("accessToken", "refreshToken", "access_token", "refresh_token", "OPENAI_API_KEY")


def blocks(data):
    for b in (data.get("claudeAiOauth"), data.get("tokens"), data):
        if isinstance(b, dict):
            yield b


def carries(path):
    """Is there a login in this file at all — the husk test, and never `mcpOAuth`.

    Those grants survive a logout, so counting them would make every corpse look alive."""
    try:
        data = json.load(open(path))
    except Exception:
        return False
    if not isinstance(data, dict):
        return False
    return any(
        any(str(b.get(k) or "").strip() for k in KEYS) for b in blocks(data)
    )


def merged(source_path, dest_path):
    """The LOGIN blocks move; nothing else does.

    `mcpOAuth` in particular stays where it is: that block holds per-box grants for MCP servers, and
    a box's work-tracking gateway belongs to its repository."""
    src = json.load(open(source_path))
    try:
        dst = json.load(open(dest_path))
    except Exception:
        dst = {}
    if not isinstance(dst, dict):
        dst = {}
    for block in ("claudeAiOauth", "tokens"):
        if isinstance(src.get(block), dict):
            dst[block] = src[block]
    for k in KEYS:
        if str(src.get(k) or "").strip():
            dst[k] = src[k]
    return dst


def place(source_path, dest_path, times=None):
    """Give the destination the source's login, and say whether anything actually changed.

    Through a temporary and a rename, because a running agent reads this file and a half-written one
    is a logged-out box. `times` carries the SOURCE's timestamps IN NANOSECONDS where the caller
    wants the destination to age with the credential rather than with the copy — nanoseconds because
    the mtime is a tiebreak, and a copy rounded a hundred nanoseconds past its own source outranks
    it and starts the two trading places.

    Returns False when there was nothing to do — and that is load-bearing, not an optimisation.
    `login_written_ms` reads this file's mtime as the evidence that a credential was REPLACED since a
    refusal was recorded against it, so rewriting identical bytes would clear every remembered
    refusal, for ever."""
    want = merged(source_path, dest_path)
    try:
        have = json.load(open(dest_path))
    except Exception:
        have = None
    if have == want:
        return False
    where = os.path.dirname(dest_path)
    os.makedirs(where, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=where)
    try:
        with os.fdopen(fd, "w") as f:
            json.dump(want, f)
        os.chmod(tmp, 0o600)
        os.replace(tmp, dest_path)
    except Exception:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        return False
    if times is not None:
        try:
            os.utime(dest_path, ns=times)
        except OSError:
            pass
    return True
"#;

/// The files that make a login a login, relative to a HOME.
pub(super) const LOGIN_FILES: [&str; 2] = [".claude/.credentials.json", ".codex/auth.json"];

/// The token keys that mean "signed in", across both runtimes' shapes.
pub(super) const LOGIN_KEYS: [&str; 5] = [
    "accessToken",
    "refreshToken",
    "access_token",
    "refresh_token",
    "OPENAI_API_KEY",
];

/// Does this credentials file actually contain a login?
///
/// The named blocks and the top level, never `mcpOAuth`: that block holds a per-repo grant for an
/// MCP server, and an MCP token says nothing about whether the *agent* is signed in. Counting it
/// would make every husk look like a login, since the grants survive a logout.
///
/// Unparseable or empty ⇒ `false`, which is the safe direction: it only ever declines to propagate.
pub(super) fn carries_login(bytes: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    let has = |b: Option<&serde_json::Value>| {
        b.and_then(|b| b.as_object()).is_some_and(|b| {
            LOGIN_KEYS.iter().any(|k| {
                b.get(*k)
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| !t.trim().is_empty())
            })
        })
    };
    has(v.get("claudeAiOauth")) || has(v.get("tokens")) || has(Some(&v))
}

/// What the host holds for one runtime — three-valued, because two of the states used to be one.
///
/// [`carries_login`] answers "is there a login here" and nothing else; it never looks at expiry.
/// That was the right question for propagation (a dead token still seeds boxes — a heal can replace
/// it, where nothing can replace a void) and the wrong one for reporting: on a fleet-wide logout
/// every surface said "signed in", so the symptom read as "each box needs a login" instead of "the
/// fleet's credential is dead". This type is the reporting answer; propagation keeps asking
/// [`carries_login`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginState {
    /// A credential a box can use, or refresh into one.
    Live,
    /// Tokens are present but the refresh token died then — every box holds the same dead one,
    /// and the fix is one `skein login`, not one per box.
    Expired { at_ms: i64 },
    /// No credential: missing file, unparseable, or a logout's husk with its tokens blanked.
    Absent,
}

/// Judge one credentials file, against `now_ms`.
///
/// **`refreshTokenExpiresAt`, not `expiresAt`** — the same eyes as the launcher's heal script
/// (`rank()` in [`heal_logins_script`]) and as [`refreshable_login_at`]: the access token expires
/// in hours and is renewed without being asked, so a past `expiresAt` is the ordinary state of a
/// healthy login. Like the heal script, only a positive number counts as a recorded expiry, and a
/// shape that records none is `Live` — the honest reading of "it did not say" is not "it is dead".
/// The block that carries the tokens is the block whose expiry is believed, first one wins, which
/// is the heal script's walk exactly; `the_host_and_the_launcher_agree_on_what_a_login_is` holds
/// the two implementations together.
pub(super) fn login_state(bytes: &[u8], now_ms: i64) -> LoginState {
    if !carries_login(bytes) {
        return LoginState::Absent;
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return LoginState::Absent;
    };
    for block in [v.get("claudeAiOauth"), v.get("tokens"), Some(&v)]
        .into_iter()
        .flatten()
        .filter_map(|b| b.as_object())
    {
        let carries = LOGIN_KEYS.iter().any(|k| {
            block
                .get(*k)
                .and_then(|t| t.as_str())
                .is_some_and(|t| !t.trim().is_empty())
        });
        if !carries {
            continue;
        }
        let dies = ["refreshTokenExpiresAt", "refresh_token_expires_at"]
            .iter()
            .filter_map(|k| block.get(*k).and_then(|d| d.as_i64()).filter(|d| *d > 0))
            .next();
        return match dies {
            Some(at_ms) if at_ms <= now_ms => LoginState::Expired { at_ms },
            _ => LoginState::Live,
        };
    }
    // Unreachable while `carries_login` and the walk above agree on what carrying means — but if
    // they ever drift, declining to report a login is the direction that only under-claims.
    LoginState::Absent
}

/// One runtime's login as the host reports it. Reporting only — nothing that seeds or heals reads
/// this; those paths keep [`carries_login`] and [`heal_logins_script`]'s own judgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLogin {
    pub runtime: &'static str,
    pub state: LoginState,
}

/// A runtime whose kept credential has died, and when — the "when" is what turns "each box wants a
/// login" into "the fleet's credential died Tuesday". Serialized into `/api/health`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExpiredLogin {
    pub runtime: String,
    /// RFC3339, UTC.
    pub expired_at: String,
    /// Which of [`expired_logins`]'s two witnesses said so.
    pub witness: Witness,
    /// The refusal's own words. Empty for [`Witness::Credential`], which has none to give: a file
    /// says when a token is due to die, not what happened when one was used.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub said: String,
}

/// Which witness reported a login dead — see [`expired_logins`] for why there are two.
///
/// **They are different sentences and they lead to different actions.** "The credential file says
/// this expired on Tuesday" is a date, knowable with nothing running; "a model call was refused at
/// 11:03 with *OAuth session expired*" is something that happened, and is the only one of the two
/// that can be wrong about the present — which is exactly why `crate::ai`'s refusal memory now
/// expires. A banner that renders them identically leaves a person unable to tell a credential
/// that is dead from one that was dead a moment ago.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Witness {
    /// The credential file's own `refreshTokenExpiresAt`, already in the past.
    Credential,
    /// A model call that came back refused.
    Refusal,
}

/// The script that spreads one live login across the fleet — down and sideways freely, **up only
/// on evidence**.
///
/// Separated from [`heal_logins`] so a test can drive it against a fixture of box roots rather than
/// against a sandbox.
///
/// `refused` is `(runtime, epoch ms)` for a model call skein made that came back refused, and it is
/// the only thing that lets a box's copy replace the fleet's own — see the placement loop, and
/// ISO-5 for what ranking by the file's own claim cost. `None` for every other call, which is every
/// caller but [`heal_logins`]: a test driving this against a fixture is asking about the election,
/// and the election is the half that does not change.
pub(super) fn heal_logins_script(refused: Option<(&str, i64)>) -> String {
    let merge = LOGIN_MERGE_PY;
    let root = sh_quote(&fleet_root());
    // One word per login file, carrying the refusal skein has observed against the FLEET's copy of
    // that runtime's credential — `<rel>|<epoch ms>`, and `0` for "nothing was refused". Packed
    // into the loop word rather than sent as a second list because the two have to stay in step:
    // a refusal paired with the wrong file is evidence about a credential it is not about.
    let files = LOGIN_FILES
        .iter()
        .map(|rel| {
            let at_ms = refused
                .filter(|(runtime, _)| *runtime == runtime_of(rel))
                .map(|(_, at_ms)| at_ms)
                .unwrap_or(0);
            sh_quote(&format!("{rel}|{at_ms}"))
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"set -u
command -v python3 >/dev/null 2>&1 || exit 0
for pair in {files}; do
  rel="${{pair%%|*}}"; refused="${{pair##*|}}"
  python3 - "$refused" "$HOME/$rel" {root}/*/home/"$rel" <<'SKEIN_HEAL'
import hashlib, json, os, sys, tempfile, time

# WHICH COPY IS THE FLEET'S LOGIN, and which of the others have to be given it.
#
# Two fields, two jobs, and collapsing them into one is what broke this twice.
#
# `refreshTokenExpiresAt` decides CANDIDACY. A refresh token past its own expiry cannot be renewed
# into anything, so a copy holding one is not a source. A shape that records none stays a candidate
# and ranks below anything that does: the honest reading of "it did not say" is not "it is dead".
#
# `expiresAt` decides WHICH CANDIDATE WINS, and that half was missing. It is the only evidence IN
# THE FILE that a credential actually WORKS: an access token can only be obtained by successfully
# exercising the refresh token, so a fresh `expiresAt` reports a refresh that HAPPENED, where
# `refreshTokenExpiresAt` is a claim about the future that an invalidated credential goes on making
# to the day it was minted to die. A dead login cannot claim a refresh it never made.
#
# Measured on a live fleet, 2026-08-29: five copies, FOUR distinct refresh tokens, every one
# claiming hundreds of hours of life. Ranking by the claim elected a copy two boxes were already
# logged out of and left the working one last; ranking by the last successful refresh elects the
# copy that had just been used.
NOW = time.time() * 1000
{merge}

def number(value):
    """The value if it is a real number, else None. `bool` is an `int` in Python, and `True` read as
    an expiry is a login dated 1970 — the launcher's `login_life` guards the same trap."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return value


def rank(path):
    """How good this credential is, or None if nothing may be seeded from it.

    A tuple, because it sorts: last successful refresh first, then the file's own mtime as the
    tiebreak for shapes that record no expiry at all.

    **This is the launcher's `login_life` written in another language, deliberately statement for
    statement** — the same walk over the same blocks, the same first-expiry-field-per-block, the
    same max across them, the same mtime tiebreak. It has to be: the two run on opposite sides of
    the box boundary on the same files, and when they drifted apart they elected opposite winners on
    a live fleet. `the_two_elections_agree_on_which_login_is_best` runs both over one fixture set
    and is what holds them together."""
    try:
        data = json.load(open(path))
    except Exception:
        return None
    if not isinstance(data, dict):
        return None
    found, worked = False, 0
    for b in blocks(data):
        # Never `mcpOAuth`: those grants survive a logout and would make every corpse look alive.
        if not any(str(b.get(k) or "").strip() for k in KEYS):
            continue
        dies = number(b.get("refreshTokenExpiresAt") or b.get("refresh_token_expires_at"))
        if dies is not None and dies <= NOW:
            continue
        found = True
        for k in ("expiresAt", "expires_at", "expiry"):
            v = number(b.get(k))
            if v is not None and v > 0:
                worked = max(worked, int(v))
                break
    if not found:
        return None
    try:
        written = os.path.getmtime(path)
    except OSError:
        written = 0
    return (worked, written)


# An unmatched glob arrives as its own pattern — a fleet whose boxes have no credentials yet
# passes the literal `*/home/...`, and the loop below would cheerfully MAKE that path, `*` and all.
# It cannot simply be dropped for not existing: the sandbox's own copy may legitimately be absent
# and is the one path here that must be created.
paths = [p for p in sys.argv[2:] if "*" not in p]

def digest(path):
    """A name for the credential in this file, so evidence about it can be told from evidence about
    the copy that replaced it.

    sha256 over the login tokens themselves, sorted so that a client which reorders its own JSON is
    not read as a new credential — `login_fingerprint` in Rust makes the same walk for the same
    reason. Truncated: this is an identity check between two things skein wrote, not a signature,
    and a short name keeps the sidecar readable by a person looking at a fleet."""
    try:
        data = json.load(open(path))
    except Exception:
        return ""
    if not isinstance(data, dict):
        return ""
    for b in blocks(data):
        tokens = sorted(str(b.get(k)) for k in KEYS if str(b.get(k) or "").strip())
        if tokens:
            return hashlib.sha256("\0".join(tokens).encode()).hexdigest()[:16]
    return ""


# WHAT SKEIN ITSELF HAS OBSERVED ABOUT THE FLEET'S OWN COPY — `login_evidence.json`, beside it.
#
# The election above ranks a credential by what its own file claims, and that is the whole of
# ISO-5: a box writes the file, so a box can write the claim. `{{"expiresAt": 9e15}}` in a box's
# private HOME used to make that box the fleet's login.
#
# Evidence skein produced cannot be written by a box. There is one piece of it and it is a
# refusal: a model call that came back "OAuth session expired" is something that HAPPENED, to a
# credential named by `refresh_hash`. `refreshed_at` is the same shape in the other direction —
# the last time skein saw a call SUCCEED with it — and nothing records one yet, so it is read
# (a success retires a failure) and not written. The hash is what retires stale evidence: a
# refusal is about the credential it names, and the moment that credential is replaced the
# evidence stops applying, which is `crate::ai`'s SKEIN-348 rule spelled here.
home_path = sys.argv[2]
evidence_path = os.path.join(os.path.dirname(home_path), "login_evidence.json")
try:
    evidence = json.load(open(evidence_path))
    if not isinstance(evidence, dict):
        evidence = {{}}
except Exception:
    evidence = {{}}

refused_at = 0
try:
    refused_at = int(sys.argv[1] or 0)
except ValueError:
    refused_at = 0
if refused_at > 0:
    evidence["failed_at"] = refused_at
    evidence["refresh_hash"] = digest(home_path)
    try:
        with open(evidence_path, "w") as f:
            json.dump(evidence, f)
    except OSError:
        pass


def the_fleets_copy_has_failed():
    """Did a call skein made with the credential that is in `$HOME` RIGHT NOW come back refused?"""
    failed = number(evidence.get("failed_at")) or 0
    worked = number(evidence.get("refreshed_at")) or 0
    if failed <= 0 or failed <= worked:
        return False
    named = str(evidence.get("refresh_hash") or "")
    return bool(named) and named == digest(home_path)

ranked = [(rank(p), p) for p in paths]
best = max(((k, p) for k, p in ranked if k is not None), default=None)
if best is None:
    sys.exit(0)
top, source = best
try:
    stamp = os.stat(source)
except OSError:
    sys.exit(0)
for key, path in ranked:
    # STRICTLY WORSE, and not merely dead — which is the whole of SKEIN-488.
    #
    # "Heal only the dead" cannot converge a fleet whose copies all look alive, and that is every
    # fleet sharing one rotating credential: each refresh mints a NEW refresh token and supersedes
    # the one every other copy holds, and nothing in a superseded file says so. The boxes that lost
    # the last rotation read as perfectly healthy and are logged out. Measured on a live fleet:
    # three of its four boxes, none of which this loop would have touched. Replacing anything
    # strictly worse is what carries a refresh in one box to the rest before they try to spend a
    # token that is gone.
    if path == source or (key is not None and key >= top):
        continue
    # AND THE FLEET'S OWN COPY IS NOT REPLACED ON A BOX'S SAY-SO (ISO-5).
    #
    # Everything above is the file's own claim about itself, and `$HOME`'s copy is the one that
    # matters: it is what seeds every new box and what `sync_fleet_login_saving_only` carries up to
    # the host a moment later, so a box that wins this election has handed skein its credential for
    # every box and every future one. `box-session.sh` refuses that direction outright — "nothing in
    # a file a box writes is evidence about that file" — and then defers to this tick, which is why
    # the two comments read as one rule. This is the rule they name.
    #
    # Two ways past it, and only two. The fleet's copy carries no usable login at all (`key is
    # None`), which is the vacuum clause: there is nothing to poison, and the alternative is a fleet
    # where every box is logged out. Or skein has evidence of its OWN that the copy does not work.
    #
    # Box to box is untouched. Those are one trust domain (architecture §9.2) and a box that has
    # just refreshed is how the others learn a rotation happened before they try to spend a token
    # that is gone — which is SKEIN-488 and must keep working.
    if path == home_path and key is not None and not the_fleets_copy_has_failed():
        continue
    # The credential's own age travels with it, not the copy's — the same `utime` the launcher's
    # `merge_login` does. Without it a copy outranks its own source the instant it is written, the
    # two trade places every tick, and the mtime tiebreak above means nothing.
    if place(source, path, (stamp.st_atime_ns, stamp.st_mtime_ns)):
        print(path)
SKEIN_HEAL
done
"#
    )
}

/// Spread one still-working login across the fleet — the sandbox's copy and every box's — in both
/// directions.
///
/// **Why both directions, when the launcher deliberately allows only one.** `box-session.sh` argues
/// that a credential may flow DOWN from the fleet to a box but never UP, because nothing in a file a
/// box writes is evidence about that file. That argument is right and survives here. Its escape
/// clause is that when the fleet "holds no login at all" a box's login heals it — "there is nothing
/// to poison: the alternative is every box logged out".
///
/// The hole was the words "at all". Holding a login was any non-empty token string, so an
/// INVALIDATED credential still counted as occupied, the vacuum clause never fired, and no box's
/// fresh login could heal anything. Reported from daily use as six or seven interactive logins a
/// day — one per box, every time Claude invalidated the sessions.
///
/// A dead credential is worth exactly what no credential is worth. So the vacuum is "no login that
/// still works", and the poisoning argument is untouched: a forged credential can only win when the
/// real one is already dead, and at that moment there is nothing to displace and nothing to steal.
///
/// **And "already dead" is a fact skein has to have observed, not one the file may assert** —
/// ISO-5, and the correction to the paragraph above. "Still works" was read out of the credential
/// itself, so a box writing `{"expiresAt": 9e15}` into its own private HOME declared every other
/// copy strictly worse and became the fleet's login: the poisoning §9.3 and §9.6 say was closed,
/// through the field the vacuum clause was measured in. The rule now has a subject — the fleet's
/// copy is replaced only when it carries no login at all, or when a model call skein made with THAT
/// copy came back refused. Box to box keeps the election, because boxes are one trust domain and
/// because carrying a refresh from the box that made it to the ones that lost it is SKEIN-488.
///
/// **And it runs on a tick, not at box start.** The launcher's rule fires when a box's session
/// starts, so a login typed inside a running box healed nothing until that box was restarted — a
/// cure worse than the disease. Returns the paths it wrote, so the caller can say what changed.
///
/// **The last leg, which used to be missing.** Everything above happens inside the sandbox, between
/// `$HOME` and the box roots. Nothing that reports a login reads any of those: [`runtime_logins`]
/// reads `fleet-home` on the HOST and nothing else, and `expired_logins`, [`signed_in_runtimes`],
/// `skein doctor` and the cockpit's sign-in banner are all built from it. The only thing that ever
/// wrote `fleet-home` was [`sync_fleet_login`], called from `ensure_fleet` and from a resize — so a
/// login typed inside a box could travel the whole way to the sandbox's HOME on this tick and STILL
/// leave the banner up, correctly, for as long as the fleet was not rebuilt. The person had done
/// the work and skein threw it away; SKEIN-294 is that gap.
///
/// So the tick carries it the rest of the way, in the order the two legs have to happen: heal the
/// sandbox from whichever box holds the working login, then save the sandbox's copy to the host's.
/// It costs two more `cat`s a minute and almost never writes — [`login_move`] returns `Neither` when
/// the two copies are already the same bytes, which is the ordinary case and which the mtime that
/// `login_written_ms` reads depends on.
pub fn heal_logins() -> Result<Vec<String>, String> {
    let sandbox = fleet_sandbox();
    // The one piece of evidence skein has that is not the credential file's own word about itself:
    // a model call that came back refused, against the credential that is still there. `ai` has
    // already checked that last part — a refusal whose fingerprint no longer matches is dropped
    // rather than reported (SKEIN-348) — so what arrives here is a refusal about the current copy
    // or nothing at all. It travels into the script, which records it beside the credential and
    // then reads it back as the one thing that lets a box's copy displace the fleet's.
    let refused = crate::ai::auth_refusal();
    let told = own_sandbox(&sandbox).exec(
        &heal_logins_script(refused.as_ref().map(|r| (r.runtime, r.at_ms))),
        Duration::from_secs(60),
    )?;
    // After the heal and not before: the copy worth keeping is the one the election above just
    // settled on, and reading `$HOME` first would save whatever it was about to replace.
    //
    // **Save only** (SKEIN-349). The election has just written `$HOME`, so the one thing this must
    // not do is put the host's older copy back over it — which `Restore` is exactly for, and which
    // it would do on any pairing where the host's copy still works and the freshly elected one
    // does not. The other two call sites keep both directions; only here is one side known to be
    // newer than the other by construction.
    sync_fleet_login_saving_only(&sandbox);
    Ok(told
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// Put the fleet's login into every box that already exists, and answer with the ones it reached.
///
/// **The hole this fills.** The launcher reconciles logins at box session START and nowhere else,
/// so `skein login` reached new boxes and no running one — which it said out loud and nobody read
/// as the problem it is. When a login is invalidated fleet-wide, the only choices left were to
/// restart every box or to sign in on every box, and both are exactly what sharing a login exists
/// to prevent. Reported from daily use: "every time Claude logs me out, I have to login separately
/// on each box."
///
/// **No comparison, and that is the point.** `box-session.sh` argues at length that a credential may
/// flow DOWN from the fleet to a box but never UP, because nothing in a file a box writes is
/// evidence about that file — and between two that both carry a login it compares `expiresAt`. This
/// runs immediately after an interactive login, which is the one moment when there is nothing to
/// compare: a person has just authenticated, so the canonical copy is the freshest credential in the
/// fleet by construction. A box mid-refresh may hold one minted seconds earlier; replacing it with
/// the one minted now costs nothing, and asking would mean a second copy of a rule whose whole
/// safety argument is about direction.
///
/// **It never leaves the sandbox.** Source and destination are both paths the sandbox can see —
/// `$HOME/<rel>` and `<box root>/home/<rel>` — so this is one script over there rather than bytes
/// read to the host and written back. The host has no business holding this even in memory.
///
/// Stopped boxes are written too. A box that is not running has a private HOME sitting on disk that
/// its next start will use, and the launcher's own reconciliation would take this copy anyway.
pub fn share_login_with_boxes() -> Result<Vec<String>, String> {
    let sandbox = fleet_sandbox();
    let told = own_sandbox(&sandbox).exec(&share_login_script(), Duration::from_secs(60))?;
    Ok(told
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// The script [`share_login_with_boxes`] runs, separated so a test can drive it against a fixture
/// rather than against a fleet.
///
/// **It merges — it does not copy the file.** This copied `.credentials.json` whole, and that file
/// is not only the login: it carries an `mcpOAuth` grant per MCP server, which is a box's identity
/// at its own work-tracking gateway and which survives a logout. So the one path reported as
/// working destroyed, on every single login, the per-box state that `heal_logins_script` takes
/// care to preserve — while claiming in that script's own comment that this path already enforced
/// the rule (SKEIN-489). [`LOGIN_MERGE_PY`] is now the only implementation either can reach.
///
/// Writes through a temporary file and a rename, because the destination is read by a running
/// agent: a half-written credentials file is a logged-out box, and writing straight over it has a
/// window where that is exactly what is on disk.
fn share_login_script() -> String {
    let merge = LOGIN_MERGE_PY;
    let root = sh_quote(&fleet_root());
    let files = LOGIN_FILES
        .iter()
        .map(|rel| sh_quote(rel))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"set -u
for rel in {files}; do
  src="$HOME/$rel"
  [ -s "$src" ] || continue
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$rel" "$src" {root}/*/ <<'SKEIN_SHARE'
import json, os, sys, tempfile
{merge}

rel = sys.argv[1]
source = sys.argv[2]
# **Only a real login travels.** This runs straight after an interactive login, and an interactive
# login can be abandoned — which leaves the file in place with its tokens blanked. Copying that
# husk into every box is a fleet-wide logout performed by the thing whose job is the opposite.
if not carries(source):
    sys.exit(0)
for root in sys.argv[3:]:
    # An unmatched glob arrives as its own pattern; a fleet with no boxes yet passes the literal.
    if "*" in root:
        continue
    home = os.path.join(root, "home")
    if not os.path.isdir(home):
        continue
    dst = os.path.join(home, rel)
    place(source, dst)
    # Named when the box HOLDS it, which is not the same as "was written": a box that already had
    # this login was reached too, and `place` deliberately writes nothing when there is no change.
    try:
        if json.load(open(dst)) == merged(source, dst):
            print(os.path.basename(root.rstrip("/")))
    except Exception:
        pass
SKEIN_SHARE
  else
    # No python3, so the grants cannot be kept. Said out loud, and the login still travels: the
    # launcher degrades the other way (propagate nothing rather than guess) because THERE the
    # unknown is whether a file is a login at all. Here it is not in doubt — a person just typed it
    # — and the cost is a box re-authorising its MCP servers, against a fleet that cannot work.
    echo "skein: no python3 here, so each box takes the fleet's MCP grants along with the login and will have to re-authorise its own" >&2
    for root in {root}/*/; do
      home="${{root%/}}/home"
      [ -d "$home" ] || continue
      dst="$home/$rel"
      mkdir -p "$(dirname "$dst")" 2>/dev/null || continue
      tmp="$dst.skein-login"
      if cp "$src" "$tmp" 2>/dev/null && chmod 600 "$tmp" 2>/dev/null && mv -f "$tmp" "$dst" 2>/dev/null; then
        name="${{root%/}}"
        printf '%s\n' "${{name##*/}}"
      else
        rm -f "$tmp" 2>/dev/null || true
      fi
    done
  fi
done
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::testkit::*;
    use crate::testutil::*;

    /// Run the launcher's own credential sync over two fixture homes, handing back what each holds
    /// afterwards as `(box, sandbox)`.
    ///
    /// `newer` names the side that gets the later mtime — the tiebreak the rule used to apply to
    /// everything — so each case can say what should happen *despite* it. Stamped rather than slept
    /// into order: writing the two a second apart cost the suite ten seconds, and a suite slow
    /// enough to skip stops catching things.
    fn credential_sync(
        root: &std::path::Path,
        box_has: Option<&str>,
        sandbox_has: Option<&str>,
        newer: &str,
    ) -> (String, String) {
        let block = BOX_SESSION_SH
            .lines()
            .skip_while(|l| !l.starts_with("login_life() {"))
            .take_while(|l| !l.starts_with("done"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\ndone";
        let rel = ".claude/.credentials.json";
        let home = root.join("boxhome");
        let sandbox = root.join("sandboxhome");
        for base in [&home, &sandbox] {
            let _ = std::fs::remove_dir_all(base);
            std::fs::create_dir_all(base.join(".claude")).unwrap();
        }
        let old = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let new = old + Duration::from_secs(10);
        for (which, body) in [("box", box_has), ("sandbox", sandbox_has)] {
            let Some(body) = body else { continue };
            let base = if which == "box" { &home } else { &sandbox };
            std::fs::write(base.join(rel), body).unwrap();
            let when = if which == newer { new } else { old };
            std::fs::File::options()
                .write(true)
                .open(base.join(rel))
                .and_then(|f| f.set_times(std::fs::FileTimes::new().set_modified(when)))
                .unwrap();
        }
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "set -uo pipefail\nhome={home}\nexport HOME={sandbox}\n{block}\n",
                home = home.display(),
                sandbox = sandbox.display(),
            ))
            .output()
            .expect("bash to run the launcher's credential sync");
        assert!(
            out.status.success(),
            "the sync itself failed, which would abort the box start: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (
            std::fs::read_to_string(home.join(rel)).unwrap_or_default(),
            std::fs::read_to_string(sandbox.join(rel)).unwrap_or_default(),
        )
    }

    /// Sharing a login must neither share nor destroy a box's MCP grants.
    ///
    /// `.credentials.json` holds an `mcpOAuth` block per MCP server as well as the agent's login,
    /// and those are per-repo: a box's work-tracking gateway belongs to its repository, which is the
    /// same reason `~/.claude.json` is kept private. The sync copied the file whole, so one box's
    /// grants landed in another and — the half that actually breaks things — the receiving box's own
    /// grants were *discarded* rather than merged. The symptom is an MCP server asking to be
    /// authorised again for no reason, a long way from this code.
    ///
    /// Latent today, because every box in the fleet happens to point at one gateway. The second repo
    /// with its own is when it would bite.
    #[test]
    fn syncing_a_login_leaves_each_boxs_own_mcp_grants_alone() {
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        // Two boxes, two repos, two gateways — the shape the fleet does not have yet.
        let with_mcp = |token: &str, server: &str, grant: &str| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"r"}},"mcpOAuth":{{"{server}":{{"accessToken":"{grant}"}}}}}}"#
            )
        };
        let (box_after, sandbox_after) = credential_sync(
            root,
            Some(&with_mcp(
                "skein-test-sk-old",
                "sync|aaaa",
                "mcp-for-my-repo",
            )),
            Some(&with_mcp(
                "skein-test-sk-new",
                "sync|bbbb",
                "mcp-for-another-repo",
            )),
            "sandbox",
        );

        // The login travels, which is the point of the sync.
        assert!(
            box_after.contains("skein-test-sk-new"),
            "the newer login did not reach the box:\n{box_after}"
        );
        // And the box keeps its own gateway grant, which is the point of this test.
        assert!(
            box_after.contains("mcp-for-my-repo"),
            "the box's own MCP grant was destroyed by a login sync:\n{box_after}"
        );
        assert!(
            !box_after.contains("mcp-for-another-repo"),
            "another repo's MCP grant was handed to this box:\n{box_after}"
        );
        // Symmetrically: nothing of the sandbox's moved but its login.
        assert!(
            sandbox_after.contains("mcp-for-another-repo")
                && !sandbox_after.contains("mcp-for-my-repo"),
            "the sandbox's MCP grants were disturbed:\n{sandbox_after}"
        );
    }

    /// A logout must not propagate, however new it is.
    ///
    /// Credentials sync both ways between a box and the sandbox so that logging in once is enough.
    /// "Newest wins" was the whole rule, and it cannot see the difference that matters: a logged-out
    /// agent leaves the file in place with its tokens blanked, and that husk is *newer* than the
    /// working copy it replaced. So a single logged-out box flowed its emptiness up on the next
    /// start, seeded every box created after it, and pulled it back down over logins that were fine
    /// — turning "log in once" into "log in to each box separately", which is the bug this was
    /// built to prevent. Found in the live fleet as boxes holding `"accessToken": ""`.
    ///
    /// Direction is not the fix and neither is order; the fix is that a file without a login never
    /// wins. These are the four states that can meet, driven through the launcher's own code.
    #[test]
    fn a_logged_out_box_cannot_log_out_the_fleet() {
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        // A real login, and the husk a logout leaves: identical structure, blanked tokens. The husk
        // is what the fleet actually had, so it is written as it was found rather than invented.
        let login = r#"{"claudeAiOauth":{"accessToken":"skein-test-sk-live","refreshToken":"skein-test-sk-ref","expiresAt":1786308957532},"mcpOAuth":{"sync|a":{"accessToken":"mcp-token"}}}"#;
        let husk = r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0},"mcpOAuth":{"sync|a":{"accessToken":"mcp-token"}}}"#;
        let run = |box_has: Option<&str>, sandbox_has: Option<&str>, newer: &str| {
            credential_sync(root, box_has, sandbox_has, newer)
        };

        // The regression: a fresh logout must not overwrite an older, working login.
        let (box_side, sandbox_side) = run(Some(husk), Some(login), "box");
        assert!(
            sandbox_side.contains("skein-test-sk-live"),
            "a newer logout overwrote the sandbox's login — one box logs out the fleet:\n{sandbox_side}"
        );
        assert!(
            box_side.contains("skein-test-sk-live"),
            "the logged-out box was not healed from the sandbox's login:\n{box_side}"
        );

        // And the same in the other direction: the sandbox being the one that went stale.
        let (box_side, sandbox_side) = run(Some(login), Some(husk), "sandbox");
        assert!(
            box_side.contains("skein-test-sk-live") && sandbox_side.contains("skein-test-sk-live"),
            "a login was lost to a newer husk on the sandbox side:\nbox {box_side}\nsandbox {sandbox_side}"
        );

        // Two real logins still resolve by recency, which is what makes "log in anywhere" work.
        let fresher = login.replace("skein-test-sk-live", "skein-test-sk-fresh");
        let (box_side, sandbox_side) = run(Some(login), Some(&fresher), "sandbox");
        assert!(
            box_side.contains("skein-test-sk-fresh")
                && sandbox_side.contains("skein-test-sk-fresh"),
            "the newer of two logins did not win:\nbox {box_side}\nsandbox {sandbox_side}"
        );

        // Two husks are nothing to choose between, and neither is worth copying anywhere.
        let (box_side, sandbox_side) = run(Some(husk), Some(husk), "box");
        assert!(
            !box_side.contains("skein-test-sk-live")
                && !sandbox_side.contains("skein-test-sk-live"),
            "invented a login from two logouts"
        );

        // A box that has never run seeds from the sandbox — the original "log in once".
        let (box_side, _) = run(None, Some(login), "sandbox");
        assert!(
            box_side.contains("skein-test-sk-live"),
            "a new box did not inherit the login:\n{box_side}"
        );
    }

    /// The fleet's login flows DOWN into a box, and a box's never flows up over it.
    ///
    /// Two rules in one test because they are one rule seen from two sides.
    ///
    /// **Down**: mtime says when a file was written; `expiresAt` says which credential is better,
    /// and they come apart exactly where it costs — a box that starts rewrites its own copy, so it
    /// holds the newer mtime whether or not its token is the older one. Found in the live fleet as
    /// boxes sitting on tokens that had expired days earlier while other boxes held good ones.
    ///
    /// **Up**: nothing. The expiry is a number inside the file and the box side of that file is a
    /// box's to write, so it is not evidence about itself — see
    /// [`Self::a_box_cannot_poison_the_fleets_login_with_an_expiry_it_made_up`]. What a box holds
    /// stays where it is, and the box keeps it rather than being handed something worse.
    #[test]
    fn the_fleets_login_reaches_a_box_and_a_boxs_never_reaches_the_fleet() {
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        let cred = |tok: &str, exp: i64| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{tok}","refreshToken":"r","expiresAt":{exp}}}}}"#
            )
        };
        let live = cred("skein-test-sk-live", 1_900_000_000_000);
        let stale = cred("skein-test-sk-stale", 1_700_000_000_000);

        // Down: the fleet's longer-lived login reaches the box despite the box's newer write.
        let (box_side, sandbox_side) = credential_sync(root, Some(&stale), Some(&live), "box");
        assert!(
            box_side.contains("skein-test-sk-live"),
            "a newer *write* of an expired token beat a live login:\nbox {box_side}"
        );
        assert!(
            sandbox_side.contains("skein-test-sk-live"),
            "the fleet's own copy was disturbed by a sync that had nothing to give it:\n{sandbox_side}"
        );

        // Up: it does not. The box keeps the better one; the fleet keeps what it had.
        let (box_side, sandbox_side) = credential_sync(root, Some(&live), Some(&stale), "sandbox");
        assert!(
            box_side.contains("skein-test-sk-live"),
            "the box was handed the worse of the two:\nbox {box_side}"
        );
        assert!(
            sandbox_side.contains("skein-test-sk-stale"),
            "a box wrote the fleet's login:\nsandbox {sandbox_side}"
        );
    }

    /// A box cannot take the fleet's login by claiming a better one.
    ///
    /// The attack in full, and it needed one line of JSON: a box writes its own credentials file
    /// with an expiry far in the future and a token of its choosing. Under expiry-wins that file was
    /// better than the fleet's by definition, so it was copied UP into the canonical copy — and
    /// every box started afterwards seeded from it. No signature, no second opinion, fleet-wide.
    ///
    /// It cannot be fixed by comparing a different field. A box legitimately holds the refresh
    /// token, so anything it can produce honestly it can produce dishonestly; no field in a file a
    /// box writes is evidence about that file. The direction carries the rule instead.
    #[test]
    fn a_box_cannot_poison_the_fleets_login_with_an_expiry_it_made_up() {
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        let real = r#"{"claudeAiOauth":{"accessToken":"skein-test-sk-real","refreshToken":"r","expiresAt":1750000000000}}"#;
        // Year 10000, and a token the box chose.
        let forged = r#"{"claudeAiOauth":{"accessToken":"skein-test-sk-attacker","refreshToken":"r","expiresAt":253402300799000}}"#;

        let (_, sandbox_side) = credential_sync(root, Some(forged), Some(real), "box");
        assert!(
            sandbox_side.contains("skein-test-sk-real"),
            "a box replaced the fleet's login with one it made up:\n{sandbox_side}"
        );
        assert!(
            !sandbox_side.contains("skein-test-sk-attacker"),
            "the forged token reached the copy every later box seeds from:\n{sandbox_side}"
        );
    }

    /// A box's login still heals a fleet that has none.
    ///
    /// The one direction that stays open, and it is open because there is nothing there to poison:
    /// the alternative is every box logged out, and any login is better than none. It is also what
    /// keeps the launcher's own fallback honest — log in inside one box, and a fleet with no
    /// canonical copy gets one.
    #[test]
    fn a_login_from_a_box_still_heals_a_fleet_that_has_none() {
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        let login = r#"{"claudeAiOauth":{"accessToken":"skein-test-sk-live","refreshToken":"r","expiresAt":1900000000000}}"#;
        let husk = r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0}}"#;

        for fleet_has in [None, Some(husk)] {
            let (_, sandbox_side) = credential_sync(root, Some(login), fleet_has, "sandbox");
            assert!(
                sandbox_side.contains("skein-test-sk-live"),
                "a fleet with no login of its own was left without one:\n{sandbox_side}"
            );
        }
    }

    /// **The heal's last leg may save and must not restore** (SKEIN-349).
    ///
    /// `heal_logins` elects a login across every box into the sandbox's `$HOME` and THEN calls this.
    /// A `Restore` there would put the host's older copy back over the credential the election just
    /// chose — undoing the heal it was called to finish. Source-shaped for the same reason as the
    /// test below it: the exec boundary cannot be crossed from here.
    #[test]
    fn the_heals_last_leg_cannot_undo_the_election_it_follows() {
        let body = fn_body(include_str!("login.rs"), "pub fn heal_logins()");
        assert!(
            body.contains("sync_fleet_login_saving_only("),
            "heal_logins must finish with the save-only leg; the two-direction one can restore the \
             host's older copy over the login the election above just wrote"
        );
        assert!(
            !body.contains("sync_fleet_login("),
            "heal_logins is calling the two-direction leg again (SKEIN-349)"
        );
    }

    /// The tick runs BOTH legs, and in the order that makes the second one mean anything.
    ///
    /// **Source-shaped, and it says so.** The boundary between the two legs is
    /// `own_sandbox(..).exec(..)` — a command inside a running sandbox — which no test on this
    /// machine can cross, so `heal_logins` itself cannot be driven. That leaves the wiring as the
    /// only thing left to check, and the wiring is exactly what was missing: both halves of
    /// SKEIN-294 were individually correct and individually tested, and the bug was that nothing
    /// called the second one. `a_login_typed_inside_a_box_reaches_the_file_the_banner_reads` drives
    /// the real script and the real decision and still passes with the call deleted — which is how
    /// the gap stayed invisible, and why this test exists beside it.
    ///
    /// What it does not prove: that either leg works. Those are that test's job and
    /// `an_invalidated_sandbox_login_cannot_destroy_the_fleets_kept_one_either`'s. This proves only
    /// that the two are joined, which is the one thing they can never prove about each other.
    ///
    /// The precedent is `the_host_and_the_launcher_agree_on_what_a_login_is`, which reads the very
    /// string `heal_logins` executes for the same reason: the alternative to reading the source is
    /// not a better test, it is no test.
    #[test]
    fn the_login_tick_saves_the_fleets_copy_after_healing_it() {
        let body = fn_body(include_str!("login.rs"), "pub fn heal_logins()");
        let exec = body.find(".exec(").expect(
            "heal_logins no longer runs the heal script; this test is reading the wrong fn",
        );
        let save = body
            .find("sync_fleet_login_saving_only(")
            .unwrap_or_else(|| {
                panic!(
                "the login tick heals the sandbox and never writes `fleet-home`, so a login typed \
                 inside a box reaches the sandbox and stops there — every surface that reports a \
                 login reads `fleet-home` and would still say the fleet is signed out (SKEIN-294)"
            )
            });
        assert!(
            save > exec,
            "`fleet-home` is written BEFORE the heal, so it keeps the copy the heal is about to \
             replace — the sandbox's, not the one the election settles on"
        );
    }

    /// The host and the launcher must not disagree about what a login is.
    ///
    /// Two implementations of one rule, in two languages, on either side of the same file: the
    /// launcher's `login_life` decides what propagates between a box and the sandbox, and the host's
    /// [`carries_login`] decides what is kept in `fleet-home` for a rebuild to restore. A drift
    /// between them is a fleet that heals in one direction and poisons in the other, which is
    /// exactly what "shared login doesn't work" looks like from outside.
    ///
    /// The mcpOAuth case is the one worth having: those grants SURVIVE a logout, so counting them
    /// would make every husk look like a login and put the original bug straight back.
    ///
    /// **And they must not disagree about what a DEAD login is.** The second phase drives the heal
    /// script's own `rank()` — the judgement that decides what propagates — against the host's
    /// [`login_state`], on `refreshTokenExpiresAt`. The host side went years asking only "is a
    /// token string non-empty", so a credential that expired days ago reported as signed in and a
    /// fleet-wide logout read as "each box needs a login" instead of "the fleet's credential is
    /// dead".
    #[test]
    fn the_host_and_the_launcher_agree_on_what_a_login_is() {
        // Locked and pinned: `$SKEIN_FLEET_ROOT` is process-global and this test only READS it,
        // which was already a race against the setters in this file and is a loud one now that
        // `util::fleet_root` refuses an unset root instead of answering `/boxes` (SKEIN-690).
        // Nothing asserted below carries the root's value. Phase two below cuts its judge
        // out of `heal_logins_script`, which resolves the root to build the script it cuts.
        let _g = crate::testutil::env_lock();
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_FLEET_ROOT", root.join("fleet"));
        let block = BOX_SESSION_SH
            .lines()
            .skip_while(|l| !l.starts_with("login_life() {"))
            .take_while(|l| !l.starts_with("better_login() {"))
            .collect::<Vec<_>>()
            .join("\n");
        let cases: [(&str, bool); 8] = [
            (
                r#"{"claudeAiOauth":{"accessToken":"sk","refreshToken":"r"}}"#,
                true,
            ),
            (
                r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0}}"#,
                false,
            ),
            // A logout leaves the MCP grants behind. They are not a login.
            (r#"{"mcpOAuth":{"sync|a":{"accessToken":"grant"}}}"#, false),
            (r#"{"claudeAiOauth":{"accessToken":"   "}}"#, false),
            (
                r#"{"tokens":{"access_token":"a","refresh_token":"b"}}"#,
                true,
            ),
            (r#"{"OPENAI_API_KEY":"skein-test-sk-x"}"#, true),
            (r#"{}"#, false),
            ("not json at all", false),
        ];
        for (body, want) in cases {
            let p = root.join("cred.json");
            std::fs::write(&p, body).unwrap();
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "set -uo pipefail\n{block}\nlogin_life {}",
                    p.display()
                ))
                .output()
                .expect("bash to run the launcher's login test");
            assert_eq!(
                out.status.success(),
                want,
                "the launcher disagrees about `{body}`"
            );
            assert_eq!(
                carries_login(body.as_bytes()),
                want,
                "the host disagrees about `{body}`"
            );
        }

        // Phase two: expiry. The launcher-side judge with expiry eyes is the heal script's
        // `rank()`, extracted from the very string [`heal_logins`] executes — a copy here would be
        // the drift this test exists to prevent. Its trailing driver (which WRITES files) is cut
        // at the `paths =` line and replaced with one that only asks.
        let judge =
            heal_judge() + "\nprint('alive' if rank(sys.argv[1]) is not None else 'dead')\n";
        // 1_000_000_000_000 is 2001 (dead under any clock this test runs on);
        // 253402300799000 is year 9999.
        const PAST: i64 = 1_000_000_000_000;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let expiries: [(&str, LoginState, &str); 7] = [
            (
                r#"{"claudeAiOauth":{"accessToken":"sk","refreshToken":"r","refreshTokenExpiresAt":253402300799000}}"#,
                LoginState::Live,
                "alive",
            ),
            (
                r#"{"claudeAiOauth":{"accessToken":"sk","refreshToken":"r","refreshTokenExpiresAt":1000000000000}}"#,
                LoginState::Expired { at_ms: PAST },
                "dead",
            ),
            // No expiry recorded: an older shape, and "it did not say" is not "it is dead".
            (
                r#"{"claudeAiOauth":{"accessToken":"sk","refreshToken":"r"}}"#,
                LoginState::Live,
                "alive",
            ),
            // The doctrine case: a past `expiresAt` is the ordinary state of a healthy login —
            // the ACCESS token dies in hours and is renewed unasked. Only the refresh expiry rules.
            (
                r#"{"claudeAiOauth":{"accessToken":"sk","refreshToken":"r","expiresAt":1000000000000,"refreshTokenExpiresAt":253402300799000}}"#,
                LoginState::Live,
                "alive",
            ),
            // A husk outranks nothing, whatever expiry it claims: blanked tokens are not a login.
            (
                r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","refreshTokenExpiresAt":253402300799000}}"#,
                LoginState::Absent,
                "dead",
            ),
            (
                r#"{"tokens":{"access_token":"a","refresh_token":"b","refresh_token_expires_at":1000000000000}}"#,
                LoginState::Expired { at_ms: PAST },
                "dead",
            ),
            (
                r#"{"mcpOAuth":{"sync|a":{"accessToken":"grant","refreshTokenExpiresAt":253402300799000}}}"#,
                LoginState::Absent,
                "dead",
            ),
        ];
        for (body, host_wants, launcher_says) in expiries {
            let p = root.join("cred.json");
            std::fs::write(&p, body).unwrap();
            let out = std::process::Command::new("python3")
                .arg("-c")
                .arg(&judge)
                .arg(&p)
                .output()
                .expect("python3 to run the heal script's rank()");
            let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
            assert_eq!(
                said,
                launcher_says,
                "the heal script disagrees about `{body}`: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let host = login_state(body.as_bytes(), now_ms);
            assert_eq!(host, host_wants, "the host disagrees about `{body}`");
            assert_eq!(
                matches!(host, LoginState::Live),
                said == "alive",
                "host and launcher disagree on whether `{body}` still works — the fleet would \
                 heal in one direction and report in the other"
            );
        }
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// Everything the heal script's python says about ONE credential, with the driver that writes
    /// files cut off — so a test can ask it questions without a fleet to answer them in.
    ///
    /// Taken from the very string [`heal_logins`] executes. A copy of the judgement here would be
    /// exactly the drift the tests using this exist to catch.
    fn heal_judge() -> String {
        heal_logins_script(None)
            .split_once("<<'SKEIN_HEAL'\n")
            .expect("the heal script embeds its python in a SKEIN_HEAL heredoc")
            .1
            .split_once("\nSKEIN_HEAL")
            .expect("the SKEIN_HEAL heredoc is unterminated")
            .0
            .split_once("\npaths = [")
            .expect("the heal python no longer ends in the driver this test cuts off")
            .0
            .to_string()
    }

    /// **The two elections must pick the same winner. Every time, on every pair.**
    ///
    /// This is SKEIN-488, and it is written as an invariant rather than as cases on purpose —
    /// SKEIN-349 was a case-wise suite watching a change swap the question underneath it and
    /// noticing nothing. There is no expected winner listed below. The assertion is only that the
    /// launcher's answer and the host's answer are the same, which stays true through any future
    /// change to what "better" means and fails the moment one side changes and the other does not.
    ///
    /// **What it costs to be wrong**, measured rather than imagined. On a live fleet, 2026-08-29:
    /// `box-session.sh:login_life` ranked five real copies by `expiresAt` and elected one box,
    /// while `heal_logins_script` ranked the same five by `refreshTokenExpiresAt` and elected a
    /// `gadget` box, putting the launcher's winner LAST.
    /// Exactly inverted. The launcher told each box "the host will carry it up within the minute"
    /// and the host carried up a credential two boxes were already logged out of.
    ///
    /// The pairs are run inside ONE bash and ONE python rather than a process per pair: a hundred
    /// spawns to compare two sort orders is a test people start skipping.
    #[test]
    fn the_two_elections_agree_on_which_login_is_best() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let root = dir.as_ref() as &std::path::Path;
        // A fixture fleet root: `util::fleet_root` refuses an unpinned test rather than answering
        // `/boxes`, which on any machine running skein is the live fleet (SKEIN-690). Nothing
        // asserted below carries the root, so a fixture is the whole of what this needs.
        std::env::set_var("SKEIN_FLEET_ROOT", root.join("fleet"));
        // Year 9999 and 2001 — the second is dead under any clock this test can run on.
        const FUTURE: i64 = 253_402_300_799_000;
        const PAST: i64 = 1_000_000_000_000;
        let oauth = |access: &str, expires: i64, refresh: i64| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"r","expiresAt":{expires},"refreshTokenExpiresAt":{refresh}}}}}"#
            )
        };
        // Every shape the two judges can disagree about, and the mtime each is pinned to — because
        // an mtime tiebreak that only ever sees files written microseconds apart is not tested.
        let cases: Vec<(&str, String, i64)> = vec![
            ("live-far", oauth("sk", FUTURE, FUTURE), 1_600_000_001),
            ("live-near", oauth("sk", FUTURE - 1000, FUTURE), 1_600_000_002),
            // THE case. A credential a sibling box superseded by refreshing: its refresh token has
            // not expired and never will, and it has not been renewed since the rotation that
            // orphaned it. Ranked by the claim it is the best copy in the fleet; ranked by the last
            // refresh that actually happened it is the worst.
            ("superseded", oauth("sk", PAST, FUTURE), 1_600_000_003),
            // The mirror: a wonderful access token behind a refresh token that is spent. Not a
            // source at all, however good it looks.
            ("spent", oauth("sk", FUTURE, PAST), 1_600_000_004),
            // Blanked on BOTH sides, because that is what a logout leaves. Blanking only the
            // access token leaves a file that still carries a login, which is not this case.
            (
                "husk",
                format!(
                    r#"{{"claudeAiOauth":{{"accessToken":"","refreshToken":"","expiresAt":{FUTURE},"refreshTokenExpiresAt":{FUTURE}}}}}"#
                ),
                1_600_000_005,
            ),
            (
                "bare-old",
                r#"{"claudeAiOauth":{"accessToken":"sk","refreshToken":"r"}}"#.to_string(),
                1_600_000_006,
            ),
            (
                "bare-new",
                r#"{"claudeAiOauth":{"accessToken":"sk","refreshToken":"r"}}"#.to_string(),
                1_700_000_000,
            ),
            // Same age to the second as `bare-new`: `-nt` is false between them and so is `>`, and
            // a tiebreak that disagreed about ties would be found here and nowhere else.
            (
                "bare-twin",
                r#"{"claudeAiOauth":{"accessToken":"sk","refreshToken":"r"}}"#.to_string(),
                1_700_000_000,
            ),
            (
                "codex",
                format!(
                    r#"{{"tokens":{{"access_token":"a","refresh_token":"b","expires_at":{FUTURE},"refresh_token_expires_at":{FUTURE}}}}}"#
                ),
                1_600_000_008,
            ),
            ("garbage", "not json at all".to_string(), 1_600_000_009),
            // A logout leaves the grants behind. They are not a login, on either side.
            (
                "grants-only",
                r#"{"mcpOAuth":{"sync|a":{"accessToken":"grant","refreshTokenExpiresAt":253402300799000}}}"#
                    .to_string(),
                1_600_000_010,
            ),
        ];
        let paths: Vec<String> = cases
            .iter()
            .map(|(name, body, mtime)| {
                let p = root.join(name);
                std::fs::write(&p, body).unwrap();
                // Set from Rust, not with `touch -d @N`: this test is deliberately NOT gated to
                // Linux — the two elections have to agree wherever skein runs — and `-d @epoch` is
                // GNU-only, which is the very spelling `tests/platform_gates.rs` records as a
                // reason for gating a test to Linux.
                let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(*mtime as u64);
                std::fs::File::options()
                    .write(true)
                    .open(&p)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_accessed(at).set_modified(at))
                    .unwrap_or_else(|e| panic!("could not pin {name}'s mtime: {e}"));
                p.display().to_string()
            })
            .collect();

        // The launcher's half, lifted whole: `login_life` decides candidacy, `better_login` decides
        // which of two candidates wins. Both, because the pair IS the launcher's election.
        let launcher = BOX_SESSION_SH
            .lines()
            .skip_while(|l| !l.starts_with("login_life() {"))
            .take_while(|l| !l.starts_with("merge_login() {"))
            .collect::<Vec<_>>()
            .join("\n");
        let said = |program: &str, code: String| {
            let out = std::process::Command::new(program)
                .arg("-c")
                .arg(&code)
                .arg("_")
                .args(&paths)
                .output()
                .unwrap_or_else(|e| panic!("{program} to run: {e}"));
            assert!(
                out.status.success(),
                "{program} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let by_launcher = said(
            "bash",
            format!(
                r#"set -u
{launcher}
for a in "$@"; do
  for b in "$@"; do
    [ "$a" = "$b" ] && continue
    la=$(login_life "$a") || la=""
    lb=$(login_life "$b") || lb=""
    if [ -z "$la" ] && [ -z "$lb" ]; then v=neither
    elif [ -z "$la" ]; then v=second
    elif [ -z "$lb" ]; then v=first
    elif better_login "$la" "$lb" "$a" "$b"; then v=first
    else v=second
    fi
    printf '%s %s %s\n' "$(basename "$a")" "$(basename "$b")" "$v"
  done
done"#
            ),
        );
        let by_heal = said(
            "python3",
            heal_judge()
                + r#"
for a in sys.argv[2:]:
    for b in sys.argv[2:]:
        if a == b:
            continue
        ka, kb = rank(a), rank(b)
        if ka is None and kb is None:
            v = "neither"
        elif ka is None:
            v = "second"
        elif kb is None:
            v = "first"
        elif ka > kb:
            v = "first"
        else:
            v = "second"
        print(os.path.basename(a), os.path.basename(b), v)
"#,
        );
        assert!(
            !by_launcher.trim().is_empty(),
            "the pairing harness produced nothing to compare"
        );
        assert_eq!(
            by_launcher, by_heal,
            "the launcher and the host disagree about which login is better — the fleet heals in \
             one direction and reports in the other, which is what \"the shared login doesn't \
             work\" looks like from outside"
        );
        // Non-vacuity: the comparison above is worth something only if these fixtures actually
        // separate. A harness that called everything a draw would pass it.
        assert!(
            by_launcher.contains(" first\n") && by_launcher.contains(" second\n"),
            "every pair tied, so nothing was ranked: {by_launcher}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// A login lands in every box that exists, and never travels the other way.
    ///
    /// Driven by RUNNING the script against a fixture rather than by reading it: the thing that
    /// matters is which file ends up where, and a source-shaped assertion would pass on a script
    /// that copied in the wrong direction — which is the one mistake here that would matter, since
    /// the launcher's whole security argument is that a box may never write the fleet's copy.
    #[test]
    fn a_shared_login_reaches_every_box_and_never_comes_back_up() {
        // See the note in `a_login_typed_inside_a_box_reaches_the_file_the_banner_reads`:
        // `SKEIN_FLEET_ROOT` is process-wide, and this test read another test's fixture root.
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let boxes = crate::testutil::tempdir();
        let boxes = boxes.as_ref() as &std::path::Path;

        // The fleet's copy: what a person just logged in as — carrying the sandbox's OWN grant at
        // a work-tracking gateway, which is the thing that must not travel with the login.
        let canonical = home.join(".claude/.credentials.json");
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        let donor = br#"{"claudeAiOauth":{"accessToken":"fresh"},"mcpOAuth":{"sync|gw":{"accessToken":"sandbox-grant"}}}"#;
        std::fs::write(&canonical, donor).unwrap();

        // Three boxes: one already holding a dead token AND its own gateway grant, one with an
        // empty private HOME, and one that is only a checkout — no HOME at all, which must be
        // skipped rather than created.
        for (name, cred) in [
            (
                "web-main",
                Some(
                    br#"{"claudeAiOauth":{"accessToken":""},"mcpOAuth":{"sync|gw":{"accessToken":"web-main-grant"}}}"#
                        .as_slice(),
                ),
            ),
            ("api-worker", None),
        ] {
            let h = boxes.join(name).join("home");
            std::fs::create_dir_all(h.join(".claude")).unwrap();
            if let Some(cred) = cred {
                std::fs::write(h.join(".claude/.credentials.json"), cred).unwrap();
            }
        }
        std::fs::create_dir_all(boxes.join("no-home").join("tree")).unwrap();

        std::env::set_var("SKEIN_FLEET_ROOT", boxes);
        let script = share_login_script();
        std::env::remove_var("SKEIN_FLEET_ROOT");

        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .env("HOME", home)
            .output()
            .expect("bash");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        // FIRST, because it is the failure that matters most and the one most easily masked. The
        // canonical copy is what a box must never be able to change; a script that swapped source
        // and destination would trip a later assertion instead and report itself as something else.
        assert_eq!(
            std::fs::read(&canonical).unwrap(),
            donor,
            "the fleet's own copy was rewritten from a box — the direction is reversed, and this is \
             the one mistake here that is a security bug rather than an inconvenience"
        );

        let mut reached: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        reached.sort();
        assert_eq!(
            reached,
            vec!["api-worker".to_string(), "web-main".to_string()],
            "the boxes it says it reached are not the boxes with a private HOME"
        );

        for name in ["web-main", "api-worker"] {
            let landed = std::fs::read(boxes.join(name).join("home/.claude/.credentials.json"))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let landed = String::from_utf8_lossy(&landed);
            assert!(
                landed.contains("fresh"),
                "{name} kept a credential the person has just replaced, so it still needs its own \
                 login — which is the whole bug"
            );
            // **And it keeps what was never the login's to replace** (SKEIN-489). `.credentials.json`
            // also carries an `mcpOAuth` grant per server — a box's identity at its own
            // work-tracking gateway, which survives a logout and is why nothing here counts it as a
            // login. This copied the file WHOLE, so every `skein login` handed every box the
            // sandbox's tracker identity and destroyed the box's own, silently: nothing refuses an
            // agent whose sync tools have simply stopped existing.
            assert!(
                !landed.contains("sandbox-grant"),
                "{name} was handed the sandbox's gateway grant along with the login"
            );
        }
        assert!(
            String::from_utf8_lossy(
                &std::fs::read(boxes.join("web-main/home/.claude/.credentials.json")).unwrap()
            )
            .contains("web-main-grant"),
            "web-main's own gateway grant was destroyed by being given a login"
        );
        // A box with no private HOME is not given one. Creating it would be skein inventing a box.
        assert!(
            !boxes.join("no-home").join("home").exists(),
            "the script created a HOME for something that is not a box"
        );
        // And no leftovers: the write goes through a temporary, and one left behind is a credential
        // sitting at a second path nobody will think to look at. Asked as "what else is in here"
        // rather than by name — the temporary is `mkstemp`'s now, and a check for one particular
        // filename would have gone on passing while the real strays piled up.
        for name in ["web-main", "api-worker"] {
            let strays: Vec<_> = std::fs::read_dir(boxes.join(name).join("home/.claude"))
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n != ".credentials.json")
                .collect();
            assert!(strays.is_empty(), "{name} left behind: {strays:?}");
        }
    }

    /// One live login reaches every box whose own is dead — and nothing else is touched.
    ///
    /// The daily tax this removes: Claude invalidates the sessions, and because an invalidated
    /// credential still counted as "the fleet holds a login", no box's fresh login could heal any
    /// other. Six or seven interactive logins a day, one per box.
    ///
    /// Runs the real script against a fixture of box roots. `refreshTokenExpiresAt` decides — the
    /// access token expires hourly on a healthy login and is not evidence of anything.
    #[cfg(target_os = "linux")]
    #[test]
    fn one_live_login_heals_every_box_whose_own_is_dead() {
        // `SKEIN_FLEET_ROOT` is process-wide; see the note in
        // `a_login_typed_inside_a_box_reaches_the_file_the_banner_reads`.
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let boxes = crate::testutil::tempdir();
        let boxes = boxes.as_ref() as &std::path::Path;

        let future = 32_503_680_000_000i64; // year 3000
        let cred = |refresh: i64| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"a","refreshToken":"r","expiresAt":1,"refreshTokenExpiresAt":{refresh}}}}}"#
            )
        };
        // The same, carrying an MCP grant — a per-box identity at a work-tracking gateway. The
        // donor's must not travel; a dead box's own must survive being healed.
        let cred_with_grant = |refresh: i64, grant: &str| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"a","refreshToken":"r","expiresAt":1,"refreshTokenExpiresAt":{refresh}}},"mcpOAuth":{{"sync|gw":{{"accessToken":"{grant}"}}}}}}"#
            )
        };
        let put = |at: &std::path::Path, body: &str| {
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, body).unwrap();
        };

        // The canonical copy is a corpse — invalidated, but still a well-formed credential file.
        put(&home.join(".claude/.credentials.json"), &cred(1));
        // One box was logged into by hand. Two others are dead. A fourth has no HOME at all.
        put(
            &boxes.join("live/home/.claude/.credentials.json"),
            &cred_with_grant(future, "donor-grant"),
        );
        put(
            &boxes.join("deadA/home/.claude/.credentials.json"),
            &cred_with_grant(1, "deadA-own-grant"),
        );
        put(
            &boxes.join("deadB/home/.claude/.credentials.json"),
            &cred(1),
        );
        std::fs::create_dir_all(boxes.join("no-home/tree")).unwrap();
        // And something that is not a credential at all, in a box that has one.
        put(&boxes.join("deadA/home/.claude/settings.json"), "{\"x\":1}");

        std::env::set_var("SKEIN_FLEET_ROOT", boxes);
        let script = heal_logins_script(None);
        std::env::remove_var("SKEIN_FLEET_ROOT");

        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .env("HOME", home)
            .output()
            .expect("bash");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let read = |at: std::path::PathBuf| std::fs::read_to_string(at).unwrap_or_default();
        let parsed = |at: std::path::PathBuf| {
            serde_json::from_str::<serde_json::Value>(&read(at)).expect("a healed file parses")
        };
        let logged_in = |v: &serde_json::Value| {
            v.get("claudeAiOauth")
                .and_then(|b| b.get("refreshTokenExpiresAt"))
                .and_then(|t| t.as_i64())
                == Some(future)
        };
        let grant = |v: &serde_json::Value| {
            v.get("mcpOAuth")
                .and_then(|m| m.get("sync|gw"))
                .and_then(|g| g.get("accessToken"))
                .and_then(|t| t.as_str())
                .map(str::to_string)
        };

        let dead_a = parsed(boxes.join("deadA/home/.claude/.credentials.json"));
        assert!(
            logged_in(&dead_a),
            "a box with a dead credential was left logged out while a live one existed — which is \
             the six-logins-a-day this exists to end: {dead_a}"
        );
        // **The login moved; the identity did not.** The healed box keeps ITS grant at ITS
        // gateway — spreading the donor's would make every per-box sync connection setting a
        // fiction, and destroy a grant that was still valid (they survive a logout).
        assert_eq!(
            grant(&dead_a).as_deref(),
            Some("deadA-own-grant"),
            "healing the login replaced the box's own MCP grant: {dead_a}"
        );

        let dead_b = parsed(boxes.join("deadB/home/.claude/.credentials.json"));
        assert!(logged_in(&dead_b));
        assert_eq!(
            grant(&dead_b),
            None,
            "a box that had no MCP grant was handed the donor's: {dead_b}"
        );

        // UP as well as down. The canonical copy was the corpse; the whole point is that a box's
        // login may heal it once it is dead, which the launcher's start-time rule cannot do.
        let host = parsed(home.join(".claude/.credentials.json"));
        assert!(
            logged_in(&host),
            "the fleet's own dead copy was left in place, so the next box to start takes a corpse"
        );
        assert_eq!(
            grant(&host),
            None,
            "the donor's MCP grant flowed up into the fleet's canonical copy — from where every \
             new box would inherit it: {host}"
        );

        // Untouched: the one that was already alive, and anything that is not a credential.
        assert_eq!(
            read(boxes.join("live/home/.claude/.credentials.json")),
            cred_with_grant(future, "donor-grant")
        );
        assert_eq!(
            read(boxes.join("deadA/home/.claude/settings.json")),
            "{\"x\":1}"
        );
        assert!(
            !boxes.join("no-home/home").exists(),
            "a HOME was invented for something that is not a box"
        );
        // No temporaries left behind — a credential at a second path nobody thinks to look at.
        let strays: Vec<_> = std::fs::read_dir(boxes.join("deadA/home/.claude"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != ".credentials.json" && n != "settings.json")
            .collect();
        assert!(strays.is_empty(), "left behind: {strays:?}");
    }

    /// **A copy the fleet has moved past is replaced, even though it swears it is alive.**
    ///
    /// The state a live fleet was actually in, 2026-08-29, and the one "heal only the dead"
    /// cannot leave: five copies, four different refresh tokens, every one of them claiming
    /// hundreds of hours of life, three boxes logged out. The mechanism is refresh-token rotation —
    /// each successful refresh mints a NEW refresh token and supersedes the one every other copy
    /// holds — and nothing in a superseded file records that it lost. What does record it is the
    /// access token: the copy that won the rotation is the only one that could mint a fresh
    /// `expiresAt`, because minting one is what winning the rotation MEANS.
    ///
    /// The old rule wrote only into copies whose refresh token had expired. None of these has one,
    /// so it wrote nothing, for ever, while three quarters of the fleet sat logged out.
    ///
    /// **And the fleet's own copy is the exception** (ISO-5). Box to box the election stands, for
    /// the reason above. Over `$HOME` it does not, because `expiresAt` is a field a box writes and
    /// promoting on it is the poisoning the architecture says is closed — so that direction needs
    /// evidence skein produced, and the second half of this test is where it gets it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_copy_the_fleet_has_moved_past_is_replaced_even_though_it_claims_to_be_alive() {
        // `SKEIN_FLEET_ROOT` is process-wide; see the note in
        // `a_login_typed_inside_a_box_reaches_the_file_the_banner_reads`.
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let boxes = crate::testutil::tempdir();
        let boxes = boxes.as_ref() as &std::path::Path;

        let now_ms = chrono::Utc::now().timestamp_millis();
        let alive_for_weeks = now_ms + 60 * 86_400_000;
        // Every copy's refresh token is good for two months. The ONLY thing separating them is
        // when each was last actually renewed.
        let cred = |token: &str, renewed: i64, grant: &str| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"{token}-r","expiresAt":{renewed},"refreshTokenExpiresAt":{alive_for_weeks}}},"mcpOAuth":{{"sync|gw":{{"accessToken":"{grant}"}}}}}}"#
            )
        };
        let put = |at: std::path::PathBuf, body: &str| {
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(&at, body).unwrap();
            at
        };
        // The box that refreshed most recently: it holds the token the rotation left standing.
        let winner = put(
            boxes.join("web-main/home/.claude/.credentials.json"),
            &cred(
                "skein-test-sk-current",
                now_ms + 5 * 3_600_000,
                "web-main-grant",
            ),
        );
        // Superseded twelve hours ago and none the wiser. This is the logged-out box.
        let stale = put(
            boxes.join("api-worker/home/.claude/.credentials.json"),
            &cred(
                "skein-test-sk-superseded",
                now_ms - 12 * 3_600_000,
                "api-worker-grant",
            ),
        );
        // And the sandbox's own copy, older still — the one `fleet-home` is written from, so a
        // fleet that stops here reports itself signed out with a working login two feet away.
        let canon = put(
            home.join(".claude/.credentials.json"),
            &cred(
                "skein-test-sk-ancient",
                now_ms - 19 * 3_600_000,
                "sandbox-grant",
            ),
        );

        let run = |refused: Option<(&str, i64)>| {
            std::env::set_var("SKEIN_FLEET_ROOT", boxes);
            let script = heal_logins_script(refused);
            std::env::remove_var("SKEIN_FLEET_ROOT");
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(&script)
                .env("HOME", home)
                .output()
                .expect("bash");
            assert!(out.status.success(), "the heal script failed: {out:?}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let healed = run(None);

        let read = |p: &std::path::Path| std::fs::read_to_string(p).unwrap();
        // **The fleet's own copy is left exactly as it was** — ISO-5. Every one of these files
        // claims two months of life and the only thing separating them is a number a box writes,
        // so on that number alone a box has just told skein what the fleet's login is. The
        // assertion is here rather than in its own test because it is the same run: the box that
        // lost the rotation is healed in the same tick that leaves `$HOME` alone.
        assert!(
            read(&canon).contains("skein-test-sk-ancient"),
            "a box's copy replaced the fleet's own on the strength of a field the box wrote: {}",
            read(&canon)
        );
        assert!(
            !healed.contains(&canon.display().to_string()),
            "the tick reported writing the fleet's copy: {healed}"
        );

        // The logged-out box, healed from its sibling — which is SKEIN-488 and is the direction
        // that keeps working.
        let got = read(&stale);
        assert!(
            got.contains("skein-test-sk-current"),
            "the logged-out box kept a credential the fleet has moved past — it claims two months \
             of life and has not been renewed since a sibling's refresh orphaned it: {got}"
        );
        assert!(
            !got.contains("skein-test-sk-superseded") && !got.contains("skein-test-sk-ancient"),
            "the logged-out box still carries the old token beside the new one: {got}"
        );
        // Its own identity at its own work-tracking gateway survives being healed. The donor's
        // must not travel.
        assert!(
            got.contains("api-worker-grant") && !got.contains("web-main-grant"),
            "the logged-out box was handed the donor's MCP grants instead of keeping its own: {got}"
        );
        assert!(
            healed.contains(&stale.display().to_string()),
            "the logged-out box was rewritten and the tick said nothing about it"
        );
        // The winner is not touched, and the copies age with the CREDENTIAL rather than with the
        // copy — without that a healed file outranks its own source the instant it is written and
        // the two trade places every minute for ever.
        assert!(read(&winner).contains("web-main-grant"));
        let mtime = |p: &std::path::Path| std::fs::metadata(p).unwrap().modified().unwrap();
        assert_eq!(
            mtime(&stale),
            mtime(&winner),
            "the copy did not take the credential's age"
        );

        // And then it is quiet. A tick that keeps writing resets the mtime `login_written_ms` reads
        // as evidence a credential was replaced, which would clear every remembered refusal for
        // ever — so "nothing to do" has to mean no write at all, not a write of the same bytes.
        assert_eq!(
            run(None),
            "",
            "the second pass wrote again with nothing left to do"
        );
        assert_eq!(mtime(&stale), mtime(&winner));

        // **Evidence skein produced, and now the fleet's copy moves.** A model call made with
        // `skein-test-sk-ancient` came back refused — something that HAPPENED, to a credential the sidecar
        // names — and that is the one thing a box cannot write, because a box cannot make skein's
        // call fail on skein's own behalf.
        assert!(
            run(Some(("claude", now_ms))).contains(&canon.display().to_string()),
            "the fleet's copy stayed put with a refusal recorded against the very credential in it"
        );
        assert!(
            read(&canon).contains("skein-test-sk-current"),
            "{}",
            read(&canon)
        );

        // The evidence is durable and it names its subject, so it retires itself: the copy it was
        // about is gone, and the next tick must not go on treating the fleet's login as refused.
        // (`crate::ai` drops a refusal on the same rule, SKEIN-348 — a DIFFERENT credential
        // contradicts it, a rewritten one does not.)
        let sidecar = home.join(".claude/login_evidence.json");
        assert!(sidecar.exists(), "no evidence was recorded beside the copy");
        // Somebody has since signed in again, so the copy the refusal was about is not there any
        // more — and it ranks below the box's, which is what makes this a question at all.
        let signed_in_again = put(
            home.join(".claude/.credentials.json"),
            &cred(
                "skein-test-sk-relogin",
                now_ms - 19 * 3_600_000,
                "sandbox-grant",
            ),
        );
        assert_eq!(
            run(None),
            "",
            "evidence about a credential that is gone was still believed, so a box replaced the \
             one somebody had just signed in with"
        );
        assert!(read(&signed_in_again).contains("skein-test-sk-relogin"));
    }

    /// Nothing is moved when every copy is dead, or when every copy is alive.
    ///
    /// The two ways a reconciler goes wrong: inventing a login out of corpses, and churning files
    /// that were already fine — the second is what turns a periodic tick into a source of writes
    /// under a running agent.
    #[cfg(target_os = "linux")]
    #[test]
    fn healing_does_nothing_when_there_is_nothing_to_heal() {
        // `SKEIN_FLEET_ROOT` is process-wide; see the note in
        // `a_login_typed_inside_a_box_reaches_the_file_the_banner_reads`.
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let boxes = crate::testutil::tempdir();
        let boxes = boxes.as_ref() as &std::path::Path;
        let cred = |refresh: i64| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":{refresh}}}}}"#
            )
        };
        let put = |at: std::path::PathBuf, body: &str| {
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(&at, body).unwrap();
            at
        };
        let run = || {
            std::env::set_var("SKEIN_FLEET_ROOT", boxes);
            let script = heal_logins_script(None);
            std::env::remove_var("SKEIN_FLEET_ROOT");
            String::from_utf8_lossy(
                &std::process::Command::new("bash")
                    .arg("-c")
                    .arg(&script)
                    .env("HOME", home)
                    .output()
                    .expect("bash")
                    .stdout,
            )
            .trim()
            .to_string()
        };

        // Everything dead: there is nothing to spread, and spreading a corpse would make every box
        // look logged in and fail on the first call.
        put(home.join(".claude/.credentials.json"), &cred(1));
        let a = put(boxes.join("a/home/.claude/.credentials.json"), &cred(1));
        assert_eq!(run(), "", "something was copied when nothing was alive");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), cred(1));

        // Everything alive: no writes at all. A tick that rewrites healthy files is a tick that
        // touches a file a running agent is reading, for no reason.
        let future = 32_503_680_000_000i64;
        put(home.join(".claude/.credentials.json"), &cred(future));
        put(
            boxes.join("a/home/.claude/.credentials.json"),
            &cred(future),
        );
        assert_eq!(run(), "", "healthy credentials were rewritten");
    }
}
