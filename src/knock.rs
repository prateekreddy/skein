//! The doorstep: what a connection is between `accept` and saying who it is.
//!
//! Every guard skein has runs **after** accept. `apiauth::authorised` is a layer over the router, so
//! it sees a request; the PTY and event caps are inside their handlers, so they count clients that
//! already presented a credential. That leaves the space before the first byte unguarded, and
//! architecture §9.4 names what lives there: the cockpit port is reachable from every box —
//! one network namespace, nothing to be done about it — and a box that opens sockets and never
//! authenticates costs the control plane, and therefore the approval surface, with no credential at
//! all. §11.5's blocked state cannot render if nothing serves it.
//!
//! ## Why a cap alone would be the wrong answer
//!
//! A limit that **refuses** when it is full converts exhaustion into denial and calls it a fix: the
//! flooder still decides who gets in, because the honest client arrives to a full room. The usual
//! escape is a per-source allowance, and skein does not have one to give — in-fleet every box shares
//! skein's network namespace, so the peer address of a box's connection is the peer address of
//! skein's own. Per-source is answerable only once §9.5's uid split exists and a connection can be
//! attributed to a uid; before it, there is nothing to key on. That is a finding, not an omission.
//!
//! So the doorstep is bounded by **eviction, not refusal**: an arrival is always admitted, and when
//! the room is over its limit the *oldest connection that still has not said who it is* leaves. The
//! asymmetry is the whole mechanism. An honest client proves itself in the time of one request and
//! stops being evictable; a flooder never does, so a flood evicts itself and the last arrival — the
//! one that will authenticate — is the one holding a place.
//!
//! A grace deadline is the second half, and cheaper than the first: a socket that has not presented
//! a credential within [`grace`] is not a cockpit, and closing it means a flood costs its author a
//! reconnection per slot rather than a socket held for free.
//!
//! What this does **not** claim: that an authenticated client cannot exhaust anything. That is what
//! the post-auth caps are for, and they are elsewhere. This module knows nothing about sockets,
//! addresses or requests — it hands out places and takes them back, and the server is what holds the
//! connection those places stand for.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

/// How many connections may be on the doorstep at once.
///
/// Generous rather than tight, because being over it is not an error: a proven connection leaves the
/// doorstep the moment its first authenticated request lands, so the only thing this bounds is
/// *concurrent handshakes*. A browser opens about six connections per tab and proves them in
/// milliseconds; sixty-four is many tabs' worth of arrivals in flight at the same instant.
pub const ROOM: usize = 64;

/// How long a connection may stay on the doorstep without saying who it is.
///
/// `$SKEIN_DOORSTEP_GRACE` (whole seconds) overrides it. That exists so the flood test can run in
/// seconds instead of minutes; it is read here rather than threaded through the server so that the
/// default is one number in one place.
pub fn grace() -> Duration {
    match std::env::var("SKEIN_DOORSTEP_GRACE") {
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(secs) if secs > 0 => Duration::from_secs(secs),
            _ => Duration::from_secs(10),
        },
        Err(_) => Duration::from_secs(10),
    }
}

/// The one doorstep this process has.
///
/// A process-wide value rather than one threaded from `main`, because there is exactly one listener
/// and the numbers are wanted by a request handler that has no way to be handed it. Two doorsteps
/// would be worse than none: each would bound its own half of the connections and the count served
/// would be one of them.
pub fn doorstep() -> &'static Arc<Doorstep> {
    static ONE: std::sync::OnceLock<Arc<Doorstep>> = std::sync::OnceLock::new();
    ONE.get_or_init(|| Doorstep::with_room(ROOM))
}

/// The room, and the order everyone arrived in.
pub struct Doorstep {
    room: usize,
    inner: Mutex<Inner>,
}

struct Inner {
    /// Arrival order. Monotonic, so "oldest" is a `first_key_value` and never a scan.
    next: u64,
    knocking: BTreeMap<u64, Arc<Notify>>,
    ousted: u64,
}

