//! Uploads into a box: the route, the stall clock that ends a stalled one, and the sink that
//! writes the stream through a crossing and takes the whole write down when it is abandoned.

use super::*;

/// Receive one attachment — pasted, dropped, or picked — and stream it into the box, returning its
/// in-box path for the agent to reference. The agent lives in the microVM: it can't see the user's
/// clipboard or filesystem, so this is the only bridge from "a file on my laptop" to "a path the
/// agent can open". Any type: image, PDF, video, archive, source file.
///
/// Raw body (not multipart) so the bytes go straight from the socket to `sbx exec -i … cat >` with no
/// buffering — a 2 GB video costs the host no memory. `X-Skein-Name` carries the file's name
/// (percent-encoded; may include a relative dir when a folder is dropped) and `X-Skein-Drop` groups
/// every file of one drop into a single `/tmp/skein-drop-<batch>/` tree.
/// It answers with its **own clock** beside the path, and that is not telemetry — it is the one
/// thing that tells the two halves of a slow attach apart (SKEIN-269). The browser can measure only
/// click-to-answer, which counts the time a request spent queued in the browser's own connection
/// pool *before* it was ever sent; `ms.total` counts from this handler starting. A big wait with a
/// small `total` happened before the request reached skein; a `total` that fills the wait is the
/// box. Guessing between those two, from five drop directories and no numbers, is exactly what this
/// endpoint left the reader to do.
///
/// The phases are named rather than summed because they fail differently: `chose` is the round trip
/// that picks the channel, `body` is the transfer, `verdict` is waiting for the box to say the file
/// is written.
pub(super) async fn api_upload(
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> Json<serde_json::Value> {
    let mut clock = UploadClock::start();
    match stream_upload(&name, &headers, body, &mut clock).await {
        Ok(path) => serde_json::json!({ "ok": true, "path": path, "ms": clock.ms() }).into(),
        // The timings ride the failure too: a refusal after eight minutes and one after eight
        // milliseconds are different bugs, and an `error` alone reports them identically.
        Err(e) => serde_json::json!({ "ok": false, "error": e, "ms": clock.ms() }).into(),
    }
}

/// What an upload spent, phase by phase, on the host's clock.
///
/// Kept by the caller and filled in as the phases end, so a failure still carries the phases that
/// completed — a struct built at the end would have nothing to say about the upload that did not
/// reach one.
struct UploadClock {
    began: std::time::Instant,
    at: std::time::Instant,
    chose: u128,
    body: u128,
    verdict: u128,
}

impl UploadClock {
    fn start() -> Self {
        let now = std::time::Instant::now();
        Self {
            began: now,
            at: now,
            chose: 0,
            body: 0,
            verdict: 0,
        }
    }
    /// End the phase that was running and start the next. Returns the millis it took.
    fn lap(&mut self) -> u128 {
        let now = std::time::Instant::now();
        let ms = now.duration_since(self.at).as_millis();
        self.at = now;
        ms
    }
    fn ms(&self) -> serde_json::Value {
        serde_json::json!({
            "chose": self.chose,
            "body": self.body,
            "verdict": self.verdict,
            "total": self.began.elapsed().as_millis(),
        })
    }
}

/// Per-attachment ceiling. Streaming means the *host* never buffers the upload, but the box's /tmp is
/// finite — this keeps a runaway (or fat-fingered) upload from filling the sandbox's disk.
const UPLOAD_CAP: u64 = 2 * 1024 * 1024 * 1024;

/// The stall budget as the reader would say it. Seconds read better and are what the deadline is
/// set in — but a test shortens it to milliseconds, and "nothing moved for 0s" is a sentence that
/// says the deadline is broken rather than that it fired.
fn stall_word() -> String {
    let d = upload_stall();
    match d.as_secs() {
        0 => format!("{}ms", d.as_millis()),
        n => format!("{n}s"),
    }
}

/// How long any one step of an upload may make **no progress** before it is a stall and says so.
///
/// It bounds **silence**, not the transfer: a `cat` that has stopped consuming, or a child that
/// will not exit. There used to be a whole-transfer budget beside it — an hour, because a 900 MB
/// video over a slow link legitimately takes most of one — and it went with the agent, which was
/// the only path that could be waiting on a socket rather than on a pipe. The two were one number
/// once, and under one number those are the same picture:
/// SKEIN-269's five uploads sat for minutes and the reader was told nothing, because nothing on
/// either side of the wire distinguished "still coming" from "never coming".
///
/// A minute rather than seconds, because the thing on the other end may be legitimately busy, and
/// waiting is only wrong when nothing is moving.
///
/// A function and not a `const` so `$SKEIN_UPLOAD_STALL_MS` can shorten it, which is what lets a
/// test drive a real stall against a real box in under a second instead of waiting a minute for the
/// deadline it is checking. Same shape as `knock::grace`; a value that does not parse, or is zero,
/// is the default rather than an error, because a mistyped knob must not disable a deadline.
fn upload_stall() -> Duration {
    let asked = std::env::var("SKEIN_UPLOAD_STALL_MS").ok();
    match asked.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(ms) if ms > 0 => Duration::from_millis(ms),
        _ => Duration::from_secs(60),
    }
}

