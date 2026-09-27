//! How a refresh asks GitHub for pull requests, and what it does when GitHub will not answer.
//!
//! One GraphQL request carries every membership rule, [`PR_FRAGMENT`] travelling once per pull
//! request. That request is the expensive thing skein does, so the two answers to a repo too big
//! for it are here as well: split the batch in half and ask again, and remember the width that
//! worked.

use super::node::PrNode;
use super::*;

/// How many review threads one pull request contributes to the batched answer.
///
/// A cap on a list that has no natural end, and it is the SIZE of this request that sets it, not
/// taste: [`PR_FRAGMENT`] is asked for up to [`SEARCH_PAGE`] pull requests per membership rule, so
/// every thread here is multiplied by a hundred. Twenty is more open threads than a reviewable pull
/// request has, and [`Pr::review_threads_total`] carries GitHub's own count beside them so a list
/// that IS short says so rather than reading as "nothing open".
///
/// Threads are cheap only because they carry no bodies — see [`ReviewThread`]. Ten, not twenty:
/// the measurement in
/// `the_conversation_is_measured_against_the_answer_it_grew_from` is what set it.
///
/// **The LAST ten, not the first ten** (SKEIN-340), which is the same rule
/// [`PR_COMMENTS_FETCHED`] states one doc comment down and for a sharper reason. The query asked
/// `reviewThreads(first: 10)` while [`Pr::review_threads`] four hundred lines up said "newest ten
/// of them" — two opposite paging directions four lines apart in [`PR_FRAGMENT`], since the
/// neighbouring `comments(last: …)` was already reading its connection from the end. It is not a
/// schema limitation: `PullRequestReviewThreads` is an ordinary Relay connection and takes `last:`.
///
/// The bias this removes is not symmetric, which is why it is a bug and not a preference. A pull
/// request goes through two rounds: twelve threads opened and resolved in the first, four still
/// open in the second. `first: 10` fetches ten resolved threads, `cockpit/src/move.mjs`'s
/// `threads()` filters them all out, `authorBlock` returns no `{kind: "threads"}`, and the page
/// tells the AUTHOR "nothing is open on the lines of this change" in the pane whose entire purpose
/// is saying what needs them. `last: 10` fetches the four that are open, because the newest threads
/// are the ones most likely to be unresolved — a resolved thread is a finished one, and finished
/// things sort old. The `unseen` caveat kept this from being silent either way; what it could not
/// do is stop the headline count and the whose-move answer from both being biased to zero in
/// exactly the case they exist for.
pub(super) const REVIEW_THREADS_FETCHED: usize = 10;

/// How many PR-level comments one pull request contributes. **These carry bodies**, so this is the
/// expensive cap and it is deliberately the smallest one.
///
/// The LAST five, not the first five: a conversation is read from its end. The item this came from
/// names the case exactly — a pull request with four hundred comments must not be the thing that
/// makes the queue slow — and without a cap that PR would ship its whole history inside a request
/// that already carries ninety-nine others. [`Pr::comments_total`] carries GitHub's own count
/// beside the five, so a conversation that was cut says how much of it is missing.
///
/// **Five, not ten (SKEIN-316).** This is the first lever that item names, and it was pulled on a
/// measurement rather than on taste:
/// `the_conversation_is_measured_against_the_answer_it_grew_from` prints the worst case the caps
/// allow — [`SEARCH_PAGE`] pull requests saturating this and [`REVIEW_THREADS_FETCHED`] — and ten
/// put it at **583,887 bytes for ONE alias**, roughly 2.9 MB for a five-alias batch, on the same
/// request `acme/thing` answers with a 504 (SKEIN-278). Five puts it at **450,827**, and
/// that test now holds a ceiling rather than only printing the number. It is this cap and not
/// [`REVIEW_THREADS_FETCHED`] because a comment node carries a body and a thread node deliberately
/// does not (SKEIN-301) — at the 120-character body that measurement uses they are already 264
/// bytes against 226, and a real comment body is several times that, so the gap this closes is
/// wider in the fleet than in the fixture.
pub(super) const PR_COMMENTS_FETCHED: usize = 5;

/// How many outstanding review requests are listed. People and teams together; a pull request
/// waiting on more than this many reviewers is not a row anybody reads a list of names off.
pub(super) const REVIEW_REQUESTS_FETCHED: usize = 20;

/// How many labels one pull request contributes (SKEIN-373).
///
/// **The number is unchanged; what changed is that hitting it is now audible.** The query asked
/// `labels(first: 20)` with no `totalCount`, so a pull request with more had the rest deleted on
/// the way in and nothing — not the row, not the blind spots, not [`Pr`] — could tell. Measured
/// against `acme/testbed#20`: GitHub's REST answer carries 22 labels, the queue
/// payload carried 20 (`area/mod-01` … `area/mod-20`). That is not cosmetic, because
/// [`Pr::labels`] is what `prwork::facts_of` turns into `workflow::Facts::labels`, and
/// `workflow::Cond::NoLabel` then read a `hold` label that sorted past the twentieth as *absent*.
///
/// **Twenty rather than GitHub's hundred, and that is a measurement rather than a taste.**
/// [`PR_FRAGMENT`] travels once per pull request for up to [`SEARCH_PAGE`] of them per membership
/// rule, in the one request `acme/thing` already answers with a 504 (SKEIN-278).
/// `the_conversation_is_measured_against_the_answer_it_grew_from` saturates this cap along with
/// the other two and holds the total under half a megabyte per alias: at twenty the worst case the
/// caps allow leaves single-digit thousands of bytes of headroom under that ceiling, so a hundred
/// would not fit and no rearrangement of the other caps makes it fit. The fix is therefore the
/// same one [`SEARCH_PAGE`] took (SKEIN-231) — ask GitHub how many there were, carry the number,
/// and say the hole out loud — and not a bigger page.
pub(super) const LABELS_FETCHED: usize = 20;

/// How many reviews one pull request contributes, on **each** of the two review connections
/// (SKEIN-386). Both are one review per author, so thirty is thirty distinct reviewers on one pull
/// request — rare, and the fleet's own repos review by area.
///
/// **The number is unchanged; what changed is that hitting it is now audible** — the same answer
/// SKEIN-373 gave the label cap, for a harder reason. The query asked `latestReviews(first: 30)`
/// with no `totalCount`, so the thirty-first reviewer's row was deleted on the way in and nothing
/// — not [`Pr::my_review`], not [`Pr::standing_approvals`], not the blind spots — could tell.
///
/// **Raising it is not available.** `the_conversation_is_measured_against_the_answer_it_grew_from`
/// holds the worst case the caps allow at 496,727 bytes for ONE alias against a 500,000-byte
/// ceiling (run it with `--nocapture`; measured 2026-08-26, after SKEIN-373 saturated the label
/// cap), on the request `acme/thing` already answers with a 504 (SKEIN-278) — and a review
/// node carries a state, a login and a commit oid, on each of two connections, for up to
/// [`SEARCH_PAGE`] pull requests. That measurement's fixture holds no review nodes at all, so the
/// sixty this cap allows per pull request are money it has not counted and the true figure is
/// ABOVE it, not below (SKEIN-398) — which only makes the case harder. So the fix is `totalCount`
/// beside the nodes, [`Pr::reviews_whole`], and a sentence a person reads, which is what
/// [`SEARCH_PAGE`] and [`LABELS_FETCHED`] both settled on before it.
pub(super) const REVIEWS_FETCHED: usize = 30;

