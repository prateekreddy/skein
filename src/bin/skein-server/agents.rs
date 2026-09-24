//! Settings → Update's list of the boxes still running an older agent CLI than the one installed,
//! and the one-box restart that moves a waiting box onto it (SKEIN-1070, SKEIN-1071).

use super::*;

/// The boxes still on an old agent CLI, per runtime — `fleet::agents_behind` over the board's own
/// view of each box, so the turn state the pane offers a restart on is the one the board shows.
///
/// Its own route rather than a field of `/api/update`: that one is polled every second or so while
/// GitHub has not answered, and this asks every running box when its agent started.
pub(super) async fn api_agents_behind() -> Json<serde_json::Value> {
    let found = tokio::task::spawn_blocking(|| {
        let boxes: Vec<skein::fleet::BoxTurn> = load_views()
            .unwrap_or_default()
            .into_iter()
            .map(|v| skein::fleet::BoxTurn {
                name: v.name,
                runtime: v.agent,
                state: v.state,
            })
            .collect();
        skein::fleet::agents_behind(&boxes)
    })
    .await;
    Json(match found {
        Ok(runtimes) => serde_json::json!({ "runtimes": runtimes }),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    })
}

/// Why a restart press was not carried out.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum NotRestarted {
    /// The box is no longer `waiting` — its turn state as the board reads it now, at the press.
    NotWaiting(String),
    /// It was waiting, and the restart itself failed.
    Failed(String),
}

/// Restart ONE box's agent onto the installed CLI — the box in the path, and no other.
///
/// **The turn state is read again here, at the press**, because the list was drawn some time ago
/// and a box that has started working since is exactly the one this must not cut off. Only
/// `waiting` is restarted; every other state is refused, and `working` is named so the page can say
/// the box started working after the list was drawn.
///
/// `state_of` and `reopen` are arguments so a test can stand in for the board and the box: the
/// route below hands it `load_views` and `sandbox::reopen_agent_session`.
pub(super) fn restart_if_waiting(
    name: &str,
    state_of: impl FnOnce(&str) -> Option<String>,
    reopen: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), NotRestarted> {
    let state = state_of(name).unwrap_or_default();
    if state != "waiting" {
        return Err(NotRestarted::NotWaiting(state));
    }
    reopen(name).map_err(NotRestarted::Failed)
}

