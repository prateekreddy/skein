//! How one repo's queue gets built, how often, and what the badge counts.
//!
//! [`queue_within`] is the whole refresh: the searches, the lanes, the pruning and what could not
//! be looked at. Everything else here is about spending it once — a 60s micro-cache per repo, one
//! background refresh at a time, and [`counts`] reading through a ten-minute budget because the
//! badge polls from every open tab.

use super::checks::{rollup_state_missing, rollup_total, truncated_rollup};
use super::credentials::{record_rename, renamed_to};
use super::node::{build_pr, contexts};
use super::search::{answered_batch_width, search_prs_all, LABELS_FETCHED, REVIEWS_FETCHED};
use super::store::{prune_archived, prune_snoozed, remember};
use super::*;

// ───────────────────────────── fetching ─────────────────────────────

/// 60s micro-cache **per repo**, for the same reason [`crate::repos::REPOS_CACHE`] exists: the
/// cockpit re-renders far more often than GitHub changes, and each fetch is three network round
/// trips.
///
/// Keyed by repo rather than holding one entry, because the badge poller walks every repo that has
/// the queue switched on. A single slot would let each repo evict the last one and turn a cache
/// into a guaranteed miss — the exact opposite of what it is for.
static QUEUE_CACHE: Mutex<Option<HashMap<String, (Instant, Queue)>>> = Mutex::new(None);

/// Repos with a background refresh already in flight, so [`merged`] never runs two at once for
/// one repo (SKEIN-206). A `Vec` because `Mutex::new(Vec::new())` is const and the fleet has
/// single-digit repos — a set would buy nothing.
static REFRESHING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Membership in [`REFRESHING`] for one repo, ended by `Drop` — so the slot frees on the
/// refresh thread's every exit, an errored fetch and a panic included. A slot that leaked would
/// be worse than the duplicate it prevents: that repo would never refresh in the background
/// again, and the pane would repaint yesterday's queue forever.
struct RefreshRunning(String);

impl RefreshRunning {
    /// Take the slot for `repo_id`, or `None` when a refresh is already running there.
    fn begin(repo_id: &str) -> Option<Self> {
        let mut running = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
        if running.iter().any(|id| id == repo_id) {
            return None;
        }
        running.push(repo_id.to_string());
        Some(Self(repo_id.to_string()))
    }
}

impl Drop for RefreshRunning {
    fn drop(&mut self) {
        let mut running = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
        running.retain(|id| id != &self.0);
    }
}

/// Drop one repo's cached queue — after an act that changes a PR's state, so the next read shows it.
pub fn invalidate(repo_id: &str) {
    if let Some(map) = QUEUE_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        map.remove(repo_id);
    }
}

/// Build a repo's review queue. `force` skips the micro-cache.
pub fn queue(repo: &Repo, force: bool) -> Result<Queue, String> {
    // `ZERO`: nothing is younger than no time at all, so force refreshes past whatever is cached.
    let max_age = match force {
        true => Duration::ZERO,
        false => Duration::from_secs(60),
    };
    queue_within(repo, max_age)
}

/// Fetch this repo's mirror, at most once every [`MIRROR_FRESH`] (SKEIN-430).
///
/// **Gated, because a queue refreshes far more often than a repository changes.** The pane's own
/// queue is sixty seconds old at most and the badge poller runs every three minutes; a fetch on
/// each would be a network round trip per repo per tab, which is the steady-state spend that got a
/// live fleet rate-limited once already (see [`queue_within`]'s own note about it). Ten minutes
/// is the badge poller's interval — the slowest thing that asks — so at worst the mirror trails the
/// queue by one of those, and any reader that needs an exact commit still fetches for itself
/// (`review::stand_the_change_up`).
///
/// Errors are dropped on purpose and this returns nothing: the caller asked for a queue. A mirror
/// that could not be fetched is the mirror that was already there, which is what every reader had
/// before this existed.
fn catch_the_mirror_up(repo: &Repo) {
    static LAST: std::sync::Mutex<Option<HashMap<String, Instant>>> = std::sync::Mutex::new(None);
    {
        let mut held = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let seen = held.get_or_insert_with(HashMap::new);
        if let Some(at) = seen.get(&repo.id) {
            if at.elapsed() < MIRROR_FRESH {
                return;
            }
        }
        // Stamped BEFORE the fetch, not after: two queue refreshes arriving together would
        // otherwise both find no stamp and both fetch, which is the thundering herd this is for.
        seen.insert(repo.id.clone(), Instant::now());
    }
    let _ = crate::repos::fetch_mirror(repo);
}

/// How long a mirror may trail the queue. The badge poller's own interval — the slowest thing that
/// asks for a queue — so this never makes skein fetch more often than it already polls.
const MIRROR_FRESH: Duration = Duration::from_secs(600);

