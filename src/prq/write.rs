//! What skein can do to a pull request, as you.
//!
//! A verdict, a merge, a resolved thread. Every call here runs on the host's own login, so what it
//! does shows up in the repository's history under your name — and each one is written to be safe
//! to press twice, because the failure that matters is a connection that dies after GitHub has
//! already acted.

use super::*;

// ───────────────────────────── acting on a PR ─────────────────────────────

/// The three things a review can say, in GitHub's own vocabulary.
///
/// One function with three verbs rather than three functions, because they differ only in a flag
/// and they must stay consistent: `request-changes` exists precisely so that "not yet" moves the PR
/// out of your lane. Offering only approve and a plain comment would leave a PR you had answered
/// sitting in Needs you forever, with the ball visibly in the wrong court.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Approve,
    RequestChanges,
    Comment,
}

/// Is this pull request still open? `None` when GitHub could not say.
///
/// Asked directly rather than inferred from the queue's absence, and the distinction is the whole
/// point: this queue is **personal** — `review-requested:you`, `author:you`, `mentions:you` — so a
/// PR leaving it means "no longer involves you" at least as often as it means "closed". Anything
/// that pruned on absence would delete the reading of a live PR whose review request moved to
/// somebody else.
///
/// `None` rather than a guess when the call fails: every caller keeps what it has on `None`, so a
/// GitHub that is down costs nothing and deletes nothing.
pub fn pr_is_open(slug: &str, number: u64) -> Option<bool> {
    let value = crate::github::get_json(
        &format!("{}/pulls/{number}", crate::github::repo_path(slug)),
        &host_token().ok()?,
    )
    .ok()?;
    Some(value.get("state").and_then(|s| s.as_str())? == "open")
}

/// Submit a review as **you**, with the token the host holds.
///
/// GitHub refuses an empty body on `--request-changes` and `--comment`, so this refuses first with a
/// sentence you can act on rather than passing the rejection through.
pub fn submit_review(
    slug: &str,
    number: u64,
    verdict: Verdict,
    body: &str,
) -> Result<String, String> {
    let body = body.trim();
    if body.is_empty() && verdict != Verdict::Approve {
        return Err(
            "GitHub needs a body for anything but a bare approval — say what you want changed."
                .into(),
        );
    }
    let event = match verdict {
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
        Verdict::Comment => "COMMENT",
    };
    crate::github::send_json(
        "POST",
        &format!("{}/pulls/{number}/reviews", crate::github::repo_path(slug)),
        &host_token()?,
        &serde_json::json!({ "event": event, "body": body }),
    )?;
    Ok(match verdict {
        Verdict::Approve => "approved",
        Verdict::RequestChanges => "changes requested",
        Verdict::Comment => "commented",
    }
    .into())
}

/// One vetted line comment on its way to GitHub. Defined here rather than borrowed from
/// [`crate::review`] because review depends on this module, not the other way round.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReviewComment {
    pub path: String,
    pub line: u64,
    pub body: String,
    /// The drafted line's own content — the text the reviewer was looking at, without the diff's
    /// `+`/` ` marker. It travels with the comment because it is the only durable anchor a moving
    /// branch leaves: a line NUMBER is a coordinate into one commit's diff and dies with it, but
    /// the line's text survives a rebase, a force-push, an insertion above it. `re_anchor` finds
    /// it again in the new diff by this text. Empty means "unknown" — an old client, or a draft
    /// that never captured it — and such a comment cannot be re-anchored, only displaced.
    #[serde(default)]
    pub text: String,
}

/// The first seven characters of a sha — the length `git log --oneline` taught everyone to read —
/// whole if it is somehow shorter.
fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// Re-anchor drafted comments against a NEWER diff, by line text. Returns
/// `(anchored, displaced)`: the anchored carry updated line numbers valid in the new diff, the
/// displaced could not be placed and belong in the review body instead.
///
/// The rule, and its trade-offs, plainly:
///
/// - A comment's candidates are the new diff's RIGHT-side lines in the SAME path whose content
///   equals `comment.text` exactly (marker stripped, trailing newline ignored). Exact equality,
///   not fuzzy matching: a near-miss anchor puts a review sentence on a line it was not about,
///   which is worse than the honest fallback of naming it in the body.
/// - Exactly one candidate → anchored there. Several → the one nearest the old line number
///   (tie → the earlier), on the theory that most pushes move a line a little, not far; a
///   same-text line far away is likelier a different occurrence.
/// - None — the line was edited, deleted, or its file left the diff — or `comment.text` is empty
///   (nothing to search for) → displaced. Deliberately conservative: displacement costs a little
///   reading, a wrong anchor costs trust in every anchor.
/// - A comment whose text appears verbatim in an unrelated spot of the same file WILL anchor
///   there if its own line vanished. That is the price of text-only matching; the nearest-line
///   rule bounds it, and the (read at…, posted against…) note in the body names the commit that
///   was actually reviewed either way.
pub fn re_anchor(
    comments: &[ReviewComment],
    new_diff: &str,
) -> (Vec<ReviewComment>, Vec<ReviewComment>) {
    // ONE diff grammar, and it lives where the vetting happens (SKEIN-233). This was a
    // second body with the same name: identical code, and `review::commentable` had a
    // THIRD reading of the same bytes that lacked the `\ No newline at end of file` case
    // and silently discarded the rest of its hunk. `prq -> review` is already declared.
    let lines = crate::review::right_side_lines(new_diff);
    let mut anchored = Vec::new();
    let mut displaced = Vec::new();
    for c in comments {
        let want = c.text.trim_end_matches(['\n', '\r']);
        if want.is_empty() {
            displaced.push(c.clone());
            continue;
        }
        let best = lines
            .iter()
            .filter(|(p, _, t)| *p == c.path && t.trim_end_matches(['\n', '\r']) == want)
            // Nearest to the old number wins; on a tie min_by_key keeps the FIRST seen, and the
            // lines arrive in file order, so the earlier line wins the tie.
            .min_by_key(|(_, n, _)| (n.abs_diff(c.line), *n));
        match best {
            Some((_, n, _)) => anchored.push(ReviewComment {
                line: *n,
                ..c.clone()
            }),
            None => displaced.push(c.clone()),
        }
    }
    (anchored, displaced)
}

/// The head sha GitHub holds for this PR right now — one REST call, for the moment before a
/// review posts. The queue's cached sha can be a minute old, and a review posted against a sha
/// nobody verified is how the 422 this module just removed used to be born.
pub fn live_head_sha(slug: &str, number: u64) -> Result<String, String> {
    let v = crate::github::get_json(
        &format!("{}/pulls/{number}", crate::github::repo_path(slug)),
        &host_token()?,
    )?;
    v.pointer("/head/sha")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .ok_or_else(|| "GitHub's answer named no head commit".into())
}

/// What a merge has to check before it happens: `(base_ref, head_sha)`, live, in one request.
/// (SKEIN-338)
///
/// **Not from the queue, and that is the point.** The two facts a merge turns on are exactly the
/// two the queue is worst at: `base_ref` moves under a stacked child the moment its parent lands
/// (GitHub retargets it onto the trunk), and `head_sha` moves on every push. `queue`'s micro-cache
/// is sixty seconds and a remembered queue is older than that, so a merge decided from it is a
/// merge decided from a photograph.
///
/// **Not `queue(repo, false)` either**, on SKEIN-272's rule: a write must not inherit the ways a
/// full refresh fails — the viewer lookup, the rename check, five membership searches — and then
/// report them in the refresh's words. This is one `GET /repos/{slug}/pulls/{number}`, whose only
/// failure is about the pull request being merged.
///
/// An `Err` here STOPS a merge rather than falling back to anything, which is the opposite of
/// [`head_to_post_against`]'s choice next door and is deliberate. A review that cannot verify its
/// head still posts, because losing a review a person just vetted is worse than a stale
/// `commit_id`. A merge that cannot verify its base does not happen, because there is nothing worse
/// than the wrong merge.
pub fn base_and_head(slug: &str, number: u64) -> Result<(String, String), String> {
    let v = crate::github::get_json(
        &format!("{}/pulls/{number}", crate::github::repo_path(slug)),
        &host_token()?,
    )?;
    let at = |p: &str| {
        v.pointer(p)
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match (at("/base/ref"), at("/head/sha")) {
        (Some(base), Some(head)) => Ok((base, head)),
        _ => Err(format!(
            "GitHub did not say what #{number} is based on or what its head commit is, so skein \
             will not merge it"
        )),
    }
}

/// The sha a review is posted against — the ONE way either write path learns it.
///
/// `remembered` is the queue's sha, and it is the fallback rather than the answer. It is up to a
/// minute old (`queue`'s micro-cache) and older still whenever the pane is painting a remembered
/// copy, so inside that window it names a commit the branch has already left. That is not only a
/// wrong `commit_id`: [`submit_review_with_comments`] decides whether to re-anchor by comparing
/// the sha the draft was read at against this one, and a draft read from the same stale queue
/// carries the same stale sha — so the two agree, `moved` reads false, nothing re-anchors, and
/// vetted comments post at line numbers computed against a diff that no longer exists. GitHub
/// resolves them against the CURRENT diff, so they land on whatever text now occupies those
/// numbers and the post reports success (SKEIN-230).
///
/// The two write paths had two answers to this and only one of them made the call. One function,
/// so they cannot drift apart again. If GitHub will not answer, the remembered sha is the best
/// truth available and the post still goes — refusing to post because a verification call failed
/// would lose the review the person just vetted.
pub fn head_to_post_against(slug: &str, number: u64, remembered: &str) -> String {
    live_head_sha(slug, number).unwrap_or_else(|_| remembered.to_string())
}

/// One review post, whole: what is said, what it is said about, and what it is said with.
///
/// A struct rather than eight arguments because five of them are strings and four of those are
/// interchangeable to the compiler — `slug`, `head_sha`, `drafted_at` and `token` in a row, where
/// transposing any two type-checks and posts the wrong thing. Naming them at the call site is what
/// makes that a compile error instead of a review filed against the wrong commit. They travel
/// together for a reason: the two shas are compared against each other to decide whether comments
/// re-anchor, and the credential is what the whole statement is posted AS.
pub struct ReviewPost<'a> {
    /// `owner/repo`, as GitHub spells it.
    pub slug: &'a str,
    pub number: u64,
    /// The LIVE head, which becomes `commit_id` — see [`head_to_post_against`].
    pub head_sha: &'a str,
    pub verdict: Verdict,
    /// The prose body. May be empty when there are comments, or when the verdict is an approval.
    pub body: &'a str,
    pub comments: &'a [ReviewComment],
    /// The head the comments were drafted against; empty means "assume current".
    pub drafted_at: &'a str,
    /// The person's own credential — a review is posted as them, never as skein. See [`host_token`].
    ///
    /// A [`crate::secret::Secret`], like every credential that crosses a function boundary in this
    /// crate: a `&str` here would be printed by the `{:?}` of any struct that ever held one.
    pub token: &'a crate::secret::Secret,
}

