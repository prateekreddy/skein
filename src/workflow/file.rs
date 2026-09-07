//! `workflows.json`: where it lives, and how it survives a round trip.
//!
//! The question this answers is *what is on disk, and can skein read back what it wrote?* Strings
//! come in, [`super::Workflow`] goes out, and the checking that happens in between is the whole
//! point of the shape being different at each end — see [`Written`].
//!
//! Two properties are load-bearing and neither is obvious from the signatures. [`load`] is **all or
//! nothing**: a file with one bad step loads none of its good ones. And [`save`] parses BEFORE it
//! writes, because a refusal that arrives after the old file is gone is a refusal that cost
//! somebody their workflows.

use super::{spell_act, spell_cond, Act, Cond, Step, Workflow};
use serde::{Deserialize, Serialize};

/// The file as it is written down. Deliberately not [`Workflow`]: what is on disk is strings, and
/// turning strings into a closed vocabulary is the whole of the checking this module does.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct Written {
    #[serde(default)]
    workflow: Vec<WrittenFlow>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct WrittenFlow {
    name: String,
    #[serde(default)]
    matches: Vec<String>,
    /// See [`Workflow::serial`]. Defaulted, so every file written before the merge train still
    /// reads — and serialized always, so an editor's round trip cannot drop it.
    #[serde(default)]
    serial: bool,
    #[serde(default)]
    steps: Vec<WrittenStep>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct WrittenStep {
    #[serde(default)]
    when: Vec<String>,
    /// `do` in the file, because that is what it is. It is a keyword in Rust and nowhere else.
    #[serde(rename = "do")]
    act: String,
}

/// Where the fleet's workflows are written down.
///
/// Beside skein's own state rather than in a repo: the owner asked for several workflows assignable
/// to any pull request in any repo, so they belong to the fleet. Which PR carries which is a
/// separate, per-repo question.
pub fn workflows_path() -> std::path::PathBuf {
    crate::config::skein_home().join("workflows.json")
}

/// Read every workflow, or refuse the file.
///
/// **All or nothing.** A file with one bad step does not load its good ones: a workflow that
/// silently lost the step between "checks passed" and "merge" is a workflow that merges without
/// checks, and half of an automation is worse than none of it. The error names the workflow, the
/// step and the word.
pub fn load() -> Result<Vec<Workflow>, String> {
    let path = workflows_path();
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        // No file is not a fault: it is a fleet where nobody has written one.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{} could not be read: {e}", path.display())),
    };
    from_bytes(&raw)
}

/// The half of [`load`] that has no filesystem in it, so every refusal can be tested.
pub fn from_bytes(raw: &[u8]) -> Result<Vec<Workflow>, String> {
    if raw.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(Vec::new());
    }
    let written: Written =
        serde_json::from_slice(raw).map_err(|e| format!("this is not a workflow file: {e}"))?;
    let mut out = Vec::new();
    for flow in written.workflow {
        let at = |what: &str| format!("workflow {:?}: {what}", flow.name);
        if flow.name.trim().is_empty() {
            return Err("a workflow with no name cannot be assigned to anything".into());
        }
        if flow.steps.is_empty() {
            return Err(at("has no steps, so it would never do anything"));
        }
        let mut matches = Vec::new();
        for atom in &flow.matches {
            matches.push(Cond::parse(atom).map_err(|why| at(&why))?);
        }
        let mut steps = Vec::new();
        for (n, step) in flow.steps.iter().enumerate() {
            let at = |what: &str| format!("workflow {:?}, step {}: {what}", flow.name, n + 1);
            let mut when = Vec::new();
            for atom in &step.when {
                when.push(Cond::parse(atom).map_err(|why| at(&why))?);
            }
            steps.push(Step {
                when,
                act: Act::parse(&step.act).map_err(|why| at(&why))?,
            });
        }
        if out.iter().any(|w: &Workflow| w.name == flow.name) {
            return Err(at(
                "is defined twice, and skein cannot tell which one you meant",
            ));
        }
        out.push(Workflow {
            name: flow.name,
            matches,
            serial: flow.serial,
            steps,
        });
    }
    Ok(out)
}