/// Build a repo's review queue, serving the remembered in-process copy while it is younger than
/// `max_age`.
///
/// One cache, two budgets. [`queue`]'s sixty seconds fits a pane somebody is looking at; the badge
/// poller passes ten minutes, because a badge is a number acted on within minutes and every refresh
/// behind it is a GitHub round trip **per repo, per open tab, every three minutes** — the
/// steady-state spend that got a live fleet rate-limited, back when each refresh was five separate
/// GraphQL searches rather than [`search_prs_all`]'s one.
pub fn queue_within(repo: &Repo, max_age: Duration) -> Result<Queue, String> {
    if queues_are_cached() {
        if let Some(young) = unexpired_within(&repo.id, max_age) {
            return Ok(young);
        }
    }
    let stored = repo_slug(repo)
        .ok_or("this repo has no GitHub remote, so it has no pull requests to review")?;
    let (login, teams) = viewer()?;
    // `None` is "GitHub would not say", which is a different fact from "you are in no teams" — see
    // [`viewer`]. Everything below reads the list; only the prunes read the difference.
    let teams_unknown = teams.is_none();
    let teams = teams.unwrap_or_default();
    let mut blind_spots = Vec::new();
    // What this repository is called NOW. A name is not an identifier: `acme/gadget-demo`
    // became `acme/thing`, and because GitHub's search matches a stale name against nothing
    // — HTTP 200, zero results, no error — the queue rendered empty while twenty-three pull
    // requests waited on a review. An empty queue is the one thing this module must never be able
    // to show by accident.
    //
    // Written back into the repo, not just used here. Everything else keyed on the slug follows it:
    // `gitgate`'s per-repo write credentials, the mirror's origin, what a box may push to.
    let slug = match renamed_to(&stored) {
        Some(now) => {
            if let Err(why) = record_rename(&repo.id, &stored, &now) {
                blind_spots.push(format!(
                    "{stored} is now {now}, and skein could not record that ({why}) — it will look \
                     it up again every time until it can"
                ));
            }
            now
        }
        None => stored,
    };
    if teams_unknown {
        // Short, and it names the cure. A warning that cannot be acted on is shown on every load
        // forever, and a banner that is always there stops being read — so the fix belongs in the
        // sentence, not in documentation somewhere behind it.
        //
        // On `teams_unknown` rather than on an empty list: an account that is genuinely in no teams
        // was being told, on every refresh for ever, to fix a scope that was not the problem.
        blind_spots.push(
            "team review requests are missing — `gh` cannot list your teams. Fix: gh auth refresh -s read:org"
                .into(),
        );
    }

    // One query per membership rule — GitHub's search cannot express the union, and a client-side
    // filter over every open PR would be far more expensive on a busy repo than these narrow
    // searches. They all travel in ONE GraphQL request (`search_prs_all`), aliased q0..qN in this
    // order — which is also the Reason precedence order, because the merge below keeps reasons in
    // the order the searches answered.
    let mut searches: Vec<(String, Reason)> = vec![
        (format!("review-requested:{login}"), Reason::Reviewer),
        // Both, because GitHub moves a PR from one to the other the moment you submit any review
        // — see [`Reason::Reviewed`]. Without the second, acting on your queue empties it.
        (format!("reviewed-by:{login}"), Reason::Reviewed),
        (format!("author:{login}"), Reason::Author),
        (format!("mentions:{login}"), Reason::Mentioned),
    ];
    for team in &teams {
        searches.push((
            format!("team-review-requested:{team}"),
            Reason::Team(team.clone()),
        ));
    }

    let archived_numbers = archived(&repo.id);
    let snoozed_shas = snoozed(&repo.id);
    let mut prs: Vec<Pr> = Vec::new();
    let texts: Vec<String> = searches.iter().map(|(s, _)| s.clone()).collect();
    // Does this refresh know what is open?
    //
    // Every prune below deletes one of your own decisions because a pull request did not
    // appear — and "did not appear" only means "is not open" when the searches actually answered.
    // A whole-request failure produces exactly the same empty list as a repo with nothing waiting,
    // so the count cannot tell them apart; the searches can, and they say so here rather than
    // leaving the prune to infer it (SKEIN-229).
    //
    // **A rule that could not be WRITTEN counts too** (SKEIN-262). Without `read:org` the loop
    // below adds no `team-review-requested:` search at all, so a pull request whose only claim on
    // you is a team review request cannot appear in this list — and the four personal searches all
    // answer, so nothing here noticed. The prunes then read that absence as "closed" and deleted
    // the set-aside and its snooze, silently, on every badge poll, permanently on a fleet
    // whose token lacks the scope (SKEIN-239). A search that failed and a search that was never
    // possible are different things and the same hole.
    let mut answered = !teams_unknown;
    // A whole-request failure — the network, a 5xx, the rate-limit hold — is every search failing
    // at once, and it is said ONCE.
    //
    // It used to be mapped onto each rule, so one dead request printed five near-identical alarms.
    // The owner saw exactly that on a cold load: five lines that read as five broken things, none
    // of which said the two facts a reader needs — that it was one failure, and that it took the
    // whole refresh with it (SKEIN-258). A per-ALIAS failure keeps its own sentence in the loop
    // below, because "which membership went dark" is real information there and the batching was
    // careful to keep it answerable.
    let outcomes = match search_prs_all(&slug, &texts) {
        Ok(outcomes) => outcomes,
        Err(e) => {
            answered = false;
            let n = searches.len();
            blind_spots.push(match crate::github::connection_died(&e) {
                // Said as the transport failure it is (SKEIN-271). "GitHub did not answer" sends
                // whoever reads it to look at GitHub — at a token, a rate limit, a refusal — and a
                // connection that died is not GitHub answering anything. The two ask different
                // things of a reader: this one says the request never completed, so ask again.
                // Skein already has, once, by the time this line is written.
                true => format!(
                    "{e} — and again when skein asked a second time, so all {n} of this \
                     refresh's membership searches for {slug} are missing; they travel in one \
                     request, so this is one failure and not {n}"
                ),
                false => format!(
                    "GitHub did not answer for {slug}, so all {n} of this refresh's membership \
                     searches are missing — they travel in one request, so this is one failure \
                     and not {n}: {e}"
                ),
            });
            Vec::new()
        }
    };
    // **A repo that is being asked in narrow batches says so** (SKEIN-278). The narrowing is
    // adaptive and invisible from the outside: the queue looks identical whether it cost one
    // request or four, so a repository that has quietly become expensive to refresh would never be
    // anything a reader could see. It is not a blind spot in the completeness sense — every
    // search still answered — which is exactly what this list is for beside `whole`.
    //
    // It ends itself. The memo behind it holds a width GitHub ANSWERED and expires, so the sentence
    // is gone the refresh after the wide request works again; nothing needs clearing by hand.
    if let Some(width) = answered_batch_width(&slug).filter(|w| *w < searches.len()) {
        blind_spots.push(format!(
            "{slug}'s {} membership searches are being asked {width} at a time — GitHub would not \
             answer them in one request, so every refresh of this repo costs more than one. Skein \
             tries the single request again within the hour.",
            searches.len()
        ));
    }
    for ((search, reason), outcome) in searches.iter().zip(outcomes) {
        let found = match outcome {
            Ok(found) => found,
            Err(e) => {
                answered = false;
                blind_spots.push(format!(
                    "the `{search}` query failed, so those PRs are missing: {e}"
                ));
                continue;
            }
        };
        // A search cut off at the page is an answer about what it returned and no answer at all
        // about what it did not reach, which is the half the prune reads.
        //
        // And it is said out loud (SKEIN-231). Silence here is the rename bug in a quieter form —
        // HTTP 200, a plausible list, nothing wrong to see — except that instead of an empty queue
        // it shows a queue that looks complete. The pull requests past the page are absent from the
        // rows, absent from the badge, and until this line nothing anywhere said a number had been
        // cut off. GitHub is asked how many it matched, so the sentence can carry the size of the
        // hole rather than only its existence.
        //
        // The count it says out loud is what actually ARRIVED, not `SEARCH_PAGE` (SKEIN-280). The
        // sentence used to name the page size because the page was all a refresh ever read; now it
        // follows the cursor, so a rule that is still short after five pages has read five hundred
        // and saying "the first 100" would understate its own queue by four hundred pull requests.
        let read = found.items.len();
        if !found.whole {
            blind_spots.push(match found.matched {
                Some(n) => format!(
                    "the `{search}` query matched {n} pull requests and skein read {read} of them \
                     — the rest are missing from this queue and from its count"
                ),
                None => format!(
                    "the `{search}` query filled every page skein followed ({read} pull requests) \
                     and GitHub says there are more — they are missing from this queue"
                ),
            });
        }
        answered &= found.whole;
        for item in found.items {
            let Some(number) = item.number else {
                continue;
            };
            if let Some(existing) = prs.iter_mut().find(|p| p.number == number) {
                if !existing.reasons.contains(reason) {
                    existing.reasons.push(reason.clone());
                }
                continue;
            }
            // A rollup whose contexts were cut off AND whose verdict GitHub did not give is the one
            // case [`rollup`] cannot answer from either source, so it says "pending" — and this is
            // the sentence that stops that reading as "CI is still running" (SKEIN-232). Said only
            // then: where GitHub gave its `state`, the cap costs the row a NAME and nothing else,
            // and a blind spot for every matrix build would be noise over a verdict that is right.
            if truncated_rollup(&item) && rollup_state_missing(&item) {
                blind_spots.push(format!(
                    "#{number}'s checks: GitHub listed {} contexts, skein read {}, and no rollup \
                     verdict came with them — so its checks read `pending` rather than a colour \
                     nothing here can stand behind",
                    rollup_total(&item).unwrap_or_default(),
                    contexts(&item).len(),
                ));
            }
            let pr = build_pr(
                &item,
                number,
                &login,
                reason,
                &archived_numbers,
                &snoozed_shas,
            );
            // A label list cut off at [`LABELS_FETCHED`] (SKEIN-373). Said out loud for the same
            // reason the truncated rollup above is: the row would otherwise show a set of labels
            // that looks like the whole set, and `workflow::Cond::NoLabel` would answer a question
            // about a label skein never received. The sentence names what it costs, because the
            // cost is not a missing chip — it is a merge train that will not act, on purpose,
            // rather than acting on a hole.
            if !pr.labels_whole() {
                blind_spots.push(format!(
                    "#{number}'s labels: GitHub says it has {}, skein read {} — so no workflow \
                     condition of the form `no-label:` holds on this pull request, and a label \
                     past the {LABELS_FETCHED}th is not on its row",
                    pr.labels_total.unwrap_or_default(),
                    pr.labels.len(),
                ));
            }
            // A review list cut off at [`REVIEWS_FETCHED`] (SKEIN-386). The third truncation said
            // out loud here, and the one whose cost is hardest to see from the row: unlike the
            // labels above, nothing acts WRONGLY on it — every reader of these two connections errs
            // towards holding, so a review skein never saw cannot promote a lane or authorise a
            // merge. What it takes away is the reading of the row. "none" and a standing-approval
            // count of nought are what a pull request nobody has looked at shows, and without this
            // sentence they are also what a pull request thirty-one people reviewed shows.
            if !pr.reviews_whole() {
                blind_spots.push(format!(
                    "#{number}'s reviews: GitHub counted {} and skein read {} — so this row's \
                     `my review` and its count of standing approvals are floors rather than \
                     answers, and a reviewer past the {REVIEWS_FETCHED}th is invisible to skein \
                     and to any workflow reading it",
                    pr.reviews_total.unwrap_or_default(),
                    pr.reviews_read.unwrap_or_default(),
                ));
            }
            prs.push(pr);
        }
    }

    newest_first(&mut prs);

    // An archived PR that is no longer open cannot be in this list, so its entry is dead weight.
    // Pruning is safe in the direction that matters: if a PR is ever reopened it comes back
    // unarchived, which is *more* of your attention, not less.
    //
    // Safe in that direction only once `answered` holds. A refresh that went dark has an empty
    // list too, and reading it as "nothing is open" rewrote both files to nothing — fleet-wide,
    // because the badge poll runs this for every repo every three minutes, so one rate-limit
    // window erased every set-aside and every snooze there was (SKEIN-229). A queue that
    // genuinely has nothing open still prunes: it answered.
    let open: Vec<u64> = prs.iter().map(|p| p.number).collect();
    if answered {
        prune_archived(&repo.id, &open, &archived_numbers);
    }

    // A snooze ends itself. An entry stops matching the moment the PR closes or its head moves,
    // and from then on it is dead weight that could only ever do harm — a branch reverted to the
    // old sha would re-hide a row nobody asked to hide. Kept only while the sha still names an
    // open PR's current head. Same safety direction as the archive prune above: this can only
    // ever DROP a hold, which returns a row, which is more of your attention rather than less.
    let live = |n: &u64, sha: &str| prs.iter().any(|p| p.number == *n && p.head_sha == sha);
    if answered {
        prune_snoozed(&repo.id, &snoozed_shas, live);
    }

    // **The mirror is caught up with the queue that was just fetched** (SKEIN-430).
    //
    // Everything skein reads ABOUT a pull request that is not the diff comes out of the mirror —
    // CODEOWNERS through `repos::Tree::open_telling`, and whether a module note is still current
    // through `moduledocs::is_fresh`, which compares a note's recorded sha against the sha the
    // mirror holds. Nothing on the reading path fetched one: `repos::fetch_mirror` is called by
    // `skein pull` and by starting a box, and by nothing else. So a fleet that reads pull requests
    // without doing either was answering from whenever it last did.
    //
    // Nothing failed, which is why it survived. A stale mirror does not refuse — it attributes
    // ownership from a CODEOWNERS that has since changed, and reports a note as fresh because the
    // module's sha has not moved *there*. Found on the rig: the mirror was sixteen hours old and
    // did not carry the head branch of the pull request being read.
    //
    // **Here, because this is the one moment skein knows what it is behind on.** GitHub has just
    // said which heads exist; anywhere else would be guessing at an interval. Only on a genuinely
    // fresh queue — an answer served from the cache pays nothing, so the cost is one fetch per real
    // refresh rather than one per caller.
    //
    // Best-effort and last: a mirror that could not be fetched is a mirror that is still there, and
    // the queue is what the caller asked for. `fetch_mirror` is cheap now that mirrors are packed
    // (SKEIN-406) — measured at 88MB before packing and 6MB after.
    catch_the_mirror_up(repo);

    // Looked up during the refresh, so an answer served from the cache never pays for it — and
    // the lookup itself is remembered per process besides.
    let trunk = trunk_of(&slug);
    let q = Queue {
        repo_id: repo.id.clone(),
        slug,
        trunk,
        viewer: login,
        ai: crate::review::summaries_enabled(),
        prs,
        blind_spots,
        as_of: chrono::Utc::now().to_rfc3339(),
        fresh: true,
        whole: answered,
    };
    // **A queue that could not be fully asked never replaces one that was** (SKEIN-447).
    //
    // `answered` is false when a membership search was refused — a rate limit, a 502, a timeout —
    // and the pull requests behind that search are simply absent rather than known to be gone. The
    // partial answer used to be cached anyway, in memory and on disk, so one refused search
    // replaced a good queue with a shorter one and, when every search was refused, with an EMPTY
    // one. The page then drew "Nothing is waiting on you. Nothing in any repo skein watches needs
    // your review." Caught on a live cockpit 2026-08-27: twelve pull requests at 08:12,
    // five of them in `needs-you`; at 08:21 the search was refused and the same endpoint answered
    // `prs: []`, and it stayed that way.
    //
    // The rule is already written down eight lines from here, in the client's own `.catch()`
    // (`src/web/index.html`): "An old queue is worth vastly more than an empty one, and the failure
    // replacing it threw away rows that were there a second ago." That door only covered a request
    // that FAILED; this is the one that succeeds and answers less, which looks identical to a queue
    // that is genuinely clear and is the more dangerous of the two.
    //
    // Served, never stored: the caller still gets what GitHub did say, with `whole: false` on it so
    // a reader can tell. What it must not do is become the remembered answer.
    if queues_are_cached() {
        QUEUE_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(repo.id.clone(), (Instant::now(), q.clone()));
        // **To disk only when the answer was WHOLE** (SKEIN-447).
        //
        // `answered` is false when a membership search was refused, so the pull requests behind it
        // are ABSENT rather than known to be gone. Serving that is deliberate and the tests around
        // this one say so — `whole: false` and `blind_spots` are how the reader is told. What it
        // must not do is become the REMEMBERED queue, because that one outlives the process: a
        // refused refresh was being written over the good copy on disk, so a restart came back with
        // a short queue, or an empty one, as though that were the answer. Seen on a live
        // cockpit 2026-08-27: twelve pull requests at 08:12, five in `needs-you`; at 08:21 the
        // search was refused and the same endpoint answered `prs: []`.
        //
        // The in-process insert above stays unconditional, and that is not an oversight: it is the
        // single-flight every pane shares, and skipping it sends all of them back to GitHub —
        // stampeding the API that just refused. `an_expired_repo_refreshes_once_no_matter_how_many_panes_ask`
        // caught exactly that when this guard was first written one line too wide.
        if answered {
            remember(&q);
        }
    }
    Ok(q)
}

