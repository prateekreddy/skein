// The fake GitHub the suites point `SKEIN_GITHUB_API` at.
//
// It exists because the client changed, not because the tests wanted rewriting: skein used to shell
// out to `gh`, so the stub was a shell script on `$PATH`. It reads the API now, so the stub is an
// API — and the tests gained something in the move, because what they assert is the request that
// actually goes out. `github::api_base` (src/github.rs:187-191) reads `SKEIN_GITHUB_API` before it
// falls back to the real api.github.com, which is the seam every suite here stands one of these up
// on; a suite that does not is a suite that asks the real GitHub about the owner's own repository
// (UI-3).
//
// Four copies grew: `review.mjs`, `actfail.mjs`, `connections.mjs` — the last two byte-identical —
// and `updatepane.mjs`. What differed was the routes; the plumbing under them (collect the body,
// strip the query, JSON or text back, 404 with the path that had no stub, listen on :0 and report
// which port that turned out to be) was the same four times.
import http from "node:http";

/**
 * An HTTP server answering `route`, listening on a port the kernel chose.
 *
 * `route({url, body, req, send})` answers by calling `send(code, payload, type)` and returning
 * something truthy; anything else falls through to a 404 that names the path, so an unstubbed route
 * shows up as itself rather than as a hang or an empty answer.
 *
 * Awaited, because `listen` is asynchronous and `address()` is null until it has happened.
 */
export function stub(route) {
  const server = http.createServer((req, res) => {
    let body = "";
    req.on("data", c => { body += c; });
    req.on("end", () => {
      // Returns `true` so a route can answer and say it answered in one `return send(...)`.
      const send = (code, payload, type = "application/json") => {
        res.writeHead(code, { "Content-Type": type });
        res.end(typeof payload === "string" ? payload : JSON.stringify(payload));
        return true;
      };
      const url = req.url.split("?")[0];
      if (route({ url, body, req, send })) return;
      send(404, { message: `no stub for ${url}` });
    });
  });
  return new Promise(resolve => {
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolve({ url: `http://127.0.0.1:${port}`, close: () => server.close() });
    });
  });
}

/** The diff both queue suites read: one hunk, one file, a real `diff --git` header so
 * `readingFiles` and the server's own scan have something with the shape of a change. */
export const ONE_FILE_DIFF =
  "diff --git a/src/parser.rs b/src/parser.rs\n--- a/src/parser.rs\n+++ b/src/parser.rs\n"
  + "@@\n-    let head = input.chars().next().unwrap();\n"
  + "+    let Some(head) = input.chars().next() else { return Ok(()) };\n";

/**
 * A GitHub the size of what a queue refresh asks: who you are, your teams, the membership searches,
 * one pull request's files and its diff. `prs` are the search nodes it answers with.
 *
 * `/user/teams` refuses, which is what a token without `read:org` really gets — and it is load
 * bearing rather than lazy: `viewer` reads a refusal as "GitHub would not say" rather than "you are
 * in no teams" (`teams_unknown`, `src/prq/refresh.rs`), so a queue built through this stub is
 * deliberately not `whole`.
 * `review.mjs` needs the other case and builds its own richer stub for it.
 */
export function queueGitHub(prs) {
  return stub(({ url, body, req, send }) => {
    if (url === "/user") return send(200, { login: "me" });
    if (url === "/user/teams") return send(403, { message: "Requires read:org" });
    if (url === "/graphql") {
      // One request carries every membership search of a refresh, aliased q0…qN (SKEIN-209), and
      // each alias answers under its own name — a fixture that still answered the single `search`
      // field left every query reading as "GitHub returned no answer for this search".
      const vars = JSON.parse(body || "{}").variables || {};
      const data = {};
      for (const [name, value] of Object.entries(vars)) {
        if (!/^q\d+$/.test(name)) continue;
        data[name] = { nodes: /review-requested:/.test(String(value)) ? prs : [] };
      }
      return send(200, { data });
    }
    if (/^\/repos\/[^/]+\/[^/]+\/pulls\/\d+\/files$/.test(url)) {
      return send(200, [{ filename: "src/parser.rs" }]);
    }
    if (/^\/repos\/[^/]+\/[^/]+\/pulls\/\d+$/.test(url)) return send(200, ONE_FILE_DIFF, "text/plain");
    return false;
  });
}