/// Write the file, having first proved skein can read back what it is about to write.
///
/// The editor sends a whole file, so this is the one moment a person can replace every workflow in
/// the fleet with something that does not parse. It is checked BEFORE the write, not after: a
/// refusal that arrives after the old file is gone is a refusal that cost somebody their workflows.
///
/// Returns what was saved, so a caller can answer with what it will read back rather than with what
/// it was handed.
pub fn save(raw: &[u8]) -> Result<Vec<Workflow>, String> {
    let flows = from_bytes(raw)?;
    let path = workflows_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    // Written from what was PARSED rather than from the bytes, so the file on disk is always in the
    // shape this module writes — an editor cannot leave a comment, a stray field or an ordering
    // that reads back differently the next time.
    let body = to_bytes(&flows)?;
    // `write_atomic` under `with_lock`, rather than the `fs::write` + `rename` this used to do.
    //
    // Two separate defects in one line. The write was atomic against a READER and not against a
    // CRASH — `util::write_atomic`'s note records the incident: the rename is journalled, the bytes
    // are still in the page cache, and a hard kill in that window leaves the file present and
    // zero-length. And there was no lock at all, so two cockpit tabs saving workflows was a lost
    // update on a file a person had just edited by hand.
    let dir = path
        .parent()
        .ok_or("no directory to write the workflows into")?
        .to_path_buf();
    crate::util::with_lock(&crate::util::lock_beside(&path)?, || {
        crate::util::write_atomic(&path, &dir, &body)
            .map_err(|e| format!("{}: {e}", path.display()))
    })?;
    Ok(flows)
}

/// Write workflows back, in the shape [`load`] reads.
///
/// Round-tripping matters more here than in most places: the cockpit edits these, and an editor
/// that cannot read back what it wrote is one that quietly loses a step.
pub fn to_bytes(flows: &[Workflow]) -> Result<Vec<u8>, String> {
    let written = Written {
        workflow: flows.iter().map(written_flow).collect(),
    };
    serde_json::to_vec_pretty(&written).map_err(|e| format!("could not write workflows: {e}"))
}

fn written_flow(w: &Workflow) -> WrittenFlow {
    WrittenFlow {
        name: w.name.clone(),
        matches: w.matches.iter().map(spell_cond).collect(),
        serial: w.serial,
        steps: w
            .steps
            .iter()
            .map(|s| WrittenStep {
                when: s.when.iter().map(spell_cond).collect(),
                act: spell_act(&s.act),
            })
            .collect(),
    }
}

/// One workflow in the shape the file has and the editor edits, as JSON.
///
/// THE one spelling, shared with [`to_bytes`], because the server used to rebuild this shape by
/// hand — name, matches, steps — and the hand copy silently dropped `serial` the day it was added.
/// The owner's file said `"serial": true`; the editor payload said nothing; a save from that editor
/// would have written the file back WITHOUT it, and the train would have quietly gone parallel. A
/// second serializer is the same defect as a second parser, and this is its funeral.
pub fn editor_shape(w: &Workflow) -> serde_json::Value {
    serde_json::to_value(written_flow(w)).unwrap_or_default()
}
#[cfg(test)]
mod tests {
    // `super::*` for this file's own items, private ones included; `crate::workflow::*` for
    // the rest of the engine, which was one namespace before this module became a directory
    // and still is from outside. A test reads the vocabulary the way a caller does.
    use super::*;
    #[allow(unused_imports)]
    use crate::workflow::*;