/// Every field the queue's parser needs from one pull request — the node body every search alias
/// in [`batched_query`] shares.
///
/// GraphQL rather than REST, and not as a preference: a pull request's reviews, the commit each was
/// left against, and its check rollup are three more REST calls **per pull request**. One search
/// returns all of it for a hundred at once. Each field here is a field of [`PrNode`], and that is
/// the whole of the parse: what this asks for is what serde reads.
///
/// Built from the caps above rather than spelling them twice. A number written once in the query
/// and again in the field's doc is a number that drifts, and the thing it would drift about is how
/// much this request costs.
///
/// **The rollup asks for GitHub's own verdict as well as the contexts** (SKEIN-232). `contexts` is
/// capped at a hundred and a matrix build (`os × rust-version × feature`) reaches three digits
/// routinely, so a verdict computed only from that array reads a pull request whose 101st context
/// is red as green — and `docs/pr-workflow.md`'s merge train reads exactly that field, so the
/// failure is not a wrong dot but a merge of a pull request whose CI failed. `state` is GitHub's
/// answer over ALL of them and costs nothing to ask for; `totalCount` says how much of the list
/// this page is. Both are read by [`rollup`]; the contexts are left to NAME what failed.
///
/// **Two review connections are asked for, and the second is not a duplicate of the first**
/// (SKEIN-354). `latestReviews` is the latest review per author *whatever it said*, so a note you
/// left after approving comes back as `COMMENTED` and would demote your own approval;
/// `latestOpinionatedReviews` is the latest review per author that DECIDED something, which is the
/// one question "is my review standing right now" is asking. [`my_review_state`] reads the
/// opinionated one for the verdict and the other only to know that you commented — which is a real
/// thing to know and the opinionated connection deliberately cannot say. Nothing else in skein
/// reads either, so this is thirty extra nodes on a fragment that already carries a hundred check
/// contexts, and it buys the difference between "you decided" and "you said something".
pub(super) static PR_FRAGMENT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        r#"
fragment PrFields on PullRequest {{
  number title url isDraft updatedAt
  headRefName headRefOid baseRefName reviewDecision mergeable mergeStateStatus
  additions deletions changedFiles
  labels(first: {labels}) {{ totalCount nodes {{ name }} }}
  author {{ login }}
  latestReviews(first: {reviews}) {{ totalCount nodes {{ state author {{ login }} submittedAt commit {{ oid }} }} }}
  latestOpinionatedReviews(first: {reviews}) {{ totalCount nodes {{ state author {{ login }} submittedAt commit {{ oid }} }} }}
  reviewRequests(first: {asked}) {{ totalCount nodes {{ requestedReviewer {{
    ... on User {{ login }}
    ... on Team {{ slug organization {{ login }} }}
  }} }} }}
  reviewThreads(last: {threads}) {{ totalCount nodes {{
    id isResolved
    comments(first: 1) {{ nodes {{ author {{ login }} url }} }}
    latest: comments(last: 1) {{ nodes {{ author {{ login }} createdAt }} }}
  }} }}
  comments(last: {comments}) {{ totalCount nodes {{ author {{ login }} body createdAt url }} }}
  commits(last: 1) {{ nodes {{ commit {{ committedDate statusCheckRollup {{ state contexts(first: 100) {{ totalCount nodes {{
    ... on CheckRun {{ name detailsUrl status conclusion }}
    ... on StatusContext {{ context targetUrl state }}
  }} }} }} }} }} }}
}}"#,
        asked = REVIEW_REQUESTS_FETCHED,
        threads = REVIEW_THREADS_FETCHED,
        comments = PR_COMMENTS_FETCHED,
        labels = LABELS_FETCHED,
        reviews = REVIEWS_FETCHED,
    )
});

/// The refresh's one request: `q0..qN`, each an aliased `search` over its own membership rule,
/// every alias reading the same node body through [`PR_FRAGMENT`].
///
/// Built per refresh rather than kept as a constant because `count` moves with your teams — and
/// text is all a GraphQL POST is, so there is nothing a constant would buy.
///
/// `issueCount` and `pageInfo` are asked for beside the nodes (SKEIN-231). Both are scalars on the
/// connection — they add nothing to the answer's size, which matters here more than it looks:
/// this request is already the heaviest thing skein sends, and a live repo has answered it
/// with a 504 (SKEIN-278). They are what turns "a hundred came back" from a guess into GitHub's
/// own statement of how many there were, and give the blind spot a number to say out loud.
///
/// `endCursor` is asked for beside `hasNextPage`, and every alias takes an `after` (SKEIN-280).
/// Knowing a search was cut off is not the same as reading the rest of it, and until this the queue
/// did the first and never the second: it said "43 are missing" on every refresh, for ever. The
/// `after` variables are declared `String` rather than `String!` because the FIRST page passes
/// `null` for every one of them — `after: null` is GraphQL's "from the beginning", so the first
/// request is byte-for-byte the request it always was apart from these declarations, and a repo
/// whose searches all fit in one page still costs exactly one request.
fn batched_query(count: usize) -> String {
    use std::fmt::Write as _;
    let mut vars = String::from("$n: Int!");
    let mut body = String::new();
    for i in 0..count {
        let _ = write!(vars, ", $q{i}: String!, $a{i}: String");
        let _ = writeln!(
            body,
            "  q{i}: search(query: $q{i}, type: ISSUE, first: $n, after: $a{i}) {{ issueCount \
             pageInfo {{ hasNextPage endCursor }} nodes {{ ...PrFields }} }}"
        );
    }
    format!("query({vars}) {{\n{body}}}\n{}", *PR_FRAGMENT)
}

/// One membership search's answer: the pull requests it returned, and whether that is all of them.
///
/// The two are separate facts because they decide different things. `items` is what fills the
/// queue. `whole` is what lets the queue act on a pull request's **absence** — and the prunes in
/// [`queue_within`] delete one of your own decisions on exactly that evidence, so they may
/// only read a search that saw everything there was.
///
/// `whole` is now GitHub's answer rather than an inference: `pageInfo { hasNextPage }` says whether
/// a page is the end of the list, where "came back short of a hundred" only ever guessed it — and
/// guessed wrong, in the safe direction, on a search that matched exactly a hundred. It falls back
/// to the length test when nothing said, because an answer that predates the field is still an
/// answer. `matched` is `issueCount`: how many the search found, which is what lets the blind spot
/// in [`queue_within`] say how many pull requests are missing rather than merely that some are.
pub(super) struct Found {
    pub(super) items: Vec<PrNode>,
    pub(super) whole: bool,
    pub(super) matched: Option<u64>,
    /// Where the next page of THIS search starts, from `pageInfo { endCursor }`.
    ///
    /// The only thing that can continue a search, and it is deliberately the only thing: a page is
    /// followed when GitHub both said there is more AND handed back somewhere to carry on from.
    /// An answer that says `hasNextPage` and gives no cursor — an older fixture, a shape GitHub
    /// changes under us — stops the paging rather than guessing an offset, and `whole` stays false
    /// so the blind spot still says what could not be seen.
    pub(super) cursor: Option<String>,
}

/// How many pull requests one membership search asks GitHub for. A search that comes back with
/// exactly this many has been cut off at the page far more often than it has landed on it exactly.
///
/// **Not the lever for a truncated queue.** Raising it is the obvious fix for SKEIN-231 and the
/// wrong one: the answer is already the heaviest thing skein sends — five searches × this many
/// nodes × [`PR_FRAGMENT`] — and `acme/thing` answered that with a 504 that only
/// [`search_prs_all`]'s split-in-halves recovered (SKEIN-278). A bigger page makes the outage more
/// likely in order to make the truncation rarer, and an outage is the failure that hides MORE. So
/// the page stays where it is and the queue says what it could not see.
pub(super) const SEARCH_PAGE: usize = 100;

/// How many pages of one membership rule a refresh will follow — the first plus this many more.
///
/// A ceiling rather than "until GitHub stops", because this runs from a poll: the badge refreshes
/// every repo every few minutes, and a rule matching four thousand open pull requests would spend
/// forty requests per repo per refresh to build a review queue no person is going to read to the
/// end of. Five pages is five hundred pull requests **per rule**, which is far past any queue the
/// owner has and still a bounded worst case.
///
/// Hitting it is not silence. The last page's `whole` is false, so [`queue_within`]'s blind spot
/// says how many were matched and how many were read — the SKEIN-231 sentence, with the hole now
/// as small as this ceiling can make it.
const SEARCH_PAGES: usize = 5;

/// Every membership search of one refresh, in ONE GraphQL request — five requests per repo per
/// refresh was where nearly all of skein's quota went (SKEIN-209) — followed to the END of any
/// rule GitHub says has more (SKEIN-280).
///
/// The outer `Result` is the request: an `Err` means nothing was asked or nothing answered, and
/// the caller must report **every** search as missing. The inner ones are per search, in the order
/// given: GraphQL delivers a failed alias as `data.qN: null` plus an `errors` entry whose `path`
/// names the alias, and that mapping is what keeps each failure its own blind spot — four good
/// answers are still four good answers, exactly as they were when each search was its own request.
///
/// **Paging is the second half of SKEIN-231, not a second mechanism.** That one taught the queue to
/// say "143 matched, I read 100"; it said it again on every refresh, for ever, because nothing ever
/// asked for the other 43. Here the cut-off rules — and ONLY those — are asked again with their own
/// `endCursor`, so a repo whose searches all fit in one page still costs exactly one request, and a
/// repo with one busy rule costs one more request rather than a bigger one. That direction matters:
/// [`SEARCH_PAGE`] argues at length that a BIGGER page is the wrong lever, because the batched
/// request is already the heaviest thing skein sends and `acme/thing` answered it with a 504.
/// A follow-up page carries one alias, so it is the smallest request in the refresh, not the
/// largest.
pub(super) fn search_prs_all(
    slug: &str,
    searches: &[String],
) -> Result<Vec<Result<Found, String>>, String> {
    // The widest batch GitHub answered anywhere in THIS refresh — the first request, a half after a
    // split, a follow-up page. Accumulated across the whole refresh rather than written per request
    // because the halves of a split answer narrower than the batch they came from, and a memo that
    // believed each half in turn would ratchet a repo down to one search per request (SKEIN-278).
    let widest = std::cell::Cell::new(0usize);
    let answered = search_pages(slug, searches, &widest);
    learn_batch_width(slug, widest.get(), searches.len());
    answered
}