/// Where an upload's bytes go: a streamed write into the box.
///
/// **One channel, where there used to be two.** The other was the in-sandbox agent's connection,
/// chosen for a declared length under a cap; it existed because the spawned path crossed a
/// host-to-guest hop that could stall, and the agent was the thing built to survive that. The hop
/// and the agent are gone (SKEIN-521), and this is the path that never had a ceiling: it streams
/// from a pipe, so neither this process nor the box holds the whole file.
struct Sink {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
}

impl Sink {
    /// Start the write, and hold the pipe into it.
    ///
    /// **A function rather than four lines inside [`stream_upload`]** so that a test can drive a
    /// real [`Self::abandon`] against the spawn production uses. The alternative is a test that
    /// builds its own `Command`, which would be asserting on its own stdio and its own process
    /// group — green whatever this function does.
    fn open(argv: &[String]) -> Result<Sink, String> {
        let mut child = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            // So that giving up on it is giving up on it. `Sink::finish` stops waiting after
            // `upload_stall()`, and without this the abandoned child would go on running — still
            // holding the box's end of a file nobody is going to be told about, and still costing a
            // process per attempt, which the reader's five retries would have made five
            // (SKEIN-269).
            .kill_on_drop(true)
            // Its own process group, which is what makes [`Sink::abandon`] end the WRITE rather
            // than the process this side holds a handle on. Nothing here is a leaf program: the
            // argv is a crossing into the box, and the shell it lands in is `dash`, which FORKS a
            // `-c` command rather than exec'ing it — so the `cat` taking the upload is already a
            // grandchild. `kill_on_drop` and `Child::kill` both reach the pid and only the pid, so
            // an abandoned upload used to leave that `cat` holding the box's end of a half-written
            // file, with `ppid` 1 and nothing that would reap it (SKEIN-912, SKEIN-916).
            //
            // **The trade-off `skein::util::run_bounded` states does not arrive here**, and that
            // is a property of this process rather than a claim about groups: a `skein-server` is
            // not attached to a terminal and `main` says why it installs no `SIGINT` handler, so
            // there is no Ctrl-C for this child to have stopped receiving. What ends it is the
            // deadline, and now the deadline ends all of it.
            .process_group(0)
            .spawn()
            .map_err(|e| format!("the write into the box could not be started: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin pipe")?;
        Ok(Sink { child, stdin })
    }

    async fn push(&mut self, chunk: &[u8]) -> Result<(), String> {
        use tokio::io::AsyncWriteExt as _;
        // Bounded, where it used to be unbounded: stdin is a pipe into a process that may have
        // stopped reading, and an `await` on that with no deadline parks this request for as long
        // as the process lives. The agent path had an hour; this one had nothing at all, which is
        // the worse half of SKEIN-269's host side.
        match tokio::time::timeout(upload_stall(), self.stdin.write_all(chunk)).await {
            Ok(r) => r.map_err(|e| format!("writing file to box: {e}")),
            Err(_) => Err(format!(
                "the box stopped taking the file — nothing moved for {}",
                stall_word()
            )),
        }
    }

    async fn finish(self) -> Result<(), String> {
        use tokio::io::AsyncWriteExt as _;
        // The verdict below is a moment away or is never coming: the body is already through and
        // `cat` exits on EOF. So it waits the stall budget rather than a whole-transfer one — a
        // write that will not exit used to hold the request with no deadline at all, and the reader
        // saw "uploading…" for as long as that lasted (SKEIN-269).
        let Sink { child, stdin } = self;
        let mut stdin = stdin;
        stdin.shutdown().await.ok();
        drop(stdin); // EOF for `cat`
        let out = match tokio::time::timeout(upload_stall(), child.wait_with_output()).await {
            Ok(r) => r.map_err(|e| format!("the write into the box failed: {e}"))?,
            Err(_) => {
                return Err(format!(
                    "the box never confirmed the file — the write did not finish within {}",
                    stall_word()
                ))
            }
        };
        if out.status.success() {
            return Ok(());
        }
        Err(format!(
            "the write into the box failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }

    /// Give up, leaving nothing running. The partial file is removed by the caller either way.
    ///
    /// **The group, not the pid.** `Child::kill` signals the process this side recorded, which is
    /// the crossing and not the `cat` under it — see the `process_group(0)` on the spawn. The
    /// negative `kill` here is the same move `skein::util::end_group` makes for the blocking
    /// sites, written out because that one takes a `std::process::Child` and this is a tokio one;
    /// the `kill().await` after it is what reaps the leader, so an abandoned upload leaves no
    /// zombie either.
    async fn abandon(self) {
        let Sink { mut child, stdin } = self;
        drop(stdin);
        // `id()` is `Some` only while the child is unreaped, and an unreaped pid cannot have been
        // handed to anybody else — so the group named here is this child's own and can be no
        // stranger's. That is the same invariant `skein::util::end_group`'s SAFETY note states.
        if let Some(pid) = child.id() {
            // SAFETY: `kill` has no memory effects, and `-pid` names the group led by a child this
            // process spawned with `process_group(0)` and has not reaped. A failure means the group
            // is already empty, which is the outcome being asked for.
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        }
        let _ = child.kill().await;
    }
}

async fn stream_upload(
    name: &str,
    headers: &axum::http::HeaderMap,
    body: axum::body::Body,
    clock: &mut UploadClock,
) -> Result<String, String> {
    let hdr = |k: &'static str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let batch = hdr("x-skein-drop");
    let mut rel = skein::util::pct_decode(&hdr("x-skein-name"));
    if rel.trim().is_empty() {
        // A clipboard paste often has no filename. Name it from the content type so the suffix still
        // says what it is (an agent keys off `.png` to treat it as an image).
        let ext = hdr("content-type")
            .split(';')
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        let ext = if ext.is_empty() { "bin".into() } else { ext };
        rel = format!("paste.{ext}");
    }
    let (dir, path) = skein::sandbox::drop_dest(&batch, &rel)?;

    // There is no channel to choose any more: one write, streamed. The choice used to be made here
    // — before a single byte was read, on the DECLARED length rather than the real one, because an
    // upload is read off a network socket exactly once and by the time the truth is known there is
    // no second copy to fall back with. With one channel that ordering has nothing left to decide.
    //
    // The argv carries its own program: where the box lives decides that too.
    let argv = skein::sandbox::box_write_argv(name, &dir, &path)?;
    let mut sink = Sink::open(&argv)?;
    // The channel is chosen; everything above is `chose`. It is its own phase because it is the one
    // that happens before a byte of the body is read, and therefore the one a reader watching an
    // upload bar would see as nothing happening at all.
    clock.chose = clock.lap();
    let mut stream = body.into_data_stream();
    let mut total: u64 = 0;
    // Collect the failure instead of returning from inside the loop: the partial file has to be
    // cleaned up on the way out. An over-cap upload is exactly the case that would otherwise leave
    // gigabytes of junk in the box's /tmp — the thing the cap exists to prevent.
    let mut failed: Option<String> = None;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                failed = Some(format!("upload interrupted: {e}"));
                break;
            }
        };
        total += chunk.len() as u64;
        if total > UPLOAD_CAP {
            failed = Some(format!("too large (cap {} MB)", UPLOAD_CAP / (1024 * 1024)));
            break;
        }
        if let Err(e) = sink.push(&chunk).await {
            failed = Some(e);
            break;
        }
    }
    clock.body = clock.lap();
    if let Some(e) = failed {
        sink.abandon().await;
        discard_partial(name, &path).await;
        return Err(e);
    }
    let said = sink.finish().await;
    clock.verdict = clock.lap();
    said?;
    Ok(path)
}

/// Best-effort removal of a half-written attachment, so a failed upload leaves nothing for the agent
/// to mistake for the real file. Bounded: a wedged box must not hold the response open.
async fn discard_partial(name: &str, path: &str) {
    let inner = format!("rm -f {}", skein::util::sh_quote(path));
    // Through the placement, like the write it is undoing — `sbx exec <box>` names no sandbox in the
    // fleet, so the cleanup would fail exactly when the upload it is cleaning up did. And through
    // the agent when there is one, for the same reason: the moment a half-written file most needs
    // removing is the moment a fresh `sbx exec` is least likely to come back.
    let name = name.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        skein::place::place_of(&name).map(|place| place.exec(&inner, Duration::from_secs(10)))
    })
    .await;
}