/// Post one review carrying line comments — the vetted output of `crate::review::critique`.
///
/// `head_sha` is the LIVE head, sent as `commit_id` — always. `drafted_at` is the head the
/// comments were drafted against; empty means "assume current". When they differ, the review is
/// not refused (a dynamically moving PR made that refusal a treadmill — SKEIN-214): the new diff
/// is fetched and each comment is re-anchored by its line's text via [`re_anchor`]. Comments that
/// survive post as line comments at their NEW numbers; the displaced fold into the body under a
/// "Reviewed at {sha} — the branch has moved since" heading, and whenever the head moved at all
/// the body names both commits, because the GitHub record must say what was actually reviewed.
/// A diff that cannot be fetched (the 20k-line 406, a network refusal) displaces every comment
/// rather than failing the post — the review always lands.
pub fn submit_review_with_comments(post: ReviewPost<'_>) -> Result<String, String> {
    let ReviewPost {
        slug,
        number,
        head_sha,
        verdict,
        body,
        comments,
        drafted_at,
        token,
    } = post;
    // A bare approval is a complete statement; anything else with neither words nor comments is a
    // press with nothing behind it.
    if body.trim().is_empty() && comments.is_empty() && verdict != Verdict::Approve {
        return Err("nothing to post — every comment was dropped and the note is empty.".into());
    }
    let moved = !drafted_at.is_empty() && drafted_at != head_sha;
    let (anchored, displaced) = match moved {
        false => (comments.to_vec(), Vec::new()),
        true => match pr_diff_text(slug, number) {
            Ok(diff) => re_anchor(comments, &diff),
            // The owner's ask is that the review always lands: an unreadable diff means no
            // anchor can be trusted, so everything travels in the body instead of a 422 or an
            // error nobody can act on.
            Err(_) => (Vec::new(), comments.to_vec()),
        },
    };
    let mut full = body.trim().to_string();
    if !displaced.is_empty() {
        if !full.is_empty() {
            full.push_str("\n\n");
        }
        full.push_str(&format!(
            "Reviewed at {} — the branch has moved since, and these lines changed:",
            short_sha(drafted_at)
        ));
        for c in &displaced {
            full.push_str(&format!("\n• {}:{} — {}", c.path, c.line, c.body));
        }
    }
    if moved {
        if !full.is_empty() {
            full.push_str("\n\n");
        }
        full.push_str(&format!(
            "(read at {}, posted against {})",
            short_sha(drafted_at),
            short_sha(head_sha)
        ));
    }
    let event = match verdict {
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
        Verdict::Comment => "COMMENT",
    };
    let mut payload = serde_json::json!({
        "event": event,
        "commit_id": head_sha,
        "body": full,
    });
    if !anchored.is_empty() {
        payload["comments"] = anchored
            .iter()
            .map(|c| {
                serde_json::json!({
                    "path": c.path, "line": c.line, "side": "RIGHT", "body": c.body,
                })
            })
            .collect();
    }
    // **Taken rather than looked up**, which every other write in this module already does
    // (`add_label`, `merge_pr`, `update_branch`). It used to call `host_token` here, and nothing
    // was wrong with the answer — `prwork`'s tick and the server's route both source the same one —
    // but a credential a function reaches for is one no caller can see. The test for a verdict step
    // had to set `GH_TOKEN` to make an assertion about a CEILING pass, which is the shape that says
    // a coupling is hidden rather than absent.
    //
    // The rule it enforced is unchanged and lives at the call site now: **a review is posted AS the
    // person**, so what arrives here is their own credential. There is deliberately no second,
    // quieter credential for automation — see `host_token`.
    let path = format!("{}/pulls/{number}/reviews", crate::github::repo_path(slug));
    // **A dead connection here is ambiguous, and that is the whole difference from the read side**
    // (SKEIN-271). Posting a review is not idempotent: the peer cancels the stream after the
    // headers, so GitHub may well have created the review before the answer was lost, and asking
    // again on that evidence posts a second review onto somebody's pull request. Nothing is ever
    // re-sent until [`review_already_landed`] has been asked what actually happened — and when it
    // cannot answer, skein stops and says so rather than guessing in the direction that duplicates.
    let mut landed_first_time = true;
    let mut tries = 0;
    loop {
        tries += 1;
        match crate::github::send_json("POST", &path, token, &payload) {
            Ok(_) => break,
            Err(why) if crate::github::connection_died(&why) => {
                landed_first_time = false;
                match review_already_landed(slug, number, head_sha, &full, token) {
                    // It was created before the stream died. The press succeeded; saying otherwise
                    // would send the person to post it a second time by hand.
                    Ok(true) => break,
                    // GitHub has no such review, so nothing is duplicated by asking again.
                    Ok(false) if tries < 2 => continue,
                    Ok(false) => {
                        return Err(format!(
                            "the connection to GitHub died twice while posting this review, so it \
                             was not posted — skein checked both times and nothing landed, so \
                             nothing is duplicated and it is safe to press again ({why})"
                        ))
                    }
                    Err(look) => {
                        return Err(format!(
                            "the connection to GitHub died while posting this review ({why}), and \
                             skein could not then find out whether it landed ({look}) — open \
                             {slug}#{number} and look before pressing again, because if it did \
                             land, pressing again posts it twice"
                        ))
                    }
                }
            }
            Err(why) => return Err(why),
        }
    }
    let said = match verdict {
        Verdict::Approve => "approved",
        Verdict::RequestChanges => "changes requested",
        Verdict::Comment => "posted the review",
    };
    let mut told = match anchored.len() {
        0 => said.to_string(),
        1 => format!("{said} — with 1 line comment"),
        n => format!("{said} — with {n} line comments"),
    };
    if !displaced.is_empty() {
        told.push_str(&format!(
            " ({} moved into the note — the branch has new commits)",
            displaced.len()
        ));
    }
    if !landed_first_time {
        told.push_str(" — the connection died mid-post, and skein checked GitHub rather than posting it twice");
    }
    Ok(told)
}