    /// The owner's own example, written down and read back.
    ///
    /// Not a synthetic fixture on purpose: if the thing that was asked for cannot be said in this
    /// vocabulary, the vocabulary is wrong, and that is a thing to find out before anything can act
    /// on it. Every step here comes from the sentence in `docs/pr-workflow.md`.
    #[test]
    fn the_workflow_that_was_asked_for_can_be_written_down() {
        let file = br#"{
          "workflow": [{
            "name": "ship-mine",
            "matches": ["mine"],
            "steps": [
              { "when": ["approved", "no-label:ci"],        "do": "add-label:ci" },
              { "when": ["checks:pending"],                 "do": "wait:CI is running" },
              { "when": ["checks:failing"],                 "do": "flag:CI is red" },
              { "when": ["approved", "not-mergeable"],      "do": "update-branch:rebase" },
              { "when": ["approved", "mergeable", "checks:passing"], "do": "merge:squash+delete" }
            ]
          }]
        }"#;
        let flows = from_bytes(file).expect("the owner's example must be expressible");
        assert_eq!(flows.len(), 1);
        let flow = &flows[0];
        assert_eq!(flow.name, "ship-mine");
        assert_eq!(flow.matches, vec![Cond::Mine]);
        assert_eq!(flow.steps.len(), 5);
        assert_eq!(
            flow.steps[0],
            Step {
                when: vec![Cond::Approved, Cond::NoLabel("ci".into())],
                act: Act::AddLabel("ci".into()),
            }
        );
        assert_eq!(flow.steps[3].act, Act::UpdateBranch(Update::Rebase));
        assert_eq!(
            flow.steps[4].act,
            Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true
            }),
            "the owner asked for merge AND delete, which is one action because the queue only \
             lists open pull requests"
        );

        // And it survives being written back out, because the cockpit edits these. An editor that
        // cannot read back what it wrote loses a step, and the step it loses is the one nobody
        // notices until a pull request merges without it.
        let again = from_bytes(&to_bytes(&flows).unwrap()).unwrap();
        assert_eq!(again, flows, "a workflow did not survive the round trip");
    }

    /// A file skein does not fully understand does not half-load.
    ///
    /// The failure this prevents: a workflow whose "checks have passed" step was dropped because of
    /// a typo, leaving the merge step with nothing in front of it. Half an automation is worse than
    /// none — so the whole file is refused, and the message says which workflow, which step, and
    /// what it can say instead.
    #[test]
    fn a_file_it_does_not_understand_is_refused_whole() {
        let bad = br#"{
          "workflow": [{
            "name": "ship-mine",
            "steps": [
              { "when": ["approved"], "do": "add-label:ci" },
              { "when": ["checks:green"], "do": "merge:squash" }
            ]
          }]
        }"#;
        let why = from_bytes(bad).expect_err("a condition nobody defined must not load");
        assert!(
            why.contains("ship-mine") && why.contains("step 2"),
            "the refusal must say where to look: {why}"
        );
        assert!(
            why.contains("passing"),
            "and what can be said instead of the word it refused: {why}"
        );

        // An action nobody defined, with the same treatment — and the list, because "unknown
        // action" leaves the reader exactly where they were.
        let bad = br#"{"workflow":[{"name":"w","steps":[{"when":[],"do":"deploy:prod"}]}]}"#;
        let why = from_bytes(bad).expect_err("an action nobody defined must not load");
        assert!(
            why.contains("deploy:prod") && why.contains("add-label"),
            "the refusal names the word and the vocabulary: {why}"
        );

        // A step whose action needs an argument and was not given one.
        let bad = br#"{"workflow":[{"name":"w","steps":[{"when":[],"do":"add-label"}]}]}"#;
        assert!(
            from_bytes(bad).is_err(),
            "a label with no name is not a label"
        );

        // A workflow with no steps would sit on a pull request doing nothing, for ever, looking
        // like automation.
        let bad = br#"{"workflow":[{"name":"empty","steps":[]}]}"#;
        assert!(
            from_bytes(bad).is_err(),
            "a workflow that cannot act is not one"
        );

        // Two workflows with one name: whichever skein picked, half the assignments would mean the
        // other one.
        let bad = br#"{"workflow":[
          {"name":"w","steps":[{"when":[],"do":"merge:merge"}]},
          {"name":"w","steps":[{"when":[],"do":"merge:squash"}]}]}"#;
        assert!(
            from_bytes(bad).is_err(),
            "a name that means two things means neither"
        );

        // And no file at all is a fleet where nobody has written one, which is not a fault.
        assert_eq!(from_bytes(b"").unwrap(), Vec::new());
        assert_eq!(from_bytes(b"  \n ").unwrap(), Vec::new());
    }

    /// `serial` survives being read, written back, and saved.
    ///
    /// The cockpit editor sends a whole file through [`save`], which re-serializes from what was
    /// PARSED — so a field the round trip dropped would be a train that quietly went parallel the
    /// first time somebody edited an unrelated workflow.
    /// The editor payload is the file's own shape — the regression this guards: the server once
    /// rebuilt it by hand and the copy dropped `serial`, so the cockpit under-reported a running
    /// train and a save from that editor would have stripped the field from the file.
    #[test]
    fn the_editor_shape_carries_serial_and_the_file_spelling() {
        let flows = from_bytes(
            br#"{ "workflow": [ { "name": "t", "serial": true,
                 "steps": [ { "when": ["approved"], "do": "add-label:ci" } ] } ] }"#,
        )
        .unwrap();
        let shape = editor_shape(&flows[0]);
        assert_eq!(
            shape.get("serial").and_then(|v| v.as_bool()),
            Some(true),
            "the editor payload lost `serial` — the hand-serializer bug is back"
        );
        // The step keeps the file's own key for the action.
        assert_eq!(
            shape["steps"][0].get("do").and_then(|v| v.as_str()),
            Some("add-label:ci"),
            "the editor payload spells the action under `do`, as the file does"
        );
    }

    #[test]
    fn serial_survives_the_round_trip_and_the_save() {
        let file = br#"{"workflow":[
          {"name":"merge-train","serial":true,"matches":["mine"],"steps":[{"when":[],"do":"merge:squash+delete"}]},
          {"name":"plain","steps":[{"when":[],"do":"flag:look"}]}]}"#;
        let flows = from_bytes(file).unwrap();
        assert!(flows[0].serial, "serial was not read");
        assert!(
            !flows[1].serial,
            "a file that says nothing means not serial"
        );

        let again = from_bytes(&to_bytes(&flows).unwrap()).unwrap();
        assert_eq!(again, flows, "serial did not survive the round trip");

        // And through the save path itself, filesystem included.
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let saved = save(file).unwrap();
        assert!(saved[0].serial);
        let reloaded = load().unwrap();
        assert_eq!(reloaded, flows, "what save wrote is not what load reads");
        std::env::remove_var("SKEIN_HOME");
    }
}
