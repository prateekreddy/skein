//! `skein-warden` — the host service that owns fleet create and destroy (architecture §8).
//!
//! Run it on the host, outside the fleet. It listens on loopback and nothing else; see
//! [`skein_warden::serve`] for why that is the answer and when it has to change.
//!
//! Its state lives beside skein's, under `~/.skein/warden/`, and it is **a thing to back up**
//! (§8.2): the outcome store is what makes a retried destroy safe, and the audit log is the only
//! account of the fleet's privileged operations that skein did not write itself.

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
    let port: u16 = std::env::var("SKEIN_WARDEN_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7879);
    let home = std::env::var("SKEIN_WARDEN_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                .join(".skein/warden")
        });

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
    eprintln!(
        "skein-warden: state in {} — back this up; it is what makes a retried destroy safe",
        home.display()
    );
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
