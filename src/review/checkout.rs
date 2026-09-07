//! Where the reading stands, and the further turns taken in the session it opened.
//!
//! A reading is a conversation, not a question, and a conversation needs an address: Claude Code
//! keys a session on the working directory of the machine it ran on, so [`Bench`] carries the
//! conversation id, the directory, what is standing there and which machine — four facts that are
//! one fact, because an id without its directory does not resume and a directory without its
//! machine resumes the wrong thing.
//!
//! The address of choice is the pull request's own review box (`docs/pr-review.md` §11); every way
//! [`at_a_review_box`] declines falls back to a checkout on skein's own filesystem, and
//! [`stand_the_change_up`] is best-effort throughout — a checkout at the WRONG commit is the one
//! outcome worse than no checkout at all, so every failure empties the tree rather than leaving
//! what was there.
//!
//! [`sweep`] and [`audit_owed`] are the turns taken afterwards. Both resume the reading's own
//! session and resend nothing, which is what makes them the cheapest recall this module has.

use super::asking::{acting_credential, review_model};
use crate::prq::review_dir;
use crate::repos::Repo;
use crate::util::*;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

/// How long the sweep gets. It resends nothing — the diff, the review and the reasoning are all
/// already in the session — so this is time to think about work already done rather than time to
/// read. Sized under stage 2's budget for that reason.
pub(super) const SWEEP_SECS: u64 = 180;

/// **The bench a reading is done at**: the conversation it belongs to, the directory that
/// conversation is filed under, what is standing there, and which machine it runs on.
///
/// Four facts and one struct, because they are one fact. Claude Code keys a session on the working
/// directory *of the machine it ran on*, so an id without its directory does not resume and a
/// directory without its machine resumes the wrong thing — or nothing. That failure has happened
/// once with only the directory unpinned (SKEIN-376); pinning the machine beside it is the same
/// lesson applied before it can happen again.
pub(super) struct Bench {
    /// The conversation id, derived from `(repo, number)`.
    pub(super) talk: String,
    /// Where the conversation is filed **on the machine below**. For a review box it is the box's
    /// own tree, which `place::Place` cds to; skein does not create it and must not.
    pub(super) at: PathBuf,
    pub(super) standing: Standing,
    /// Which machine, and it owns the box's name for as long as the reading needs it.
    on: Option<String>,
}

impl Bench {
    pub(super) fn machine(&self) -> crate::ai::Machine<'_> {
        match &self.on {
            Some(name) => crate::ai::Machine::Box(name),
            None => crate::ai::Machine::Wherever,
        }
    }
}

/// Which conversation a pull request's readings belong to, and the directory it is filed under.
///
/// The directory is the one this repo's readings already live in ([`crate::prq::review_dir`]), for
/// the reason [`crate::ai::Turn`] gives: Claude Code keys a session on the working directory, so
/// the conversation has to run somewhere stable and per-repo or `--resume` will never find it. The
/// repo's bare mirror was the other candidate and is the wrong one — creating it when it is absent
/// would leave a directory that `repos::mirror_is_made` reads as a half-made clone, so a session
/// would be bought at the price of breaking the thing boxes clone from.
///
/// It is ALSO where the code being reviewed is checked out (SKEIN-395) — the two were separate
/// problems and landed as one directory, because the conversation has to run somewhere and the
/// somewhere may as well be the change. What is standing there is the third return value, and it
/// is a fact rather than an assumption: everything about the checkout is best-effort, so nothing
/// downstream may tell a model it has code without being told that it does.
pub(super) fn conversation_of(repo: &Repo, number: u64, head_sha: &str, base_ref: &str) -> Bench {
    let talk = crate::ai::conversation_for(&repo.id, number);
    // **The pull request's own review box first** (`docs/pr-review.md` §11), and it is not an
    // optimisation: a box is a checkout of the commit under review, a private `$HOME`, a cgroup,
    // and a conversation that survives a stop — which is every property this reading has ever
    // wanted and three it could not have while it ran as a child of `skein-server`.
    if let Some(bench) = at_a_review_box(repo, number, head_sha, base_ref, &talk) {
        return bench;
    }
    let at = review_dir(&repo.id).join("trees").join(number.to_string());
    // The directory is the conversation's address (SKEIN-376), so it is made whether or not the
    // checkout below succeeds and it never moves. A cwd that changed with the weather would file
    // round two's session somewhere round one cannot be found.
    let _ = fs::create_dir_all(&at);
    let standing = stand_the_change_up(repo, number, &at, head_sha, base_ref);
    Bench {
        talk,
        at,
        standing,
        on: None,
    }
}

