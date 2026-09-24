//! The embedded terminals: a box's terminal and the login terminal over a WebSocket, the PTY
//! bridge between them, the cap on open terminals, and the close codes a pane is told.

use super::*;

/// How many embedded terminals may be open at once.
///
/// Named rather than spelled inside [`PTY_LIMIT`] because the refusal quotes it. A person told
/// "too many terminals open" and not told how many is being asked to guess at the rule they just
/// hit, and a number written twice is a number that stops agreeing with itself.
const PTY_MAX: usize = 24;

/// Cap concurrent embedded terminals so a flood of WS connections can't exhaust PTYs / file
/// descriptors on the host. Each live terminal holds one permit for its whole session.
static PTY_LIMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(PTY_MAX);

/// One terminal's slot, held for the session — and **it says so when it is given back**.
///
/// A bare `SemaphorePermit` releases silently, which was fine while the only thing that could
/// happen next was somebody pressing a button. It is not fine now: a pane refused for the cap is
/// told to close another terminal, and it then watches the board for the slot rather than waiting
/// to be clicked (SKEIN-702). Only the release knows when that is, so the release is what publishes
/// it.
///
/// **`Option`, and taken before the announcement, because the order is the whole correctness of
/// this.** A field is dropped *after* the enclosing `Drop::drop` returns, so announcing first would
/// announce a slot that is not yet free — and the pane it wakes would race the very release that
/// woke it, be refused again, and go back to waiting for a permit that was already gone. `take()`
/// returns the permit to the semaphore inside this line; the send is on the line after it.
struct PtySlot(Option<tokio::sync::SemaphorePermit<'static>>);

impl Drop for PtySlot {
    fn drop(&mut self) {
        drop(self.0.take());
        skein::stream::pty_freed();
    }
}

/// Upgrade to a WebSocket that bridges the browser terminal to a PTY.
/// `?launch=<branch>` creates the box, then attaches to its first persistent agent session.
pub(super) async fn terminal(
    ws: WebSocketUpgrade,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> Response {
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin terminal blocked").into_response();
    }
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let launch = q.get("launch").filter(|s| !s.is_empty()).cloned();
    let shell = q
        .get("shell")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    let agent = q
        .get("agent")
        .filter(|a| skein::runtime::valid_runtime(a))
        .cloned();
    let handoff = q
        .get("handoff")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    let from = q
        .get("from")
        .filter(|a| skein::runtime::valid_runtime(a))
        .cloned();
    ws.on_upgrade(move |socket| terminal_session(socket, name, launch, shell, agent, handoff, from))
}

/// The WS↔PTY bridge: attach to an in-box tmux session through `sbx exec`, pipe bytes both ways,
/// and honour resizes.
/// Override the spawned command with $SKEIN_ATTACH_CMD (run via `sh -c`) for local testing.
async fn terminal_session(
    mut socket: WebSocket,
    name: String,
    launch: Option<String>,
    shell: bool,
    agent: Option<String>,
    handoff: bool,
    from: Option<String>,
) {
    // Hold a permit for the whole session; reject (rather than queue) when the cap is hit so a
    // hung browser can't silently stall new terminals. Dropped on every return → released, and the
    // release says so on the board's stream ([`PtySlot`]) so a pane refused here can come back
    // without being clicked.
    let _permit = match PTY_LIMIT.try_acquire() {
        Ok(p) => PtySlot(Some(p)),
        Err(_) => {
            refuse(&mut socket, pty_limit_reached(), AFTER_WAIT_PTY).await;
            return;
        }
    };
    let target_agent = agent.unwrap_or_else(|| skein::repos::agent_for_box(&name));
    if handoff && !shell {
        let hn = name.clone();
        let ht = target_agent.clone();
        let hf = from.clone();
        match tokio::task::spawn_blocking(move || {
            skein::handoff::prepare_handoff(&hn, hf.as_deref(), &ht)
        })
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                let _ = socket
                    .send(Message::Text(format!(
                        "skein: handoff brief failed: {e}\r\n"
                    )))
                    .await;
            }
            Err(e) => {
                let _ = socket
                    .send(Message::Text(format!(
                        "skein: handoff task failed: {e}\r\n"
                    )))
                    .await;
            }
        }
    }

    // Build the command. Propagate env + cwd so `sbx`/`sh` resolve on PATH.
    // Default: reconnect to the box's *existing* provider-specific tmux session,
    // rooted at the dir the box registered.
    //
    // $SKEIN_ATTACH_CMD fully overrides it (run via `sh -c`) — `{name}` and `{dir}` in the
    // value are substituted first, so you can tune the exact sbx invocation per box without
    // recompiling, e.g. SKEIN_ATTACH_CMD='sbx exec -it {name} tmux attach -t skein-agent'
    // A fleet box's tmux server does not survive its sandbox cycling, while its tree, private HOME
    // and cgroup do. Restart the session before we address its namespace, or the terminal opens on
    // `nsenter: cannot open /proc/<pid>/ns/user` — a namespace error for a box that just needs
    // starting again. A no-op for a live box and for one that owns its sandbox.
    if launch.is_none() {
        // A box that does not exist cannot be attached to, and trying is not harmless: `sbx exec`
        // names a sandbox, sbx says it has never heard of it, the browser reconnects, and the loop
        // buries the real error from the failed start under a message about a sandbox that was
        // never meant to exist. Say what is wrong once and stop, rather than forever and mislead.
        let boxed = name.clone();
        if let Ok(Some(why)) =
            tokio::task::spawn_blocking(move || skein::fleet::absent_box_reason(&boxed)).await
        {
            // `why` is the best recovery sentence skein has — it names the command to run and the
            // command NOT to run. What it could not say is that nobody has to come back here
            // afterwards, so that is added rather than left to be guessed at (SKEIN-702).
            refuse(
                &mut socket,
                format!("skein: {why}{}", watching_for_box(&name)),
                AFTER_WAIT_BOX,
            )
            .await;
            return;
        }
        let boxed = name.clone();
        if let Ok(Err(e)) =
            tokio::task::spawn_blocking(move || skein::fleet::ensure_box_session(&boxed)).await
        {
            let _ = socket.send(Message::Text(format!("skein: {e}\r\n"))).await;
        }
    }
    let dir = skein::sbx::lookup_dir(&name).unwrap_or_default();
    // $SKEIN_SHELL_CMD overrides the shell command, $SKEIN_ATTACH_CMD the agent attach (both run via
    // `sh -c`, `{name}`/`{dir}` substituted). Default agent: reconnect to the box's tmux+`claude
    // --continue` session; default shell: `sbx exec -it <box> /bin/bash` — a plain terminal.
    let override_var = if shell {
        "SKEIN_SHELL_CMD"
    } else {
        "SKEIN_ATTACH_CMD"
    };
    let mut cmd = if let Some(branch) = &launch {
        // create-a-box mode: provision without an agent attach, then enter the same tmux-backed
        // agent session all future UI reloads reconnect to.
        let mut b = CommandBuilder::new("sh");
        b.arg("-c");
        b.arg(skein::sandbox::launch_command_with_agent(
            &name,
            branch,
            Some(&target_agent),
        ));
        b
    } else {
        match std::env::var(override_var) {
            Ok(c) if !c.is_empty() => {
                let c = c
                    .replace("{name}", &skein::util::sh_quote(&name))
                    .replace("{dir}", &skein::util::sh_quote(&dir));
                let mut b = CommandBuilder::new("sh");
                b.arg("-c");
                b.arg(c);
                b
            }
            _ => {
                // The program comes from the argv rather than being spelled here. It is `sbx` on a
                // host and something else in the fleet, and a spawner that names it cannot be told
                // otherwise — which is how a terminal ends up attached to the wrong machine.
                let argv = if shell {
                    skein::sandbox::shell_argv(&name)
                } else {
                    skein::sandbox::attach_argv_as(&name, &dir, &target_agent)
                };
                let mut b = CommandBuilder::new(argv.first().map(String::as_str).unwrap_or("sh"));
                for a in argv.iter().skip(1) {
                    b.arg(a);
                }
                b
            }
        }
    };
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }
    // Run launch/attach from the repo ($SKEIN_REPO, else cwd) so a *relative* command resolves —
    // e.g. SKEIN_LAUNCH_CMD='dev-sandbox/setup-sandbox.sh {branch}' works without an absolute path.
    let run_dir = std::env::var("SKEIN_REPO")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    if let Some(dir) = run_dir {
        cmd.cwd(dir);
    }

    let launching = launch.is_some();
    let code = pump_pty(&mut socket, cmd, AFTER_RETRY).await;
    // The one failure a box start cannot record for itself: `skein` never ran, so nothing inside it
    // wrote `starts/<box>.err`, and the reconnect that follows this PTY closing was told "There is
    // no record of a start having been attempted" — about a launch someone had just pressed a button
    // for (SKEIN-589). Kept here, and said here, because the shell's own `not found` scrolls away
    // with the terminal that carried it.
    if launching {
        if let Some(code) = code {
            let boxed = name.clone();
            if let Ok(Some(why)) = tokio::task::spawn_blocking(move || {
                skein::sandbox::remember_launch_never_ran(&boxed, code)
            })
            .await
            {
                let _ = socket
                    .send(Message::Text(format!("\r\nskein: {why}\r\n")))
                    .await;
            }
        }
    }
    // Say which of the two ways this session could have ended actually ended it, because the browser
    // cannot tell and the answer decides what it draws over the last thing on screen (SKEIN-672).
    // [`pump_pty`] returns a code only for a child that exited under a live socket, so this is
    // exactly "the command is over"; every other way out leaves without it and reads as the
    // connection having gone away.
    if let Some(code) = code {
        close_saying(
            &mut socket,
            AFTER_CHILD_ENDED,
            &format!("the command exited {code}"),
        )
        .await;
    }
}

