//! `skein-warden` — the host service that owns fleet create and destroy (architecture §8).
//!
//! Run it on the host, outside the fleet. It listens on loopback and nothing else; see
//! [`skein_warden::serve`] for why that is the answer and when it has to change.
//!
//! Its state lives beside skein's, under `~/.skein/warden/`, and it is **a thing to back up**
//! (§8.2): the outcome store is what makes a retried destroy safe, and the audit log is the only
//! account of the fleet's privileged operations that skein did not write itself.

use skein_warden::audit::Log;
use skein_warden::doer::Unattended;
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

    let warden = Arc::new(Warden {
        store: Store::new(home.join("outcomes"), RETENTION),
        log: Log::new(home.join("audit.jsonl")),
        // Refuses everything, and that is the state of the design rather than a placeholder: §8.1's
        // approval surface is not built yet, and a warden that ran privileged host commands in the
        // meantime would be the thing it exists to prevent.
        approver: Box::new(Unattended),
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
    eprintln!(
        "skein-warden: no approval surface is built, so every doer refuses. Nothing here can run a \
         privileged command yet."
    );
    warden.serve(listener);
}