/// The paging itself, with the refresh's widest answered batch accumulating into `widest`.
fn search_pages(
    slug: &str,
    searches: &[String],
    widest: &std::cell::Cell<usize>,
) -> Result<Vec<Result<Found, String>>, String> {
    let mut out = one_batch(slug, searches, &vec![None; searches.len()], widest)?;
    for _ in 0..SEARCH_PAGES {
        // Which rules GitHub says it has more of AND handed a cursor back for. A `hasNextPage`
        // with no `endCursor` is not a page anyone can ask for, so it ends the paging with
        // `whole` still false rather than being guessed at.
        let more: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, found)| found.as_ref().is_ok_and(|f| !f.whole && f.cursor.is_some()))
            .map(|(i, _)| i)
            .collect();
        if more.is_empty() {
            break;
        }
        let again: Vec<String> = more.iter().map(|&i| searches[i].clone()).collect();
        let after: Vec<Option<String>> = more
            .iter()
            .map(|&i| out[i].as_ref().ok().and_then(|f| f.cursor.clone()))
            .collect();
        // A page that will not come is where this stops. Everything already read stays in the
        // queue and every unfinished rule keeps `whole: false`, so the refresh degrades into
        // exactly the answer it gave before paging existed rather than into an error.
        let Ok(pages) = one_batch(slug, &again, &after, widest) else {
            break;
        };
        for (&i, page) in more.iter().zip(pages) {
            // A page that failed on its own leaves the rule where it was: partial, and saying so.
            // Its earlier pages are real pull requests and are not thrown away over a later one.
            let Ok(page) = page else { continue };
            let Ok(sofar) = out[i].as_mut() else { continue };
            sofar.items.extend(page.items);
            sofar.whole = page.whole;
            sofar.cursor = page.cursor;
            // `matched` is GitHub's count of the whole rule and is the same on every page; the
            // first page's answer is kept so a later page that omits it cannot erase the number
            // the blind spot is built from.
            sofar.matched = sofar.matched.or(page.matched);
        }
    }
    Ok(out)
}

/// One batch of searches at one set of cursors, halved and re-asked when GitHub refuses to take
/// it whole. The paging above calls this once per page.
fn one_batch(
    slug: &str,
    searches: &[String],
    after: &[Option<String>],
    widest: &std::cell::Cell<usize>,
) -> Result<Vec<Result<Found, String>>, String> {
    // **A repo that has to be asked in halves is asked in halves, without failing first**
    // (SKEIN-278). The split below recovers a refresh; it does not remember anything, so
    // `acme/thing` re-learned it by 504 on every single refresh — one wasted heavy request
    // per poll per repo, for ever, announcing itself in the fleet's log each time.
    //
    // What is remembered is a WIDTH GITHUB ANSWERED, never a refusal — the rule
    // [`what_github_said`] states for the lookups above it, and the pattern SKEIN-281 names. So the
    // memo cannot pin a repo shut over a bad minute: the worst it can say is "the last thing that
    // worked here was three searches at a time", it expires ([`BATCH_WIDTH_LIFE`]) so the wide
    // batch is tried again, and [`forget_batch_widths`] clears it by hand.
    if searches.len() > 1 && answered_batch_width(slug).is_some_and(|w| searches.len() > w) {
        return Ok(split_in_two(slug, searches, after, widest));
    }
    match one_request(slug, searches, after) {
        Ok(found) => {
            widest.set(widest.get().max(searches.len()));
            Ok(found)
        }
        // **Too heavy is not the same as unavailable** (SKEIN-266). Batching took five requests per
        // repo down to one — and made that one the most expensive thing skein sends: five `search`
        // connections of up to a hundred nodes each, every node carrying the whole PR fragment.
        // GitHub sheds those at the edge, twice on one live fleet within an hour: once as a 200
        // with no body, once as nginx's own `502 Bad Gateway`. `github` retries such a shrug once
        // already; when the retry fails too, the batch itself is the thing to give up on, not the
        // refresh.
        //
        // So halve it and ask again. The quota win survives where it was won — one request whenever
        // one request works — and where it does not, skein spends two, or four, rather than showing
        // an empty queue over a repo full of pull requests. A single search that still fails is
        // reported as itself, which is the per-alias blind spot the batching was careful to keep.
        // Only when GitHub refused to TAKE it. An outage, a rate-limit hold or a 500 that carries
        // a real message is GitHub answering, and asking those again in halves would spend more
        // requests to be told the same thing twice — and would turn SKEIN-258's one honest
        // sentence back into five. `github::edge_refused` owns that distinction, beside the words
        // it is reading.
        Err(why) if searches.len() > 1 && crate::github::edge_refused(&why) => {
            let out = split_in_two(slug, searches, after, widest);
            // Said where a refusal actually happened, and nowhere else. It used to be said on every
            // split — which, once a repo needed splitting, was every refresh for ever: a reader
            // saw this line about `acme/thing` over and over, and it was reporting skein
            // asking a question it already knew the answer to. A split skein chose from what it
            // learned is not news; a refusal it had not seen coming is.
            eprintln!(
                "skein: GitHub would not take {slug}'s {} searches in one request ({why}) — asked \
                 in two, and the next refresh will start there",
                searches.len()
            );
            Ok(out)
        }
        Err(why) => Err(why),
    }
}

/// Ask the same searches as two narrower batches. Each half goes back through [`one_batch`], so a
/// half GitHub also refuses splits again, and a half it answers records its width.
fn split_in_two(
    slug: &str,
    searches: &[String],
    after: &[Option<String>],
    widest: &std::cell::Cell<usize>,
) -> Vec<Result<Found, String>> {
    let (left, right) = searches.split_at(searches.len() / 2);
    let (left_after, right_after) = after.split_at(searches.len() / 2);
    let mut out = one_batch(slug, left, left_after, widest)
        .unwrap_or_else(|e| left.iter().map(|_| Err(e.clone())).collect());
    out.extend(
        one_batch(slug, right, right_after, widest)
            .unwrap_or_else(|e| right.iter().map(|_| Err(e.clone())).collect()),
    );
    out
}

/// How long a batch width GitHub answered at stands in for asking again.
///
/// The same hour, and the same trade, as `crate::ai`'s `REFUSAL_LIFE` — stated rather than tuned.
/// What it costs is ONE wide request per hour per repo on a fleet whose GitHub genuinely sheds
/// them. What it buys is that nothing skein learned from a bad afternoon can outlive the afternoon:
/// a repo narrowed to two searches at a time widens back on its own, with nobody pressing anything.
const BATCH_WIDTH_LIFE: Duration = Duration::from_secs(60 * 60);

/// The widest batch of membership searches GitHub has **answered** for a repository, and when.
///
/// Every number in here is an answer, never a refusal — see [`one_batch`]. It is read to decide
/// where to START a refresh, and a stale one costs the refresh nothing worse than a split it did
/// not need.
static BATCH_WIDTHS: Mutex<BTreeMap<String, (usize, i64)>> = Mutex::new(BTreeMap::new());

/// Forget where the refreshes start, for tests and for a person who has just fixed their GitHub.
pub fn forget_batch_widths() {
    if let Ok(mut seen) = BATCH_WIDTHS.lock() {
        seen.clear();
    }
}

/// The remembered width, **if it still describes anything**.
pub(super) fn answered_batch_width(slug: &str) -> Option<usize> {
    let seen = BATCH_WIDTHS.lock().unwrap_or_else(|e| e.into_inner());
    let (width, at_ms) = seen.get(slug).copied()?;
    (now_ms().saturating_sub(at_ms) <= BATCH_WIDTH_LIFE.as_millis() as i64).then_some(width)
}

/// Write down what this refresh managed, once the refresh is over.
///
/// **Only a NARROWING is remembered.** `widest >= asked` means GitHub took everything it was
/// handed, and there is nothing about this repo worth writing down — so the entry is removed
/// rather than set to the number of searches this particular refresh happened to have. Recording
/// that number would cap the repo at it: a fleet whose token could not list teams asks four, and
/// the day `read:org` arrives the fifth search would be "wider than GitHub has answered" and split
/// for no reason at all.
///
/// The clock is the other load-bearing part, and it moves in one direction. A refresh that got
/// WIDER than the standing memo restarts it: GitHub took more than skein expected, which is the
/// condition healing, and the new answer deserves its own full hour. A refresh that got narrower —
/// or exactly as narrow as last time, which is what a repo that splits on every poll produces —
/// updates the width and leaves the clock alone. Otherwise a repo would keep its own cap alive by
/// confirming it every three minutes, which is the memo pattern (SKEIN-281) rebuilt out of
/// successes: a note that outlives its cause with nothing able to end it.
fn learn_batch_width(slug: &str, widest: usize, asked: usize) {
    // A refresh where nothing answered learned nothing. Leaving the memo alone is what keeps a
    // rate-limit hold or a dead network from being read as "GitHub will not take one search".
    if widest == 0 {
        return;
    }
    let mut seen = BATCH_WIDTHS.lock().unwrap_or_else(|e| e.into_inner());
    if widest >= asked {
        seen.remove(slug);
        return;
    }
    match seen.get_mut(slug) {
        Some((known, at_ms)) => {
            if widest > *known {
                *at_ms = now_ms();
            }
            *known = widest;
        }
        None => {
            seen.insert(slug.to_string(), (widest, now_ms()));
        }
    }
}