/// Does [`queue_within`] use the caches it is written around?
///
/// Always, except in this crate's own unit tests, where it is off unless a test has asked for it
/// with [`CachedQueues`]. Note the narrowness: `cfg(test)` is set only when the library is
/// compiled as its own test binary, so every integration test in `tests/` already runs against
/// the real thing.
///
/// **Why it is off by default** — the cache is a process-global `static` and unit tests share one
/// process, so a queue built by one test would be served to another under the same repo id, and
/// a test that never mentions caching would fail because of one that does.
///
/// **Why it can be switched on** (SKEIN-314). Off unconditionally, no test could put a queue that
/// is OLD in front of a caller, so three stated defences had nothing that could fail on them: the
/// `invalidate` after a workflow acts (`crate::prwork::sweep`), the 60s-vs-600s split between the
/// pane and the badge poll, and anything else that turns on a queue being stale rather than
/// absent. Each was pinned by reading the source instead — which catches a call being deleted and
/// nothing about whether it works.
fn queues_are_cached() -> bool {
    #[cfg(not(test))]
    {
        true
    }
    #[cfg(test)]
    {
        CACHE_UNDER_TEST.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Whether this crate's unit tests have asked for the queue cache. See [`CachedQueues`].
#[cfg(test)]
static CACHE_UNDER_TEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Test-only: [`queue_within`] caches exactly as it does in production, for the life of this
/// guard, and a queue can be planted in the cache already old (SKEIN-314).
///
/// **A guard rather than a pair of functions**, because the thing being switched on is
/// process-global: it has to go off again on the way out, including out of a panicking test, or
/// every test that runs afterwards in this process inherits a cache it never asked for. `Drop`
/// empties [`QUEUE_CACHE`] as well as clearing the flag, so nothing this test planted can be
/// served to the next one.
///
/// **Take [`crate::testutil::env_lock`] first.** It is this crate's serialization for
/// process-global state, which is what this is, and every test that seeds a queue is setting
/// `$SKEIN_HOME` anyway.
///
/// Seeding is a method rather than a free function so it cannot be called without holding the
/// guard — planting an entry while the cache is off would be a fixture nothing reads, which is
/// the failure this whole seam exists to stop being possible.
#[cfg(test)]
pub(crate) struct CachedQueues(());

#[cfg(test)]
impl CachedQueues {
    /// Switch the cache on until this value is dropped.
    pub(crate) fn live() -> Self {
        CACHE_UNDER_TEST.store(true, std::sync::atomic::Ordering::SeqCst);
        Self(())
    }

    /// Put `q` in the cache stamped `age` ago — the one thing a test cannot otherwise do, since
    /// an `Instant` cannot be set and a real test cannot wait ten minutes.
    pub(crate) fn stamped(&self, repo_id: &str, age: Duration, q: &Queue) {
        let at = Instant::now()
            .checked_sub(age)
            .expect("this process has not been up long enough to stamp a queue that far back");
        QUEUE_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(repo_id.to_string(), (at, q.clone()));
    }
}

#[cfg(test)]
impl Drop for CachedQueues {
    fn drop(&mut self) {
        CACHE_UNDER_TEST.store(false, std::sync::atomic::Ordering::SeqCst);
        *QUEUE_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// The in-process copy, if one is young enough to be worth calling fresh.
///
/// Separate from [`queue`] so a caller can ask "would this cost a network round trip" without
/// taking one. That is the whole difference between painting now and painting in four seconds.
pub fn unexpired(repo_id: &str) -> Option<Queue> {
    unexpired_within(repo_id, Duration::from_secs(60))
}

/// The TTL rule itself: the in-process copy, if it is younger than `max_age`.
///
/// Its own function rather than three lines inside [`queue_within`], because until SKEIN-314 that
/// path bypassed the cache in unit tests altogether and this was the only piece a test could hold.
/// It is still the smallest statement of the rule, and [`CachedQueues`] is how a test now asks the
/// same question of `queue_within` itself.
fn unexpired_within(repo_id: &str, max_age: Duration) -> Option<Queue> {
    let cache = QUEUE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let (at, q) = cache.as_ref()?.get(repo_id)?;
    (at.elapsed() < max_age).then(|| q.clone())
}

/// How many PRs are waiting on you, per repo — the badge's whole content.
#[derive(Debug, Clone, Serialize)]
pub struct Count {
    pub repo_id: String,
    pub needs_you: usize,
    /// Set when this repo's count could not be taken. Rendered rather than swallowed: a badge that
    /// silently shows nothing because `gh` is broken is indistinguishable from an empty queue, and
    /// that is the one thing this whole feature must never be.
    pub error: String,
    /// Set when this repo was **not asked** — the queue is switched off, or it has no GitHub remote.
    ///
    /// Not an error, and not nothing either. These repos used to be filtered out before the list was
    /// built, which made "you have no PRs waiting" and "skein never looked" the same empty badge. That
    /// is the same failure `error` exists to prevent, arriving one step earlier: a repo whose queue is
    /// quietly off looks exactly like a repo with a clean queue, and the only way to tell was to open
    /// the pane and notice the repo was missing from it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub skipped: String,
    /// PRs whose workflow has stopped and is waiting on a person — the merge train's skips. On the
    /// badge poll rather than the pane's answer, because the pane is only open when somebody is
    /// already looking: this is the row that has to reach them when they are not.
    ///
    /// **Filled by the counts route, not here.** The stops live in `prwork`'s file, and `prq`
    /// reading them would put `prq` inside the module cycle (`docs/modules.toml`); the server
    /// already stands on both modules, so the decoration is its one line. Everything `prq` builds
    /// leaves this empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopped: Vec<StoppedPr>,
    /// What this repo's queue could **not** see, in the queue's own words — carried onto the
    /// badge rather than left behind in the pane.
    ///
    /// `error` above says the count could not be taken at all. This says it WAS taken and is
    /// incomplete, which is the harder failure and the one that had no field: `prq::queue` records
    /// a blind spot and still returns `Ok`, so every blind spot it recorded arrived here as a
    /// plain integer with nothing attached. On one live fleet a token without `read:org`
    /// means the `team-review-requested:` searches are never issued at all, so the badge read 2
    /// while 11 were waiting — unmarked, with a tooltip that said nothing — and under a
    /// rate-limit hold that same zero is then served from the ten-minute cache. That is the
    /// invariant `error`'s comment states in as many words, broken one field over.
    ///
    /// Empty means the count is whole, and that is the only case a bare number may be drawn for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blind_spots: Vec<String>,
}

/// One pull request a workflow has stopped on, and why — the shape the counts poll carries to the
/// cockpit's banner row. Defined here beside [`Count`], the payload it rides in; `prwork::stops`
/// produces it, because the stops file is that module's.
#[derive(Debug, Clone, Serialize)]
pub struct StoppedPr {
    pub number: u64,
    pub why: String,
}

/// Take the count for every repo that has a review queue switched on.
///
/// Skips repos with no GitHub remote before touching the network — they cannot have PRs, so asking
/// would be a guaranteed error rather than a real one. Goes through the same per-repo cache as the
/// pane, but under a **ten-minute** budget where the pane insists on sixty seconds: the badge is
/// "a number you act on within minutes" — the UI's own words for it — so a ten-minute-old answer
/// is the right answer, and it cuts the steady-state GraphQL spend of a poll that runs every
/// three minutes per open tab by ~10x. That spend is what got a live fleet rate-limited.
pub fn counts() -> Vec<Count> {
    crate::repos::load_repos()
        .into_iter()
        .map(|repo| {
            // Why a repo was not asked, before spending anything on it. Reported rather than filtered
            // away: a repo skein never looked at is a different answer from a repo with nothing
            // waiting, and the badge could not tell them apart because only one of them was on the
            // list at all.
            let skipped = match (repo.review_queue, repo_slug(&repo)) {
                (false, _) => "review queue is switched off for this repo".to_string(),
                (true, None) => {
                    "no GitHub remote, so there are no pull requests to list".to_string()
                }
                (true, Some(_)) => String::new(),
            };
            if !skipped.is_empty() {
                return Count {
                    repo_id: repo.id,
                    needs_you: 0,
                    error: String::new(),
                    skipped,
                    stopped: Vec::new(),
                    // Nothing was looked at, so there is nothing this repo failed to see.
                    // `skipped` is the whole story for it.
                    blind_spots: Vec::new(),
                };
            }
            match queue_within(&repo, Duration::from_secs(600)) {
                Ok(q) => Count {
                    repo_id: repo.id,
                    needs_you: q.prs.iter().filter(|p| p.lane == Lane::NeedsYou).count(),
                    error: String::new(),
                    skipped: String::new(),
                    stopped: Vec::new(),
                    // The number and what it is missing travel together, or the number is a
                    // claim the queue never made (SKEIN-239).
                    blind_spots: q.blind_spots,
                },
                Err(e) => Count {
                    repo_id: repo.id,
                    needs_you: 0,
                    error: e,
                    skipped: String::new(),
                    stopped: Vec::new(),
                    // No queue was built, so there are no blind spots to report — `error` is
                    // already the strongest thing this can say.
                    blind_spots: Vec::new(),
                },
            }
        })
        .collect()
}

/// Every repo's queue, in one answer — the merged review the pane opens on (SKEIN-146).
///
/// **This costs no GitHub call the badge was not already costing.** `counts()` builds the complete
/// queue for every repo each poll and throws away everything but one integer; this returns what it
/// built. Same per-repo cache, same remembered copies — one repo answering slowly (`fresh: false`)
/// or failing does not stale or sink the others, which is why the shape is a list of queues and a
/// list of failures rather than one flattened result that could only be as good as its worst repo.
#[derive(Debug, Clone, Serialize)]
pub struct MergedQueue {
    /// The one global switch, said once — the per-queue `ai` repeats it, but the pane asks the
    /// merged answer, not a queue it may not have.
    pub ai: bool,
    pub queues: Vec<Queue>,
    /// Repos that could not be read, each with its reason. Attributed, never pooled: "a repo
    /// failed" hides exactly the information that decides whether you care.
    pub failed: Vec<Count>,
    /// Repos skein deliberately did not ask about — queue switched off, or no GitHub remote.
    /// Reported rather than omitted, same rule as `counts()`: "never looked" and "nothing waiting"
    /// must not be the same silence.
    pub skipped: Vec<Count>,
}

pub fn merged(force: bool) -> MergedQueue {
    let mut out = MergedQueue {
        ai: crate::review::summaries_enabled(),
        queues: Vec::new(),
        failed: Vec::new(),
        skipped: Vec::new(),
    };
    for repo in crate::repos::load_repos() {
        let skipped = match (repo.review_queue, repo_slug(&repo)) {
            (false, _) => "review queue is switched off for this repo".to_string(),
            (true, None) => "no GitHub remote, so there are no pull requests to list".to_string(),
            (true, Some(_)) => String::new(),
        };
        if !skipped.is_empty() {
            out.skipped.push(Count {
                stopped: Vec::new(),
                blind_spots: Vec::new(),
                repo_id: repo.id,
                needs_you: 0,
                error: String::new(),
                skipped,
            });
            continue;
        }
        // **Paint now, refresh behind — per repo**, the same rule the per-repo route has. The
        // first version of this called `queue()` cold and the pane blocked on every repo's three
        // GraphQL searches again, which is the exact regression the remembered copies exist to
        // prevent. `force` is the explicit refresh and always waits.
        if !force {
            if let Some(fresh) = unexpired(&repo.id) {
                out.queues.push(fresh);
                continue;
            }
            if let Some(old) = remembered(&repo.id) {
                // The refresh nobody is waiting for: it lands in the cache and on disk, so the
                // pane's follow-up ask (it retries a stale answer on its own) is a hit. A plain
                // thread, because this module is synchronous and the caller already runs it off
                // the async runtime.
                //
                // At most ONE per repo (SKEIN-206). The pane retries a stale answer at
                // 4s/8s/16s/…, and every retry lands here — without the guard each one spawned
                // its own refresh, so a single pane-open ran several concurrent fetches per repo,
                // four GraphQL searches each: "we aren't bombarding github right?" We were. The
                // refresh is also NOT forced: force is for a human's explicit "try again", and a
                // sibling's refresh landing first should be answered from the cache it just
                // filled, not fetched a second time.
                if let Some(running) = RefreshRunning::begin(&repo.id) {
                    let refresh = repo.clone();
                    std::thread::spawn(move || {
                        let _running = running;
                        let _ = queue(&refresh, false);
                    });
                }
                out.queues.push(old);
                continue;
            }
        }
        match queue(&repo, force) {
            Ok(q) => out.queues.push(q),
            Err(e) => out.failed.push(Count {
                stopped: Vec::new(),
                // The queues this repo's siblings DID build carry their own blind spots on the
                // `Queue` itself; a repo that built none has only its error.
                blind_spots: Vec::new(),
                repo_id: repo.id,
                needs_you: 0,
                error: e,
                skipped: String::new(),
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prq::fixtures::{batched_github, batched_repo, routing_github};

    /// **A stale mirror is a wrong answer, not a slow one** (SKEIN-430), and the queue refresh is
    /// what stops it.
    ///
    /// Everything skein says about a pull request that is not the diff comes out of the mirror:
    /// CODEOWNERS through `repos::Tree`, and whether a module note is still current through
    /// `moduledocs::is_fresh`, which compares the note's recorded sha against the sha the mirror
    /// holds. Nothing on the reading path ever fetched one — `repos::fetch_mirror` is called by
    /// `skein pull` and by starting a box, and by nothing else.
    ///
    /// The harm is silent and it points the WRONG WAY: a stale mirror does not report a note as
    /// missing, it reports it as **fresh**, because the module's commit has not moved *there*. So
    /// the reader is handed standing guidance about a file that has since changed, with nothing
    /// saying it is out of date. Found on the rig, where the mirror was sixteen hours old.
    #[test]
    fn a_module_note_is_not_called_fresh_because_the_mirror_never_heard_about_the_change() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);

        let src = home.join("origin");
        std::fs::create_dir_all(src.join("docs")).unwrap();
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
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(src.join("docs/thing.md"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "one"]);

        let repo: Repo = serde_json::from_value(serde_json::json!({
            "id": "acme",
            "source": src.to_string_lossy(),
            "source_tree": src.to_string_lossy(),
            "store": "",
        }))
        .unwrap();
        crate::repos::ensure_mirror(&repo).expect("the fixture repo is mirrored");

        // A note written against the module as it stands. Fresh, correctly.
        let at_first = crate::moduledocs::current_sha(&repo, "docs/thing.md");
        assert!(!at_first.is_empty(), "the fixture's module has no commit");

        // The module changes upstream. The mirror has not been told.
        std::fs::write(
            src.join("docs/thing.md"),
            "two — this is a different module now\n",
        )
        .unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "two"]);
        assert_eq!(
            crate::moduledocs::current_sha(&repo, "docs/thing.md"),
            at_first,
            "the fixture did not actually leave the mirror behind, so this proves nothing"
        );

        // What the queue refresh does. Afterwards the mirror knows, so a note written against the
        // OLD sha is correctly no longer current.
        super::catch_the_mirror_up(&repo);
        let now = crate::moduledocs::current_sha(&repo, "docs/thing.md");
        assert_ne!(
            now, at_first,
            "the mirror was not caught up when the queue was fetched, so a note written against \
             the old commit still reads as fresh and the reader is handed standing guidance about \
             a file that has since changed — with nothing saying so"
        );

        // And it is gated: a second call inside the window must not fetch again, or every queue
        // poll becomes a network round trip per repo per tab.
        std::fs::write(src.join("docs/thing.md"), "three\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "three"]);
        super::catch_the_mirror_up(&repo);
        assert_eq!(
            crate::moduledocs::current_sha(&repo, "docs/thing.md"),
            now,
            "the mirror was fetched twice inside the freshness window — a queue refreshes far more \
             often than a repository changes, and a fetch per poll per tab is the spend that got \
             this fleet rate-limited once already"
        );

        // **And it is WIRED IN.** Everything above proves the helper works; none of it proves the
        // queue calls it, and a fix nothing reaches is not a fix. Asserted on the source in the
        // idiom this file already uses for wiring (see the counts test below), because driving a
        // fresh queue needs a GitHub stub and what would break here is the call going missing, not
        // the stub.
        //
        // Between the fresh-build section and the `Queue {` it produces: that is the one path that
        // has just heard from GitHub, and putting it anywhere else would be guessing at an interval
        // instead of acting on what was learned.
        let source =
            std::fs::read_to_string("src/prq/refresh.rs").expect("read this module's own source");
        let fresh_build = source
            .split_once("let trunk = trunk_of(&slug);")
            .map(|(before, _)| before)
            .unwrap_or_default();
        let last_call = fresh_build.rfind("catch_the_mirror_up(repo);").expect(
            "the fresh-queue path does not catch the mirror up, so nothing on the reading \
                 path fetches one and skein answers about whichever commit it last happened to \
                 hold — the state this item was filed for",
        );
        assert!(
            last_call > fresh_build.rfind("if queues_are_cached()").unwrap_or(0)
                || fresh_build.matches("catch_the_mirror_up(repo);").count() >= 1,
            "the mirror is caught up somewhere other than the fresh-queue path"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A queue inside the budget is served from memory; one outside it is fetched again**
    /// (SKEIN-314).
    ///
    /// The rule asserted where every caller meets it — [`queue_within`] — rather than on the
    /// helper underneath. That distinction is the whole item: the cache was skipped outright in
    /// this crate's unit tests, so nothing could put a queue that is OLD in front of a caller, and
    /// the 60s-vs-600s split between the pane and the badge poll had no test that could fail on
    /// it. [`CachedQueues`] is the seam; this is the first thing it makes sayable.
    ///
    /// Measured on the wire and not on the answer, because "did it refetch" is a request to
    /// GitHub. The planted queue carries a pull request the fake never serves, so the two cases
    /// are also told apart by what comes back: the seeded row on a hit, the fake's on a miss.
    #[test]
    fn a_queue_inside_the_budget_is_served_and_one_outside_it_is_refetched() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, asked) = routing_github();
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "demo",
            "source": "https://github.com/acme/new-name.git",
            "work": "",
            "store": "",
            "review_queue": true,
        }))
        .unwrap();
        crate::repos::save_repos(std::slice::from_ref(&repo)).unwrap();

        let cache = CachedQueues::live();
        // Through serde, so fields this test has no opinion about keep their real defaults.
        let planted: Queue = serde_json::from_value(serde_json::json!({
            "repo_id": "demo",
            "slug": "acme/new-name",
            "viewer": "me",
            "ai": false,
            "prs": [{
                "number": 4242, "title": "planted", "author": "someone", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": "main",
                "draft": false, "updated_at": "2026-08-01T00:00:00Z", "committed_at": "",
                "checks": "none", "my_review": "none", "review_is_current": false,
                "reasons": ["reviewer"], "lane": "needs-you", "box_name": "b"
            }],
            "blind_spots": [],
        }))
        .unwrap();
        let graphqls = || {
            asked
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.starts_with("/graphql"))
                .count()
        };

        // Ninety seconds old: inside the badge's ten minutes, outside the pane's sixty seconds.
        cache.stamped("demo", Duration::from_secs(90), &planted);
        let badge = queue_within(&repo, Duration::from_secs(600)).expect("the badge's read");
        assert_eq!(
            graphqls(),
            0,
            "a ninety-second-old queue cost a GitHub round trip on the ten-minute budget — that \
             spend is per repo, per open tab, every three minutes: {:?}",
            asked.lock().unwrap()
        );
        assert_eq!(
            badge.prs.first().map(|p| p.number),
            Some(4242),
            "the badge was served something other than the queue that was in the cache"
        );

        // The same entry, the same moment, read on the pane's budget: too old, so it is refetched.
        let pane = queue_within(&repo, Duration::from_secs(60)).expect("the pane's read");
        assert_eq!(
            graphqls(),
            1,
            "the pane's sixty seconds served a ninety-second-old answer as fresh: {:?}",
            asked.lock().unwrap()
        );
        assert!(
            pane.prs.iter().all(|p| p.number != 4242),
            "the refetched queue still carries the planted row, so nothing was actually refetched"
        );

        // And the refetch replaced the entry, so the badge's next read is inside the budget again
        // — the write half of the cache, which was skipped in tests along with the read half.
        let after = queue_within(&repo, Duration::from_secs(600)).expect("the badge again");
        assert_eq!(
            graphqls(),
            1,
            "a queue built one line ago was not put in the cache: {:?}",
            asked.lock().unwrap()
        );
        assert!(after.prs.iter().all(|p| p.number != 4242));

        // Dropping the guard puts the process back as it was for every test that runs after this
        // one: the cache off, and nothing this test planted left in it.
        drop(cache);
        assert!(
            unexpired_within("demo", Duration::from_secs(600)).is_none(),
            "the guard left this test's queue in a process-global cache"
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
        forget_trunks();
    }

    /// The TTL rule the badge rides on: a remembered in-process queue is served only while it is
    /// younger than the caller's age budget.
    ///
    /// Asserted on `unexpired_within` directly: the rule itself, in isolation, with no GitHub and
    /// no repo. `a_queue_inside_the_budget_is_served_and_one_outside_it_is_refetched` asks the
    /// same question of [`queue_within`], which is the function every caller actually reaches.
    #[test]
    fn an_in_process_queue_is_served_only_within_the_callers_age_budget() {
        // The cache is a process-wide static; the env lock is this file's serialization for those.
        let _g = crate::testutil::env_lock();
        // Through serde like the other fixtures here, so fields this test does not care about keep
        // their real defaults.
        let remembered: Queue = serde_json::from_value(serde_json::json!({
            "repo_id": "ttl-probe",
            "slug": "acme/ttl",
            "viewer": "me",
            "ai": false,
            "prs": [],
            "blind_spots": [],
        }))
        .unwrap();
        let stamp = |age: Duration| {
            let at = Instant::now()
                .checked_sub(age)
                .expect("this host has been up longer than eleven minutes");
            QUEUE_CACHE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert_with(HashMap::new)
                .insert("ttl-probe".to_string(), (at, remembered.clone()));
        };

        // Nine minutes old: young enough for the badge's ten minutes, far too old for the pane.
        stamp(Duration::from_secs(9 * 60));
        assert!(
            unexpired_within("ttl-probe", Duration::from_secs(600)).is_some(),
            "a nine-minute-old queue is within a ten-minute budget and was going to be refetched"
        );
        assert!(
            unexpired_within("ttl-probe", Duration::from_secs(60)).is_none(),
            "the pane's sixty seconds served a nine-minute-old answer as fresh"
        );

        // Eleven minutes old: past even the badge's budget.
        stamp(Duration::from_secs(11 * 60));
        assert!(
            unexpired_within("ttl-probe", Duration::from_secs(600)).is_none(),
            "an eleven-minute-old queue outlived the ten-minute budget"
        );

        invalidate("ttl-probe");
    }

    /// The badge reads through the ten-minute budget, not the pane's sixty seconds.
    ///
    /// Asserted against the source, the way `nothing_here_shells_out_to_gh` is. What this pins is
    /// the call itself — pointing `counts` back at `queue(&repo, false)` is the regression that
    /// rebuilt every repo's queue through a 60s cache every three minutes per open tab, and it is
    /// exactly what this fails on. That the two budgets then behave differently is no longer taken
    /// on trust either: since SKEIN-314 it is measured against `queue_within` in
    /// `a_queue_inside_the_budget_is_served_and_one_outside_it_is_refetched`.
    #[test]
    fn the_badge_reads_through_a_ten_minute_budget() {
        let source = std::fs::read_to_string(file!()).expect("this file");
        let counts = source
            .split("pub fn counts()")
            .nth(1)
            .and_then(|after| after.split("\npub fn ").next())
            .expect("counts() is in this file");
        assert!(
            counts.contains("queue_within(&repo, Duration::from_secs(600))"),
            "counts() no longer reads through the ten-minute budget:\n{counts}"
        );
        assert!(
            !counts.contains("queue(&repo,"),
            "counts() went back to the sixty-second path the badge was rate-limited on:\n{counts}"
        );
    }

    /// A `Repo` through serde, so fields this test does not care about keep their real defaults.
    fn repo_at(id: &str, work: &std::path::Path, queue_on: bool) -> Repo {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "source": work.to_string_lossy(),
            "work": work.to_string_lossy(),
            "store": "",
            "review_queue": queue_on,
        }))
        .unwrap()
    }

    /// A repo skein did not look at must say so, rather than vanishing from the list.
    ///
    /// This is the bug the whole `skipped` field exists for: both states below produced a count list
    /// that simply did not mention the repo, so the badge showed nothing — identical to a queue with
    /// nothing waiting in it. Someone whose only repo had its queue switched off, or whose clone had
    /// no GitHub remote, saw a clean board while PRs piled up on GitHub, with nowhere to find out why.
    #[test]
    fn a_repo_that_was_never_asked_says_so_instead_of_disappearing() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let root = home.as_ref() as &std::path::Path;
        // SAFETY: guarded by the crate-wide env lock, as every $SKEIN_HOME test is.
        unsafe { std::env::set_var("SKEIN_HOME", root) };

        // A git repo with a GitHub origin, so only the *switch* is what stops it being asked.
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&work)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "git@github.com:acme/thing.git"]);

        // And one with no remote at all, which cannot have pull requests.
        let bare = root.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&bare)
            .output()
            .unwrap();

        crate::repos::save_repos(&[
            repo_at("queue-off", &work, false),
            repo_at("no-remote", &bare, true),
        ])
        .unwrap();

        let counts = counts();
        let of = |id: &str| {
            counts
                .iter()
                .find(|c| c.repo_id == id)
                .unwrap_or_else(|| panic!("{id} is missing from the counts entirely: {counts:?}"))
        };
        assert!(
            of("queue-off").skipped.contains("switched off"),
            "a repo with the queue off must name that, not read as an empty queue: {:?}",
            of("queue-off")
        );
        assert!(
            of("no-remote").skipped.contains("no GitHub remote"),
            "and a repo with nowhere to look must say which: {:?}",
            of("no-remote")
        );
        // Neither is a *fault* — the badge paints `error` red, and being switched off is a choice.
        for id in ["queue-off", "no-remote"] {
            assert!(of(id).error.is_empty(), "{id} is not broken");
            assert_eq!(of(id).needs_you, 0);
        }
        // Nothing reached the network: `gh` is never invoked for a repo that was not asked, which is
        // what makes reporting them free rather than three round trips each.
        unsafe { std::env::remove_var("SKEIN_HOME") };
    }

    /// The badge's number and what the queue could not see travel together (SKEIN-239).
    ///
    /// Both halves of this test report `needs_you: 0` with no `error` and nothing `skipped`. The
    /// ONLY thing telling "nothing is waiting on you" apart from "skein could not look at the
    /// searches where something might be waiting" is the blind spots — and `counts()` used to drop
    /// them on the floor, so the two were the same integer. On one live fleet the second half
    /// is the everyday state: no `read:org`, so the `team-review-requested:` searches are never
    /// issued and every team-requested PR is absent from the count with nothing saying so.
    #[test]
    fn a_count_carries_what_its_queue_could_not_see() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::repos::save_repos(&[batched_repo("acme/thing")]).unwrap();

        // Teams listable, every search answering: a genuinely empty queue. Five aliases, because
        // the team the viewer belongs to adds its own.
        let (base, _seen) = batched_github(
            true,
            200,
            r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]},"q4":{"nodes":[]}}}"#
                .to_string(),
        );
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let whole = counts();
        let whole = whole
            .iter()
            .find(|c| c.repo_id == "thing")
            .expect("counted");
        assert_eq!(whole.needs_you, 0);
        assert!(
            whole.blind_spots.is_empty(),
            "a queue that saw everything and found nothing must carry NO blind spot, or the badge              can never draw a plain zero: {:?}",
            whole.blind_spots
        );

        // Same repo, same empty answers — but the token cannot list teams, so a whole class of
        // pull request was never searched for.
        let (base, _seen) = batched_github(
            false,
            200,
            r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                .to_string(),
        );
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let blind = counts();
        let blind = blind
            .iter()
            .find(|c| c.repo_id == "thing")
            .expect("counted");
        assert_eq!(
            blind.needs_you, 0,
            "the count is still zero — that is the whole problem, and why the zero needs company"
        );
        assert!(
            blind.error.is_empty() && blind.skipped.is_empty(),
            "neither existing field fires here, which is how this stayed invisible: {blind:?}"
        );
        assert!(
            blind
                .blind_spots
                .iter()
                .any(|b| b.contains("team review requests")),
            "the count must say it could not see team review requests: {:?}",
            blind.blind_spots
        );

        // The wire contract the badge reads, pinned by name: the cockpit renders from this JSON,
        // so a renamed field is a badge that silently goes back to a bare number.
        let on_the_wire = serde_json::to_value(blind).unwrap();
        assert!(
            on_the_wire["blind_spots"]
                .as_array()
                .is_some_and(|b| !b.is_empty()),
            "blind_spots must reach the client under that name: {on_the_wire}"
        );
        assert!(
            serde_json::to_value(whole)
                .unwrap()
                .get("blind_spots")
                .is_none(),
            "and a whole count must not carry an empty array — absent is what lets the page tell \
             `nothing missing` from `this skein is too old to say`"
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
    }

    /// The rate-limited half, which is the one that actually happened: GitHub answers 200 carrying
    /// `RATE_LIMITED`, so the whole batched request fails, `queue_within` still returns `Ok` with
    /// an empty list, and the badge showed a confident zero — then served it from the ten-minute
    /// cache for the next ten minutes.
    #[test]
    fn a_rate_limited_count_is_not_reported_as_an_empty_queue() {
        let _lock = crate::testutil::env_lock();
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::repos::save_repos(&[batched_repo("acme/thing")]).unwrap();

        let (base, _seen) = batched_github(
            false,
            200,
            r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded for user ID 123"}]}"#
                .to_string(),
        );
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let counted = counts();
        let counted = counted
            .iter()
            .find(|c| c.repo_id == "thing")
            .expect("counted");
        assert_eq!(counted.needs_you, 0);
        assert!(
            counted.error.is_empty(),
            "the queue returned Ok, so `error` is empty — the field that was supposed to catch              this never fires: {counted:?}"
        );
        assert!(
            counted
                .blind_spots
                .iter()
                .any(|b| b.contains("GitHub did not answer for acme/thing")),
            "a zero standing on a refresh that answered nothing must say so: {:?}",
            counted.blind_spots
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
    }

    /// **A set-aside pull request that only a team review request would list survives a refresh
    /// made without `read:org`** (SKEIN-262).
    ///
    /// SKEIN-229 gated both prunes on `answered` — every membership search skein RAN answered in
    /// full. A team search that was never RUN is a different hole and was still open: without
    /// `read:org` the `for team in &teams` loop adds no search at all, the four personal rules all
    /// answer, `answered` stays true, and the prune deletes your archive entry and snooze
    /// on the evidence of an open set that structurally could not contain the row. Silent, every
    /// three minutes on the badge poll, and permanent on a fleet whose token lacks the scope.
    ///
    /// Both halves, because either alone passes for the wrong reason: with the scope missing the
    /// decisions survive, and with the scope present the very same refresh prunes them. The second
    /// is what makes the first mean "skein declined to prune" rather than "the fixture could not
    /// refresh".
    #[test]
    fn a_set_aside_pr_no_search_could_have_listed_survives_a_refresh_without_read_org() {
        let _g = crate::testutil::env_lock();
        // Right after the env lock, per `github::HoldClear`'s own rule: a test elsewhere in this
        // binary can engage the rate-limit hold, and a held hold refuses every request before it
        // reaches the fixture — which reads here as a request that was never sent.
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");

        // #77's only claim on you is a team review request, so none of the four personal searches
        // will ever return it — which is exactly what makes its absence no evidence at all.
        set_archived("team-blind", 77, true).expect("archived");
        set_snoozed("team-blind", 77, Some("sha77")).expect("snoozed");

        let four_empty =
            r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#;
        let (base, _seen) = batched_github(false, 200, four_empty.to_string());
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let blind = queue(&batched_repo("acme/team-blind"), true).expect("the queue answered");
        assert!(
            blind
                .blind_spots
                .iter()
                .any(|b| b.contains("team review requests are missing")),
            "the fixture is not the one this test is about: {:?}",
            blind.blind_spots
        );
        assert!(
            archived("team-blind").contains(&77),
            "a set-aside pull request was deleted because a search that could not be RUN did not \
             list it — the same erasure SKEIN-229 fixed for a search that ran and failed"
        );
        assert!(
            snoozed("team-blind").contains_key(&77),
            "and the snooze on the same pull request went with it"
        );
        assert!(
            !blind.whole,
            "a queue that never asked about team review requests told everything downstream it \
             had seen every open pull request"
        );

        // The same refresh, with a token that CAN list teams. Every rule that exists was asked,
        // every one answered, nothing came back — so #77 really is closed and both files are
        // pruned. Without this half, deleting the prune entirely would pass the test above.
        let five_empty = r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]},"q4":{"nodes":[]}}}"#;
        let (base, _seen) = batched_github(true, 200, five_empty.to_string());
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let seeing = queue(&batched_repo("acme/team-blind"), true).expect("the queue answered");
        assert!(
            seeing.whole && seeing.blind_spots.is_empty(),
            "a refresh that asked every rule there is reported a hole: {:?}",
            seeing.blind_spots
        );
        assert!(
            archived("team-blind").is_empty() && snoozed("team-blind").is_empty(),
            "an answered refresh stopped pruning — a queue that says it saw everything must still \
             clear decisions about pull requests that are gone"
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
    }

    /// `review::prune` reads this queue's list, and it may only do so when the list is whole.
    ///
    /// Asserted against the source, the way [`the_badge_reads_through_a_ten_minute_budget`] is and
    /// for the same reason: the call lives in the server binary, behind an HTTP handler and a
    /// detached task, so no test in this module can reach it — and the thing that goes wrong is
    /// the guard being dropped, which a runtime test of the pruning itself would never notice.
    ///
    /// What it costs when it is dropped: every pull request past a truncated search's page is
    /// absent from `open`, so its summary takes the "closed and merged is asked" road and pays a
    /// `pr_is_open` REST call — per file, per tab open, for ever, for pull requests that are alive
    /// and merely past the hundredth (SKEIN-231).
    #[test]
    fn the_only_other_reader_of_this_list_stands_down_when_it_is_partial() {
        let server = std::fs::read_to_string("src/bin/skein-server.rs").expect("the server");
        assert_eq!(
            server.matches("review::prune(").count(),
            1,
            "there is more than one caller now, and only one of them is pinned here"
        );
        let guarded = server
            .split("review::prune(")
            .next()
            .expect("the source before the call");
        // The PROPERTY, not one spelling of it: somewhere before the call, the queues that reach
        // it are filtered on `whole`. Pinned this way round because the exact expression has
        // already moved once — it was `slug.filter(|_| queue.whole)` inline, and is now a filter
        // in the helper that builds the list — and a test that fails on a refactor which KEEPS
        // the guard teaches whoever meets it to delete the test.
        assert!(
            guarded
                .lines()
                .any(|line| line.contains("filter") && line.contains("whole")),
            "the queue's list is pruned against without asking whether it saw everything — a \
             search cut off at its page makes every pull request past the hundredth absent for a \
             reason that has nothing to do with it"
        );
    }

    /// **A queue that could not be fully asked never replaces one that was** (SKEIN-447).
    ///
    /// The live failure, on a live cockpit 2026-08-27: twelve pull requests at 08:12, five of
    /// them in `needs-you`; at 08:21 a membership search was refused, the same endpoint answered
    /// `prs: []`, and the pane drew "Nothing is waiting on you." The partial answer had been cached
    /// over the good one, in memory and on disk, so it stayed that way.
    ///
    /// Asserted on the guard itself rather than by driving a refusal, and that limit is real: the
    /// refusal has to come from GitHub, and the stub that could produce one would be asserting its
    /// own shape. What this holds is the one condition whose loss brings the whole symptom back.
    #[test]
    fn a_partly_answered_queue_is_served_but_never_remembered() {
        let me = include_str!("refresh.rs");
        let at = me
            .find("fn queue_within")
            .expect("queue_within has been renamed; this test can no longer see it");
        let body = &me[at..];
        let body = &body[..body
            .find("\n}\n")
            .expect("queue_within has no end, so this is not reading a function body")];
        assert!(
            body.contains("if answered {\n            remember(&q);"),
            "an incomplete queue is being remembered again — one refused search will replace a \
             good queue with a shorter one, and a fully refused refresh with an empty one that \
             reads as \"nothing is waiting on you\""
        );
        // And the in-process cache must stay UNCONDITIONAL. Skipping it for a partial answer
        // sends every pane back to GitHub, which is what earns the refusal in the first place —
        // `an_expired_repo_refreshes_once_no_matter_how_many_panes_ask` caught exactly that when
        // this guard was first written one line too wide.
        assert!(
            body.contains("if queues_are_cached() {\n        QUEUE_CACHE"),
            "the in-process cache is now conditional, so N panes each re-fetch a refused refresh"
        );
        // And it must still be SERVED: what GitHub did say is worth having, with `whole: false` on
        // it so a reader can tell. A guard that refused the whole refresh would trade one silent
        // wrong answer for a louder one.
        assert!(
            body.contains("whole: answered"),
            "the caller can no longer tell a complete answer from a partial one"
        );
    }
}
