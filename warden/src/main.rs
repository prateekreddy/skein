//! `skein-warden` — the host service that owns fleet create and destroy (architecture §8).
//!
//! Run it on the host, outside the fleet. It listens on loopback and nothing else; see
//! [`skein_warden::serve`] for why that is the answer and when it has to change.
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

    let listener = match bind(port) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("skein-warden: could not bind 127.0.0.1:{port}: {e}");
            std::process::exit(1);
        }
    };
    let addr = listener.local_addr().ok();

    // The controlling terminal, if there is one. `/dev/tty` failing to open is not an error to
    // handle — it is the answer "nobody is here", and a warden with nobody at it refuses every doer
    // rather than finding some other way to say yes. See `approval.rs`.
    let (approver, surface): (Box<dyn Approver>, &str) = match Console::at_the_terminal() {
        Some(console) => (Box::new(console), "this terminal"),
        None => (Box::new(Unattended), "none"),
    };

    let warden = Arc::new(Warden {
        store: Store::new(home.join("outcomes"), RETENTION),
        log: Log::new(home.join("audit.jsonl")),
        approver,
        doorway: skein_warden::flooding::Doorway::new(),
        secret: skein_warden::secret::Secret::kept_in(&home),
    });

    eprintln!(
        "skein-warden: listening on {} — capabilities: {}",
        addr.map(|a| a.to_string()).unwrap_or_else(|| "?".into()),
        match skein_warden::capability::linked().as_slice() {
            [] => "none built".to_string(),
            linked => linked
                .iter()
                .map(|c| c.name())
                .collect::<Vec<_>>()
                .join(", "),
        }
    );

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
    warden.serve(listener);
}