/// What a person sees when every terminal slot is taken, in both places it can happen.
///
/// **It quotes [`PTY_MAX`] and it promises the pane comes back on its own**, and both halves are the
/// point (SKEIN-702). The old sentence — "too many terminals open — close one and retry" — named
/// neither the rule that had been hit nor who was retrying, so somebody who closed a pane was left
/// to work out that they now had to find and press something. They do not: the release publishes
/// `Tick::PtyFreed` and the pane reconnects itself. Saying so is what stops them waiting for nothing
/// or clicking for no reason.
fn pty_limit_reached() -> String {
    format!(
        "skein: too many terminals open — {PTY_MAX} at once is the limit. Close another terminal \
         and this one reopens on its own; there is nothing else to do.\r\n"
    )
}

/// The line added under `absent_box_reason`'s refusal: skein is watching, so nobody need come back.
fn watching_for_box(name: &str) -> String {
    format!(
        "skein is watching the board for {name}, and reopens this terminal on its own once the box \
         is there.\r\n"
    )
}

/// What a person sees when the bridge itself could not be built: **skein's own failure, said as
/// one**, and the one class of refusal with nothing to watch for.
///
/// Three parts, and each is load-bearing (SKEIN-702). `what` is what broke, in skein's words rather
/// than only the OS error, because "pty reader: Too many open files" tells somebody nothing about
/// whose fault it is. **"not anything you did" is part of the next step, not sympathy** — without it
/// the reader goes looking through their own box for the cause, which is the most expensive way
/// there is to make no progress. And `likely` names the thing that can actually be checked.
///
/// **What comes next depends on who asked, and `after` is that answer** (SKEIN-883). A box
/// terminal is closed with [`AFTER_RETRY`], and its pane tries again by itself on a bounded backoff
/// — each of these four is a condition that can clear on its own (a pty or an fd freeing, a program
/// appearing on PATH), and `openpty` fails before anything is spawned, so a retry risks nothing. The
/// login modal is closed with [`AFTER_NO_WATCH`], for the reason [`login_session`] gives: a modal
/// that reopened itself over whatever somebody had moved on to is worse than the button. The
/// sentence agrees with whichever the pane is about to do.
fn skeins_own_fault(what: &str, error: &str, likely: &str, after: &str) -> String {
    let next = match after {
        AFTER_RETRY => "This pane tries again by itself for a few minutes.",
        _ => {
            "There is no condition here for skein to wait on, so nothing will reopen this \
             terminal by itself: use Try again below."
        }
    };
    format!(
        "skein: {what}: {error}\r\nThis is skein's own failure, not anything you did — nothing in \
         your box is wrong. {next} If it keeps failing, {likely}.\r\n"
    )
}