/// **An abandoned upload ends the write, not only the process the server holds a handle on.**
///
/// The fourth of the four sites SKEIN-916 names. `Sink::abandon` ran `child.kill().await`, which
/// reaches the crossing and not the `cat` under it — and `kill_on_drop(true)`, which looks like the
/// belt to that braces, reaches exactly the same pid. So a reader whose upload failed over (five
/// retries is the shape SKEIN-269 measured) left five writes holding five half-written files inside
/// the box, each with `ppid` 1 and nothing that would reap it.
#[cfg(test)]
mod upload_deadline {
    use super::*;

    /// **This process's own copy of `place::grouptest`**, and the duplication is a process boundary
    /// rather than an oversight: that module is `#[cfg(test)]` inside the LIBRARY, so it exists in
    /// the library's test binary and in no other — a binary target linking `skein` cannot see it.
    /// The rule it encodes is written out there; what is repeated here is the mechanism.
    struct Escapee {
        token: String,
        pidfile: std::path::PathBuf,
    }

    impl Escapee {
        fn new(dir: &std::path::Path) -> Escapee {
            Escapee {
                // A `sleep` duration, and therefore a name in the grandchild's own argv. This
                // process's pid is in it, so the scan below can match nothing a neighbouring suite
                // started.
                token: format!("600.{}", std::process::id()),
                pidfile: dir.join("abandoned.grandchild"),
            }
        }

