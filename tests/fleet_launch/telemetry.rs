//! Claude Code's built-in `telemetry` plugin is off in a box unless its repo turned it on.

use super::*;

// -------------------------------------------------------------------------------------------------
// The launcher's telemetry-plugin block, over every shape a box's user settings can be in
// (SKEIN-1225)
// -------------------------------------------------------------------------------------------------
//
// The launcher writes `enabledPlugins["telemetry@builtin"] = false` into the box's own
// `~/.claude/settings.json` unless `$SKEIN_BOX_TELEMETRY` says the box's repo allows the plugin,
// and takes that value out again when it does. That the variable reaches the launcher from the
// repo's switch, and that a really started and a really relaunched box carry the result, are
// asserted in `start.rs` and in `fleet::start`'s own tests; this is the block's judgement over
// files a real start rarely meets.

/// The settings key Claude Code reads to enable or disable its built-in `telemetry` plugin.
const PLUGIN: &str = "telemetry@builtin";

/// Run the launcher's telemetry-plugin block against one fixture home, with `$SKEIN_BOX_TELEMETRY`
/// set to `setting` or, for `None`, not set at all — which is what an older host passes.
fn run_telemetry_block(home: &Path, setting: Option<&str>) -> std::process::Output {
    let block = launcher_block(
        "skein_telemetry=",
        "unset skein_telemetry SKEIN_BOX_TELEMETRY",
    );
    assert!(
        block.contains(PLUGIN),
        "the lifted block no longer names the plugin it is meant to turn off, so this harness is \
         running something that cannot answer the question it was written for"
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
        .expect("bash runs the launcher's telemetry-plugin block")
}

/// One row of the table below: the case, `$SKEIN_BOX_TELEMETRY`, the box's `settings.json` before
/// the block runs (`None` for no file), and the plugin's `enabledPlugins` value expected after
/// (`None` for no entry).
type Case = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    Option<serde_json::Value>,
);