/// Is the review skein was posting when the connection died already on GitHub? (SKEIN-271)
///
/// The question a write has to answer before it may ask again. Three facts have to agree, and the
/// third is the one that makes this safe in both directions:
///
/// * the **viewer** wrote it — this token's own login, since another reviewer's review at the same
///   commit says nothing about ours;
/// * the **commit** is the one this post named as `commit_id`;
/// * the **body is byte-for-byte what was sent**. Viewer-and-commit alone is too loose: a person
///   who approved at this head an hour ago and is now leaving comments on it would match, and
///   declining then would silently throw away the review they had just vetted. It is also exact
///   rather than approximate — a stream that dies mid-send truncates the JSON, which GitHub rejects
///   as a 400 rather than storing half a review, so the body GitHub holds is either the whole of
///   what was sent or there is no review at all.
///
/// **`Err` means "could not find out", never "no"** — that is why the pages are followed to the
/// end rather than reading the first thirty. A partial listing that happens not to contain the
/// review is indistinguishable from one that would have, and treating it as absence is precisely
/// the double post this exists to prevent.
fn review_already_landed(
    slug: &str,
    number: u64,
    head_sha: &str,
    body: &str,
    token: &crate::secret::Secret,
) -> Result<bool, String> {
    let login = crate::github::get_json("/user", token)?
        .get("login")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if login.is_empty() {
        return Err("GitHub named no login for this token".into());
    }
    // Line endings are the one thing a round trip may normalise; everything else is compared as
    // sent.
    let same = |a: &str| a.replace("\r\n", "\n") == body.replace("\r\n", "\n");
    const PER_PAGE: usize = 100;
    // Ten pages of a hundred. A pull request with a thousand reviews on it is not a thing, and an
    // unbounded loop on an error path is.
    for page in 1..=10 {
        let listed = crate::github::get_json(
            &format!(
                "{}/pulls/{number}/reviews?per_page={PER_PAGE}&page={page}",
                crate::github::repo_path(slug)
            ),
            token,
        )?;
        let reviews = listed
            .as_array()
            .ok_or("GitHub's answer was not a list of reviews")?;
        if reviews.iter().any(|r| {
            r.pointer("/user/login").and_then(|v| v.as_str()) == Some(login.as_str())
                && r.get("commit_id").and_then(|v| v.as_str()) == Some(head_sha)
                && r.get("body").and_then(|v| v.as_str()).is_some_and(same)
        }) {
            return Ok(true);
        }
        if reviews.len() < PER_PAGE {
            return Ok(false);
        }
    }
    Err("this pull request has more reviews than skein will page through".into())
}

/// **What the author says this change is for** — the pull request's description.
///
/// Skein could not see it. [`PR_FRAGMENT`] asks for `title` and never `body`, so every reading
/// since the first has triaged a change from its title and its diff while the paragraph explaining
/// why it exists sat one field away. The reader could not see it either: the pane draws `title`.
///
/// It is NOT added to that fragment, for the reason its own doc gives — the fragment is asked for
/// up to [`SEARCH_PAGE`] pull requests at a time across every membership rule, so "a body added
/// here is a body multiplied by a hundred", on the request `acme/thing` already answers with
/// a 504. This is the shape that doc names instead: **one pull request's own request**, made only
/// where a body is about to be read.
///
/// **GraphQL, not `GET /repos/{owner}/{repo}/pulls/{n}`**, and the reason is a test rather than a
/// preference: `one head must cost one diff download for both outputs` counts requests to that URL
/// on the wire, and [`pr_diff_text`] is that URL with a diff media type. A body fetched from the
/// same path would read as a second diff download to the one guard that would notice a real one —
/// so it asks a different question at a different address, and the guard keeps meaning what it
/// says.
///
/// GitHub sends `null` for a pull request opened with no description. That is not an error and
/// reads here as empty.
pub fn pr_body(slug: &str, number: u64) -> Result<String, String> {
    let (owner, name) = slug.split_once('/').ok_or("a repo slug is owner/name")?;
    let out = crate::github::graphql(
        "query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) { pullRequest(number: $number) { body } }
}",
        serde_json::json!({ "owner": owner, "name": name, "number": number }),
        &host_token()?,
    )?;
    Ok(out
        .pointer("/repository/pullRequest/body")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string())
}

/// A pull request's diff, as a diff — the media type is the whole of what `gh pr diff` did.
pub fn pr_diff_text(slug: &str, number: u64) -> Result<String, String> {
    let token = host_token()?;
    match crate::github::get_text(
        &format!("{}/pulls/{number}", crate::github::repo_path(slug)),
        &token,
        "application/vnd.github.diff",
    ) {
        Ok(diff) => Ok(diff),
        // **GitHub refuses to serve a diff over 20,000 lines**, and answers 406:
        //
        //     Sorry, the diff exceeded the maximum number of lines (20000)
        //
        // Reported live as a pull request that could not be read at all. That refusal is about
        // SERVING it, not about size being a problem here: `review` truncates every diff to a byte
        // cap before it reaches a model anyway, so a change this big was always going to be read in
        // part. The only thing the 406 actually cost was reading it at all.
        //
        // So it is assembled from the per-file endpoint, which serves the same hunks a file at a
        // time. Marked as assembled, because a reader has to know it is looking at part of a change
        // and not the whole of a small one.
        Err(why) if why.contains("too_large") || why.contains("exceeded the maximum") => {
            assembled_diff(slug, number, &token).map_err(|e| {
                format!(
                    "its diff is too large for GitHub to serve, and the file list would not \
                         read either: {e}"
                )
            })
        }
        Err(why) => Err(why),
    }
}

/// A diff put back together from `/pulls/{n}/files`, for the ones GitHub will not serve whole.
///
/// Each file comes with its own patch, so this is the same text arriving in pieces — with the header
/// lines `diff --git` and `+++` that everything downstream keys on, because `shape` and `contracts`
/// read a diff by those and a stream of bare hunks would parse as nothing.
///
/// A file whose patch GitHub also omits (binary, or too large on its own) is named with its
/// numbers rather than dropped: "this file changed and you cannot see it here" is a fact a reviewer
/// needs, and silence would read as "nothing happened here".
fn assembled_diff(
    slug: &str,
    number: u64,
    token: &crate::secret::Secret,
) -> Result<String, String> {
    // 120s, not the default 30: a hundred files each carrying its own patch is megabytes of JSON,
    // and this runs on the background reader's clock, not a cockpit poll's.
    let files = crate::github::get_json_within(
        &format!(
            "{}/pulls/{number}/files?per_page=100",
            crate::github::repo_path(slug)
        ),
        token,
        std::time::Duration::from_secs(120),
    )?;
    let files = files.as_array().ok_or("GitHub did not list the files")?;
    if files.is_empty() {
        return Err("GitHub listed no files for it".into());
    }
    let mut out = String::new();
    for file in files {
        let name = file
            .get("filename")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)");
        out.push_str(&format!("diff --git a/{name} b/{name}\n"));
        match file.get("patch").and_then(|v| v.as_str()) {
            Some(patch) => {
                out.push_str(&format!("--- a/{name}\n+++ b/{name}\n"));
                out.push_str(patch);
                out.push('\n');
            }
            None => {
                let n = |k: &str| file.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                out.push_str(&format!(
                    "--- a/{name}\n+++ b/{name}\n@@ no patch available @@\n\
                     (+{} -{}, {} — GitHub did not include this file's contents)\n",
                    n("additions"),
                    n("deletions"),
                    file.get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("changed"),
                ));
            }
        }
    }
    // One page. A change across more than a hundred files is not one this tool is helping with, and
    // saying so beats a reader assuming they have seen all of it.
    if files.len() >= 100 {
        out.push_str(
            "\n(this pull request touches more than 100 files; only the first 100 are here)\n",
        );
    }
    Ok(out)
}