/// The bench in this pull request's own review box, or `None` — **and `None` is ordinary**.
///
/// Every way this declines is a reading that happens exactly as it happened before review boxes
/// existed, which is what makes the substitution safe to make for every reading rather than only
/// the engine's. It declines when the repo may not be read at all, when the fleet is already
/// holding its limit of review boxes and this pull request does not have one, and when the box
/// will not start or will not stand at the head.
///
/// **The cap bounds creating a box, never using one.** A pull request that already has a box uses
/// it whatever the count is — otherwise a reading would run in a box one round and on skein's own
/// filesystem the next, and the conversation the box exists for would be lost to the ordinary
/// business of other repos being busy.
pub(super) fn at_a_review_box(
    repo: &Repo,
    number: u64,
    head_sha: &str,
    base_ref: &str,
    talk: &str,
) -> Option<Bench> {
    // The money door, and it is the same one `Act::Read` asks: a repo whose pull requests skein
    // may not read is not one to open a box for either. `auto_review` is NOT asked — that flag is
    // about acting unattended, and a person pressing "read it" has asked for this reading.
    if !repo.read_prs {
        return None;
    }
    let existing = crate::reviewbox::theirs(&repo.id);
    if !existing.iter().any(|(_, n)| *n == number) {
        if let Some(full) = crate::reviewbox::room_for_another(existing.len()) {
            eprintln!("skein: #{number} is being read without a box of its own — {full}");
            return None;
        }
    }
    let name = match crate::reviewbox::open_at(repo, number, head_sha) {
        Ok(name) => name,
        Err(why) => {
            // Said out loud rather than swallowed: the reading still happens, but it happens
            // without the checkout §11 is about, and a person watching a reading get thinner has
            // to be able to find out why.
            eprintln!("skein: #{number} has no review box, so it is read the old way — {why}");
            return None;
        }
    };
    // What the box has, asked of the box. `None` is `Standing::Head`: the code is readable and the
    // change is not, which is a real state and not a failure.
    let standing = match crate::reviewbox::change_starts_at(&name, base_ref) {
        Some(from) => Standing::Change { from },
        None => Standing::Head,
    };
    Some(Bench {
        talk: talk.to_string(),
        // The box's own tree. Named rather than created — it is inside the box's mount namespace,
        // and `place::Place` is what cds into it.
        at: PathBuf::from(format!("{}/tree", crate::fleet::box_root(&name))),
        standing,
        on: Some(name),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Standing {
    /// Nothing is there — so the reviewer must be HANDED the change, and nothing may tell a model
    /// to go and read code that is not on disk. What still lands here: a branch deleted since the
    /// head was recorded, a repository the fetch could not reach, and a model that runs somewhere
    /// this filesystem is not. A fork's pull request no longer does — see
    /// [`crate::repos::fetch_pull_head`].
    Nothing,
    /// The head commit is checked out, and `from` is the commit the change starts from: `git diff
    /// {from} HEAD` **is** this pull request. A sha rather than a ref name on purpose — a ref
    /// leaves the model resolving `origin/main` against a mirror that may be days behind, and a
    /// merge base resolved here cannot drift between being named and being read.
    Change { from: String },
    /// The head commit is checked out but the base is not here to diff against, so the CODE is
    /// readable and the CHANGE is not. The review falls back to being handed the diff; a question
    /// or a drafted comment still gets its checkout, because neither needs the base.
    Head,
}

/// The one sentence that tells a model whether the code is on disk — **or says nothing at all**.
///
/// It exists because the sentence used to be a constant. [`ask`] and [`draft_comment`] both stated
/// "you are standing in a checkout of the commit under review" unconditionally, and
/// [`stand_the_change_up`] is best-effort by design: for a fork's head, or a branch deleted since,
/// the directory is empty and the claim was false. A model told it has code it does not have does
/// not stop to check — it answers from the question alone, and sounds exactly as sure.
///
/// [`Standing::Head`] gets the sentence: neither of these two needs the base, only the code.
pub(super) fn standing_line(standing: &Standing, doing: &str) -> String {
    match standing {
        Standing::Nothing => String::new(),
        _ => format!(
            "\nYou are standing in a checkout of the commit under review, so go and read what you \
             need rather than {doing} from memory of it.\n"
        ),
    }
}

/// Put the code being reviewed where the reviewer can read it — **or leave nothing at all**
/// (SKEIN-395).
///
/// Measured 2026-08-26, the same prompt over the same 26KB diff, run twice with only the working
/// directory different: with no checkout the reviewer made ZERO tool calls in one turn, read 29,579
/// tokens and cost $0.56; standing in a checkout it made 30 calls over 31 turns, read 2,288,629
/// tokens and cost $1.58. It is not aimless with one — it ran `git show --stat` to find what moved,
/// grepped for the types the diff mentions, read the changed file around each hunk, and followed
/// the caller into another file. That is the behaviour that found a wildcard match over
/// `ai::Unread` in skein's own code, which a diff-only reader had no way to see. The owner chose
/// the depth over the 2.8x: "give it the checkout".
///
/// **Exactly this commit, or an empty directory.** A checkout at the WRONG commit is the one
/// outcome worse than no checkout at all — it is SKEIN-395's own second possibility, a reviewer
/// confidently describing code that is not in this pull request, which is the failure hardest to
/// notice and worst for trust. That is why every failure below empties the tree rather than leaving
/// whatever was there: "nothing here" is always safe, "here is the base branch" never is.
///
/// **A fork's pull request used to be one of those empty answers, and is not any more.** Its
/// commits are in no `refs/heads/*` of this repository, so both hops below fail however often they
/// run; [`crate::repos::fetch_pull_head`] asks GitHub for the one ref it keeps for this pull
/// request. Third in order rather than first, because a same-repo head is always already here.
///
/// Best-effort throughout: every failure leaves the directory empty and the reading goes ahead
/// exactly as it did before this existed. The reviewer is worth paying for; it is not worth
/// refusing a reading over.
pub(super) fn stand_the_change_up(
    repo: &Repo,
    number: u64,
    at: &std::path::Path,
    head_sha: &str,
    base_ref: &str,
) -> Standing {
    // **The checkout and the model are in the same place, by construction.**
    //
    // There used to be a guard here, and it was the first thing this function did: a checkout on
    // skein's filesystem is worth nothing to a model that runs somewhere else, and on a host-driven
    // fleet it ran in the sandbox. Found live on the owner's fleet, on `acme/thing#740`: the
    // reading came back describing "the three visible files" of a migration squash,
    // `truncated: true`, and carrying
    //
    //     the coverage pass did not finish — `claude` exited 1: mkdir: Permission denied
    //
    // — the sandbox trying to `cd` into `/Users/you/.skein/review/…`, a path that exists on the
    // laptop and nowhere in the sandbox.
    //
    // Both destinations are now this filesystem. Skein runs inside the fleet sandbox (SKEIN-576),
    // so a model call it spawns is a local process; and a reading that opened a review box is in a
    // namespace of this same machine, which is the whole of §11. There is no deployment left in
    // which the tree stands somewhere the reader is not, so the question is not asked.
    //
    // What made it worth asking is worth keeping in view: the prompt is built from the answer.
    // `Standing::Change` sends NO diff and tells the model to go and read the tree, and claimed
    // wrongly that is the worst outcome this reading has — a confident review of code the model
    // never saw.

    // A sha skein did not get from GitHub is not a commit to go looking for.
    if head_sha.len() < 7 || !head_sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Standing::Nothing;
    }
    let git = |args: &[&str], secs: u64| {
        let mut c = std::process::Command::new("git");
        c.arg("-C").arg(at).args(args);
        bounded_output(&mut c, "git", Duration::from_secs(secs))
            .ok()
            .filter(|o| o.status.success())
    };
    if !at.join(".git").exists() {
        let Ok(mirror) = crate::repos::ensure_mirror(repo) else {
            return Standing::Nothing;
        };
        let mut clone = std::process::Command::new("git");
        clone
            .args(["clone", "--quiet"])
            .arg(&mirror)
            .arg(at)
            // `.` because `clone` names the destination itself and `-C` would fight it.
            .current_dir(".");
        if bounded_output(&mut clone, "git clone", Duration::from_secs(300))
            .ok()
            .filter(|o| o.status.success())
            .is_none()
        {
            return Standing::Nothing;
        }
    }
    // **Detached, at the commit, and nowhere else.** `--detach` because there is no branch to be on
    // and moving one would be a write to something a person owns; `git checkout <sha> -- .` would
    // leave the index describing a different commit.
    //
    // Tried before any fetch, because the overwhelmingly common case is a commit already here: the
    // clone brought the whole history down and a second round of the same head needs nothing new.
    if git(&["checkout", "--quiet", "--detach", head_sha], 120).is_none() {
        // **Not here yet, so go and get it — from GITHUB, not from the mirror.** Found on the rig
        // (2026-08-27), and it is the difference between this feature working and quietly doing
        // nothing: the checkout's origin is the mirror, so fetching it only ever asks a mirror that
        // may itself be days behind. Nothing on the reading path refreshes the mirror — `skein
        // pull` and a box start do — so a pull request pushed since the last one has no branch
        // here, the checkout stays empty, and the reviewer silently goes back to reading the diff
        // alone. Measured on the rig: the mirror was 16 hours old and did not carry the head of the
        // pull request being read.
        //
        // Two hops, in the order that costs least: the mirror is fetched from its remote, then the
        // checkout from the mirror. Only ever reached when the commit is genuinely absent, so an
        // ordinary round still pays nothing.
        if crate::repos::fetch_mirror(repo).is_ok() {
            let _ = git(&["fetch", "--quiet", "origin"], 300);
        }
        // **And if it is still not here, it is a fork's** — or a branch whose commits never
        // belonged to this repository at all. `refs/heads/*` cannot carry it however often it is
        // fetched, so the two hops above will fail for ever on exactly the pull requests that most
        // deserve reading: somebody else's. `repos::fetch_pull_head` asks for the one ref GitHub
        // keeps for this pull request and nothing else, into the mirror; the same string then
        // comes down the hop this function already makes.
        //
        // Tried third and not first, because for a same-repo pull request the head is ALWAYS
        // already reachable — its branch is in `refs/heads/*` — so an ordinary reading still pays
        // nothing, and this costs one fetch on the readings that would otherwise have got nothing.
        if git(&["checkout", "--quiet", "--detach", head_sha], 120).is_none() {
            if let Ok(refspec) = crate::repos::fetch_pull_head(repo, number) {
                let _ = git(
                    &[
                        "fetch",
                        "--quiet",
                        "origin",
                        &format!("+{refspec}:{refspec}"),
                    ],
                    300,
                );
            }
        }
        if git(&["checkout", "--quiet", "--detach", head_sha], 120).is_none() {
            // It really is not here — a fork's head, or a branch deleted since. Empty is the honest
            // answer, and the previous round's checkout must not be left behind wearing this
            // round's name: the reviewer would read it and be wrong about which change it is
            // looking at.
            clear_the_tree(at);
            return Standing::Nothing;
        }
    }
    // What a `git checkout` of a moved head leaves behind: the file deleted in this commit is still
    // sitting there from the last one, and the reviewer reads it as part of the change.
    let _ = git(&["clean", "--quiet", "-fdx"], 120);
    // Where the change STARTS. `git diff A...B` already means "from the merge base", so resolving
    // it here buys nothing a two-dot diff could not — except a sha, and the sha is the whole point:
    // it is what lets the prompt name the range without asking a model to trust `origin/{base}`
    // against a mirror nothing on this path refreshes.
    let base = format!("origin/{base_ref}");
    match git(&["merge-base", &base, "HEAD"], 60)
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|sha| sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()))
    {
        Some(from) => Standing::Change { from },
        None => Standing::Head,
    }
}