/// Now, in epoch milliseconds — the one spelling this module compares memo ages against.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `repo:<slug>` — the one place this module names a repository to GitHub — or a refusal to ask at
/// all, for a slug that cannot be naming one. (SKEIN-641)
///
/// **A search qualifier is not a path segment, and [`crate::github::path_segment`] is deliberately
/// not used here.** A path segment is opaque bytes that GitHub percent-decodes back before it looks
/// anything up, so every byte has an escape and SKEIN-633's encoder can turn *any* name into one
/// legal segment. A search qualifier has no such escape. GitHub's search grammar separates
/// qualifiers on whitespace and matches this one against repository names, so a slug carrying a
/// space does not produce a malformed query — it produces a well-formed query about a **different
/// repository**, answered 200. Measured on this module's own stub: `acme/space one` reached the
/// wire as `repo:acme/space one is:pr is:open author:me`, which scopes the search to `acme/space`
/// and leaves `one` behind as a free-text term.
///
/// Quoting it would not fix that, which is why this refuses rather than escapes: no repository
/// GitHub hosts has a space in its name, so `repo:"acme/space one"` would only turn a search of the
/// **wrong** repository into a search of **no** repository — the same empty queue, with the same
/// silence. That is SKEIN-238's damage, and an empty queue over a repository full of pull requests
/// is the one thing this module must never be able to show by accident.
///
/// The rule is the producer's own rather than a second alphabet beside it:
/// [`crate::gitgate::slug_from_path`] is what cuts `owner/name` out of a registered remote, and a
/// slug it would not hand back unchanged is one no road into this module should have produced. The
/// registered road already cannot: `gitgate::slug_from_url` refuses a space when the queue reads
/// the repo's source. What reaches here unchecked is the rename — `credentials::renamed_to` adopts
/// whatever `full_name` the API answered with — so this is the check at the place that would be
/// harmed rather than at one of the roads in.
fn repo_qualifier(slug: &str) -> Result<String, String> {
    match crate::gitgate::slug_from_path(slug).as_deref() == Some(slug) {
        true => Ok(format!("repo:{slug}")),
        false => Err(format!(
            "{slug:?} is not a name GitHub could be hosting, so skein did not ask: a search \
             scoped by it would answer about some other repository rather than fail"
        )),
    }
}