/// The paths a pull request touches.
///
/// Its own endpoint rather than parsing the diff for `+++` lines: a rename, a binary file and a
/// mode-only change are all files GitHub names here and none of them appear the way a parser would
/// expect. One page of 100 — a review over that many files is not one this tool is helping with.
pub fn pr_files(slug: &str, number: u64) -> Result<Vec<String>, String> {
    // Same budget as `assembled_diff`, for the same reason: the listing carries each file's patch
    // whether or not the caller wants it, so on a big change this answer is big.
    Ok(crate::github::get_json_within(
        &format!(
            "{}/pulls/{number}/files?per_page=100",
            crate::github::repo_path(slug)
        ),
        &host_token()?,
        std::time::Duration::from_secs(120),
    )?
    .as_array()
    .map(|files| {
        files
            .iter()
            .filter_map(|f| f.get("filename").and_then(|v| v.as_str()))
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default())
}

/// Merge a PR by number. Separate from approving on purpose: with a protected base branch your
/// approval is one of several, and merging is a different decision that is often not yours to make.
///
/// `$SKEIN_MERGE_METHOD` (default `--squash`) picks the method — the same variable the retired
/// box-level merge honoured, so an existing setting keeps working.
///
/// **`expected_head` is not optional, and the empty string is refused rather than sent** (SKEIN-338).
/// It goes out as GitHub's `sha`, which makes the merge conditional on the branch still being what
/// the person read: GitHub answers 409 if somebody pushed in between, and this translates that 409
/// into the sentence a reader can act on instead of leaving GitHub's own wording — *"Head branch
/// was modified"* — to stand for it. Until SKEIN-338 this function sent `merge_method` and nothing
/// else, while `prwork::merge_pr` beside it sent `sha`; the automated merge could not land a
/// revision nobody had looked at and the merge a PERSON pressed could, which is the wrong way round.
///
/// The empty string is refused rather than defaulted to the live head because "assume current" is
/// precisely the hole: a caller with no idea what the reader saw must say so and be stopped, not
/// have skein invent an answer that agrees with whatever GitHub has now. The one caller is
/// [`crate::prwork::merge_by_hand`], which checks the trunk as well — there is no merge in this
/// crate that goes to GitHub past neither guard.
pub fn merge(slug: &str, number: u64, expected_head: &str) -> Result<String, String> {
    if expected_head.trim().is_empty() {
        return Err(format!(
            "skein does not know which commit of #{number} you are looking at, and will not merge \
             a revision it cannot name. Refresh the queue and read the change again."
        ));
    }
    let method = std::env::var("SKEIN_MERGE_METHOD")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "--squash".into());
    // The same values, without the leading dashes `gh` wanted. An unrecognised setting is refused
    // here rather than sent: GitHub answers a bad `merge_method` with a 422 whose message is about
    // JSON, which reads as a skein bug.
    let method = match method.trim().trim_start_matches("--") {
        "squash" => "squash",
        "merge" => "merge",
        "rebase" => "rebase",
        other => {
            return Err(format!(
                "$SKEIN_MERGE_METHOD is {other:?}; GitHub takes squash, merge or rebase"
            ))
        }
    };
    let out = crate::github::send_json(
        "PUT",
        &format!("{}/pulls/{number}/merge", crate::github::repo_path(slug)),
        &host_token()?,
        // `sha` is the head the person read. Same field, same reason, as `prwork::merge_pr`: GitHub
        // refuses with a 409 if the branch has moved, and a merge decided about code that is no
        // longer what would be merged is the one outcome this whole path exists to prevent.
        &serde_json::json!({ "merge_method": method, "sha": expected_head }),
    )
    // Two translations, and they cannot both fire: one is gated on a 409 and the other on a 405,
    // so the order here is readability and nothing else.
    .map_err(|e| the_branch_moved(number, expected_head, e))
    .map_err(|e| it_conflicts_with_its_base(number, e))?;
    Ok(out
        .get("message")
        .and_then(|m| m.as_str())
        .filter(|m| !m.trim().is_empty())
        .unwrap_or("merged")
        .to_string())
}

/// GitHub's 409 on a conditional merge, said in the reader's terms.
///
/// **Matched on the status skein itself formatted, not on GitHub's prose.** `crate::github` turns a
/// non-2xx into one of two sentences — `format!("GitHub said {status}: {m}")` when the body carries
/// a `message`, and `complaint`'s `format!("GitHub answered {status}: …")` when it does not — so
/// those two prefixes are the whole surface, and both are checked. Reading GitHub's own words
/// (*"Head branch was modified. Review and try the merge again."*) instead would be a string match
/// on somebody else's copy, which changes without notice and would fail open into "merge failed,
/// unclear why" on the one act that cannot be taken back.
///
/// Every other error is passed through untouched: a 405 (not mergeable), a 422, a rate limit and a
/// dead connection are all real answers and none of them mean the branch moved. The 405 that names
/// conflicts is [`it_conflicts_with_its_base`] below — a second translation on the same rule, never
/// a widening of this one.
fn the_branch_moved(number: u64, expected_head: &str, said: String) -> String {
    let conflict = said.starts_with("GitHub said 409") || said.starts_with("GitHub answered 409");
    match conflict {
        false => said,
        true => format!(
            "the branch moved since you read it — #{number} is no longer at {}, so nothing was \
             merged. Read the new code, then merge.",
            short_sha(expected_head)
        ),
    }
}

/// GitHub's 405 on a merge it will not attempt, when the reason is conflicts (SKEIN-411).
///
/// A reader pressing merge on a conflicted pull request was shown `GitHub said 405: Pull Request
/// has merge conflicts` — measured on `acme/testbed#20` and quoted in SKEIN-385's
/// commit. That is a status code and somebody else's noun phrase, and it does not say what to do.
///
/// **The status is the gate, and it is one skein formatted itself.** Same rule as
/// [`the_branch_moved`] above, same two prefixes: `crate::github` turns a non-2xx into `GitHub said
/// {status}: {m}` or `complaint`'s `GitHub answered {status}: …` and nothing else, so matching
/// those is matching skein's own words.
///
/// **GitHub's prose narrows WITHIN that status; it never opens the gate.** A 405 on a merge means
/// "not mergeable", which is more than one situation — conflicts, a draft, a blocking rule — and
/// only conflicts are answered by going and resolving conflicts. The `message` is the one thing
/// that tells them apart, so it is read, and it is read for the word alone rather than for the
/// whole sentence. If that copy changes, a 405 stops matching and the reader gets the raw sentence
/// they get today: the failure available here is the one that under-translates, and telling
/// somebody to resolve conflicts that are not there is not.
///
/// Every other status is untouched. A 409 is the branch moving, a 422, a rate limit and a dead
/// connection are all real answers, and none of them are conflicts.
fn it_conflicts_with_its_base(number: u64, said: String) -> String {
    match refused_for_conflicts(&said) {
        false => said,
        true => format!(
            "#{number} has conflicts with its base, so GitHub will not merge it until they are \
             resolved. Resolve them on the branch, push, then merge."
        ),
    }
}

/// Is this GitHub's refusal to merge a branch that conflicts with its base? (SKEIN-411, SKEIN-423)
///
/// The gate [`it_conflicts_with_its_base`] above states in full, and nothing but the gate — the
/// rule written out there is what this holds, and the paragraphs there are its documentation.
///
/// **Shared because the wire shape is one fact and the sentence is two.** There are two merges in
/// this crate: [`merge`], which a person presses, and `prwork::merge_pr`, which a train drives, and
/// SKEIN-423 is the second one arriving at the same 405. What they must agree about is which
/// answers from GitHub *are* this refusal — that is knowledge of `crate::github`'s two wrappers,
/// and a second copy of it is a second thing to miss when a wrapper changes. What they must NOT
/// share is the words: "resolve them on the branch, push, then merge" is advice to somebody
/// standing at a button, and the reader of a stop is somebody who was not watching, reading later.
/// So the predicate is `pub(crate)` and each caller writes its own sentence
/// (`prwork::conflicts_stopped_the_train` is the other one).
pub(crate) fn refused_for_conflicts(said: &str) -> bool {
    let refused = said.starts_with("GitHub said 405") || said.starts_with("GitHub answered 405");
    refused && said.to_ascii_lowercase().contains("conflict")
}

/// Mark a review thread resolved on GitHub (SKEIN-305).
///
/// `thread_id` is [`ReviewThread::id`] — GitHub's node id, which is exactly the argument
/// `resolveReviewThread` takes, so this needs no lookup and no slug. That is why the whole
/// conversation carries thread ids: the panel draws a thread from its author, time and permalink,
/// and the id is the one field on it that exists only so this call can be made.
///
/// **Here rather than in the caller**, because this is the only module that may reach GitHub:
/// [`crate::github::graphql`] is `pub(crate)`, so `src/bin/skein-server.rs` is a different crate
/// and cannot call it, and `review` reaching `github` is an edge `docs/modules.toml` does not
/// declare — `tools/module-check.py` fails on it. `prq -> github` is declared, so this is where it
/// goes.
///
/// **No queue read.** A thread id and nothing else, so a resolve costs one request and cannot
/// inherit the ways a refresh fails (the SKEIN-272 lesson, in the form it takes here). Invalidating
/// the cached queue afterwards is the caller's, on the same rule as every other act that touched
/// GitHub.
pub fn resolve_review_thread(thread_id: &str) -> Result<(), String> {
    set_thread_resolved(thread_id, true)
}

/// The inverse, so the panel's eight-second undo (SKEIN-162) is a real retraction rather than a
/// row that redraws itself while GitHub still says resolved.
pub fn unresolve_review_thread(thread_id: &str) -> Result<(), String> {
    set_thread_resolved(thread_id, false)
}

/// The one mutation both directions send, with only its name and the state it asserts differing.
///
/// **[`crate::github::graphql`], never `graphql_partial`.** The sibling asks a dead connection
/// again, and its own doc says why that must not carry a mutation: an ambiguous failure may be one
/// that already ran. It is also the half that fails the whole request on any `errors` entry, which
/// is what this needs — an `Ok(())` on a GraphQL error would let the undo window close over a
/// resolve that never happened.
///
/// GitHub's answer is read back rather than discarded, on the rule this file uses everywhere: what
/// GitHub SAID is used, and what it did not say is not invented. `isResolved` coming back against
/// what was asked is a write that did not take, and it is reported as one; `isResolved` absent is
/// not a contradiction, so it is accepted.
fn set_thread_resolved(thread_id: &str, resolved: bool) -> Result<(), String> {
    if thread_id.trim().is_empty() {
        return Err("no review thread was named, so there is nothing to resolve".into());
    }
    let field = match resolved {
        true => "resolveReviewThread",
        false => "unresolveReviewThread",
    };
    let query = format!(
        "mutation($id: ID!) {{\n\
        \x20 {field}(input: {{threadId: $id}}) {{ thread {{ id isResolved }} }}\n\
        }}"
    );
    let out = crate::github::graphql(
        &query,
        serde_json::json!({ "id": thread_id }),
        &host_token()?,
    )?;
    let said = out
        .get(field)
        .and_then(|v| v.get("thread"))
        .and_then(|t| t.get("isResolved"))
        .and_then(serde_json::Value::as_bool);
    match said {
        Some(is) if is != resolved => Err(format!(
            "GitHub accepted the {} and reports the thread as {}",
            match resolved {
                true => "resolve",
                false => "unresolve",
            },
            match is {
                true => "still resolved",
                false => "still open",
            }
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prq::fixtures::wired;

    /// The credential every stub GitHub below is called with.
    ///
    /// Prefixed `skein-test-` deliberately: a fixture that looked like a real token
    /// (`gho_…`, `ghp_…`) is indistinguishable from one in a grep, and this tree has already had
    /// to sweep a client's real strings out of its fixtures once.
    fn fixture_token() -> crate::secret::Secret {
        crate::secret::Secret::new("skein-test-github-token")
    }

    /// A pull request too big for GitHub to serve a diff for is still readable.
    ///
    /// Reported live:
    ///
    /// ```text
    /// not summarised — its diff could not be read: GitHub answered 406: {"message":"Sorry, the
    /// diff exceeded the maximum number of lines (20000)", … "code":"too_large"}
    /// ```
    ///
    /// GitHub declines to SERVE a diff over 20,000 lines. That is not the same as the size being a
    /// problem here — `review` truncates every diff to a byte cap before a model sees it, so a
    /// change this big was always going to be read in part. The 406 cost reading it at all.
    ///
    /// Assembled from `/files` instead, and the shape matters as much as the content: everything
    /// downstream reads a diff by its `diff --git` and `+++` lines, so a stream of bare hunks would
    /// parse as an empty change and summarise as "nothing here".
    #[test]
    fn a_diff_too_large_to_serve_is_assembled_from_its_files() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        forget_host_token();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                use std::io::{Read as _, Write as _};
                let mut stream = stream;
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                // GitHub's own answer, word for word.
                let (status, answer) = if head.contains("/files") {
                    (
                        200,
                        r#"[{"filename":"src/a.rs","status":"modified","additions":2,"deletions":1,
                             "patch":"@@ -1,3 +1,4 @@\n kept\n-old\n+new\n+more"},
                           {"filename":"assets/logo.png","status":"modified","additions":0,
                             "deletions":0}]"#
                            .to_string(),
                    )
                } else {
                    (
                        406,
                        r#"{"message":"Sorry, the diff exceeded the maximum number of lines (20000)",
                            "errors":[{"resource":"PullRequest","field":"diff","code":"too_large"}]}"#
                            .to_string(),
                    )
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        env.set("SKEIN_GITHUB_API", &base);

        let diff =
            pr_diff_text("acme/thing", 7).expect("a diff GitHub will not serve is still read");

        // The headers everything downstream keys on. Without them `shape` and `contracts` read this
        // as an empty change, and the pull request summarises as though nothing had happened in it.
        assert!(
            diff.contains("diff --git a/src/a.rs b/src/a.rs") && diff.contains("+++ b/src/a.rs"),
            "the assembled diff is not shaped like a diff: {diff}"
        );
        assert!(diff.contains("+new"), "the hunk itself was dropped: {diff}");

        // A file GitHub gave no patch for is NAMED, with its numbers. Dropping it would read as
        // "nothing happened here", and a binary asset changing is a thing a reviewer wants to know.
        assert!(
            diff.contains("assets/logo.png") && diff.contains("no patch available"),
            "a file with no patch vanished instead of being named: {diff}"
        );

        forget_host_token();
    }

    // ---- SKEIN-214: a review drafted against one commit still lands after the branch moves ----

    /// A comment as the reading view drafts it: `text` is the line the reviewer was looking at.
    fn drafted(path: &str, line: u64, body: &str, text: &str) -> ReviewComment {
        ReviewComment {
            path: path.into(),
            line,
            body: body.into(),
            text: text.into(),
        }
    }

    /// The PR after one more push: one line replaced by two above `fn target() {}`, so everything
    /// below shifted down, and the old `fn gone() {}` no longer exists. The `-` line is
    /// deliberate: it must NOT advance the new-file counter, and only a diff that has one can
    /// catch a counter that thinks otherwise.
    const MOVED_DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs\n\
                              --- a/src/lib.rs\n\
                              +++ b/src/lib.rs\n\
                              @@ -1,4 +1,5 @@\n \
                              fn keep() {}\n\
                              -fn old() {}\n\
                              +fn added() {}\n\
                              +fn extra() {}\n \
                              fn target() {}\n \
                              tail\n";

    /// **A merge with no expected head never reaches the network.**
    ///
    /// The backstop, one layer below `prwork::merge_by_hand`'s own refusal, and it is here rather
    /// than only there because this is the function holding the `PUT`. Until SKEIN-338 it sent
    /// `{"merge_method": …}` and nothing else, so every caller — present and future — merged
    /// whatever HEAD happened to be. Making the argument required is only half of that; refusing
    /// the empty string is the other half, because `""` is what a caller with no idea passes.
    ///
    /// `$SKEIN_GITHUB_API` points at an address nothing is listening on, so the assertion is not
    /// "it returned an error" — it would do that anyway — but that the error is the guard's and not
    /// a connection's. A refusal that had gone to the wire would say so.
    #[test]
    fn a_merge_that_cannot_name_a_commit_is_refused_before_the_wire() {
        let _g = crate::testutil::env_lock();
        // Port 1 on loopback: nothing listens there, so any request at all fails loudly and
        // differently from the refusal being asserted.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_GITHUB_API", "http://127.0.0.1:1");
        env.set("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        forget_host_token();

        for empty in ["", " ", "\t", "\n  "] {
            let why = merge("acme/thing", 41, empty).unwrap_err();
            assert!(
                why.contains("which commit") && why.contains("#41"),
                "a merge with head {empty:?} was refused for the wrong reason — this reads like it \
                 reached GitHub: {why}"
            );
            assert!(
                !why.contains("GitHub"),
                "a merge that could not name a commit was still sent: {why}"
            );
        }

        forget_host_token();
    }

    /// **Only a 409 becomes "the branch moved", and it never says 409.**
    ///
    /// The translation is matched against the sentences `crate::github` itself formats — `GitHub
    /// said {status}: …` from `json`, `GitHub answered {status}: …` from `complaint` — rather than
    /// against GitHub's prose, which is somebody else's copy and changes without notice. So the
    /// thing worth asserting is that the match is exact in both directions: every other status a
    /// merge can draw passes through whole, because a 405 (not mergeable), a 422 and a rate limit
    /// are real answers and none of them mean somebody pushed.
    ///
    /// A status this turned into "the branch moved" wrongly would send a reader to re-read a change
    /// that was never the problem; one it failed to translate would leave the single most likely
    /// merge failure reading as an unexplained error.
    #[test]
    fn only_a_conflict_is_reported_as_the_branch_having_moved() {
        // Both shapes `crate::github` produces, across the statuses a merge actually draws.
        for status in [401, 403, 404, 405, 409, 422, 500, 502] {
            for said in [
                format!("GitHub said {status}: Head branch was modified. Review and try the merge again."),
                format!("GitHub answered {status}: <html>no</html>"),
            ] {
                let out = the_branch_moved(41, "abc1234def", said.clone());
                let translated = out != said;
                assert_eq!(
                    translated,
                    status == 409,
                    "status {status} was {} translated into \"the branch moved\": {out}",
                    match translated {
                        true => "wrongly",
                        false => "not",
                    }
                );
                if translated {
                    assert!(
                        out.contains("moved since you read it")
                            && out.contains("abc1234")
                            && out.contains("#41"),
                        "the translation lost the pull request or the commit it was read at: {out}"
                    );
                    assert!(
                        !out.contains("409"),
                        "the raw status survived into the reader's sentence: {out}"
                    );
                }
            }
        }

        // Not a status at all — a dead connection, a rate limit — is untouched. There is no number
        // in these, and inventing a merge conflict out of one would be the worst kind of guess.
        for said in [
            "GitHub sent nothing at all".to_string(),
            "GitHub is rate limiting skein — resuming in about 15m".to_string(),
            "the 409 in this sentence is not a status".to_string(),
        ] {
            assert_eq!(
                the_branch_moved(41, "abc1234def", said.clone()),
                said,
                "an answer that was not a 409 was reported as the branch moving"
            );
        }
    }

    /// **Only a 405 that names conflicts becomes a sentence about conflicts, and it never says
    /// 405.** (SKEIN-411)
    ///
    /// Two things have to hold at once and they pull in opposite directions. The status is the
    /// gate — a 409, a 422 or a rate limit whose body happens to contain the word "conflict" must
    /// not be turned into "go and resolve conflicts", because none of them are that. And the gate
    /// is not enough on its own — a 405 is GitHub's answer to every kind of "not mergeable", so a
    /// draft or a blocking rule must still arrive verbatim rather than sending the reader to look
    /// for conflicts that do not exist.
    #[test]
    fn only_a_405_naming_conflicts_is_reported_as_conflicts_with_the_base() {
        // GitHub's own words for a conflicted merge, measured on the testbed. Both shapes
        // `crate::github` wraps them in, across the statuses a merge actually draws.
        for status in [401, 403, 404, 405, 409, 422, 500, 502] {
            for said in [
                format!("GitHub said {status}: Pull Request has merge conflicts"),
                format!("GitHub answered {status}: <html>merge conflicts</html>"),
            ] {
                let out = it_conflicts_with_its_base(41, said.clone());
                let translated = out != said;
                assert_eq!(
                    translated,
                    status == 405,
                    "status {status} was {} translated into a sentence about conflicts: {out}",
                    match translated {
                        true => "wrongly",
                        false => "not",
                    }
                );
                if translated {
                    assert!(
                        out.contains("conflicts with its base") && out.contains("#41"),
                        "the translation lost the pull request or what is wrong with it: {out}"
                    );
                    assert!(
                        out.contains("resolved") || out.contains("Resolve"),
                        "the reader was told what is wrong and not what to do about it: {out}"
                    );
                    assert!(
                        !out.contains("405"),
                        "the raw status survived into the reader's sentence: {out}"
                    );
                }
            }
        }

        // A 405 that is not about conflicts. GitHub answers every unmergeable pull request with
        // this status, and only one of the reasons is fixed by resolving anything.
        for said in [
            "GitHub said 405: Pull Request is not mergeable".to_string(),
            "GitHub said 405: Base branch was modified".to_string(),
            "GitHub answered 405: <html>no</html>".to_string(),
        ] {
            assert_eq!(
                it_conflicts_with_its_base(41, said.clone()),
                said,
                "a 405 that says nothing about conflicts was reported as a conflict"
            );
        }

        // Not a status at all, and the word appearing anywhere else. `the_branch_moved`'s own
        // sentence is the one that matters here: the two translations run one after the other on
        // the same merge, and the first one's output must not be eaten by the second.
        for said in [
            "GitHub sent nothing at all".to_string(),
            "the branch moved since you read it — #41 is no longer at abc1234, so nothing was \
             merged. Read the new code, then merge."
                .to_string(),
            "the 405 in this sentence is not a status, and neither is this conflict".to_string(),
        ] {
            assert_eq!(
                it_conflicts_with_its_base(41, said.clone()),
                said,
                "an answer that was not a 405 was reported as conflicts with the base"
            );
        }
    }

    #[test]
    fn re_anchor_keeps_an_unmoved_line_at_its_number() {
        let (kept, gone) = re_anchor(
            &[drafted("src/lib.rs", 1, "note", "fn keep() {}")],
            MOVED_DIFF,
        );
        assert!(gone.is_empty());
        assert_eq!((kept[0].line, kept[0].path.as_str()), (1, "src/lib.rs"));
    }

    #[test]
    fn re_anchor_follows_a_line_pushed_down_by_an_insertion_above() {
        // Drafted at line 3; one line above became two, so it now lives at 4 — and the `-` line
        // between must not be counted on the way there.
        let (kept, gone) = re_anchor(
            &[drafted("src/lib.rs", 3, "note", "fn target() {}")],
            MOVED_DIFF,
        );
        assert!(
            gone.is_empty(),
            "the line still exists and was displaced anyway"
        );
        assert_eq!(
            kept[0].line, 4,
            "the comment did not follow its line to its new number"
        );
        assert_eq!(kept[0].body, "note", "the body must travel untouched");
    }

    #[test]
    fn re_anchor_prefers_the_duplicate_nearest_the_old_line_and_the_earlier_on_a_tie() {
        let twice = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
                     @@ -1,9 +1,9 @@\n one\n+same\n three\n four\n five\n six\n+same\n eight\n nine\n";
        // `+same` sits at new lines 2 and 7. Old line 8 → 7 is nearer than 2.
        let (kept, _) = re_anchor(&[drafted("a.rs", 8, "n", "same")], twice);
        assert_eq!(kept[0].line, 7, "nearest-to-old did not win");
        // Old line 4 or 5 is a near-tie; make it exact: |2-4|=2 vs |7-4|=3 → 2. And a true tie —
        // candidates 2 and 7 from old line 4.5 cannot be written, so test equidistance directly:
        // old line at the midpoint via a diff whose duplicates sit at 2 and 6, old 4 → tie → earlier.
        let tie = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
                   @@ -1,7 +1,7 @@\n one\n+same\n three\n four\n five\n+same\n seven\n";
        let (kept, _) = re_anchor(&[drafted("a.rs", 4, "n", "same")], tie);
        assert_eq!(kept[0].line, 2, "a tie must break toward the earlier line");
    }

    #[test]
    fn re_anchor_displaces_a_deleted_line() {
        let (kept, gone) = re_anchor(&[drafted("src/lib.rs", 9, "n", "fn gone() {}")], MOVED_DIFF);
        assert!(
            kept.is_empty(),
            "anchored a comment to a line that no longer exists"
        );
        assert_eq!(gone.len(), 1);
        assert_eq!(
            gone[0].line, 9,
            "the displaced comment must keep its original coordinates"
        );
    }

    #[test]
    fn re_anchor_displaces_a_comment_with_no_text_to_search_for() {
        // `text` empty means an old client or a draft that never captured the line — matching
        // by nothing would anchor everywhere, so it anchors nowhere.
        let (kept, gone) = re_anchor(&[drafted("src/lib.rs", 1, "n", "")], MOVED_DIFF);
        assert!(kept.is_empty() && gone.len() == 1);
    }

    #[test]
    fn re_anchor_displaces_a_comment_on_a_file_the_new_diff_no_longer_touches() {
        // Same text exists — in a DIFFERENT file. Text matching never crosses paths.
        let (kept, gone) = re_anchor(
            &[drafted("src/other.rs", 1, "n", "fn keep() {}")],
            MOVED_DIFF,
        );
        assert!(kept.is_empty() && gone.len() == 1);
    }

    /// A GitHub for the moved-head posting path: serves one PR's diff (or refuses with a 500 when
    /// `diff` is `None`), answers every POST with `{}`, and records `"METHOD path body"` — the
    /// wire is the thing under test, exactly as `fake_github` argues above.
    fn reanchor_github(
        diff: Option<&'static str>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let mut parts = request.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut length = 0usize;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let mut body = vec![0u8; length];
                if length > 0 {
                    reader.read_exact(&mut body).ok();
                }
                let body = String::from_utf8_lossy(&body).into_owned();
                recorder
                    .lock()
                    .unwrap()
                    .push(format!("{method} {path} {body}"));
                let (status, answer) = match (method.as_str(), diff) {
                    ("POST", _) => (200, "{}".to_string()),
                    (_, Some(d)) => (200, d.to_string()),
                    (_, None) => (500, r#"{"message":"boom"}"#.to_string()),
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    /// The recorded review POST, parsed. Panics with the whole record if none was made.
    fn posted_review(seen: &std::sync::Mutex<Vec<String>>) -> serde_json::Value {
        let seen = seen.lock().unwrap();
        let post = seen
            .iter()
            .find(|r| r.starts_with("POST "))
            .unwrap_or_else(|| panic!("no review reached GitHub: {seen:?}"));
        serde_json::from_str(post.splitn(3, ' ').nth(2).unwrap()).unwrap()
    }

    /// The commit every SKEIN-271 test below posts against, and the review the viewer had already
    /// left on it before any of this — the decoy that makes "a review by me at this head" the
    /// wrong test to write.
    const POST_HEAD: &str = "cccccccc333333333333333333333333333333333";
    const DECOY: &str = "I approved this an hour ago";

    /// A GitHub whose review POST dies MID-ANSWER, the way a live one did (SKEIN-271).
    ///
    /// `creates_before_dying` is the ambiguity itself: GitHub cancels the stream after the headers,
    /// so from skein's side "the review exists" and "the review does not exist" are the same
    /// failure. Both halves are served from the reviews this fixture actually holds — seeded with
    /// [`DECOY`], a review by the same viewer at the same commit — so a test reads exactly the
    /// evidence skein reads. `lookup_dies` is the third case: the connection is gone and stays
    /// gone, so the question cannot be answered at all.
    ///
    /// Returns the request record and the reviews GitHub ends up holding.
    #[allow(clippy::type_complexity)]
    fn dying_review_github(
        deaths: usize,
        creates_before_dying: bool,
        lookup_dies: bool,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let held = std::sync::Arc::new(std::sync::Mutex::new(vec![serde_json::json!({
            "user": { "login": "me" }, "commit_id": POST_HEAD, "body": DECOY,
        })]));
        let (recorder, reviews) = (seen.clone(), held.clone());
        std::thread::spawn(move || {
            let mut posts = 0usize;
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let mut parts = request.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut length = 0usize;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let mut body = vec![0u8; length];
                if length > 0 {
                    reader.read_exact(&mut body).ok();
                }
                let body = String::from_utf8_lossy(&body).into_owned();
                recorder
                    .lock()
                    .unwrap()
                    .push(format!("{method} {path} {body}"));
                // A length promised and not delivered, then the socket goes: curl exits non-zero
                // with no status and no body.
                let die = |stream: &mut std::net::TcpStream| {
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\nhalf an ans");
                    let _ = stream.flush();
                };
                let posting = method == "POST" && path.ends_with("/reviews");
                let listing = method == "GET" && path.contains("/reviews");
                let keep = |body: &str| {
                    let sent: serde_json::Value = serde_json::from_str(body).unwrap();
                    reviews.lock().unwrap().push(serde_json::json!({
                        "user": { "login": "me" },
                        "commit_id": sent["commit_id"],
                        "body": sent["body"],
                    }));
                };
                if posting {
                    posts += 1;
                    if posts <= deaths {
                        if creates_before_dying {
                            keep(&body);
                        }
                        die(&mut stream);
                        continue;
                    }
                    keep(&body);
                }
                if listing && lookup_dies {
                    die(&mut stream);
                    continue;
                }
                let answer = match (posting, listing, path.as_str()) {
                    (true, _, _) => "{}".to_string(),
                    (_, true, _) => {
                        serde_json::Value::Array(reviews.lock().unwrap().clone()).to_string()
                    }
                    (_, _, "/user") => r#"{"login":"me"}"#.to_string(),
                    _ => "{}".to_string(),
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (format!("http://127.0.0.1:{port}"), seen, held)
    }

    /// How many reviews actually reached GitHub, and how many times skein pressed.
    fn posts_and_reviews(
        seen: &std::sync::Mutex<Vec<String>>,
        held: &std::sync::Mutex<Vec<serde_json::Value>>,
    ) -> (usize, usize) {
        let posts = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("POST "))
            .count();
        (posts, held.lock().unwrap().len())
    }

    /// **The one SKEIN-271 exists to prevent.** The stream dies AFTER GitHub created the review, so
    /// the failure skein sees is indistinguishable from one where nothing happened. A blind retry
    /// — which is what the read half does, correctly, for a query — posts your review onto their
    /// pull request twice. Skein must go and look instead, find it, and stop.
    #[test]
    fn a_review_created_before_the_stream_died_is_found_rather_than_posted_again() {
        let (base, seen, held) = dying_review_github(1, true, false);
        let _env = wired(&base);

        let said = submit_review_with_comments(ReviewPost {
            slug: "acme/thing",
            number: 7,
            head_sha: POST_HEAD,
            verdict: Verdict::Comment,
            body: "looks fine",
            comments: &[drafted("src/lib.rs", 2, "tighten this", "fn target() {}")],
            drafted_at: POST_HEAD,
            token: &fixture_token(),
        })
        .expect("a review that GitHub already holds is a success, not a failure to report");

        let (posts, reviews) = posts_and_reviews(&seen, &held);
        assert_eq!(
            posts, 1,
            "the review was posted onto the pull request twice"
        );
        assert_eq!(
            reviews, 2,
            "GitHub holds more than the decoy and the one review that was meant"
        );
        assert!(
            said.contains("posted the review") && said.contains("connection died"),
            "the answer must say it landed AND that skein had to go and check: {said}"
        );
    }

    /// The other half of the same ambiguity: the stream died before GitHub created anything, so
    /// there is nothing to find and the review must actually be posted. The decoy is what makes
    /// this a real test — a review by the same viewer at the same commit is already there, and
    /// matching on that alone would silently discard the review the person had just vetted.
    #[test]
    fn a_review_the_dead_stream_never_created_is_posted_on_the_second_attempt() {
        let (base, seen, held) = dying_review_github(1, false, false);
        let _env = wired(&base);

        submit_review_with_comments(ReviewPost {
            slug: "acme/thing",
            number: 7,
            head_sha: POST_HEAD,
            verdict: Verdict::Comment,
            body: "looks fine",
            comments: &[],
            drafted_at: POST_HEAD,
            token: &fixture_token(),
        })
        .expect("nothing landed, so the review must be posted rather than declined");

        let (posts, reviews) = posts_and_reviews(&seen, &held);
        assert_eq!(posts, 2, "the retry never happened");
        assert_eq!(reviews, 2, "the vetted review never reached GitHub");
        assert!(
            held.lock()
                .unwrap()
                .iter()
                .any(|r| r["body"] == "looks fine"),
            "the review that landed is not the one that was written"
        );
    }

    /// When the ambiguity cannot be resolved, skein stops. A second press might be a duplicate and
    /// might be the only copy, and the one thing it must not do is choose for you in the
    /// direction that writes.
    #[test]
    fn a_post_that_cannot_be_verified_refuses_to_press_again_and_says_where_to_look() {
        let (base, seen, held) = dying_review_github(1, true, true);
        let _env = wired(&base);

        let why = submit_review_with_comments(ReviewPost {
            slug: "acme/thing",
            number: 7,
            head_sha: POST_HEAD,
            verdict: Verdict::Comment,
            body: "looks fine",
            comments: &[],
            drafted_at: POST_HEAD,
            token: &fixture_token(),
        })
        .expect_err("an unresolvable ambiguity is not a success");

        let (posts, _) = posts_and_reviews(&seen, &held);
        assert_eq!(
            posts, 1,
            "skein pressed again without knowing what happened"
        );
        assert!(
            why.contains("connection to GitHub died") && why.contains("acme/thing#7"),
            "the reader is not told what happened or where to look: {why}"
        );
        assert!(
            why.contains("pressing again posts it twice"),
            "the reader is not told what the risk of pressing again is: {why}"
        );
    }

    /// SKEIN-214, the whole ask on one wire: the branch moved after drafting, and the review still
    /// lands — the comment whose line survives follows it to its NEW number, the one whose line
    /// changed folds into the body naming the commit it was read at, `commit_id` is the LIVE head,
    /// and the body says read-at/posted-against so the GitHub record is honest about what was
    /// actually reviewed.
    #[test]
    fn a_review_of_a_moved_branch_lands_with_reanchored_lines_and_an_honest_body() {
        let (base, seen) = reanchor_github(Some(MOVED_DIFF));
        let _env = wired(&base);

        let drafted_at = "aaaaaaa1111111111111111111111111111111111";
        let live_head = "bbbbbbb2222222222222222222222222222222222";
        let said = submit_review_with_comments(ReviewPost {
            slug: "acme/thing",
            number: 7,
            head_sha: live_head,
            verdict: Verdict::Comment,
            body: "overall: fine",
            comments: &[
                drafted("src/lib.rs", 3, "tighten this", "fn target() {}"),
                drafted("src/lib.rs", 9, "dead code?", "fn gone() {}"),
            ],
            drafted_at,
            token: &fixture_token(),
        })
        .expect("a moved branch must not make the review unpostable");

        let payload = posted_review(&seen);
        assert_eq!(
            payload["commit_id"], *live_head,
            "commit_id must be the live head, never the drafted one"
        );
        let comments = payload["comments"].as_array().unwrap();
        assert_eq!(
            comments.len(),
            1,
            "the displaced comment leaked into the line comments"
        );
        assert_eq!(
            (comments[0]["line"].as_u64(), comments[0]["side"].as_str()),
            (Some(4), Some("RIGHT")),
            "the surviving comment did not move to its new line number"
        );
        assert_eq!(comments[0]["body"], "tighten this");
        let body = payload["body"].as_str().unwrap();
        assert!(
            body.contains(
                "Reviewed at aaaaaaa — the branch has moved since, and these lines changed:"
            ),
            "the displaced heading is missing: {body}"
        );
        assert!(
            body.contains("• src/lib.rs:9 — dead code?"),
            "the displaced comment's bullet is missing: {body}"
        );
        assert!(
            body.contains("(read at aaaaaaa, posted against bbbbbbb)"),
            "the record does not say what was actually reviewed: {body}"
        );
        assert!(
            said.contains("1 line comment"),
            "the answer under-reports: {said}"
        );
    }

    /// The unmoved case pays nothing: same head → no diff fetch, and the payload is byte-for-byte
    /// today's shape — no heading, no read-at line, the drafted numbers as given.
    #[test]
    fn a_review_of_an_unmoved_branch_posts_exactly_as_before() {
        let (base, seen) = reanchor_github(Some(MOVED_DIFF));
        let _env = wired(&base);

        let head = "cccccccc333333333333333333333333333333333";
        submit_review_with_comments(ReviewPost {
            slug: "acme/thing",
            number: 7,
            head_sha: head,
            verdict: Verdict::Comment,
            body: "looks fine",
            comments: &[drafted("src/lib.rs", 2, "tighten this", "fn target() {}")],
            drafted_at: head,
            token: &fixture_token(),
        })
        .unwrap();

        assert_eq!(
            posted_review(&seen),
            serde_json::json!({
                "event": "COMMENT",
                "commit_id": head,
                "body": "looks fine",
                "comments": [
                    { "path": "src/lib.rs", "line": 2, "side": "RIGHT", "body": "tighten this" }
                ],
            }),
            "the unmoved payload must be identical to the pre-SKEIN-214 shape"
        );
        let requests = seen.lock().unwrap().clone();
        assert_eq!(
            requests.len(),
            1,
            "an unmoved head must cost no diff fetch: {requests:?}"
        );
    }

    /// A diff GitHub will not serve (the 20k-line 406, a network refusal) displaces EVERY comment
    /// into the body — the review lands anyway, because "post it" was the whole of the ask, and an
    /// error here would strand a finished review behind an unreadable diff.
    #[test]
    fn an_unfetchable_diff_moves_every_comment_into_the_body_and_still_posts() {
        let (base, seen) = reanchor_github(None);
        let _env = wired(&base);

        submit_review_with_comments(ReviewPost {
            slug: "acme/thing",
            number: 7,
            head_sha: "bbbbbbb2222222222222222222222222222222222",
            verdict: Verdict::Comment,
            body: "",
            comments: &[
                drafted("src/lib.rs", 2, "tighten this", "fn target() {}"),
                drafted("src/lib.rs", 9, "dead code?", "fn gone() {}"),
            ],
            drafted_at: "aaaaaaa1111111111111111111111111111111111",
            token: &fixture_token(),
        })
        .expect("an unreadable diff must not make the review unpostable");

        let payload = posted_review(&seen);
        assert!(
            payload.get("comments").is_none(),
            "with no diff to anchor against, no line comment can be trusted: {payload}"
        );
        let body = payload["body"].as_str().unwrap();
        assert!(
            body.contains("• src/lib.rs:2 — tighten this")
                && body.contains("• src/lib.rs:9 — dead code?"),
            "a comment vanished instead of riding in the body: {body}"
        );
        assert!(body.contains("(read at aaaaaaa, posted against bbbbbbb)"));
    }

    /// The fallback [`head_to_post_against`] uses, now that the post no longer refreshes the
    /// queue to produce one (SKEIN-272). It is what this machine already remembers, read from
    /// disk — and the point is what it must NOT be: the sha the draft was read at. Handing that in
    /// as its own fallback makes "did the branch move" compare a value against itself, nothing
    /// re-anchors, and vetted comments post at line numbers computed against a diff that no longer
    /// exists (SKEIN-230).
    #[test]
    fn the_head_a_post_falls_back_to_is_the_one_this_machine_remembers() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        assert_eq!(
            remembered_head("crit", 11),
            None,
            "a repo nothing is remembered about must say so rather than invent a sha"
        );

        let pr: Pr = serde_json::from_value(serde_json::json!({
            "number": 11, "title": "t", "author": "a", "url": "u",
            "head_ref": "f", "head_sha": "remembered111", "base_ref": "main",
            "draft": false, "updated_at": "", "committed_at": "",
            "checks": "passing", "my_review": "none", "review_is_current": false,
            "reasons": [], "lane": "needs-you", "box_name": "b",
        }))
        .unwrap();
        // Through the same door `review.rs`'s post tests use, so it cannot rot unnoticed.
        remember_for_test(&Queue {
            repo_id: "crit".into(),
            slug: "acme/thing".into(),
            trunk: "main".into(),
            viewer: "me".into(),
            ai: false,
            prs: vec![pr],
            blind_spots: Vec::new(),
            as_of: String::new(),
            fresh: false,
            whole: true,
        });

        assert_eq!(
            remembered_head("crit", 11).as_deref(),
            Some("remembered111"),
            "the post has no second opinion on where the branch was"
        );
        assert_eq!(
            remembered_head("crit", 12),
            None,
            "a pull request nothing is remembered about must not borrow another one's sha"
        );

        // And no network was needed for any of it: SKEIN_GITHUB_API points nowhere at all.
        std::env::remove_var("SKEIN_HOME");
    }

    // ───────────────── resolving a review thread from the panel (SKEIN-305) ─────────────────

    /// A GitHub that answers each connection from a script and records what it was handed.
    ///
    /// `Some(body)` is a 200 carrying that JSON; `None` is the failure this must be tested against
    /// — headers written, then the connection dropped, which is the shape SKEIN-271 met live and
    /// the one where "it ran" and "it did not run" look identical from here. Every request's body
    /// is recorded, so a test reads the mutation that actually went out rather than the one the
    /// source appears to build.
    fn scripted_github(
        script: Vec<Option<&'static str>>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        std::thread::spawn(move || {
            for (turn, stream) in listener.incoming().flatten().enumerate() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let mut length = 0usize;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let mut body = vec![0u8; length];
                if length > 0 {
                    reader.read_exact(&mut body).ok();
                }
                recorder.lock().unwrap().push(format!(
                    "{} {}",
                    request.split_whitespace().nth(1).unwrap_or(""),
                    String::from_utf8_lossy(&body)
                ));
                let answer = script.get(turn).copied().flatten();
                match answer {
                    Some(json) => {
                        let _ = stream.write_all(
                            format!(
                                "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                                json.len()
                            )
                            .as_bytes(),
                        );
                    }
                    // Headers, then nothing — the stream dies where the answer should have been.
                    None => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 X\r\nContent-Length: 128\r\nConnection: close\r\n\r\n",
                        );
                    }
                }
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    /// The one GraphQL request each direction sends, read off the wire (SKEIN-305).
    ///
    /// Both halves, because a resolve that sends `unresolveReviewThread` and an unresolve that
    /// sends `resolveReviewThread` are the same one-word mistake, and the pane's undo is the place
    /// it would be met. The thread id travels as a **variable** rather than interpolated into the
    /// query, so this also pins that: a node id spliced into the mutation text is an injection and
    /// a syntax error waiting on the first id with a quote in it.
    #[test]
    fn resolving_and_unresolving_send_the_mutation_github_names() {
        let (api, seen) = scripted_github(vec![
            Some(
                r#"{"data":{"resolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":true}}}}"#,
            ),
            Some(
                r#"{"data":{"unresolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":false}}}}"#,
            ),
        ]);
        let _wired = wired(&api);
        resolve_review_thread("PRRT_1").expect("GitHub said the thread is resolved");
        unresolve_review_thread("PRRT_1").expect("GitHub said the thread is open again");

        let sent = seen.lock().unwrap().clone();
        assert_eq!(
            sent.len(),
            2,
            "one request each, and no lookup beside it: {sent:?}"
        );
        for (i, (field, other)) in [
            ("resolveReviewThread", "unresolveReviewThread"),
            ("unresolveReviewThread", "resolveReviewThread"),
        ]
        .iter()
        .enumerate()
        {
            let (path, body) = sent[i].split_once(' ').expect("path and body");
            assert_eq!(
                path, "/graphql",
                "the mutation did not go to GraphQL: {}",
                sent[i]
            );
            let body: serde_json::Value = serde_json::from_str(body).expect("a JSON request");
            let query = body["query"].as_str().unwrap_or_default();
            assert!(
                query.contains(&format!(" {field}(input: {{threadId: $id}})")),
                "the {field} mutation is not what went out: {query}"
            );
            // The leading space is load-bearing: `resolveReviewThread` is a substring of
            // `unresolveReviewThread`, so a bare `contains` cannot tell the two apart in the
            // direction that matters — which is exactly the mistake being tested for.
            assert!(
                !query.contains(&format!(" {other}(")),
                "the two directions send the same mutation: {query}"
            );
            assert_eq!(
                body["variables"]["id"], "PRRT_1",
                "the thread id must travel as a variable, not spliced into the query: {body}"
            );
        }
    }

    /// **A refusal is an error, not a closed undo window** (SKEIN-305).
    ///
    /// Two ways GitHub says no, and both used to be the same `Ok(())` if the answer were dropped
    /// on the floor: a GraphQL `errors` entry, and a 200 whose thread comes back in the state it
    /// started in. The pane draws a receipt and starts an eight-second countdown on `ok`, so a
    /// swallowed failure is a thread the reader believes they resolved and a window that closes
    /// over it.
    #[test]
    fn a_resolve_github_did_not_perform_is_reported_rather_than_swallowed() {
        let (api, _seen) = scripted_github(vec![
            Some(r#"{"errors":[{"message":"Could not resolve to a node with the global id"}]}"#),
            Some(
                r#"{"data":{"resolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":false}}}}"#,
            ),
        ]);
        let _wired = wired(&api);
        let why =
            resolve_review_thread("PRRT_1").expect_err("a GraphQL error is not a resolved thread");
        assert!(
            why.contains("global id"),
            "GitHub's own reason did not reach the caller: {why}"
        );
        let why = resolve_review_thread("PRRT_1")
            .expect_err("a thread GitHub reports as still open was not resolved");
        assert!(
            why.contains("still open"),
            "a write that did not take was reported as one that did: {why}"
        );
        // And nothing is sent at all when there is no thread to name — a request GitHub would
        // answer with a schema complaint that reads as a skein bug.
        assert!(resolve_review_thread("  ").is_err());
    }

    /// **A mutation whose connection dies is never sent twice** (SKEIN-271, SKEIN-305).
    ///
    /// This is the routing test: [`crate::github::graphql_partial`] asks a dead connection again,
    /// [`crate::github::graphql`] does not, and a resolve sent twice is a write nobody
    /// ask for — the second one lands on a thread somebody may have reopened in between. Counted
    /// on the wire rather than read out of the source, so it holds however the call is spelled.
    #[test]
    fn a_resolve_whose_connection_dies_is_never_sent_twice() {
        let (api, seen) = scripted_github(vec![
            None,
            Some(
                r#"{"data":{"resolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":true}}}}"#,
            ),
        ]);
        let _wired = wired(&api);
        let why = resolve_review_thread("PRRT_1")
            .expect_err("a dead connection on a mutation is an error, not a retry");
        assert!(!why.is_empty());
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "the resolve reached GitHub twice — the retry belongs to reads only, and this is a \
             write: {:?}",
            seen.lock().unwrap()
        );
    }
}