        /// The argv for a write that starts a grandchild and then does not finish. The background
        /// `sleep` is a child of the shell, which is the child the server spawned — so it is
        /// exactly the process a kill on the recorded pid does not reach.
        fn argv(&self) -> Vec<String> {
            vec![
                "/bin/sh".into(),
                "-c".into(),
                format!(
                    "sleep {} & echo $! > {}; sleep {}",
                    self.token,
                    self.pidfile.display(),
                    self.token
                ),
            ]
        }

        /// **It is THERE.** Without this half, a stand-in that started nothing would pass the
        /// "gone" assertion below and report the fix working (SKEIN-833).
        fn there(&self) -> u32 {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(2500);
            while std::time::Instant::now() < until {
                if let Some(pid) = std::fs::read_to_string(&self.pidfile)
                    .ok()
                    .and_then(|raw| raw.trim().parse::<u32>().ok())
                {
                    if self.naming().contains(&(pid as libc::pid_t)) {
                        return pid;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!(
                "no grandchild naming {} was running before the sink was abandoned, so its absence \
                 afterwards would prove nothing about the kill",
                self.token
            );
        }

        /// **It is GONE** — the whole set, because the defect leaves more than one process behind
        /// and a test that watched one of them would report the other as fixed.
        fn gone(&self, pid: u32) {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(3000);
            let mut left = Vec::new();
            while std::time::Instant::now() < until {
                left = self.naming();
                if left.is_empty() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!(
                "the sink was abandoned and {left:?} were still running — pid {pid} is the \
                 GRANDCHILD the script recorded, and every one of these names `sleep {}`, so the \
                 kill reached the handle this side recorded and not the work it started",
                self.token
            );
        }

        /// Every process whose argv carries this fixture's token, read from `/proc/<pid>/cmdline`
        /// — never a pattern over a program name (SKEIN-647).
        fn naming(&self) -> Vec<libc::pid_t> {
            let Ok(entries) = std::fs::read_dir("/proc") else {
                return Vec::new();
            };
            let mut found = Vec::new();
            for entry in entries.flatten() {
                let Ok(pid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else {
                    continue;
                };
                if let Ok(raw) = std::fs::read(entry.path().join("cmdline")) {
                    if String::from_utf8_lossy(&raw).contains(&self.token) {
                        found.push(pid);
                    }
                }
            }
            found
        }
    }

    impl Drop for Escapee {
        /// Nothing this fixture started outlives it, on the panicking path as much as the returning
        /// one — which is the path that matters, because a failing test here is the one with
        /// something still running.
        fn drop(&mut self) {
            for pid in self.naming() {
                // SAFETY: `kill` has no memory effects, and `pid` names a process whose argv
                // carries a token minted by this process — one this fixture's own script started.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    /// The grandchild is running while the sink is open, and gone once it is abandoned.
    ///
    /// Through [`Sink::open`], which is production's spawn: a test that built its own `Command`
    /// would be asserting on its own process group and would stay green however
    /// [`stream_upload`] spawns.
    ///
    /// **What makes it fail:** removing `.process_group(0)` from [`Sink::open`] and dropping the
    /// negative `kill` from [`Sink::abandon`] — the two lines this test exists for. `kill_on_drop`
    /// and `Child::kill` then reach the shell alone, the backgrounded `sleep` is reparented to init
    /// and goes on running, and `gone` fires naming the pids it can still see.
    #[test]
    fn an_abandoned_upload_takes_its_grandchildren_with_it() {
        let dir = scratch_dir("dl916-sink");
        let escapee = Escapee::new(&dir);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime for this test's body");
        let sink = runtime.block_on(async { Sink::open(&escapee.argv()) });
        let sink = sink.expect("the write did not start");

        let pid = escapee.there();
        runtime.block_on(sink.abandon());
        escapee.gone(pid);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