impl Doorstep {
    pub fn with_room(room: usize) -> Arc<Self> {
        Arc::new(Self {
            room: room.max(1),
            inner: Mutex::new(Inner {
                next: 0,
                knocking: BTreeMap::new(),
                ousted: 0,
            }),
        })
    }

    /// Take a place. Never refuses; over the limit, the oldest unproven place is taken back instead.
    ///
    /// The eviction happens *after* the arrival is recorded, so a full room evicts somebody else and
    /// not the caller — which is the property the whole module exists for.
    pub fn admit(self: &Arc<Self>) -> Knock {
        let evict = Arc::new(Notify::new());
        let mut inner = self.inner.lock().expect("doorstep");
        let seq = inner.next;
        inner.next += 1;
        inner.knocking.insert(seq, evict.clone());
        while inner.knocking.len() > self.room {
            let oldest = *inner
                .knocking
                .keys()
                .next()
                .expect("over the limit, so nonempty");
            if let Some(going) = inner.knocking.remove(&oldest) {
                going.notify_one();
                inner.ousted += 1;
            }
        }
        Knock {
            door: self.clone(),
            seq,
            evict,
            proven: AtomicBool::new(false),
        }
    }

    /// How many are still on the doorstep.
    pub fn knocking(&self) -> usize {
        self.inner.lock().expect("doorstep").knocking.len()
    }

    /// How many places have been taken back.
    ///
    /// Served at `/api/machine/doorstep` and declared as [`crate::signal::Signal::MachineDoorstep`],
    /// because a doorstep that is evicting steadily means one of two opposite things — something is
    /// flooding the port, or [`ROOM`] is too small for how the cockpit is really used and honest
    /// handshakes are being displaced. Bounding the flood is what makes it harmless; that is also
    /// what would make it invisible, and a counter nobody can read is not a defence.
    pub fn turned_away(&self) -> u64 {
        self.inner.lock().expect("doorstep").ousted
    }

    fn leave(&self, seq: u64) {
        self.inner.lock().expect("doorstep").knocking.remove(&seq);
    }
}

/// One connection's place on the doorstep. Dropping it gives the place back.
pub struct Knock {
    door: Arc<Doorstep>,
    seq: u64,
    evict: Arc<Notify>,
    proven: AtomicBool,
}

impl Knock {
    /// This connection has authenticated. It leaves the doorstep and stops being evictable — which
    /// is what makes a flood of connections that never authenticate unable to displace it.
    ///
    /// Idempotent: a client makes many authenticated requests on one connection, and only the first
    /// changes anything.
    pub fn prove(&self) {
        if !self.proven.swap(true, Ordering::SeqCst) {
            self.door.leave(self.seq);
        }
    }

    pub fn proven(&self) -> bool {
        self.proven.load(Ordering::SeqCst)
    }

    /// Resolves when this connection's place has been taken back and it has **not** proven itself.
    ///
    /// The loop is the race: a connection can be evicted in the instant before its first
    /// authenticated request is read. Waking and finding itself proven, it goes back to waiting —
    /// forever, since nothing else will notify it — so the caller's `select` never closes an
    /// authenticated connection over a notification that arrived a moment too late.
    pub async fn ousted(&self) {
        loop {
            self.evict.notified().await;
            if !self.proven() {
                return;
            }
        }
    }
}

