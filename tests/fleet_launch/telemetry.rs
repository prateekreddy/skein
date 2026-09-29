//! Claude Code's usage telemetry to Anthropic is off in a box unless its repo turned it on.

use super::*;

// -------------------------------------------------------------------------------------------------
// The launcher's telemetry block, over every shape a box's user settings can be in (SKEIN-1225)
// -------------------------------------------------------------------------------------------------
//
// The launcher writes `env.DISABLE_TELEMETRY = "1"` into the box's own `~/.claude/settings.json`
// unless `$SKEIN_BOX_TELEMETRY` says the box's repo allows it, and takes that value out again when
// it does. That the variable reaches the launcher from the repo's switch, and that a really started
// and a really relaunched box carry the result, are asserted in `start.rs` and in
// `fleet::start`'s own tests; this is the block's judgement over files a real start rarely meets.

/// Run the launcher's telemetry block against one fixture home, with `$SKEIN_BOX_TELEMETRY` set to
/// `setting` or, for `None`, not set at all — which is what an older host passes.
fn run_telemetry_block(home: &Path, setting: Option<&str>) -> std::process::Output {
    let block = launcher_block(
        "skein_telemetry=",
        "unset skein_telemetry SKEIN_BOX_TELEMETRY",
    );
    assert!(
        block.contains("DISABLE_TELEMETRY"),
        "the lifted block no longer writes the variable Claude Code's telemetry is gated on, so \
         this harness is running something that cannot answer the question it was written for"
    );
    let set = match setting {
        Some(v) => format!("export SKEIN_BOX_TELEMETRY={}\n", skein::util::sh_quote(v)),
        None => "unset SKEIN_BOX_TELEMETRY\n".into(),
    };
    Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail\nhome='{}'\n{set}{block}",
            home.display()
        ))
        .output()
        .expect("bash runs the launcher's telemetry block")
}

