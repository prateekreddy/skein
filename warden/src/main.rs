//! `skein-warden` — the host service that owns fleet create and destroy (architecture §8).
//!
//! Run it on the host, outside the fleet. It listens on loopback and on each Docker bridge, and
//! never on `0.0.0.0`; see [`skein_warden::serve`] for the measurement that decided that.
//!
//! Its state lives under the volume root — `{$SKEIN_HOME | ~/.skein}/warden/`, the derivation
//! [`skein_warden::home`] explains, `$SKEIN_WARDEN_HOME` overriding for tests and development —
//! and it is **a thing to back up** (§8.2): the outcome store is what makes a retried destroy
//! safe, and the audit log is the only account of the fleet's privileged operations that skein
//! did not write itself.

use skein_warden::approval::Console;
use skein_warden::audit::Log;
use skein_warden::doer::{Approver, Unattended};
use skein_warden::outcome::Store;
use skein_warden::serve::{bind, Warden};
use std::sync::Arc;
use std::time::Duration;

/// How long an outcome is kept. Past it an operation id is answered `unknown` — never re-run; see
/// the note in `outcome.rs` on why the id itself is kept for ever.
const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

fn main() {
    use skein_warden::serve::WHERE_SKEIN_LOOKS;
    let port: u16 = std::env::var("SKEIN_WARDEN_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(WHERE_SKEIN_LOOKS);
    // Derived from the volume root, so the secret below sits under the cover skein derives over
    // the same root (§9.5 R5) — `home()` says why, and `warden_client::secret` is the other end.
    let home = skein_warden::home();
    // A secret from before the home followed the volume is moved in, not re-minted — the pairing
    // survives the path change, and nothing secret-shaped stays at the uncovered old default.
    skein_warden::secret::adopt_left_behind(&home);
    // The RECORD goes the other way (SKEIN-218). The home above follows the volume for the
    // secret's sake, and delivery 4c mounts that volume into the fleet — so the log and the
    // outcomes, which must sit where the audited thing cannot reach them, live beside it instead.
    // `audit_home()` derives that, and anything left under the volume by an earlier warden is
    // moved out rather than read in place.
    let record = skein_warden::audit_home();
    skein_warden::audit::adopt_left_behind(&home, &record);

    // Loopback and each Docker bridge; `serve::bind` says why, and why never `0.0.0.0`. The error
    // names the addresses it was going to take rather than one guessed name, because on a Linux
    // host the one that fails is usually the bridge and "could not bind 127.0.0.1" would send the
    // reader to the wrong place entirely.
    let listeners = match bind(port) {
        Ok(listeners) => listeners,
        Err(e) => {
            let wanted = std::iter::once("127.0.0.1".to_string())
                .chain(
                    skein_warden::serve::bridge_addresses()
                        .iter()
                        .map(|a| a.to_string()),
                )
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!("skein-warden: could not listen on port {port} at {wanted}: {e}");
            std::process::exit(1);
        }
    };
    let addrs = listeners
        .iter()
        .filter_map(|l| l.local_addr().ok())
        .map(|a| a.to_string())
        .collect::<Vec<_>>();

    // The controlling terminal, if there is one. `/dev/tty` failing to open is not an error to
    // handle — it is the answer "nobody is here", and a warden with nobody at it refuses every doer
    // rather than finding some other way to say yes. See `approval.rs`.
    let (approver, surface): (Box<dyn Approver>, &str) = match Console::at_the_terminal() {
        Some(console) => (Box::new(console), "this terminal"),
        None => (Box::new(Unattended), "none"),
    };

    let warden = Arc::new(Warden {
        store: Store::new(record.join("outcomes"), RETENTION),
        log: Log::new(record.join("audit.jsonl")),
        approver,
        doorway: skein_warden::flooding::Doorway::new(),
        secret: skein_warden::secret::Secret::kept_in(&home),
    });

    eprintln!(
        "skein-warden: listening on {} — capabilities: {}",
        match addrs.is_empty() {
            true => "?".to_string(),
            false => addrs.join(", "),
        },
        match skein_warden::capability::linked().as_slice() {
            [] => "none built".to_string(),
            linked => linked
                .iter()
                .map(|c| c.name())
                .collect::<Vec<_>>()
                .join(", "),
        }
    );

    // **And the pairing said at the moment somebody moves the VOLUME.**
    //
    // The home above is derived from `$SKEIN_HOME`, and the two processes read their own
    // environments: `SKEIN_HOME=/mnt/backup skein …` with a warden started without it leaves skein
    // reading a secret at one path and the warden minting one at another. The failure is loud —
    // every request refused, which is what a mismatched secret is supposed to look like — but the
    // cause is not, and "the warden refuses everything" reads as a broken warden rather than as two
    // processes disagreeing about where the volume is. Whatever repoints one must repoint both.
    //
    // The record does NOT move with it, by design: see `audit_home()`.
    if let Some(volume) = std::env::var_os("SKEIN_HOME").filter(|s| !s.is_empty()) {
        eprintln!(
            "skein-warden: volume {} (from $SKEIN_HOME) — skein must be pointed at the same one, \
             or every request is refused for a mismatched secret; the record is at {}",
            std::path::Path::new(&volume).display(),
            record.display()
        );
    }

    // **The pairing, said at the moment somebody changes the port.**
    //
    // The two ends have separate variables — `$SKEIN_WARDEN_PORT` here, `$SKEIN_WARDEN` there —
    // because a warden binds loopback and takes no host (§8.6), while a client after 4c has to name
    // one. That is right and it is also the trap: move this one and skein goes on asking 7879, where
    // it now meets whatever else is listening and reports a warden that refuses rather than one that
    // is missing. Saying it here costs a line and closes the loop where the decision was made.
    if port != WHERE_SKEIN_LOOKS {
        eprintln!(
            "skein-warden: this is not where skein looks by default — export \
             SKEIN_WARDEN=127.0.0.1:{port} for skein and skein-server, or they keep asking {WHERE_SKEIN_LOOKS}"
        );
    }
    eprintln!(
        "skein-warden: state in {} — back this up; it is what makes a retried destroy safe",
        home.display()
    );
    // Said at start, because the two failures look identical from a client: a warden that refuses
    // everything and a warden nobody is talking to both read as "the warden is not working".
    match warden.secret.missing() {
        true => eprintln!(
            "skein-warden: NO SECRET at {} — it could not be read or minted, so every request is \
             refused. Fix the path and restart.",
            warden.secret.where_().display()
        ),
        false => eprintln!(
            "skein-warden: skein is recognised by the secret at {} — nothing outside the mount \
             cover can read it",
            warden.secret.where_().display()
        ),
    }
    match surface {
        "none" => eprintln!(
            "skein-warden: no controlling terminal, so there is nobody to approve anything and \
             every doer refuses. Run it where a person can answer it."
        ),
        at => eprintln!(
            "skein-warden: approvals are asked at {at}, and answered by typing the operation id"
        ),
    }
    warden.serve(listeners);
}