/// Empty it, keeping the directory itself — it is the conversation's address (SKEIN-376) and losing
/// it would lose every earlier round with it.
pub(super) fn clear_the_tree(at: &std::path::Path) {
    let Ok(entries) = fs::read_dir(at) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match path.is_dir() {
            true => {
                let _ = fs::remove_dir_all(&path);
            }
            false => {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

/// **The second turn: what did that review actually cover?** (SKEIN-393)
///
/// Measured before any of this: a review asked to account for its own coverage, in the conversation
/// it just reviewed in, finds real problems it skimmed past on the first pass — on
/// `acme/testbed#30` it produced two genuine bugs beyond the planted set. It is the cheapest
/// recall this reading has, because it rides the first turn's context rather than buying its own.
///
/// **The standard it checks the answer against**, in the owner's words (2026-08-26): "someone else
/// finding issues we couldn't is a bigger failure". Everything in [`merged_prompt`]'s review half
/// pushes toward saying less, which is right, and which a model can also satisfy by opening three
/// of eleven changed files.
///
/// **It asks for named things, not "anything else?".** The open question is an invitation to
/// manufacture, and manufacturing is the failure the precision wording exists to prevent — buying
/// recall with precision is not a trade, it is the same bug from the other side. So the sweep asks
/// which files went unread, puts the failure classes against each one, and is told in as many words
/// that finding nothing new is the expected answer. That is what [`SWEEP_PROMPT`]'s doc points here
/// for.
///
/// **It answers to nobody, and that is the change.** The sweep used to hand skein a parsed review
/// to fold into the one it already held, and a fold that could lose a finding was the bug
/// SKEIN-442 was written for; the function that folded is gone with the folding. There is nothing
/// to fold now: the first turn posted its
/// review to GitHub itself, so a sweep that finds something posts the addition itself too, in the
/// same session, under the same credential. What comes back is a sentence nothing reads.
///
/// Best-effort throughout. A sweep that refuses, times out, or answers nothing leaves the review
/// exactly as the first turn posted it — which is why it is safe to run unattended and why its
/// failure is not worth a word to the reader. It can only ever add.
///
/// **It does answer to one thing now: [`Summary::swept`].** The sentence it returns is still read
/// by nobody — the review on GitHub is the artefact — but *whether it came back at all* is the
/// only evidence in this tree that a pass covered the whole change, and `docs/pr-review.md` §7c
/// makes an approval wait on it. So `true` means the turn ran and said something, and every
/// other outcome — refused, timed out, out of budget, an empty answer — is `false`, which the
/// engine reads as "unknown" and never as "partial".
#[must_use]
pub(super) fn sweep(
    id: &str,
    at: &std::path::Path,
    github: Option<&crate::secret::Secret>,
    machine: crate::ai::Machine<'_>,
) -> Option<String> {
    crate::ai::claude_in_turn(
        SWEEP_PROMPT,
        review_model(Some("claude-sonnet-5")).as_deref(),
        Duration::from_secs(SWEEP_SECS),
        crate::ai::Turn::Resuming { id, at },
        github,
        // **The same machine as turn one, and this is where that matters most.** The sweep is a
        // `Turn::Resuming` — it resends nothing, because the diff and the review are already in
        // the session — so a sweep that ran anywhere else would find no session, come back empty,
        // and leave `Summary::swept` false. An approval would then be unreachable for ever, on a
        // reading that was in fact complete.
        machine,
    )
    // An empty answer is not an answer. The prompt asks for one line either way, so a turn that
    // exits successfully having printed nothing did not get to the end of it.
    //
    // **The text is kept now**, where it used to be thrown away. It carries the sweep's second
    // answer — whether what the review raised must block — and [`findings_block`] is what reads it.
    .ok()
    .filter(|said| !said.trim().is_empty())
}

/// **Does the review this sweep just accounted for have to block?** — `docs/pr-review.md` §7b.
///
/// Three-valued, and the third value is the one that matters. `workflow::Facts::findings_blocking`
/// is `None` for "there is no reading to read findings off", and an unparseable or absent verdict
/// line is exactly that: the sweep did not answer, so nobody looked. Reading it as `false` would be
/// skein stating that a review it cannot parse raised nothing that blocks — which is the one
/// direction that lets a verdict out.
///
/// The LAST such line wins. The model is asked for it on the final line, and a prompt that names
/// both forms is a prompt whose own text can appear in an answer that quotes it back.
pub(super) fn findings_block(said: &str) -> Option<bool> {
    said.lines()
        .rev()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("BLOCKING:")?;
            match rest.trim().to_ascii_lowercase().as_str() {
                "yes" => Some(true),
                "no" => Some(false),
                _ => None,
            }
        })
        .next()
}

/// What the sweep asks. Every clause is load-bearing; see [`sweep`] for why the open question is
/// not one of them.
pub(super) const SWEEP_PROMPT: &str = r###"Before that review is shown to the reviewer, account for what it actually covered. You have already read this change in this session — do not read it again from scratch, and do not restate any of it.

Work through this in order:
1. List every file this change touches. For each one, say honestly whether you read what it changed or skimmed past it.
2. Go back and read the ones you skimmed.
3. For every file, put each of these against what it changed: bugs, correctness risks, races, security holes, data loss, unhandled error paths that can actually fail, misleading names that will cause a wrong call later, real performance traps.
4. Note anything you considered raising and decided against, and why. Those do NOT go in the review.

Then POST — as an addition to the review you already left, on the same pull request, the same way you left it — ONLY what is genuinely NEW: a real problem you did not already raise. Every rule from the review still holds: no style, no formatting, no praise, no hedged maybes, nothing that restates what the change does, nothing raised twice in different words. Still a comment review: never as an approval, and never as a request for changes.

Finding nothing new is the expected outcome and the correct answer. Post nothing at all and say "nothing new". Do not post a comment to show that you looked.

Then answer in one line: either "nothing new", or one sentence on what you added.

Then, on its own final line and in exactly this form, answer whether the review as it now stands is a refusal:

BLOCKING: yes
BLOCKING: no

"yes" only if something you raised is a defect that must be fixed before this change lands — wrong behaviour, data loss, a security hole, a correctness risk that will actually bite. Anything you would be content to see merged and followed up is "no". A review that raised nothing is "no"."###;

/// Longer than the sweep's, because an audit is a search rather than a re-read: "where is this
/// guarantee made instead" is a question about the whole tree, and the box has the whole tree.
pub(super) const AUDIT_SECS: u64 = 300;

/// **Ask this pull request's own review session one check the repository owes** —
/// `docs/pr-review.md` §8, and [`crate::workflow::Act::Audit`]'s whole implementation.
///
/// # Why it is a turn in the reading's conversation and not a reading of its own
///
/// This module fought hardest against having a second reader, and an audit that stood the change
/// up again, downloaded the diff again and asked a fresh model about it would be exactly that —
/// with the added defect that the second reader would not know what the first one had already
/// said. [`sweep`] settled the shape: a `Turn::Resuming` in the pull request's own session, on the
/// same machine, resending nothing. The audit inherits the whole reading as context for free, and
/// costs one turn.
///
/// **The same machine as the reading, and here that is not merely tidy.** A resuming turn that ran
/// anywhere else finds no session and comes back empty — the failure [`Bench`] exists to prevent,
/// and the reason the machine is pinned beside the directory rather than derived again here.
///
/// # It fails loudly, unlike the sweep
///
/// The sweep is best-effort because its only consumer is [`Summary::swept`], which reads every
/// failure as "unknown" and leaves the pull request at full attention. This one is different: its
/// consumer is [`crate::owed::record`], and recording a check nobody answered would satisfy §8's
/// condition with nothing behind it — a verdict released by a model call that timed out. So an
/// empty answer is an error, and the caller records nothing.
pub fn audit_owed(
    repo: &Repo,
    number: u64,
    head_sha: &str,
    base_ref: &str,
    owed: &str,
) -> Result<String, String> {
    let bench = conversation_of(repo, number, head_sha, base_ref);
    let said = crate::ai::claude_in_turn(
        &audit_prompt(owed),
        review_model(Some("claude-sonnet-5")).as_deref(),
        Duration::from_secs(AUDIT_SECS),
        crate::ai::Turn::Resuming {
            id: &bench.talk,
            at: &bench.at,
        },
        acting_credential().as_ref(),
        bench.machine(),
    )
    // `Unread::say` rather than the variant: this sentence goes into the workflow journal and onto
    // a row, and each variant carries its own cure.
    .map_err(|e| e.say())?;
    match said.trim() {
        // The prompt asks for a verdict line either way, so a turn that exited having printed
        // nothing did not reach the end of it — and "it ran and said nothing" is the shape a
        // resume takes when it found no session to resume.
        "" => Err(format!(
            "the audit turn for #{number} came back empty, so nothing was checked — the reading's \
             session was not there to resume, or the turn did not finish"
        )),
        answer => Ok(answer.to_string()),
    }
}

/// What an audit asks. One check, named by the repository, against the tree the box is standing in.
pub(super) fn audit_prompt(owed: &str) -> String {
    format!(
        r###"Before this review becomes a verdict, this repository owes one check. You have already read this change in this session — do not read it again from scratch, and do not restate the review.

The check: {owed}

You are standing in a checkout of this change. Use it. Go and look at the code rather than reasoning from the diff you were shown — the diff may have been truncated, and the answer to "where is that guaranteed instead" is usually in a file the diff does not contain.

If the check turns up a real problem, POST it as an addition to the review you already left, on the same pull request, the same way you left it. Every rule from the review still holds: no style, no praise, no hedged maybes, nothing raised twice in different words. Still a comment review: never an approval, never a request for changes.

Finding nothing is the expected outcome and the correct answer. Post nothing at all and say so. Do not post a comment to show that you looked.

Then answer in one line, beginning either "clear:" or "found:", saying what you actually checked and what came of it."###
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::testkit::*;

    /// **A verdict nobody gave is not "nothing blocks".**
    ///
    /// `findings_block` is what makes `Act::PostChanges` reachable, so the failure directions are
    /// not symmetric. Reading an absent or unparseable answer as `false` costs an approval that
    /// should have waited; reading one as `true` posts a request for changes on somebody's pull
    /// request off a sweep that never ran. Both are refused by the rule the rest of the engine's
    /// facts use: unknown satisfies neither a condition nor its opposite.
    ///
    /// **The last line wins**, and that row is not hypothetical: the prompt names both forms, so a
    /// model that quotes the instruction back before answering would otherwise have skein read the
    /// instruction as the answer.
    ///
    /// **What would make each row fail:** defaulting to `Some(false)` when there is no line, which
    /// is the approval-by-silence direction; matching with `contains` rather than on the line, or
    /// taking the first match rather than the last — the quoted-prompt row catches both.
    #[test]
    fn a_blocking_verdict_is_read_only_when_one_was_actually_given() {
        assert_eq!(
            super::findings_block("nothing new\nBLOCKING: no"),
            Some(false)
        );
        assert_eq!(
            super::findings_block("added one\nBLOCKING: yes"),
            Some(true)
        );
        assert_eq!(
            super::findings_block("BLOCKING: YES"),
            Some(true),
            "case is not the answer"
        );

        assert_eq!(
            super::findings_block("nothing new"),
            None,
            "a sweep that answered the first question and not the second was read as saying \
             nothing blocks, which is an approval granted by silence"
        );
        assert_eq!(super::findings_block(""), None);
        assert_eq!(
            super::findings_block("BLOCKING: maybe"),
            None,
            "an answer outside the two forms was taken for one of them"
        );

        // The model quoting the instruction back before answering.
        assert_eq!(
            super::findings_block(
                "I was asked for\nBLOCKING: yes\nBLOCKING: no\n\nnothing new\nBLOCKING: no"
            ),
            Some(false),
            "the prompt's own text was read as the answer"
        );
    }

    /// The prompt asks for the line [`super::findings_block`] parses. Neither is any use alone, and
    /// they live thirty lines apart.
    #[test]
    fn the_sweep_asks_for_the_verdict_its_reader_parses() {
        assert!(
            super::SWEEP_PROMPT.contains("BLOCKING: yes")
                && super::SWEEP_PROMPT.contains("BLOCKING: no"),
            "the sweep no longer asks for the blocking verdict, so `findings_block` reads a line \
             nothing is asked to write and every reading answers `None`"
        );
    }

    // ── the reviewer stands in the change (SKEIN-395) ─────────────────────────────────────────
    //
    // Measured, not assumed: the same prompt over the same diff, run twice with only the working
    // directory different — no checkout gave ZERO tool calls in one turn ($0.56); a checkout gave
    // 30 calls over 31 turns ($1.58), reading the changed file around each hunk and following the
    // caller into another file. The owner chose the depth. What these hold is the safety property
    // that makes it worth having: exactly this commit, or nothing.

    /// Two commits, and the reviewer sees the one being reviewed — including after the branch
    /// moves, which is the round-two case and the one where a leftover file reads as part of the
    /// change.
    #[test]
    fn the_reviewer_stands_in_the_commit_being_reviewed_and_not_the_one_before_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let (repo, first, second) = a_repo_with_two_commits(home);

        let bench = super::conversation_of(&repo, 7, &first, "main");
        let at = bench.at.clone();
        assert_eq!(
            fs::read_to_string(at.join("only-in-first.txt"))
                .ok()
                .as_deref(),
            Some("one\n"),
            "the reviewer is not standing in the commit it was asked to review — with nothing to \
             read it makes no tool calls at all, which is the whole of what this buys"
        );

        // **Something the reviewer itself left.** `claude` writes into the directory it runs in —
        // scratch files, a `.claude` of its own — and a round that inherits the last round's litter
        // shows it to the reviewer as part of the change. Tracked deletions are git's job and it
        // does them; this is the part that is nobody's unless it is asked for.
        fs::write(at.join("scratch-from-the-last-round.txt"), "litter\n").unwrap();

        // The branch moves. `only-in-first.txt` is deleted in the second commit, and a checkout
        // that left it behind would show the reviewer a file this change does not contain.
        let bench = super::conversation_of(&repo, 7, &second, "main");
        let again = bench.at.clone();
        assert_eq!(
            again, at,
            "the checkout moved, so the conversation moved with it and every earlier round is \
             filed where the next resume will not look (SKEIN-376)"
        );
        assert!(
            at.join("only-in-second.txt").exists(),
            "the second commit's own file is missing, so the reviewer is reading the commit before \
             the one under review"
        );
        assert!(
            !at.join("only-in-first.txt").exists(),
            "a file this commit deletes is still sitting in the checkout, so the reviewer reads it \
             as part of the change — that is a review confidently wrong about the code, which is \
             worse than no checkout at all"
        );
        assert!(
            !at.join("scratch-from-the-last-round.txt").exists(),
            "an untracked file from the previous round is still in the checkout, so the reviewer \
             reads litter as part of the change — the same wrongness as a stale tracked file, and \
             the one git will not clear on its own"
        );

        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_NO_GH_SECRET");
    }

    /// **A commit that landed since the mirror was last fetched is still stood up** — found on the
    /// rig (2026-08-27), where it was the difference between this feature working and quietly doing
    /// nothing at all.
    ///
    /// The checkout's origin is the MIRROR, so fetching it only asks a mirror that may itself be
    /// behind, and nothing on the reading path refreshes one. A pull request pushed since the last
    /// `skein pull` therefore had no branch anywhere skein could see, the checkout stayed empty,
    /// and the reviewer silently went back to reading the diff alone — with no failure anywhere,
    /// which is why only standing it up against a real repository caught it.
    #[test]
    fn a_commit_pushed_since_the_mirror_was_fetched_is_still_stood_up() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let (repo, _, second) = a_repo_with_two_commits(home);
        // A reading happens, so the mirror and the checkout both exist and are current.
        let bench = super::conversation_of(&repo, 7, &second, "main");
        let at = bench.at.clone();
        assert!(
            at.join("only-in-second.txt").exists(),
            "the fixture never stood up"
        );

        // Now somebody pushes. The mirror knows nothing about it — exactly the rig's state, where
        // the mirror was sixteen hours old and did not carry the head of the pull request skein
        // was reading.
        let src = home.join("origin");
        let git_src = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&src)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        fs::write(src.join("pushed-after-the-mirror.txt"), "three\n").unwrap();
        git_src(&["add", "-A"]);
        git_src(&["commit", "-qm", "three"]);
        let third = git_src(&["rev-parse", "HEAD"]);

        let bench = super::conversation_of(&repo, 7, &third, "main");
        let again = bench.at.clone();
        assert_eq!(again, at, "the conversation's address moved");
        assert!(
            at.join("pushed-after-the-mirror.txt").exists(),
            "a commit pushed since the mirror was last fetched left the checkout empty, so the \
             reviewer reads the diff alone and the whole checkout does nothing — and nothing \
             anywhere fails, which is why this is a test and not a bug report"
        );

        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_NO_GH_SECRET");
    }

    /// **A commit skein cannot get is an EMPTY directory, never the wrong one.** A pull request
    /// from a fork has no branch in the mirror; standing the reviewer in the base branch and
    /// letting it believe that is the change is SKEIN-395's second possibility, the failure hardest
    /// to notice and worst for trust.
    #[test]
    fn a_commit_the_mirror_does_not_have_leaves_nothing_rather_than_the_wrong_code() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let (repo, first, _) = a_repo_with_two_commits(home);
        let bench = super::conversation_of(&repo, 9, &first, "main");
        let at = bench.at.clone();
        assert!(
            at.join("only-in-first.txt").exists(),
            "the fixture never stood up"
        );

        // A head skein was told about and the mirror has never heard of — a fork's.
        let bench = super::conversation_of(&repo, 9, &"b".repeat(40), "main");
        let same = bench.at.clone();
        assert_eq!(same, at, "the conversation's address moved");
        assert!(
            !at.join("only-in-first.txt").exists(),
            "a pull request whose commit skein could not get was reviewed against whatever the \
             checkout happened to hold — the reviewer describes code that is not in this change"
        );
        assert!(
            at.is_dir(),
            "the directory itself was removed, taking every earlier round of the conversation \
             filed under it (SKEIN-376)"
        );

        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_NO_GH_SECRET");
    }

    /// **What stood up is reported, and the three answers are three different answers.**
    ///
    /// The whole cut below rests on this one value: with [`super::Standing::Change`] the reviewer
    /// is told to go and read the change and is sent no diff, so a checkout that reported success
    /// it did not have would produce a review of nothing — confident, well-formed, and about code
    /// the model never saw. [`super::Standing::Head`] exists for the same reason from the other
    /// side: the code being there is not the same fact as the CHANGE being there.
    #[test]
    fn what_is_standing_in_the_conversation_is_reported_and_not_assumed() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let (repo, first, _second) = a_repo_with_two_commits(home);

        let bench = super::conversation_of(&repo, 7, &first, "main");
        let standing = bench.standing.clone();
        assert_eq!(
            standing,
            super::Standing::Change {
                from: first.clone()
            },
            "the commit the change starts from was not resolved, so the review falls back to \
             being handed a diff it did not need — or, worse, names a range that is not there"
        );

        // The code is here; the base is not. A branch skein has no ref for is the ordinary case on
        // a mirror that has not been fetched since the base branch was created.
        let bench = super::conversation_of(&repo, 8, &first, "a-branch-nobody-has");
        let no_base = bench.standing.clone();
        assert_eq!(
            no_base,
            super::Standing::Head,
            "a checkout with no base to diff against reported itself as a readable CHANGE, so the \
             prompt names `git diff <nothing> HEAD` and the reviewer is left to guess the range"
        );

        // A commit that is not in the mirror — a fork's head, or a branch deleted since.
        let bench = super::conversation_of(&repo, 9, &"b".repeat(40), "main");
        let gone = bench.standing.clone();
        assert_eq!(
            gone,
            super::Standing::Nothing,
            "a commit skein could not get reported as standing, which is the one failure worse \
             than no checkout at all: the reviewer is told to go and read an empty directory"
        );

        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_NO_GH_SECRET");
    }

    /// **A fork's pull request stands up too** — from the one ref `fetch_mirror` does not ask for.
    ///
    /// The head of a pull request opened from a fork is in no `refs/heads/*` of the base
    /// repository, so both hops in [`super::stand_the_change_up`] miss however often they run: the
    /// mirror's refspec is heads and tags, and the checkout's `origin` IS the mirror. Every such
    /// reading answered [`super::Standing::Nothing`] and the reviewer read a diff where a whole
    /// tree was available — the right answer to the wrong question, which is the shape this file
    /// keeps finding.
    ///
    /// **The pull ref is added AFTER the mirror is made**, and that is what gives this test teeth
    /// rather than being incidental: `clone_mirror` clones with `--mirror`, so a ref that already
    /// existed would be in the mirror from creation and this would pass with the fetch removed. It
    /// is also the real case — a pull request opened after skein first saw the repository.
    ///
    /// Remove the `fetch_pull_head` call from `stand_the_change_up` and this fails: both hops
    /// miss, `clear_the_tree` runs, and `Standing::Nothing` comes back.
    #[test]
    fn a_pull_request_with_no_branch_is_still_stood_up_from_its_pull_ref() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let src = home.join("origin");
        fs::create_dir_all(&src).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&src)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        fs::write(src.join("base.txt"), "base\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);

        let repo: Repo = serde_json::from_value(serde_json::json!({
            "id": "acme",
            "source": src.to_string_lossy(),
            "store": "",
        }))
        .unwrap();
        // The mirror is made while the repository has ONE branch and no pull refs at all.
        crate::repos::ensure_mirror(&repo).expect("the fixture repo is mirrored");

        // Now the contribution arrives, the way a fork's does: a commit this repository can serve
        // but that no branch of it points at. The branch is deleted so nothing in `refs/heads/*`
        // can reach it — which is exactly a fork's head as the BASE repository sees it.
        git(&["checkout", "-q", "-b", "contrib"]);
        fs::write(src.join("contributed.txt"), "from a fork\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "contributed"]);
        let head = git(&["rev-parse", "HEAD"]);
        git(&["update-ref", &crate::repos::pull_head_ref(7), &head]);
        git(&["checkout", "-q", "main"]);
        git(&["branch", "-qD", "contrib"]);

        let bench = super::conversation_of(&repo, 7, &head, "main");
        let at = bench.at.clone();
        let standing = bench.standing.clone();
        assert!(
            matches!(standing, super::Standing::Change { .. }),
            "a pull request whose head is only under refs/pull/7/head was not stood up: \
             {standing:?} — the reviewer would be handed a diff with a whole tree available"
        );
        assert_eq!(
            fs::read_to_string(at.join("contributed.txt"))
                .ok()
                .as_deref(),
            Some("from a fork\n"),
            "the tree is standing somewhere, but not at the contributed commit"
        );
    }

    // ── a review that already went (SKEIN-397) ────────────────────────────────────────────────
    //
    // Found on the rig against real GitHub, not in a test: post, receipt written, post again, TWO
    // identical reviews on the pull request. The receipt existed the whole time and nothing read it.

    // ── the second turn (SKEIN-393) ────────────────────────────────────────────────────────────
    //
    // The review accounts for its own coverage before anybody sees it. Three things decide whether
    // that is worth anything, and all three are testable without a model: what the turn is told
    // (the flags), what it is asked (the prompt), and what it is allowed to do to the review it
    // came back to (the fold).

    /// What the second turn is TOLD, and what every other call is not. `Alone` adding a flag would
    /// change every model call skein makes, silently, from a change about reviews.
    #[test]
    fn a_turn_names_its_conversation_and_a_lone_call_says_nothing() {
        use crate::ai::Turn;
        let at = std::path::Path::new("/tmp/skein-turn-test");
        assert!(
            Turn::Alone.args().is_empty(),
            "a call that belongs to no conversation grew a flag, so this changed every other \
             model call skein makes"
        );
        assert_eq!(
            Turn::Opening { id: "abc", at }.args(),
            vec!["--session-id", "abc"],
            "the first turn does not name the conversation it is opening, so the second cannot \
             find it"
        );
        assert_eq!(
            Turn::Resuming { id: "abc", at }.args(),
            vec!["--resume", "abc"],
            "the second turn opens a NEW conversation instead of resuming — which fails on a \
             collision and, worse, costs the whole diff again when it does not"
        );
    }

    // ── the pull request's own conversation (SKEIN-376) ───────────────────────────────────────
    //
    // Measured against the installed CLI on 2026-08-26, and both halves matter:
    //   * `--resume` on an id it does not hold exits 1 with "No conversation found with session
    //     ID: <id>" and spends nothing — which is what makes trying the resume first affordable;
    //   * a session opened in one directory and resumed from ANOTHER gets that same answer, and
    //     resumed from the directory that opened it answers from memory. That is the failure this
    //     item exists for: unpinned, every resume misses and the feature does nothing while
    //     looking like it works.

    /// The id is derived from the pull request, so two rounds of the same one meet in the same
    /// conversation and two different ones never do.
    #[test]
    fn a_pull_request_reads_under_an_id_derived_from_it_and_not_from_the_moment() {
        let a = crate::ai::conversation_for("acme", 41);
        assert_eq!(
            a,
            crate::ai::conversation_for("acme", 41),
            "the same pull request produced two different conversation ids, so the second round \
             cannot resume what the first one left and every round is a cold read"
        );
        assert_ne!(
            a,
            crate::ai::conversation_for("acme", 42),
            "two pull requests share one conversation, so a review can answer about the wrong \
             change"
        );
        assert_ne!(
            a,
            crate::ai::conversation_for("other", 41),
            "the same number in two repos shares one conversation — the id is keyed on the number \
             alone, so it depends on where the call runs to stay correct"
        );
        assert_eq!(
            a.len(),
            36,
            "the id is not uuid-shaped and --session-id is documented as taking one: {a}"
        );
        assert!(
            a.chars().filter(|c| *c == '-').count() == 4
                && a.chars().all(|c| c == '-' || c.is_ascii_hexdigit()),
            "the id is not hex-and-dashes: {a}"
        );
    }

    /// **The pin, which is the one that ships broken without being noticed.** A conversation is
    /// filed under the directory the call ran in, so a turn that does not carry one resumes
    /// nothing — and nothing fails, it just quietly costs the whole diff every round.
    #[test]
    fn a_turn_in_a_conversation_carries_the_directory_it_is_filed_under() {
        use crate::ai::Turn;
        let at = std::path::Path::new("/tmp/skein-turn-test");
        assert_eq!(
            Turn::Opening { id: "abc", at }.at(),
            Some(at),
            "the first turn does not say where it runs, so it opens the conversation wherever the \
             server happens to have been started"
        );
        assert_eq!(
            Turn::Resuming { id: "abc", at }.at(),
            Some(at),
            "the resuming turn does not say where to look, so it looks in the server's directory \
             and is told there is no such conversation"
        );
        assert_eq!(
            Turn::Alone.at(),
            None,
            "a call in no conversation was pinned to a directory anyway, which changes where every \
             other model call skein makes runs"
        );
    }

    /// Where a pull request's conversation lives, and that it is there to be run in.
    ///
    /// **Per PULL REQUEST, not per repo** (SKEIN-395): the directory is now also the checkout the
    /// reviewer stands in, and two pull requests sharing one would have their heads fighting over
    /// it — a reading of #7 could be looking at #9's code. That is why the address moved down a
    /// level rather than the checkout being bolted onto the side of it.
    #[test]
    fn a_pull_requests_conversation_is_filed_where_it_can_be_stood_up_and_nowhere_shared() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");
        let (repo, head, _) = a_repo_with_two_commits(home);

        let bench = super::conversation_of(&repo, 7, &head, "main");
        let id = bench.talk.clone();
        let at = bench.at.clone();
        assert_eq!(id, crate::ai::conversation_for("acme", 7));
        assert!(
            at.is_dir(),
            "the directory the conversation is filed under does not exist, so the spawn that \
             opens it fails before it starts: {}",
            at.display()
        );
        let bench = super::conversation_of(&repo, 9, &head, "main");
        let other = bench.at.clone();
        assert_ne!(
            at, other,
            "two pull requests share one directory, so they share a checkout — a reading of one \
             can be standing in the other's code, and their sessions are filed together"
        );
        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_NO_GH_SECRET");
    }

    /// The sweep asks for NAMED things. "Anything else?" is an invitation to manufacture, and
    /// manufacturing is the precision failure — buying recall with it is not a trade, it is the
    /// same bug from the other side.
    #[test]
    fn the_sweep_asks_what_went_unread_and_says_that_finding_nothing_is_correct() {
        let p = super::SWEEP_PROMPT;
        assert!(
            p.contains("skimmed") && p.contains("List every file"),
            "the sweep does not ask which files went unread, so it cannot tell a review that \
             covered everything from one that covered three files well"
        );
        assert!(
            p.contains("Finding nothing new is the expected outcome"),
            "nothing tells the sweep that an empty answer is the right one, so a turn asked to \
             look again will find something to say"
        );
        assert!(
            p.contains("do not read it again from scratch"),
            "the sweep does not say the change has already been read in this session, so the \
             cheap turn can go and buy the expensive thing back"
        );
        // The sweep POSTS what it finds, on the same terms as the review it is adding to — and
        // never as a verdict. A second turn that quietly gained the power to approve would be the
        // one place in this feature where a model decides something on the reader's behalf.
        assert!(
            p.contains("POST") && p.contains("never as a request for changes"),
            "the sweep's finding goes nowhere, or goes as something other than a comment"
        );
        assert!(
            p.contains("no hedged maybes") && p.contains("nothing raised twice"),
            "the review's own discipline was not carried into the sweep, so the second turn is \
             free to pad what the first was stopped from padding"
        );
    }
}
