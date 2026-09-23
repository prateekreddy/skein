//! The tree skein cloned is the one the agent trusts, and no other path is.

use super::*;

// -------------------------------------------------------------------------------------------------
// The tree skein cloned for a box is trusted in that box, and nothing else is (SKEIN-959)
// -------------------------------------------------------------------------------------------------
//
// With the login screen gone, a new box stopped next on Claude Code's workspace-trust dialog for its
// own tree. The launcher now records `projects[$tree].hasTrustDialogAccepted = true` in the box's
// `~/.claude.json`. The dialog is a security prompt — it guards against a repo's own hooks and MCP
// servers — so what is asserted is the narrow claim as well as the broad one: `$tree` is trusted,
// and no other `projects` key appears or changes, and nothing else in the file is lost.

/// Run the launcher's trust block against one fixture home and tree, as a box start runs it.
///
/// `login_life` is defined alongside, although the block does not call it today: if somebody puts
/// the block behind the login guard, the harness runs that guard as the launcher would rather than
/// failing on a missing function, and the no-login cases below say what went wrong.
fn run_trust_block(home: &Path, tree: &Path) -> std::process::Output {
    let block = launcher_block(
        "if command -v python3 >/dev/null 2>&1 && [ -d \"$tree\" ]",
        "fi",
    );
    assert!(
        block.contains("hasTrustDialogAccepted"),
        "the lifted block no longer writes the key Claude Code's trust dialog is gated on, so this \
         harness is running something that cannot answer the question it was written for"
    );
    let life = launcher_block("login_life() {", "}");
    Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail\nhome='{}'\ntree='{}'\n{life}\n{block}",
            home.display(),
            tree.display()
        ))
        .output()
        .expect("bash runs the launcher's trust block")
}

/// What `~/.claude.json` says about trust for `path`, for a home that has one.
pub(super) fn trust_of(home: &Path, path: &str) -> Option<serde_json::Value> {
    let text = fs::read_to_string(home.join(".claude.json")).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
    parsed
        .get("projects")?
        .get(path)?
        .get("hasTrustDialogAccepted")
        .cloned()
}

/// A fixture's `~/.claude.json` before the launch, given the tree's path and its parent's; `None`
/// is a file that is not there at all.
type BeforeOf = fn(&str, &str) -> Option<String>;