/// A box's user settings, parsed; `None` for a file that is not there.
fn settings_of(home: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(home.join(".claude/settings.json")).ok()?;
    Some(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}

/// **Off unless the repo says on, and only skein's own value is ever taken back out.**
///
/// **What makes each assertion fail**, planted and watched before this was believed:
///
///   * the block not writing the key (`env["DISABLE_TELEMETRY"] = "1"` deleted) — `sends Anthropic
///     its usage telemetry although its repo never allowed it` fails, for the off cases;
///   * the block ignoring `$SKEIN_BOX_TELEMETRY` (reading `sent` as always false) — `still has
///     skein's DISABLE_TELEMETRY although its repo turned telemetry on` fails;
///   * replacing `env` rather than adding to it (`env = {}` always) — `the env entry ... was there
///     and is gone` fails, naming the entry;
///   * taking out any value on, not only `1` (the `!= "1"` guard removed) — `somebody's own value
///     ... was taken out` fails;
///   * writing over an unparseable file — `left exactly as it was` fails on the byte comparison.
#[test]
fn a_box_sends_no_telemetry_unless_its_repo_allows_it() {
    if !have("python3") {
        return skip("the launcher writes this key with python3, and there is none here");
    }
    let dir = scratch_named("telemetry");
    // (case, $SKEIN_BOX_TELEMETRY, settings.json before, DISABLE_TELEMETRY expected after)
    let cases: [(&str, Option<&str>, Option<&str>, Option<&str>); 10] = [
        ("a new box, no settings file", Some("0"), None, Some("1")),
        (
            "an older host that never sets the variable",
            None,
            None,
            Some("1"),
        ),
        (
            "a used file, with other settings and other env in it",
            Some("0"),
            Some(
                r#"{"crossSessionInbound":"accept","env":{"SYNC_MCP_URL":"https://example.invalid"},"model":"x"}"#,
            ),
            Some("1"),
        ),
        (
            "a file with somebody's own value, off",
            Some("0"),
            Some(r#"{"env":{"DISABLE_TELEMETRY":"yes please"}}"#),
            Some("yes please"),
        ),
        (
            "a repo switched on, over skein's value and other env",
            Some("1"),
            Some(
                r#"{"crossSessionInbound":"accept","env":{"DISABLE_TELEMETRY":"1","SYNC_MCP_URL":"https://example.invalid"}}"#,
            ),
            None,
        ),
        (
            "a repo switched on, over skein's value alone",
            Some("1"),
            Some(r#"{"env":{"DISABLE_TELEMETRY":"1"}}"#),
            None,
        ),
        (
            "a repo switched on, over somebody's own value",
            Some("1"),
            Some(r#"{"env":{"DISABLE_TELEMETRY":"true"}}"#),
            Some("true"),
        ),
        (
            "a repo switched on, no settings file",
            Some("1"),
            None,
            None,
        ),
        (
            "a file that is not JSON",
            Some("0"),
            Some("not json at all"),
            None,
        ),
        (
            "a file whose env is not an object",
            Some("0"),
            Some(r#"{"env":["DISABLE_TELEMETRY"]}"#),
            None,
        ),
    ];

    let mut off = 0;
    let mut on = 0;
    let mut left_alone = 0;
    for (n, (case, setting, before, want)) in cases.iter().enumerate() {
        let home = dir.join(format!("box-{n}/home"));
        fs::create_dir_all(home.join(".claude")).unwrap();
        if let Some(body) = before {
            fs::write(home.join(".claude/settings.json"), body).unwrap();
        }
        let out = run_telemetry_block(&home, *setting);
        assert!(
            out.status.success(),
            "the launcher's telemetry block failed for `{case}`: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        let after_text = fs::read_to_string(home.join(".claude/settings.json")).ok();
        let was: Option<serde_json::Value> = before.and_then(|b| serde_json::from_str(b).ok());
        let extendable = match &was {
            None => before.is_none(),
            Some(serde_json::Value::Object(o)) => o.get("env").is_none_or(|e| e.is_object()),
            Some(_) => false,
        };

        if !extendable {
            left_alone += 1;
            assert_eq!(
                after_text.as_deref(),
                *before,
                "`{case}` was rewritten; a file skein cannot change must be left exactly as it was"
            );
            assert!(
                said.contains("settings.json") && said.contains("telemetry"),
                "`{case}` was left alone in silence, so nobody can tell why the box may still send \
                 its telemetry: {said:?}"
            );
            continue;
        }

        let after = settings_of(&home);
        let got = after
            .as_ref()
            .and_then(|s| s.get("env"))
            .and_then(|e| e.get("DISABLE_TELEMETRY"))
            .and_then(|v| v.as_str());
        match *want {
            Some("1") if before.is_none_or(|b| !b.contains("DISABLE_TELEMETRY")) => {
                assert_eq!(
                    got,
                    Some("1"),
                    "`{case}`: this box sends Anthropic its usage telemetry although its repo never \
                     allowed it: {after_text:?}"
                );
                off += 1;
            }
            None => {
                assert_eq!(
                    got, None,
                    "`{case}`: this box still has skein's DISABLE_TELEMETRY although its repo turned \
                     telemetry on: {after_text:?}"
                );
                if before.is_none() {
                    assert!(
                        after_text.is_none(),
                        "`{case}`: a settings file was created only to say nothing: {after_text:?}"
                    );
                }
                on += 1;
            }
            Some(theirs) => assert_eq!(
                got,
                Some(theirs),
                "`{case}`: somebody's own value for DISABLE_TELEMETRY was taken out or replaced, \
                 and only skein's `1` is skein's: {after_text:?}"
            ),
        }

        // Nothing else in the file is lost, in `env` or beside it.
        if let Some(serde_json::Value::Object(whole)) = &was {
            for (key, value) in whole {
                if key == "env" {
                    continue;
                }
                assert_eq!(
                    after.as_ref().and_then(|a| a.get(key)),
                    Some(value),
                    "`{case}`: the key `{key}` was in the box's settings and is not in what skein \
                     wrote back: {after_text:?}"
                );
            }
            if let Some(serde_json::Value::Object(env)) = whole.get("env") {
                for (key, value) in env {
                    if key == "DISABLE_TELEMETRY" {
                        continue;
                    }
                    assert_eq!(
                        after
                            .as_ref()
                            .and_then(|a| a.get("env"))
                            .and_then(|e| e.get(key)),
                        Some(value),
                        "`{case}`: the env entry `{key}` was there and is gone: {after_text:?}"
                    );
                }
            }
        }
    }
    assert!(
        off == 3 && on == 3 && left_alone == 2,
        "this table was read as {off} switched off, {on} switched on and {left_alone} left alone, \
         which is not the split its cases were written to have"
    );
}
