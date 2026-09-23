//! Test helpers shared by more than one of this module's test files.

/// Blocks the caller until a just-spawned responder thread confirms it is actually about to
/// `accept()`, not merely that its listener is bound.
///
/// **SKEIN-1024:** a narrowing, not the fix. `TcpListener::bind` already completes the
/// kernel-side `listen()`, so a probe's connection lands in the backlog whether or not the
/// responder thread has run; what this adds is that the probe starts only once the thread
/// has been scheduled at least once, so a responder that never ran before the probe's own
/// fixed budget (`curl -m 8`) expired is ruled out rather than guessed about. Holding the
/// thread past that budget does reproduce the ticket's text, but no load this box could
/// produce ever held a thread that long.
///
/// **What the ticket actually saw** is the race [`finish_responder`] closes: the fixture's
/// own deadline, counted from thread start, dropped the listener before a late-starting
/// probe connected, so the probe was refused (`000`) and the captured request was empty.
/// Planting an 11s delay between this call and the probe reproduced that verbatim against
/// a 10s thread-start deadline, and passes now that no deadline exists.
///
/// Kept because it is cheap and removes one way for the responder to lose a race it cannot
/// see; what makes these tests safe on a loaded box is that the test side has no wall clock
/// at all (see [`finish_responder`]). The production timeouts are untouched: they are the
/// budget a real proxy gets, and no business of this fixture's.
pub(super) fn wait_until_accepting(ready: std::sync::mpsc::Receiver<()>) {
    ready
        .recv()
        .expect("the fake server's thread panicked before it could start accepting");
}