/// `POST /api/update-agents/boxes/:name/restart`. Answers `{ok}`, `{ok:false, why:"not-waiting",
/// state}` or `{ok:false, why:"failed", error}`.
pub(super) async fn api_restart_onto_update(Path(name): Path<String>) -> Json<serde_json::Value> {
    if !skein::util::valid_name(&name) {
        return Json(
            serde_json::json!({ "ok": false, "why": "failed", "error": "invalid box name" }),
        );
    }
    let out = tokio::task::spawn_blocking(move || {
        restart_if_waiting(
            &name,
            |name| {
                load_views()
                    .ok()?
                    .into_iter()
                    .find(|v| v.name == name)
                    .map(|v| v.state)
            },
            skein::sandbox::reopen_agent_session,
        )
    })
    .await;
    Json(match out {
        Ok(Ok(())) => serde_json::json!({ "ok": true }),
        Ok(Err(NotRestarted::NotWaiting(state))) => {
            serde_json::json!({ "ok": false, "why": "not-waiting", "state": state })
        }
        Ok(Err(NotRestarted::Failed(e))) => {
            serde_json::json!({ "ok": false, "why": "failed", "error": e })
        }
        Err(e) => serde_json::json!({ "ok": false, "why": "failed", "error": e.to_string() }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// **A press on a box that turned busy is refused, and nothing is restarted** (SKEIN-1071).
    ///
    /// **What makes it fail:** dropping the re-check (restarting whatever the page asked for) makes
    /// the `working` press call `reopen`; checking `!= "working"` instead of `== "waiting"` lets a
    /// `needs-input` box through.
    #[test]
    fn a_press_on_a_box_that_is_no_longer_waiting_is_refused() {
        for state in ["working", "needs-input", "compacting", ""] {
            let reopened = RefCell::new(Vec::<String>::new());
            let out = restart_if_waiting(
                "box-a",
                |_| Some(state.to_string()),
                |n| {
                    reopened.borrow_mut().push(n.to_string());
                    Ok(())
                },
            );
            assert_eq!(out, Err(NotRestarted::NotWaiting(state.to_string())));
            assert!(
                reopened.borrow().is_empty(),
                "a {state:?} box was restarted: {:?}",
                reopened.borrow()
            );
        }
    }

    /// **One press restarts one box: the one it names.**
    ///
    /// **What makes it fail:** a restart that walked the list (every waiting box) calls `reopen`
    /// more than once, or for a name other than the one pressed.
    #[test]
    fn a_press_restarts_exactly_the_box_it_names() {
        let reopened = RefCell::new(Vec::<String>::new());
        let out = restart_if_waiting(
            "box-a",
            |_| Some("waiting".into()),
            |n| {
                reopened.borrow_mut().push(n.to_string());
                Ok(())
            },
        );
        assert_eq!(out, Ok(()));
        assert_eq!(*reopened.borrow(), vec!["box-a".to_string()]);

        let failed = restart_if_waiting("box-a", |_| Some("waiting".into()), |_| Err("no".into()));
        assert_eq!(failed, Err(NotRestarted::Failed("no".into())));
    }

    /// **No route restarts more than one box per press** (SKEIN-1071), read off the source for the
    /// reason `cockpit_routes` gives: axum will not enumerate its routes.
    ///
    /// Three things together: the reopen is reached from this file alone; in it, from one handler
    /// alone, which takes one name from its path; and that handler is routed once, under a path
    /// with one `:name` in it.
    ///
    /// **What makes it fail:** a "restart all" — a second handler here, or one in another route
    /// file, that loops over the waiting boxes and reopens each — fails the first or second count;
    /// routing the handler under a path that names no box, or twice, fails the third.
    #[test]
    fn no_route_restarts_more_than_one_box_per_press() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/skein-server");
        let needle = concat!("reopen_agent", "_session");
        let mut reaching: Vec<String> = std::fs::read_dir(&dir)
            .expect("the server's route files")
            .filter_map(|e| e.ok())
            .filter(|e| std::fs::read_to_string(e.path()).is_ok_and(|text| text.contains(needle)))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        reaching.sort();
        assert_eq!(reaching, vec!["agents.rs".to_string()]);

        let this = std::fs::read_to_string(dir.join("agents.rs")).unwrap();
        // The code, without its comments: a doc line that names the reopen calls nothing.
        let production: String = this[..this.find("#[cfg(test)]").expect("a test module")]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let handler = production
            .find(concat!(
                "async fn ",
                "api_restart_onto_update(Path(name): Path<String>)"
            ))
            .expect("the handler takes one box name from its path");
        let uses: Vec<usize> = production.match_indices(needle).map(|(i, _)| i).collect();
        assert_eq!(
            uses.len(),
            1,
            "the reopen is reached more than once in this file's handlers"
        );
        assert!(
            uses[0] > handler,
            "the one use of the reopen is not in the one-name handler"
        );

        let main = std::fs::read_to_string(dir.join("main.rs")).unwrap();
        let routed: Vec<&str> = main
            .split(concat!(".", "route", "("))
            .skip(1)
            .filter(|entry| entry.contains(concat!("(api_restart_", "onto_update)")))
            .map(|entry| entry.split('"').nth(1).unwrap_or(""))
            .collect();
        assert_eq!(
            routed,
            vec!["/api/update-agents/boxes/:name/restart"],
            "the restart has to be routed once, under one box's name"
        );
    }
}