/// The tree skein cloned is trusted, whatever `~/.claude.json` was — and no other path is.
///
/// **What makes each assertion fail**, planted and watched before this was believed:
///
///   * deleting `entry["hasTrustDialogAccepted"] = True` from the launcher — `is not trusted`
///     fails for every case the block can extend;
///   * trusting a different path — `tree = os.path.dirname(tree)` (the parent), or `tree = "/"` —
///     `no other projects key may appear` fails, naming the key;
///   * clobbering the tree's existing entry rather than merging into it (`entry = {}` always) —
///     `the tree's own entry lost` fails, naming the field;
///   * replacing `projects` rather than adding to it (`projects = {}`) — `was there and is gone`
///     fails, naming the entry;
///   * writing over an unparseable file — `left exactly as it was` fails on the byte comparison;
///   * putting the block behind the login guard (`&& login_life …` in its condition) — the
///     no-credential cases fail `a box with no login must still have its tree trusted`.
#[test]
fn the_tree_skein_cloned_is_trusted_and_no_other_path_is() {
    if !have("python3") {
        return skip("the launcher writes this key with python3, and there is none here");
    }
    const LOGIN: &str = r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r"}}"#;
    let dir = scratch_named("trust");
    // The fixture's box root, tree and home, laid out as the launcher lays out a real one.
    let cases: [(&str, Option<&str>, BeforeOf); 12] = [
        ("no .claude.json at all", Some(LOGIN), |_, _| None),
        ("an empty .claude.json", Some(LOGIN), |_, _| {
            Some(String::new())
        }),
        (
            "a used one, with other projects in it — the parent and / among them",
            Some(LOGIN),
            |_, parent| {
                Some(format!(
                    r#"{{"projects":{{"/w":{{"history":["a turn"],"hasTrustDialogAccepted":false}},"{parent}":{{"hasTrustDialogAccepted":false}},"/":{{"allowedTools":[]}}}},"userID":"u","numStartups":46}}"#
                ))
            },
        ),
        (
            "one whose own entry for the tree carries a record and says not trusted",
            Some(LOGIN),
            |tree, _| {
                Some(format!(
                    r#"{{"projects":{{"{tree}":{{"hasTrustDialogAccepted":false,"allowedTools":["Bash(ls)"],"lastSessionId":"s","mcpServers":{{"m":{{"command":"x"}}}}}},"/w":{{"history":[]}}}},"userID":"u"}}"#
                ))
            },
        ),
        (
            "one that already trusts the tree",
            Some(LOGIN),
            |tree, _| {
                Some(format!(
                    r#"{{"projects":{{"{tree}":{{"hasTrustDialogAccepted":true,"lastCost":1.5}}}}}}"#
                ))
            },
        ),
        (
            "one with no projects key but a whole record otherwise",
            Some(LOGIN),
            |_, _| {
                Some(r#"{"hasCompletedOnboarding":true,"userID":"u","tipsHistory":{"t":3}}"#.into())
            },
        ),
        // The trust has nothing to do with the credential: a box with no login still gets it.
        ("no credential at all, and no .claude.json", None, |_, _| {
            None
        }),
        ("no credential at all, and a record", None, |_, parent| {
            Some(format!(
                r#"{{"projects":{{"{parent}":{{"x":1}}}},"userID":"u"}}"#
            ))
        }),
        ("one that is not JSON", Some(LOGIN), |_, _| {
            Some("not json at all".into())
        }),
        ("one that is JSON but not an object", Some(LOGIN), |_, _| {
            Some("[1, 2, 3]".into())
        }),
        (
            "one whose projects is not an object",
            Some(LOGIN),
            |_, _| Some(r#"{"projects":["/w"],"userID":"u"}"#.into()),
        ),
        (
            "one whose entry for the tree is not an object",
            Some(LOGIN),
            |tree, _| Some(format!(r#"{{"projects":{{"{tree}":"trusted?"}}}}"#)),
        ),
    ];

    let mut trusted = 0;
    let mut trusted_without_a_login = 0;
    let mut left_alone = 0;
    for (n, (case, credential, before_of)) in cases.iter().enumerate() {
        let at = dir.join(format!("box-{n}"));
        let home = at.join("home");
        let tree_dir = at.join("tree");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(&tree_dir).unwrap();
        let tree = tree_dir.display().to_string();
        let parent = at.display().to_string();
        let before = before_of(&tree, &parent);
        if let Some(body) = credential {
            fs::write(home.join(".claude/.credentials.json"), body).unwrap();
        }
        if let Some(body) = &before {
            fs::write(home.join(".claude.json"), body).unwrap();
        }

        let out = run_trust_block(&home, &tree_dir);
        assert!(
            out.status.success(),
            "the launcher's trust block failed for `{case}`: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        let after = fs::read_to_string(home.join(".claude.json")).ok();
        let was: Option<serde_json::Value> = before
            .as_deref()
            .filter(|b| !b.trim().is_empty())
            .map(|b| serde_json::from_str(b).unwrap_or(serde_json::Value::Null));
        let could_extend = match &was {
            None => true,
            Some(serde_json::Value::Object(o)) => match o.get("projects") {
                None => true,
                Some(serde_json::Value::Object(p)) => p.get(&tree).is_none_or(|e| e.is_object()),
                Some(_) => false,
            },
            Some(_) => false,
        };

        if !could_extend {
            left_alone += 1;
            assert_eq!(
                after.as_deref(),
                before.as_deref(),
                "`{case}` was rewritten; a file skein cannot extend must be left exactly as it was"
            );
            assert!(
                said.contains(".claude.json") && said.contains(&tree),
                "`{case}` was left alone in silence, so nobody can tell why the box still asks to \
                 trust its tree: {said:?}"
            );
            continue;
        }

        let text = after.unwrap_or_else(|| {
            panic!(
                "`{case}`: no ~/.claude.json after the trust block ran, so the tree is not trusted \
                 — a box with no login must still have its tree trusted, and if this case has no \
                 credential the trust has been put behind the login, which is a different fact"
            )
        });
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("`{case}` left ~/.claude.json unreadable ({e}): {text}"));
        let projects = parsed
            .get("projects")
            .and_then(|p| p.as_object())
            .unwrap_or_else(|| {
                panic!("`{case}`: no `projects` object after the trust write: {text}")
            });
        let projects_before = was
            .as_ref()
            .and_then(|w| w.get("projects"))
            .and_then(|p| p.as_object())
            .cloned()
            .unwrap_or_default();

        // NOTHING ELSE IS TRUSTED. Every key under `projects` other than the tree was there before,
        // with the value it had. Checked first, so a write aimed at the wrong path is named as that
        // and not merely as "the tree is not trusted".
        for (key, value) in projects {
            if *key == tree {
                continue;
            }
            assert_eq!(
                projects_before.get(key),
                Some(value),
                "`{case}`: no other projects key may appear or change, and `{key}` did — trust in \
                 Claude Code is inherited by every folder beneath it, and skein vouches only for \
                 the tree it cloned: {text}"
            );
        }
        for key in projects_before.keys() {
            assert!(
                projects.contains_key(key),
                "`{case}`: the projects entry `{key}` was there and is gone: {text}"
            );
        }

        // THE INVARIANT.
        assert_eq!(
            projects.get(&tree).and_then(|e| e.get("hasTrustDialogAccepted")),
            Some(&serde_json::Value::Bool(true)),
            "`{case}`: the tree skein cloned is not trusted, so the box opens on the trust dialog: \
             {text}"
        );
        trusted += 1;
        if credential.is_none() {
            trusted_without_a_login += 1;
        }

        // Merged into, never replaced: the tree's own entry keeps every field it had…
        if let Some(serde_json::Value::Object(entry)) = projects_before.get(&tree) {
            for (field, value) in entry {
                if field == "hasTrustDialogAccepted" {
                    continue;
                }
                assert_eq!(
                    projects[&tree].get(field),
                    Some(value),
                    "`{case}`: the tree's own entry lost `{field}` — the trust write replaced the \
                     entry rather than adding one key to it: {text}"
                );
            }
        }
        // …and so does the file.
        if let Some(serde_json::Value::Object(whole)) = &was {
            for (key, value) in whole {
                if key == "projects" {
                    continue;
                }
                assert_eq!(
                    parsed.get(key),
                    Some(value),
                    "`{case}`: the key `{key}` was in ~/.claude.json and is not in what skein wrote \
                     back: {text}"
                );
            }
        }
    }
    assert!(
        trusted == 8 && trusted_without_a_login == 2 && left_alone == 4,
        "this table was read as {trusted} trusted ({trusted_without_a_login} with no login) and \
         {left_alone} left alone, which is not the split its cases were written to have"
    );
}