/// Wait to be told the close was read, instead of hanging up on the sentence.
///
/// **A close frame is not delivered by having been written.** Returning from here drops the socket,
/// and a socket dropped while bytes it never read are still queued on it is closed by the kernel
/// with RST rather than FIN — which discards whatever the PEER had queued and not yet read. So the
/// close code is destroyed by the same reset that ends the connection, and the browser reports 1006:
/// a launch that ran to completion, read as a connection that went away, with the reconnect panel
/// back over the one line saying what happened (SKEIN-746, the defect SKEIN-672 removed).
///
/// The cockpit supplies both halves by itself. It writes on that socket without being asked —
/// `sendResize` fires off `requestAnimationFrame` and off xterm's own `onResize`, neither of which
/// is timed by anything here — while [`pump_pty`] stops reading the instant the child's PTY closes,
/// so anything arriving between that instant and this one is never read. And a page whose renderer
/// is short of CPU is exactly the page that has not yet read what it was sent. Measured with a
/// client that stops reading and keeps writing resizes: 30 of 30 sessions lost the close code, the
/// shell's error and skein's recorded reason, all three, and reported ECONNRESET instead.
///
/// Reading until the peer's own close empties that queue, and is the closing handshake RFC 6455
/// §7.1.4 describes. Its arrival is also the only proof the code was read, which is why this waits
/// for that rather than for a fixed moment. Bounded, because a peer that answers nothing must not
/// hold its PTY permit for ever, and generous, because the browser this is for is a slow one.
const CLOSE_ACK_WAIT: Duration = Duration::from_secs(5);