impl Drop for Knock {
    fn drop(&mut self) {
        if !self.proven() {
            self.door.leave(self.seq);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the whole module is for: the newest arrival keeps its place, and the oldest one
    /// that never said who it was is the one that loses it.
    #[tokio::test]
    async fn the_room_makes_space_by_evicting_the_oldest_stranger() {
        let door = Doorstep::with_room(3);
        let a = door.admit();
        let b = door.admit();
        let c = door.admit();
        assert_eq!(door.knocking(), 3);
        let d = door.admit();

        tokio::time::timeout(Duration::from_secs(1), a.ousted())
            .await
            .expect("the oldest stranger leaves");
        assert_eq!(door.turned_away(), 1);
        for (who, k) in [("b", &b), ("c", &c), ("d", &d)] {
            assert!(
                tokio::time::timeout(Duration::from_millis(50), k.ousted())
                    .await
                    .is_err(),
                "{who} arrived after the oldest and must keep its place"
            );
        }
    }

    /// A flood cannot displace a client that authenticated, however long it goes on: proving leaves
    /// the doorstep, and eviction only reaches what is still standing on it.
    #[tokio::test]
    async fn a_connection_that_said_who_it_is_survives_any_flood() {
        let door = Doorstep::with_room(2);
        let honest = door.admit();
        honest.prove();
        assert_eq!(door.knocking(), 0, "proving leaves the doorstep");

        let flood: Vec<Knock> = (0..50).map(|_| door.admit()).collect();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), honest.ousted())
                .await
                .is_err(),
            "an authenticated connection is not on the doorstep to be evicted"
        );
        assert_eq!(door.knocking(), 2, "the flood is bounded by the room");
        assert_eq!(door.turned_away(), 48, "and it evicted itself, nobody else");
        drop(flood);
    }

    /// A connection that hangs up gives its place back without anyone being evicted — otherwise a
    /// busy cockpit would evict on churn alone.
    #[tokio::test]
    async fn leaving_gives_the_place_back() {
        let door = Doorstep::with_room(2);
        let first = door.admit();
        let second = door.admit();
        drop(second);
        assert_eq!(door.knocking(), 1);
        let third = door.admit();
        assert_eq!(
            door.turned_away(),
            0,
            "room was made by leaving, not by eviction"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), first.ousted())
                .await
                .is_err()
        );
        drop(third);
    }

    /// An eviction that lands in the same instant as the first authenticated request does not close
    /// the connection: `ousted` goes back to waiting, and waits forever.
    #[tokio::test]
    async fn a_notification_that_arrives_a_moment_too_late_is_ignored() {
        let door = Doorstep::with_room(1);
        let racing = door.admit();
        let _next = door.admit(); // evicts `racing`
        racing.prove(); // …which authenticates a moment later
        assert!(
            tokio::time::timeout(Duration::from_millis(50), racing.ousted())
                .await
                .is_err(),
            "a proven connection is never closed by an eviction it outran"
        );
    }

    #[test]
    fn the_grace_is_overridable_for_tests_and_sane_without_one() {
        // Serialised, and the argument for not bothering is written down in `docs/env-lock.toml`
        // as the reason this was the weakest of three exemptions rather than a safe one: being the
        // only READER does not make a write to a table every thread shares safe, and the restore
        // below is one `#[should_panic]` sibling away from leaking `nonsense` into the next test.
        let _env = crate::testutil::env_lock();
        let before = std::env::var("SKEIN_DOORSTEP_GRACE").ok();
        std::env::remove_var("SKEIN_DOORSTEP_GRACE");
        assert_eq!(grace(), Duration::from_secs(10));
        std::env::set_var("SKEIN_DOORSTEP_GRACE", "2");
        assert_eq!(grace(), Duration::from_secs(2));
        std::env::set_var("SKEIN_DOORSTEP_GRACE", "nonsense");
        assert_eq!(
            grace(),
            Duration::from_secs(10),
            "a bad value is not no deadline"
        );
        std::env::set_var("SKEIN_DOORSTEP_GRACE", "0");
        assert_eq!(
            grace(),
            Duration::from_secs(10),
            "zero would close every connection"
        );
        match before {
            Some(v) => std::env::set_var("SKEIN_DOORSTEP_GRACE", v),
            None => std::env::remove_var("SKEIN_DOORSTEP_GRACE"),
        }
    }
}