/// A box's user settings, parsed; `None` for a file that is not there.
fn settings_of(home: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(home.join(".claude/settings.json")).ok()?;
    Some(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}

/// **Off unless the repo says on, and only skein's own value is ever taken back out.**
///
/// **What makes each assertion fail**, planted and watched before this was believed:
///
///   * the block not writing the key (`plugins[key] = False` deleted) — `runs Claude Code's
///     telemetry plugin although its repo never allowed it` fails, for the off cases;
///   * the block ignoring `$SKEIN_BOX_TELEMETRY` (reading `allowed` as always false) — `still has
///     skein's telemetry@builtin: false although its repo allowed the plugin` fails;
///   * replacing `enabledPlugins` rather than adding to it (`plugins = {}` always) — `the
///     enabledPlugins entry ... was there and is gone` fails, naming the entry;
///   * taking out any value on, not only `false` (the `is not False` guard removed) — `somebody's
///     own value ... was taken out` fails;
///   * writing over an unparseable file — `left exactly as it was` fails on the byte comparison.
#[test]
fn a_box_runs_without_the_telemetry_plugin_unless_its_repo_allows_it() {
    if !have("python3") {
        return skip("the launcher writes this key with python3, and there is none here");
    }
    let off = || Some(serde_json::Value::Bool(false));
    let dir = scratch_named("telemetry");
    let cases: [Case; 10] = [
        ("a new box, no settings file", Some("0"), None, off()),
        (
            "an older host that never sets the variable",
            None,
            None,
            off(),
        ),
        (
            "a used file, with other settings and other plugins in it",
            Some("0"),
            Some(
                r#"{"crossSessionInbound":"accept","enabledPlugins":{"sync@sync":true},"env":{"SYNC_MCP_URL":"https://example.invalid"}}"#,
            ),
            off(),
        ),
        (
            "a file with somebody's own value, off",
            Some("0"),
            Some(r#"{"enabledPlugins":{"telemetry@builtin":true}}"#),
            Some(serde_json::Value::Bool(true)),
        ),
        (
            "a repo switched on, over skein's value and other plugins",
            Some("1"),
            Some(
                r#"{"crossSessionInbound":"accept","enabledPlugins":{"telemetry@builtin":false,"sync@sync":true}}"#,
            ),
            None,
        ),
        (
            "a repo switched on, over skein's value alone",
            Some("1"),
            Some(r#"{"enabledPlugins":{"telemetry@builtin":false}}"#),
            None,
        ),
        (
            "a repo switched on, over somebody's own value",
            Some("1"),
            Some(r#"{"enabledPlugins":{"telemetry@builtin":"no"}}"#),
            Some(serde_json::Value::String("no".into())),
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
            "a file whose enabledPlugins is not an object",
            Some("0"),
            Some(r#"{"enabledPlugins":["telemetry@builtin"]}"#),
            None,
        ),
    ];

    let mut switched_off = 0;
    let mut switched_on = 0;
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
            "the launcher's telemetry-plugin block failed for `{case}`: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        let after_text = fs::read_to_string(home.join(".claude/settings.json")).ok();
        let was: Option<serde_json::Value> = before.and_then(|b| serde_json::from_str(b).ok());
        let extendable = match &was {
            None => before.is_none(),
            Some(serde_json::Value::Object(o)) => {
                o.get("enabledPlugins").is_none_or(|e| e.is_object())
            }
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
                said.contains("settings.json") && said.contains("telemetry plugin"),
                "`{case}` was left alone in silence, so nobody can tell why the box may still run \
                 the telemetry plugin: {said:?}"
            );
            continue;
        }

        let after = settings_of(&home);
        let got = after
            .as_ref()
            .and_then(|s| s.get("enabledPlugins"))
            .and_then(|e| e.get(PLUGIN))
            .cloned();
        let had_one = before.is_some_and(|b| b.contains(PLUGIN));
        match want {
            Some(serde_json::Value::Bool(false)) if !had_one => {
                assert_eq!(
                    got.as_ref(),
                    want.as_ref(),
                    "`{case}`: this box runs Claude Code's telemetry plugin although its repo never \
                     allowed it: {after_text:?}"
                );
                switched_off += 1;
            }
            None => {
                assert_eq!(
                    got, None,
                    "`{case}`: this box still has skein's telemetry@builtin: false although its \
                     repo allowed the plugin: {after_text:?}"
                );
                if before.is_none() {
                    assert!(
                        after_text.is_none(),
                        "`{case}`: a settings file was created only to say nothing: {after_text:?}"
                    );
                }
                switched_on += 1;
            }
            Some(theirs) => assert_eq!(
                got.as_ref(),
                Some(theirs),
                "`{case}`: somebody's own value for telemetry@builtin was taken out or replaced, \
                 and only skein's `false` is skein's: {after_text:?}"
            ),
        }

        // Nothing else in the file is lost, in `enabledPlugins` or beside it.
        if let Some(serde_json::Value::Object(whole)) = &was {
            for (key, value) in whole {
                if key == "enabledPlugins" {
                    continue;
                }
                assert_eq!(
                    after.as_ref().and_then(|a| a.get(key)),
                    Some(value),
                    "`{case}`: the key `{key}` was in the box's settings and is not in what skein \
                     wrote back: {after_text:?}"
                );
            }
            if let Some(serde_json::Value::Object(plugins)) = whole.get("enabledPlugins") {
                for (key, value) in plugins {
                    if key == PLUGIN {
                        continue;
                    }
                    assert_eq!(
                        after
                            .as_ref()
                            .and_then(|a| a.get("enabledPlugins"))
                            .and_then(|e| e.get(key)),
                        Some(value),
                        "`{case}`: the enabledPlugins entry `{key}` was there and is gone: \
                         {after_text:?}"
                    );
                }
            }
        }
    }
    assert!(
        switched_off == 3 && switched_on == 3 && left_alone == 2,
        "this table was read as {switched_off} switched off, {switched_on} switched on and \
         {left_alone} left alone, which is not the split its cases were written to have"
    );
}