async fn hang_up(socket: &mut WebSocket) {
    let _ = tokio::time::timeout(CLOSE_ACK_WAIT, async {
        while let Some(Ok(msg)) = socket.recv().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

/// The close code skein's own end puts on a terminal socket when **there is nothing here to
/// reconnect to** — as against every other way a socket ends, which is the connection going away.
///
/// The cockpit draws a "session not connected · click to reconnect" panel over a terminal whose
/// socket has closed. Over a launch that ran to completion that panel covers the one line saying
/// what happened, and offers to reconnect to something that no longer exists — which is the whole of
/// SKEIN-672. `ws.onclose` cannot draw the distinction on its own: a child that exited and a
/// connection that dropped arrive at the browser identically. So the side that knows says which.
///
/// **It was `CLOSE_CHILD_ENDED`, and the rename is the point rather than tidying** (SKEIN-702). The
/// same panel went up over every *refusal* too — "too many terminals open", "box … does not exist"
/// — because those closed with no code at all and the browser read 1006. Sending them this code is
/// what uncovers their sentences, and the moment a refusal carries it "the child ended" is false:
/// a refusal has no child, and nothing ended. The page's question is not *what happened* but
/// **whether to offer a reconnect**, and a finished child and a refused start are the same answer to
/// it. One code, saying that one thing. Leaving the old name while widening what it covers is
/// SKEIN-748's defect exactly: a sentence true when it was written and quietly false afterwards.
///
/// **4000-4999 is the only range an application may define**, per RFC 6455 §7.4.2: 0-999 is unused,
/// 1000-2999 belongs to the protocol and to IANA, and 3000-3999 is for libraries registered with
/// IANA. A code from any of those would be either a lie about a protocol condition or a claim on
/// somebody else's registration. A browser reports 1006 for a connection that simply died and never
/// a 4xxx, so the ABSENCE of this code is what "the connection went away" is read from — which is
/// the direction that matters, because a close nobody wrote is the common one.
///
/// The reason beside it is one of the `AFTER_*` words below — the first word says what the pane
/// should wait for, and anything after it is for a person reading a trace.
const CLOSE_NOTHING_TO_RECONNECT: u16 = 4001;

/// What a pane should do next, written as the FIRST WORD of the close reason.
///
/// **The code and this answer two different questions, and both had to be answered.**
/// [`CLOSE_NOTHING_TO_RECONNECT`] answers "offer a reconnect?", and one code covers every way in
/// because the answer is always no. What is left is what the pane should do *instead*, and there
/// the paths genuinely differ (SKEIN-702): a terminal refused for the cap is waiting for a slot, one
/// refused for a missing box is waiting for the box, and one that fell over inside `pump_pty` is
/// waiting for nothing at all and must offer the control rather than pretend. **A pane that is
/// watching says what for; a pane that is watching nothing offers a button. A spinner that waits for
/// nothing is worse than a button**, so this is the field that stops the page inventing either one.
///
/// **A first word rather than the whole reason**, so the trace text a person reads in devtools can
/// keep following it — `child-ended the command exited 127`. The page splits on the first space; a
/// close reason is capped at 123 bytes, which none of these approaches.
///
/// This is not "branching on free text", which the old doc comment rightly refused. The token is
/// written here, read in `ws.onclose`, and is as much of the contract as the number above it — the
/// alternative was one close code per condition, which is three numbers agreeing about the thing the
/// number was deliberately not made to carry.
/// The command ran and is over. Nothing to wait for and nothing to retry — it finished.
const AFTER_CHILD_ENDED: &str = "child-ended";

/// Refused for the terminal cap. Wait for `skein::stream::Tick::PtyFreed` on the board's stream.
const AFTER_WAIT_PTY: &str = "wait-pty";

/// Refused because skein has no placement for the box. Wait for the box on the board's stream.
const AFTER_WAIT_BOX: &str = "wait-box";

/// Skein's own failure, where the pane must not reopen by itself. Offer the control.
const AFTER_NO_WATCH: &str = "no-watch";

/// Skein's own failure inside [`pump_pty`], for a box terminal (SKEIN-883). Nothing names the moment
/// it clears, so the pane retries on a bounded backoff — 3s, 10s, 30s, 60s, 120s — and falls back to
/// the control after the fifth.
const AFTER_RETRY: &str = "retry";

/// Close a terminal socket the way [`CLOSE_NOTHING_TO_RECONNECT`] describes, and wait to be told the
/// close was read.
///
/// One helper rather than the same four lines at nine sites, because the four lines are the part
/// that was missing: **every one of these paths used to write its sentence and return**, and
/// returning drops the socket, which destroys both the sentence and the code (see [`hang_up`]).
async fn close_saying(socket: &mut WebSocket, after: &str, trace: &str) {
    let reason = match trace.is_empty() {
        true => after.to_string(),
        false => format!("{after} {trace}"),
    };
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: CLOSE_NOTHING_TO_RECONNECT,
            reason: reason.into(),
        })))
        .await;
    hang_up(socket).await;
}