/// One batched request, as it has always been — the recursion above is what turns a refusal of the
/// whole batch into halves, and `after` is what turns it into the next page.
fn one_request(
    slug: &str,
    searches: &[String],
    after: &[Option<String>],
) -> Result<Vec<Result<Found, String>>, String> {
    // Before the token and before the wire: a repository skein cannot name is not asked about, and
    // the `Err` is not one [`crate::github::edge_refused`] recognises, so [`one_batch`] passes it
    // straight up rather than re-asking it in halves.
    let repo = repo_qualifier(slug)?;
    let token = token_for(slug, Need::Read)?;
    let mut variables = serde_json::Map::new();
    variables.insert("n".into(), serde_json::json!(SEARCH_PAGE));
    for (i, search) in searches.iter().enumerate() {
        // `is:pr is:open` and the repo are what `gh pr list --repo … --state open` added for us.
        // Spelled out here because the search string is now ours to build rather than gh's.
        variables.insert(
            format!("q{i}"),
            serde_json::json!(format!("{repo} is:pr is:open {search}")),
        );
        // `null` on the first page, which is GraphQL's "from the beginning" — so the first request
        // of a refresh is the request it always was.
        variables.insert(
            format!("a{i}"),
            match after.get(i).and_then(|c| c.clone()) {
                Some(cursor) => serde_json::Value::String(cursor),
                None => serde_json::Value::Null,
            },
        );
    }
    let (data, errors) = crate::github::graphql_partial(
        &batched_query(searches.len()),
        serde_json::Value::Object(variables),
        &token,
    )?;
    Ok((0..searches.len())
        .map(|i| {
            let alias = format!("q{i}");
            match data.get(&alias) {
                Some(chunk) if !chunk.is_null() => {
                    let nodes = chunk
                        .get("nodes")
                        .and_then(|n| n.as_array())
                        .cloned()
                        .unwrap_or_default();
                    // A search that matches an issue rather than a pull request comes back as an
                    // empty object — the fragment simply does not apply — so those are dropped
                    // rather than parsed into a PR with number 0.
                    let more = chunk
                        .get("pageInfo")
                        .and_then(|p| p.get("hasNextPage"))
                        .and_then(|v| v.as_bool());
                    Ok(Found {
                        matched: chunk.get("issueCount").and_then(|v| v.as_u64()),
                        cursor: chunk
                            .get("pageInfo")
                            .and_then(|p| p.get("endCursor"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        // GitHub's own word for it where there is one. The fallback counts before
                        // the filter below, because the page is what GitHub filled against
                        // `first: $n` — dropping a non-PR from it makes the answer shorter without
                        // making it any more complete.
                        whole: match more {
                            Some(more) => !more,
                            None => nodes.len() < SEARCH_PAGE,
                        },
                        // Deserialised here, at the one place GitHub's answer arrives, so
                        // everything downstream reads fields rather than string keys. A node
                        // this struct cannot hold at all is dropped exactly as a non-pull-request
                        // hit is — every field in [`PrNode`] defaults, so only a type GitHub
                        // changed could do it, and dropping is what already happens to the empty
                        // object an issue match returns.
                        items: nodes
                            .iter()
                            .filter_map(|node| serde_json::from_value::<PrNode>(node.clone()).ok())
                            .filter(|pr| pr.number.is_some())
                            .collect(),
                    })
                }
                // This alias came back null or absent: find ITS errors by path. An error that
                // names no alias is ambient — attributed to every failed alias rather than
                // dropped, because a blind spot with no reason reads as skein's own fault.
                _ => {
                    let mine = errors
                        .iter()
                        .filter(|e| {
                            e.get("path")
                                .and_then(|p| p.as_array())
                                .and_then(|p| p.first())
                                .and_then(|s| s.as_str())
                                == Some(alias.as_str())
                        })
                        .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                        .collect::<Vec<_>>()
                        .join("; ");
                    Err(match mine.is_empty() {
                        false => mine,
                        true => {
                            let ambient = errors
                                .iter()
                                .filter(|e| e.get("path").is_none())
                                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                                .collect::<Vec<_>>()
                                .join("; ");
                            match ambient.is_empty() {
                                false => ambient,
                                true => "GitHub returned no answer for this search".into(),
                            }
                        }
                    })
                }
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prq::fixtures::{
        batched_github, batched_github_answering, batched_github_pages, batched_repo,
        graphql_requests, recording_github, search_node,
    };

    /// **The threads skein fetches are the newest ones, and the doc that says so is checked
    /// against the query that does it** (SKEIN-340).
    ///
    /// The defect was not a wrong number, it was two directions: [`Pr::review_threads`] promised
    /// "newest [`REVIEW_THREADS_FETCHED`] of them" while [`PR_FRAGMENT`] asked
    /// `reviewThreads(first: 10)`, four lines from a `comments(last: 5)` that already read its own
    /// connection from the end. Nothing failed, because nothing compared them: the doc was prose
    /// and the query was a string, and a reader who trusted either was right about half the code.
    /// What it cost is in [`REVIEW_THREADS_FETCHED`] — an author told "nothing is open" with four
    /// unresolved threads on GitHub, because the ten the cap admitted were the ten already
    /// resolved.
    ///
    /// **What this can and cannot prove.** The paging itself is GitHub's, so no test here can watch
    /// `last:` return the newer end; what is testable is the thing that actually broke, which is
    /// the two halves disagreeing. So the direction is written ONCE, as the pair below, and both
    /// halves are read back out of the tree — the query from [`batched_query`], which is the text
    /// that goes on the wire rather than the constant it is built from, and the promise from this
    /// file's own source. Flip either one alone and this fails naming the other.
    #[test]
    fn the_newest_review_threads_are_fetched_not_the_oldest() {
        // The direction, said once: the word the doc uses, and the argument the query must pass.
        let (promise, argument) = ("newest", "last");

        // **The wire.** `batched_query` is what the refresh POSTs, fragment and all; asserting on
        // `PR_FRAGMENT` alone would pass on a query that never carried the fragment.
        let sent = batched_query(1);
        assert!(
            sent.contains(&format!(
                "reviewThreads({argument}: {REVIEW_THREADS_FETCHED})"
            )),
            "the query pages review threads from the wrong end — `first:` is the OLDEST \
             {REVIEW_THREADS_FETCHED}, and on a pull request past its first round of review those \
             are the resolved ones, so `move.mjs` filters every one of them out and tells the \
             AUTHOR nothing is open. `PullRequestReviewThreads` takes `last:`; the neighbouring \
             `comments(last: {PR_COMMENTS_FETCHED})` proves it. Query: {sent}"
        );

        // **The promise.** The sentence `Pr::review_threads` makes to everything that reads the
        // field, taken from the doc block immediately above the declaration rather than from
        // anywhere else the words might appear. `find` takes the first occurrence, which is the
        // declaration itself — the struct is defined long before this module.
        let source = include_str!("types.rs");
        let at = source
            .find("    pub review_threads: Vec<ReviewThread>,")
            .expect("Pr::review_threads is declared in this file");
        let mut block: Vec<&str> = source[..at]
            .lines()
            .rev()
            .take_while(|l| {
                let l = l.trim_start();
                l.starts_with("///") || l.starts_with("#[")
            })
            .collect();
        // Walked upwards, printed downwards — a failure message in reverse is a failure message
        // nobody reads.
        block.reverse();
        let doc = block.join("\n");
        assert!(
            doc.contains(&format!("{promise} [`REVIEW_THREADS_FETCHED`] of them")),
            "`Pr::review_threads` stopped promising the {promise} threads while the query still \
             asks for them with `{argument}:` — the drift SKEIN-340 was. Change both or neither: \
             a reader believes the doc and a merge train believes the query. The doc block \
             read:\n{doc}"
        );
    }

    /// What the conversation costs on the wire, measured rather than asserted (SKEIN-301).
    ///
    /// [`PR_FRAGMENT`] travels once per pull request, up to [`SEARCH_PAGE`] of them per membership
    /// rule, in one request — the request `acme/thing` already answers with a 504
    /// (SKEIN-278). So "does this make it worse" is a number, and the number is built here from a
    /// stated profile rather than from a guess: **54 pull requests, each with 2 review threads,
    /// 3 PR comments of 120 characters, and 1 outstanding reviewer.** That is a real queue's size
    /// (SKEIN-301's brief) with a conversation load a busy repo would recognise.
    ///
    /// The ceiling is what the test enforces. It is deliberately loose — the point is not the exact
    /// byte count, which moves with every field anybody adds, but that this change stays in the
    /// same order of magnitude as the answer it grew from. The measured numbers go in the item.
    ///
    /// **Every cap is saturated, `LABELS_FETCHED` among them since SKEIN-373.** That cap is why
    /// the answer to a label list that overflows is to say so rather than to ask for GitHub's
    /// hundred: at twenty the worst case sits a few thousand bytes under the ceiling below, so a
    /// hundred does not fit and no rearrangement of the other two makes it fit.
    ///
    /// **The worst case has a ceiling too, since SKEIN-316.** Both numbers were printed and only
    /// the profile one was checked, so the number the 504 is actually about — every pull request
    /// saturating both caps, a whole page of them — could be tripled by a cap nobody re-measured
    /// and the test would still pass. It is the per-ALIAS figure that carries the ceiling because
    /// that is what a cap multiplies; a refresh sends five of these in one request.
    #[test]
    fn the_conversation_is_measured_against_the_answer_it_grew_from() {
        let thread = |n: usize| {
            format!(
                r#"{{"id":"PRRT_kwDOAbCdEf4A{n:04}","isResolved":false,"isOutdated":false,"comments":{{"nodes":[{{"author":{{"login":"reviewer"}},"createdAt":"2026-08-18T09:00:00Z","url":"https://github.com/acme/thing/pull/{n}#discussion_r1234567890"}}]}}}}"#
            )
        };
        let comment = |n: usize| {
            format!(
                r#"{{"author":{{"login":"someone"}},"body":"{body}","createdAt":"2026-08-18T10:00:00Z","url":"https://github.com/acme/thing/pull/{n}#issuecomment-1234567890"}}"#,
                body = "x".repeat(120)
            )
        };
        // What the two SKEIN-301 figures were measured with, unchanged so they stay comparable
        // with the numbers in that item: one short label, and no count beside it.
        const ONE_LABEL: &str = r#""nodes":[{"name":"ready"}]"#;
        // A pull request's labels, as a page of `count` of them (SKEIN-373). The names are the
        // fixture's own — `acme/testbed#20` labels by area, which is what put 22 on
        // one pull request — so the worst case is measured against a real naming scheme rather
        // than a short word chosen to flatter the number.
        let labels = |count: usize| {
            let nodes = (1..=count)
                .map(|i| format!(r#"{{"name":"area/mod-{i:02}"}}"#))
                .collect::<Vec<_>>()
                .join(",");
            format!(r#""totalCount":{count},"nodes":[{nodes}]"#)
        };
        let base = |n: usize, labels: &str| {
            format!(
                r#""number":{n},"title":"a change to something","url":"https://github.com/acme/thing/pull/{n}","isDraft":false,"updatedAt":"2026-08-18T10:00:00Z","headRefName":"feat-{n}","headRefOid":"0123456789abcdef0123456789abcdef01234567","baseRefName":"main","reviewDecision":"REVIEW_REQUIRED","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","additions":120,"deletions":30,"changedFiles":4,"labels":{{{labels}}},"author":{{"login":"someone"}},"latestReviews":{{"nodes":[]}},"commits":{{"nodes":[{{"commit":{{"committedDate":"2026-08-18T09:00:00Z","statusCheckRollup":{{"state":"SUCCESS","contexts":{{"totalCount":3,"nodes":[{{"name":"build","detailsUrl":"https://ci/1","status":"COMPLETED","conclusion":"SUCCESS"}}]}}}}}}}}]}}"#
            )
        };
        let before: String = (1..=54)
            .map(|n| format!("{{{}}}", base(n, ONE_LABEL)))
            .collect::<Vec<_>>()
            .join(",");
        let after: String = (1..=54)
            .map(|n| {
                format!(
                    r#"{{{base},"reviewRequests":{{"totalCount":1,"nodes":[{{"requestedReviewer":{{"login":"alice"}}}}]}},"reviewThreads":{{"totalCount":2,"nodes":[{t1},{t2}]}},"comments":{{"totalCount":3,"nodes":[{c},{c},{c}]}}}}"#,
                    base = base(n, ONE_LABEL),
                    t1 = thread(n),
                    t2 = thread(n + 100),
                    c = comment(n),
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        // And the worst case the caps allow, which is the number the 504 risk is actually about:
        // every pull request saturating every cap, in a page of `SEARCH_PAGE` rather than 54.
        // `LABELS_FETCHED` is one of them since SKEIN-373 — the cheapest node in the fragment, and
        // still 20 of them on 100 pull requests, which is what makes "just ask for a hundred" a
        // measurable answer rather than an opinion.
        let saturated: String = (1..=SEARCH_PAGE)
            .map(|n| {
                let threads = (0..REVIEW_THREADS_FETCHED)
                    .map(|i| thread(n + i * 1000))
                    .collect::<Vec<_>>()
                    .join(",");
                let comments = (0..PR_COMMENTS_FETCHED)
                    .map(|_| comment(n))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    r#"{{{base},"reviewRequests":{{"totalCount":1,"nodes":[{{"requestedReviewer":{{"login":"alice"}}}}]}},"reviewThreads":{{"totalCount":{tc},"nodes":[{threads}]}},"comments":{{"totalCount":{cc},"nodes":[{comments}]}}}}"#,
                    base = base(n, &labels(LABELS_FETCHED)),
                    tc = REVIEW_THREADS_FETCHED,
                    cc = PR_COMMENTS_FETCHED,
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        let (was, now) = (before.len(), after.len());
        println!("SKEIN-301 payload for 54 pull requests: {was} bytes -> {now} bytes");
        println!(
            "SKEIN-301 worst case, {SEARCH_PAGE} pull requests at every cap: {} bytes",
            saturated.len()
        );
        // Both parse, which is what makes the two numbers comparable rather than two strings.
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&format!("[{after}]")).expect("the after shape is real JSON");
        assert_eq!(parsed.len(), 54);
        assert!(
            now < was * 4,
            "the conversation more than quadrupled the answer ({was} -> {now} bytes for 54 pull \
             requests) — PR_FRAGMENT travels for up to {SEARCH_PAGE} of them per membership rule, \
             and this is the request that already 504s (SKEIN-278)"
        );

        // **The worst case the caps allow, held under half a megabyte per alias** (SKEIN-316). A
        // refresh sends one of these per membership rule in ONE request, so this figure is a fifth
        // of what GitHub is asked to compute — and `acme/thing` already answers that with a
        // 504. The ceiling is a round number rather than the measurement plus a margin, so that
        // reading it says what is being defended instead of what today happens to cost; the
        // headroom under it is stated in the message, so a failure says how far past it went and
        // which lever SKEIN-316 names first.
        const WORST_CASE_CEILING: usize = 500_000;
        assert!(
            saturated.len() < WORST_CASE_CEILING,
            "the worst case the caps allow is {} bytes for ONE alias ({} for a five-alias \
             refresh), past the {WORST_CASE_CEILING}-byte ceiling — this is the request \
             acme/thing answers with a 504 (SKEIN-278). The levers, in SKEIN-316's order: \
             PR_COMMENTS_FETCHED (now {PR_COMMENTS_FETCHED}, the only cap whose nodes carry \
             bodies), then REVIEW_THREADS_FETCHED (now {REVIEW_THREADS_FETCHED}), then \
             LABELS_FETCHED (now {LABELS_FETCHED}). Do NOT raise SEARCH_PAGE (now {SEARCH_PAGE}) — its own doc argues a bigger page trades a rare \
             truncation for a likelier outage",
            saturated.len(),
            saturated.len() * 5,
        );
    }

    /// The 5→1 cut itself: one refresh is ONE `/graphql` request carrying q0..q4 and the shared
    /// fragment — and the parsed queue is what five separate requests produced before. The fixture
    /// is `tests/review_queue.rs`'s "appears once with every reason" case ported to the batched
    /// wire: a PR found by two rules carries both Reasons, in the order the searches are listed.
    #[test]
    fn one_refresh_is_one_graphql_request_carrying_every_membership_rule() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        let answer = format!(
            r#"{{"data":{{"q0":{{"nodes":[{one},{seven}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[{seven}]}},"q3":{{"nodes":[]}},"q4":{{"nodes":[{nine}]}}}}}}"#,
            one = search_node(1),
            seven = search_node(7),
            nine = search_node(9),
        );
        let (base, seen) = batched_github(true, 200, answer);
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();

        let q = queue(&batched_repo("acme/batch-one"), true).expect("the queue answered");

        let requests = graphql_requests(&seen);
        assert_eq!(
            requests.len(),
            1,
            "five membership rules must cost ONE GraphQL request, got {requests:#?}"
        );
        let sent = &requests[0];
        for alias in ["q0", "q1", "q2", "q3", "q4"] {
            assert!(
                sent.contains(&format!("{alias}: search(query: ${alias}")),
                "alias {alias} is missing from the one request: {sent}"
            );
        }
        assert!(
            sent.contains("fragment PrFields on PullRequest") && sent.contains("...PrFields"),
            "the aliases must share the PR node through one fragment: {sent}"
        );
        for rule in [
            "review-requested:me",
            "reviewed-by:me",
            "author:me",
            "mentions:me",
            "team-review-requested:acme/core",
        ] {
            assert!(
                sent.contains(&format!("repo:acme/batch-one is:pr is:open {rule}")),
                "the `{rule}` search is missing from the variables: {sent}"
            );
        }

        // The same queue five requests built: each alias contributes, a PR found by several
        // aliases appears once with every reason, in search-list order.
        let mut numbers: Vec<u64> = q.prs.iter().map(|p| p.number).collect();
        numbers.sort();
        assert_eq!(
            numbers,
            vec![1, 7, 9],
            "every alias's PRs are in the one queue"
        );
        let seven = q.prs.iter().find(|p| p.number == 7).unwrap();
        assert_eq!(
            seven.reasons,
            vec![Reason::Reviewer, Reason::Author],
            "both memberships kept, in query order"
        );
        assert_eq!(
            q.prs.iter().find(|p| p.number == 9).unwrap().reasons,
            vec![Reason::Team("acme/core".into())]
        );
        assert!(
            q.blind_spots.is_empty(),
            "nothing was hidden: {:?}",
            q.blind_spots
        );

        forget_host_token();
    }

    /// GraphQL's partial failure — `data.qN: null` plus an error whose `path` names the alias —
    /// maps back to ITS search's blind spot, and the aliases that answered still fill the queue.
    /// This is exactly what five separate requests gave: partial answers beat none.
    ///
    /// The failed alias is deliberately a MIDDLE one (`q2`, `author:me`): a mapping that pins
    /// every failure on the first alias would pass a q0 fixture by accident, and the wrong-rule
    /// blind spot it produces is precisely the lie this test exists to make loud.
    #[test]
    fn a_failed_alias_is_its_own_blind_spot_and_the_rest_still_answer() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        let answer = format!(
            r#"{{"data":{{"q0":{{"nodes":[{five}]}},"q1":{{"nodes":[]}},"q2":null,"q3":{{"nodes":[]}}}},"errors":[{{"message":"HTTP 403: forbidden","path":["q2"]}}]}}"#,
            five = search_node(5),
        );
        let (base, seen) = batched_github(false, 200, answer);
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();

        let q = queue(&batched_repo("acme/batch-partial"), true).expect("the queue answered");

        assert_eq!(graphql_requests(&seen).len(), 1);
        assert!(
            q.blind_spots.iter().any(|b| b
                .contains("the `author:me` query failed, so those PRs are missing")
                && b.contains("403")),
            "the failed alias must name ITS membership rule, with GitHub's reason: {:?}",
            q.blind_spots
        );
        for survivor in ["review-requested:me", "reviewed-by:me", "mentions:me"] {
            assert!(
                !q.blind_spots
                    .iter()
                    .any(|b| b.contains(&format!("`{survivor}` query failed"))),
                "an alias that answered was reported as failed: {:?}",
                q.blind_spots
            );
        }
        assert_eq!(
            q.prs.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![5],
            "the aliases that answered still contribute"
        );
        assert_eq!(q.prs[0].reasons, vec![Reason::Reviewer]);

        forget_host_token();
    }

    /// A whole-request failure — here a 500, live it is just as often the network — is every
    /// membership rule going dark at once, and it is reported as the one failure it is.
    ///
    /// The negative half is the point (SKEIN-258): the per-rule sentence is right for a per-alias
    /// failure and wrong here, where repeating it once per rule turned one dead request into five
    /// alarms on one cold load.
    #[test]
    fn a_dead_batched_request_says_once_that_every_membership_is_missing() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, _seen) = batched_github(false, 500, r#"{"message":"boom"}"#.to_string());
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();

        let q = queue(&batched_repo("acme/batch-dead"), true).expect("the queue still answers");

        assert!(q.prs.is_empty());
        assert!(
            q.blind_spots.iter().any(|b| {
                b.contains("GitHub did not answer for acme/batch-dead")
                    && b.contains("membership searches are missing")
                    && b.contains("500")
            }),
            "the refresh's total loss went unreported: {:?}",
            q.blind_spots
        );
        for rule in [
            "review-requested:me",
            "reviewed-by:me",
            "author:me",
            "mentions:me",
        ] {
            assert!(
                !q.blind_spots
                    .iter()
                    .any(|b| b.contains(&format!("the `{rule}` query failed"))),
                "one dead request must not be reported as one broken rule per membership: {:?}",
                q.blind_spots
            );
        }

        forget_host_token();
    }

    /// A refresh whose CONNECTION died says so, rather than "GitHub did not answer" (SKEIN-271).
    ///
    /// The two ask different things of whoever reads them. "GitHub did not answer" sends them to
    /// look at GitHub — a token, a rate limit, a refusal — and the connection dying is not GitHub
    /// answering anything; it says the request never completed, so ask again. Skein already has,
    /// once, by the time this line is written, and the sentence says that too.
    #[test]
    fn a_refresh_whose_connection_died_says_so_rather_than_blaming_github() {
        let _g = crate::testutil::env_lock();
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, seen) = recording_github(Some(r#"{"login":"me"}"#), Some("/graphql"));
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/batch-cut"), true).expect("the queue still answers");

        assert!(q.prs.is_empty());
        let said = q
            .blind_spots
            .iter()
            .find(|b| b.contains("membership searches for acme/batch-cut are missing"))
            .unwrap_or_else(|| {
                panic!(
                    "the refresh's total loss went unreported: {:?}",
                    q.blind_spots
                )
            });
        assert!(
            said.contains("connection to GitHub died"),
            "the reader is sent to look at GitHub for something GitHub never said: {said}"
        );
        assert!(
            !said.contains("GitHub did not answer for"),
            "the two diagnoses must not be the same sentence: {said}"
        );
        assert!(
            said.contains("asked a second time"),
            "a reader deciding whether to press again is not told skein already did: {said}"
        );
        // …and it really did ask twice, rather than only claiming to.
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .filter(|r| r.contains("/graphql"))
                .count()
                >= 2,
            "the retry the sentence promises never happened"
        );

        forget_host_token();
        forget_renames();
    }

    /// A rate-limited batch is both at once: the refresh's whole loss stated, and the hold engaged
    /// — the next refresh dies at home, never reaching the wire (SKEIN-208's contract, kept through
    /// the merge into one request).
    #[test]
    fn a_rate_limited_batch_engages_the_hold_and_says_the_refresh_is_missing() {
        let _g = crate::testutil::env_lock();
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, seen) = batched_github(
            false,
            200,
            r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded for user ID 123"}]}"#
                .to_string(),
        );
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();

        let first = queue(&batched_repo("acme/batch-limited"), true)
            .expect("a rate-limited refresh still answers, with its blind spots");
        assert!(
            first.blind_spots.iter().any(|b| {
                b.contains("GitHub did not answer for acme/batch-limited")
                    && b.contains("membership searches are missing")
                    && b.contains("rate limiting skein")
            }),
            "a rate-limited batch must say the whole refresh is missing, and why: {:?}",
            first.blind_spots
        );

        // The hold is engaged: the next refresh is refused before the wire — viewer() is the
        // first call a refresh makes, and it never leaves the process.
        let second = queue(&batched_repo("acme/batch-limited"), true)
            .expect_err("a held refresh cannot even identify the viewer");
        assert!(
            second.contains("not calling GitHub"),
            "the refusal says what is happening: {second}"
        );
        assert_eq!(
            graphql_requests(&seen).len(),
            1,
            "the second refresh must never reach the server"
        );

        forget_host_token();
    }

    /// **A membership search cut off at its page says how many it could not show** (SKEIN-231).
    ///
    /// The truncation was undetectable by construction: GitHub answers HTTP 200 with exactly a
    /// hundred nodes, and the queue rendered a list that looks complete. That is the rename bug in
    /// a quieter form — there, a stale name matched nothing and the queue was empty; here the queue
    /// is full and merely short, which is harder to notice and just as wrong.
    ///
    /// Three things travel together, and the test insists on all three: the sentence with the
    /// count in it, `whole: false` so nothing downstream reads absence as evidence, and the
    /// archive entry for a pull request past the page surviving the refresh.
    #[test]
    fn a_membership_search_cut_off_at_its_page_says_how_many_it_could_not_show() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");

        // Set aside by hand, and past the page of whatever the searches return: the queue cannot
        // see it, and "cannot see it" must not read as "it closed".
        set_archived("search-cut", 4242, true).expect("archived");

        // Teams listable, so the only thing this queue cannot see is the page — a token that
        // cannot list teams carries its own blind spot and its own `whole: false` (SKEIN-262),
        // which would make every assertion below pass for the wrong reason.
        let answer = |more: bool| {
            format!(
                r#"{{"data":{{"q0":{{"issueCount":143,"pageInfo":{{"hasNextPage":{more}}},"nodes":[{five}]}},
                   "q1":{{"issueCount":0,"pageInfo":{{"hasNextPage":false}},"nodes":[]}},
                   "q2":{{"issueCount":0,"pageInfo":{{"hasNextPage":false}},"nodes":[]}},
                   "q3":{{"issueCount":0,"pageInfo":{{"hasNextPage":false}},"nodes":[]}},
                   "q4":{{"issueCount":0,"pageInfo":{{"hasNextPage":false}},"nodes":[]}}}}}}"#,
                five = search_node(5),
            )
        };

        let (base, seen) = batched_github(true, 200, answer(true));
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/search-cut"), true).expect("the queue answered");

        let sent = &graphql_requests(&seen)[0];
        assert!(
            sent.contains("issueCount") && sent.contains("hasNextPage"),
            "the search must ask how many it matched and whether it reached the end: {sent}"
        );
        // The count is what ARRIVED, not the page size (SKEIN-280): the refresh follows the cursor
        // now, so "the first 100" would be a guess about a number the queue already knows. This
        // fixture answers `hasNextPage: true` with no `endCursor` — which is also the assertion
        // that a page nobody can ask for ends the paging instead of being guessed at, since one
        // node came back and one node is what the sentence reports.
        assert!(
            q.blind_spots.iter().any(|b| {
                b.contains(
                "the `review-requested:me` query matched 143 pull requests and skein read 1 of them"
            )
            }),
            "a truncated search must name ITS rule and the size of the hole: {:?}",
            q.blind_spots
        );
        assert_eq!(
            graphql_requests(&seen).len(),
            1,
            "GitHub said there was more and gave nowhere to carry on from, and skein asked again \
             anyway — a page with no cursor is a page nobody can request"
        );
        assert!(
            !q.whole,
            "a queue missing 43 pull requests must not tell anybody it saw them all"
        );
        assert!(
            archived("search-cut").expect("readable").contains(&4242),
            "a set-aside pull request past the page was deleted because a truncated search did \
             not list it — the same erasure SKEIN-229 fixed for an outage"
        );
        // The rules that answered in full are not tarred with it.
        for whole in ["reviewed-by:me", "author:me", "mentions:me"] {
            assert!(
                !q.blind_spots.iter().any(|b| b.contains(whole)),
                "a search that reached the end was reported as truncated: {:?}",
                q.blind_spots
            );
        }

        // The same shape, reaching the end. Nothing is said, `whole` holds, and the prune runs —
        // which is what stops "say it is partial" from becoming "never prune anything".
        let (base, _seen) = batched_github(true, 200, answer(false));
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/search-cut"), true).expect("the queue answered");
        assert!(
            !q.blind_spots.iter().any(|b| b.contains("query matched")) && q.whole,
            "a search that saw everything must say nothing about being cut off: {:?}",
            q.blind_spots
        );
        assert!(
            !archived("search-cut").expect("readable").contains(&4242),
            "a queue that saw everything still prunes a set-aside PR that is no longer open"
        );
        // Nothing here can prove what the OTHER reader of this list does with it — see
        // `the_only_other_reader_of_this_list_stands_down_when_it_is_partial`.

        forget_host_token();
        forget_renames();
    }

    /// **A repo GitHub will not answer in one request is not asked in one request twice**
    /// (SKEIN-278).
    ///
    /// Reported live on `acme/thing`: the split-in-halves (SKEIN-266) recovered the refresh
    /// exactly as designed, and then did it again ten minutes later, and again — one doomed request
    /// per poll per repo, each carrying GitHub's own 504 latency, each announcing itself in the
    /// fleet's log. The recovery was never the complaint; re-learning it by failing was.
    ///
    /// What is remembered is a width GitHub ANSWERED, never the refusal — the rule
    /// `what_github_said` states for the lookups above it and the pattern SKEIN-281 names — so the
    /// memo expires and [`forget_batch_widths`] clears it. Both halves are asserted here, because
    /// either alone is a bug: the second refresh must not spend the doomed request, and the queue
    /// must SAY that it is being asked narrowly, which the item asks for in as many words ("the
    /// narrowing must be visible ... not a silent adaptation").
    #[test]
    fn a_repo_github_will_not_take_whole_is_not_asked_whole_on_the_next_refresh() {
        let _g = crate::testutil::env_lock();
        // Right after the env lock, per `github::HoldClear`'s own rule: a test elsewhere in this
        // binary can engage the rate-limit hold, and a held hold refuses every request before it
        // reaches the fixture — which reads here as a request that was never sent.
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        forget_batch_widths();

        // A 504 carrying JSON: `edge_refused` reads it as "would not take it", and `edge_shrug`
        // does not, so it costs ONE request rather than github.rs's own retry-once. Everything
        // after it answers.
        let empty = |aliases: usize| {
            let body = (0..aliases)
                .map(|i| {
                    format!(
                        r#""q{i}":{{"issueCount":0,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}}"#
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!(r#"{{"data":{{{body}}}}}"#)
        };
        // Refused EVERY time it is asked wide, answered every time it is asked narrow — which is
        // what a 504 caused by the repository's own size actually is. A stub that refused once
        // could not tell "skein remembered the width" from "GitHub stopped refusing". Four aliases
        // or more is wide here, so the five-search refresh is refused and its halves (two and
        // three) are not; the answer carries five, so a narrower half reads the first few of it
        // and one body serves every width.
        let too_big = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let refusing = too_big.clone();
        let (base, seen) = batched_github_answering(true, move |_, body| {
            let wide = body.contains("$q3: String!");
            match wide && refusing.load(std::sync::atomic::Ordering::SeqCst) {
                true => (
                    504,
                    r#"{"message":"We couldn't respond to your request in time"}"#.to_string(),
                ),
                false => (200, empty(5)),
            }
        });
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let first = queue(&batched_repo("acme/too-big"), true).expect("the split recovered");
        let after_one = graphql_requests(&seen).len();
        assert_eq!(
            after_one, 3,
            "the first refresh should be the doomed request plus the two halves: {after_one}"
        );

        // The refresh that matters. Nothing about GitHub changed; what changed is that skein was
        // told once and wrote down the width that WORKED.
        let second = queue(&batched_repo("acme/too-big"), true).expect("the queue answered");
        let after_two = graphql_requests(&seen).len() - after_one;
        assert_eq!(
            after_two, 2,
            "the second refresh spent the doomed request again — the repo re-learns by failing on \
             every single poll, which is the whole bug: {after_two} requests"
        );

        // And it is VISIBLE. A repo that has quietly become expensive to refresh looks identical
        // from outside, so the narrowing is said on the queue itself.
        for q in [&first, &second] {
            assert!(
                q.blind_spots
                    .iter()
                    .any(|b| b.contains("membership searches are being asked 3 at a time")),
                "the narrowing is a silent adaptation — nothing a reader can see says this repo \
                 costs more than one request per refresh: {:?}",
                q.blind_spots
            );
        }
        // It is not a completeness claim: every search answered, so the prunes still run.
        assert!(
            second.whole,
            "asking in halves was reported as not having seen everything, which stops every prune"
        );

        // **The memo is a width GitHub answered, and it ends.** Cleared by hand here — the same
        // door `forget_renames` and `forget_trunks` give, and the reason SKEIN-281's pattern does
        // not apply: there is something a person can clear, and it expires on its own besides.
        forget_batch_widths();
        let before = graphql_requests(&seen).len();
        let again = queue(&batched_repo("acme/too-big"), true).expect("the split recovered");
        assert_eq!(
            graphql_requests(&seen).len() - before,
            3,
            "after forgetting the width the refresh did not go back to asking wide — the memo is a \
             narrowing nobody can undo"
        );
        assert!(
            again
                .blind_spots
                .iter()
                .any(|b| b.contains("being asked 3 at a time")),
            "the refusal is still standing and the queue stopped saying so: {:?}",
            again.blind_spots
        );

        // **And when GitHub gets better, the notice goes.** Nothing is pressed here except the
        // memo, which is what the hourly expiry does on its own in a running fleet: one request,
        // no sentence, and the repo is back where it started.
        too_big.store(false, std::sync::atomic::Ordering::SeqCst);
        forget_batch_widths();
        let before = graphql_requests(&seen).len();
        let healed = queue(&batched_repo("acme/too-big"), true).expect("the queue answered");
        assert_eq!(
            graphql_requests(&seen).len() - before,
            1,
            "GitHub took the wide request and the refresh split it anyway"
        );
        assert!(
            !healed.blind_spots.iter().any(|b| b.contains("being asked")),
            "the narrowing notice outlived the narrowing: {:?}",
            healed.blind_spots
        );

        forget_host_token();
        forget_renames();
        forget_batch_widths();
    }

    /// **A membership rule with more pull requests than one page is read to the END** (SKEIN-280).
    ///
    /// SKEIN-231 taught the queue to notice a truncated search and say so. It said so on every
    /// refresh, for ever, because nothing ever asked for the rest: a repo where more than a hundred
    /// pull requests match one rule showed a permanently short queue with a permanent apology
    /// beside it. Noticing is not reading.
    ///
    /// Four facts, and each one fails on its own:
    ///
    ///   * the pull request on page TWO is in the queue — the point of the whole change;
    ///   * the cursor GitHub handed back is the `after` skein sent, so the second request is the
    ///     next page rather than the same page again;
    ///   * only the rule that had more is asked again — the other three are finished and must not
    ///     cost a second request each;
    ///   * and once the last page says `hasNextPage: false` the queue is `whole` with NO blind
    ///     spot, because a search that was followed to the end saw everything there was. That is
    ///     what lets the prunes in `queue_within` and `review::prune` run again.
    #[test]
    fn a_membership_rule_longer_than_one_page_is_followed_to_the_end() {
        let _g = crate::testutil::env_lock();
        // Right after the env lock, per `github::HoldClear`'s own rule: a test elsewhere in this
        // binary can engage the rate-limit hold, and a held hold refuses every request before it
        // reaches the fixture — which reads here as a request that was never sent.
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");

        // Page one: #5 and a cursor. Page two: #6 and the end. The other three rules finish on
        // page one, which is what makes "only the unfinished rule is asked again" observable.
        let page_one = format!(
            r#"{{"data":{{"q0":{{"issueCount":2,"pageInfo":{{"hasNextPage":true,"endCursor":"CUR-2"}},"nodes":[{five}]}},
               "q1":{{"issueCount":0,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
               "q2":{{"issueCount":0,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
               "q3":{{"issueCount":0,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
               "q4":{{"issueCount":0,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}}}}}}"#,
            five = search_node(5),
        );
        // The follow-up carries ONE alias, so its answer has one: `q0` is the only rule that was
        // asked again, and the parser reads aliases positionally from what it sent.
        let page_two = format!(
            r#"{{"data":{{"q0":{{"issueCount":2,"pageInfo":{{"hasNextPage":false,"endCursor":"CUR-3"}},"nodes":[{six}]}}}}}}"#,
            six = search_node(6),
        );

        let (base, seen) = batched_github_pages(true, vec![(200, page_one), (200, page_two)]);
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/two-pages"), true).expect("the queue answered");

        let numbers: Vec<u64> = q.prs.iter().map(|p| p.number).collect();
        assert!(
            numbers.contains(&6),
            "the pull request past the first page never reached the queue — the refresh noticed \
             the truncation and did nothing about it: {numbers:?}"
        );
        assert!(
            numbers.contains(&5),
            "the first page was thrown away when the second arrived: {numbers:?}"
        );

        let sent = graphql_requests(&seen);
        assert_eq!(
            sent.len(),
            2,
            "one rule had a second page and the refresh cost {} requests: {sent:?}",
            sent.len()
        );
        assert!(
            sent[1].contains("CUR-2"),
            "the second request did not carry the cursor GitHub gave, so it asked for the same \
             page again: {}",
            sent[1]
        );
        // Only the unfinished rule. The three that reached their end on page one must not be
        // re-asked — that would make paging cost a full batch per page rather than one alias.
        assert!(
            sent[1].contains("review-requested:me") && !sent[1].contains("mentions:me"),
            "a search that had already reached its end was asked again on the next page: {}",
            sent[1]
        );

        // Followed to the end means whole, and whole means silent.
        assert!(
            q.whole,
            "a queue that read every page still says it might be missing pull requests, so \
             nothing downstream will ever prune again"
        );
        assert!(
            !q.blind_spots.iter().any(|b| b.contains("query matched")),
            "the truncation apology outlived the truncation: {:?}",
            q.blind_spots
        );

        forget_host_token();
        forget_renames();
    }

    /// **A repository skein cannot name is not asked about at all** (SKEIN-641).
    ///
    /// GitHub's search grammar is whitespace-separated qualifiers, so a slug carrying a space does
    /// not make a malformed query — it makes a well-formed query about a DIFFERENT repository,
    /// answered 200. Measured on this stub before [`repo_qualifier`] existed: `acme/space one`
    /// reached the wire as `repo:acme/space one is:pr is:open author:me`, which scopes the search
    /// to `acme/space` and leaves `one` behind as a free-text term. An empty queue over a
    /// repository full of pull requests is SKEIN-238's damage, and nothing in it is visible to a
    /// reader.
    ///
    /// **The positive half is half the test.** A guard that refused every slug would pass the two
    /// assertions above it and empty every queue in the fleet, so a nameable slug is asked for in
    /// the same breath and its qualifier read back off the wire.
    #[test]
    fn a_repository_name_skein_cannot_name_is_never_asked_about() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, seen) =
            batched_github(false, 200, r#"{"data":{"q0":{"nodes":[]}}}"#.to_string());
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();

        let refused = search_prs_all("acme/space one", &["author:me".to_string()]);

        let why = match refused {
            Err(why) => why,
            Ok(answers) => panic!(
                "a slug with a space was searched for rather than refused; {} searches answered",
                answers.len()
            ),
        };
        assert!(
            why.contains("acme/space one"),
            "the refusal must name the slug it refused, so a reader can act on it: {why}"
        );
        assert!(
            graphql_requests(&seen).is_empty(),
            "skein asked GitHub about a repository it cannot name — what went on the wire was {:?}",
            graphql_requests(&seen)
        );

        let asked = search_prs_all("acme/space-two", &["author:me".to_string()])
            .expect("a nameable slug is still searched for");
        assert_eq!(asked.len(), 1, "the one search must still answer");
        let sent = graphql_requests(&seen);
        assert_eq!(
            sent.len(),
            1,
            "the nameable slug cost exactly one request: {sent:?}"
        );
        assert!(
            sent[0].contains("repo:acme/space-two is:pr is:open author:me"),
            "the qualifier a nameable slug builds changed: {}",
            sent[0]
        );

        forget_host_token();
    }
}