/// Refuse a terminal: say what happened and what to do about it, say there is nothing to reconnect
/// to, say what the pane should wait for, and stay until that has been read.
async fn refuse(socket: &mut WebSocket, sentence: String, after: &str) {
    let _ = socket.send(Message::Text(sentence)).await;
    close_saying(socket, after, "").await;
}

/// The WS↔PTY byte pump shared by the box terminal ([`terminal_session`]) and the login terminal
/// ([`login_session`]): open a fresh PTY, spawn `cmd` on it, pipe bytes both ways, honour
/// `{"resize":…}` frames, ping every 30s, and reap the child on the way out.
///
/// Returns the child's exit code when the CHILD ended the session — PTY EOF with the socket still
/// up — and `None` when the socket went first or the bridge never got started. **In the
/// never-started case this has already refused and closed the socket itself** ([`refuse`], with
/// [`AFTER_NO_WATCH`]): its four failures are skein's own, they are the same four wherever the pump
/// is used, and the caller cannot say anything more useful about them than the pump can. So a caller
/// seeing `None` has nothing left to write and no socket to write it on. A socket that drops mid-session kills the child, which is exactly
/// what the login flow wants: a login is a one-shot flow, not a tmux-backed session to resume, so
/// an abandoned OAuth prompt dies with its browser tab instead of waiting forever for input nobody
/// can give it.
///
/// `own_fault` is the token its four failures close with: [`AFTER_RETRY`] from a box terminal,
/// [`AFTER_NO_WATCH`] from the login modal (see [`skeins_own_fault`]).
async fn pump_pty(socket: &mut WebSocket, cmd: CommandBuilder, own_fault: &str) -> Option<u32> {
    let pair = match native_pty_system().openpty(PtySize {
        rows: 30,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            refuse(
                socket,
                skeins_own_fault(
                    "could not open a terminal device",
                    &e.to_string(),
                    "the machine skein is running on has run out of pseudo-terminals",
                    own_fault,
                ),
                own_fault,
            )
            .await;
            return None;
        }
    };

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            refuse(
                socket,
                skeins_own_fault(
                    "could not start the program behind this terminal",
                    &e.to_string(),
                    "the program named in the error is missing from where skein is running",
                    own_fault,
                ),
                own_fault,
            )
            .await;
            return None;
        }
    };
    drop(pair.slave); // release the slave fd in the parent so EOF propagates on child exit

    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            refuse(
                socket,
                skeins_own_fault(
                    "opened this terminal and then could not read from it",
                    &e.to_string(),
                    "skein is out of file descriptors, and restarting the server clears that",
                    own_fault,
                ),
                own_fault,
            )
            .await;
            return None;
        }
    };
    let mut writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            refuse(
                socket,
                skeins_own_fault(
                    "opened this terminal and then could not write to it",
                    &e.to_string(),
                    "skein is out of file descriptors, and restarting the server clears that",
                    own_fault,
                ),
                own_fault,
            )
            .await;
            return None;
        }
    };
    let master = pair.master; // kept for resize

    // PTY output → channel (blocking read on a thread).
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Channel → PTY input (blocking write on a thread), and the channel is UNBOUNDED on purpose.
    //
    // **A bounded one puts the child's stdin in charge of the whole bridge** (SKEIN-750). Writing to
    // a PTY master whose child is not reading blocks after a few kilobytes — 20480 bytes in raw
    // mode, 8960 in canonical mode with a newline in the input, on one measurement of each, which is
    // an agent mid-turn, a full-screen TUI, or a paste into a `sleep`. (Canonical mode with no
    // newline never blocks at all: the discipline discards past its buffer instead. The probe those
    // three readings came from is in `tests/ui/ptystall.mjs`, which drives the reachable one through
    // this pump.) A blocked write stops `in_rx` draining, a bounded channel then fills, and the forward
    // below used to be `in_tx.send(b).await` *inside* a `tokio::select!` branch. A `select!` polls
    // nothing while a branch's handler is awaiting, so PTY output stopped reaching the browser, the
    // keepalive stopped and resize frames stopped being honoured — all three at once, for as long as
    // the child ignored its stdin, and nothing on screen could say why.
    //
    // The other repair was `try_send` and a sentence when the queue is full, and dropping is worse
    // here than dropping usually is: this is a **byte stream into a shell**, so half of
    // `rm -rf /tmp/scratch` is still a command that runs, and a notice afterwards does not unrun it.
    // Order and completeness are the contract; the pane going quiet is the symptom, not the trade.
    //
    // What bounds it in practice is the sender. These bytes reached us over a socket the browser had
    // to hold them in memory to write — `flushAttach` sends one array it has already built — so the
    // queue can only mirror what the page was already carrying, and it drains the instant the child
    // reads. A queue whose producer is bounded is not the same thing as an unbounded queue.
    let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Some(bytes) = in_rx.blocking_recv() {
            if writer.write_all(&bytes).is_err() {
                break;
            }
            let _ = writer.flush();
        }
    });

    // Periodic ping surfaces a browser that vanished without a Close frame, so we reap the PTY
    // promptly instead of leaving `sbx` running until the box happens to emit output.
    let mut keepalive = tokio::time::interval(Duration::from_secs(30));
    keepalive.tick().await; // the first tick fires immediately — discard it

    // Who ended the bridge decides what the caller may say afterwards: only a child that exited
    // under a still-open socket has an exit code worth reporting to anyone.
    let mut child_ended = false;
    loop {
        tokio::select! {
            out = out_rx.recv() => match out {
                Some(bytes) => {
                    if socket.send(Message::Binary(bytes)).await.is_err() { break; }
                }
                None => { child_ended = true; break; } // PTY closed (child exited)
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(b))) => {
                    // Never `.await` here. This is the branch handler of the `select!` above, and
                    // an await in it is an await with nothing else being polled (SKEIN-750).
                    let _ = in_tx.send(b);
                }
                Some(Ok(Message::Text(t))) => {
                    // resize control frame: {"resize":{"cols":N,"rows":M}}
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                        if let Some(r) = v.get("resize") {
                            let cols = r.get("cols").and_then(|x| x.as_u64()).unwrap_or(100) as u16;
                            let rows = r.get("rows").and_then(|x| x.as_u64()).unwrap_or(30) as u16;
                            let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            _ = keepalive.tick() => {
                if socket.send(Message::Ping(Vec::new())).await.is_err() { break; }
            }
        }
    }

    // Reap the child so it doesn't linger as a zombie. Kill (a no-op for one that already exited),
    // then wait off the async runtime (Child::wait blocks); dropping `master`/the reader closes the
    // PTY so descendants get SIGHUP.
    let _ = child.kill();
    let status = tokio::task::spawn_blocking(move || child.wait()).await;
    match (child_ended, status) {
        (true, Ok(Ok(st))) => Some(st.exit_code()),
        _ => None,
    }
}

/// Upgrade to a WebSocket that runs the interactive runtime login — the same flow `skein login`
/// attaches to a terminal, on a PTY the cockpit owns. The UI half opens this when the fleet's
/// credential expires (`/api/health` → `expired_logins`), so repair is a click rather than a shell.
pub(super) async fn login_terminal(
    ws: WebSocketUpgrade,
    Path(runtime): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin terminal blocked").into_response();
    }
    if !skein::runtime::valid_runtime(&runtime) {
        return (StatusCode::BAD_REQUEST, "unsupported runtime").into_response();
    }
    ws.on_upgrade(move |socket| login_session(socket, runtime))
}

/// The login half of the WS↔PTY bridge; [`pump_pty`] is shared with [`terminal_session`].
///
/// Deliberately NOT tmux-backed, unlike the box terminal: a login is a one-shot flow, and a socket
/// that drops mid-login should kill it — resuming a half-finished OAuth prompt in a session nobody
/// is attached to helps no one, and the next click simply starts a fresh one.
///
/// On exit 0 the post-login tail (`fleet::after_login`) runs HERE, in the server process — the CLI
/// path clears the refusal memory of the CLI process, which the long-running server never sees.
/// The sentences it returns go down the socket, and the socket closing is the UI's completion
/// signal either way.
async fn login_session(mut socket: WebSocket, runtime: String) {
    // Same cap as the box terminals: a login PTY is a PTY.
    //
    // **`AFTER_NO_WATCH` here, and `AFTER_WAIT_PTY` for a box terminal, for the same condition.**
    // The token says what the PANE should do, not what the server knows, and this pane cannot wait:
    // the login surface is a modal that closes with its socket, and one that reopened itself over
    // whatever somebody had moved on to would be worse than the walk back. So it names the control
    // that is still on screen behind it, which is the "log in" button they just pressed.
    let _permit = match PTY_LIMIT.try_acquire() {
        Ok(p) => PtySlot(Some(p)),
        Err(_) => {
            refuse(
                &mut socket,
                format!(
                    "skein: too many terminals open — {PTY_MAX} at once is the limit. Close a \
                     terminal and press log in again.\r\n"
                ),
                AFTER_NO_WATCH,
            )
            .await;
            return;
        }
    };
    // Infallible, and it used to be a `match` with a refusal arm (SKEIN-774). The only `Err`
    // `login_spawn_argv` could ever return was "no fleet sandbox configured", and `load_config`
    // repairs a blank `fleet_sandbox` before anybody reads it, so no value could reach that arm —
    // its sentence, its close code and its "press log in again" line were work spent on a pane
    // nobody can be shown. `docs/recovery-survey.md` §5 records it as GONE rather than fixed.
    let (program, argv) = skein::fleet::login_spawn_argv(&runtime);
    if runtime == "claude" {
        // The same coaching `skein login` prints: claude has no login subcommand, so the flow is
        // the TUI plus a slash command, and nothing on screen says so.
        let _ = socket
            .send(Message::Text(
                "skein: type /login once it starts, then /exit — `setup-token` returns a token to \
                 export and leaves no credential to seed boxes with\r\n"
                    .into(),
            ))
            .await;
    }
    let mut cmd = CommandBuilder::new(program);
    for a in &argv {
        cmd.arg(a);
    }
    // Propagate env so `sbx`/`bash` resolve on PATH, exactly as the box terminal does.
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }
    // `AFTER_NO_WATCH` for the pump's own failures too, and not the box terminal's `AFTER_RETRY`: the
    // cap refusal above says why a login modal must not reopen itself.
    match pump_pty(&mut socket, cmd, AFTER_NO_WATCH).await {
        Some(0) => {
            let rt = runtime.clone();
            let said = tokio::task::spawn_blocking(move || skein::fleet::after_login(&rt))
                .await
                .unwrap_or_else(|e| {
                    vec![format!("logged in, but the post-login share failed: {e}")]
                });
            for line in said {
                let _ = socket
                    .send(Message::Text(format!("skein: {line}\r\n")))
                    .await;
            }
        }
        Some(code) => {
            let _ = socket
                .send(Message::Text(format!(
                    "skein: login exited {code} — nothing changed\r\n"
                )))
                .await;
        }
        // The browser went first, or the pump refused and closed the socket saying so. Either way
        // there is nobody to tell and nothing left to tell them on.
        None => return,
    }
    // **The close is written and waited for, rather than left to the drop** (SKEIN-746). Every
    // sentence above is the last thing this flow says — `after_login`'s account of what was shared
    // with which boxes, or the exit code of an abandoned login — and the overlay toasts the last of
    // them. Returning here would drop the socket with the browser's own resizes still unread on it,
    // and a socket dropped on unread bytes is reset rather than closed, which discards the peer's
    // queue: the toast then says "nothing changed" about a login that worked.
    close_saying(&mut socket, AFTER_CHILD_ENDED, "the login flow is over").await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The pump's own failures say what the pane is about to do, and the login modal keeps the
    /// button** (SKEIN-883).
    ///
    /// The box terminal's sentence is the owner's approved one; the login modal's is the one it had,
    /// because its pane does not retry. And which of the two each caller gets is the argument it
    /// hands `pump_pty`, read off the source: the call is inside two websocket handlers no unit test
    /// can drive, and the browser suite (`tests/ui/recovery.mjs`) only reaches the box terminal.
    ///
    /// **What would make this fail:** `login_session` handing `pump_pty` `AFTER_RETRY` — the login
    /// modal would then close as `retry`, a token its page has no strip for — or `skeins_own_fault`
    /// ignoring `after` and saying the same thing to both.
    #[test]
    fn the_box_terminal_retries_and_the_login_modal_keeps_the_button() {
        let retry = skeins_own_fault("w", "e", "check the thing", AFTER_RETRY);
        assert!(
            retry.contains(
                "nothing in your box is wrong. This pane tries again by itself for a few minutes. \
                 If it keeps failing, check the thing."
            ),
            "{retry}"
        );
        let button = skeins_own_fault("w", "e", "check the thing", AFTER_NO_WATCH);
        assert!(button.contains("use Try again below"), "{button}");
        assert!(!button.contains("tries again by itself"), "{button}");

        // Everything above this module, so the strings this test searches for are not found in the
        // test itself.
        let whole = include_str!("terminal.rs");
        let src = &whole[..whole
            .find("#[cfg(test)]\nmod tests")
            .expect("the test module")];
        let login = &src[src.find("async fn login_session").expect("login_session")..];
        assert!(
            login.contains("pump_pty(&mut socket, cmd, AFTER_NO_WATCH)"),
            "the login modal's pump failures no longer close as no-watch"
        );
        let session = &src[..src.find("async fn login_session").unwrap()];
        assert!(
            session.contains("pump_pty(&mut socket, cmd, AFTER_RETRY)"),
            "the box terminal's pump failures no longer close as retry"
        );
    }
}
