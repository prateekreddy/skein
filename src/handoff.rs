//! The brief one box leaves for the next one.
//!
//! Provider-neutral by construction: a Claude transcript means nothing to Codex, so what crosses is
//! prose plus the box's own git status, written by the box-handoff hook that runs *inside* the
//! source. Two copies land — a durable one that stays readable afterwards, and a one-shot addressed
//! to the destination so its first SessionStart consumes it and no later session picks it up again.

use crate::diff::changed_files;
use crate::repos::agent_for_box;
use crate::runtime::valid_runtime;
use crate::signals::current_task;
use crate::util::write_atomic;
use crate::{session_digest, store_for_box, valid_name};
use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};

/// Write a durable, provider-neutral takeover brief plus a one-shot copy for `target`. Native
/// transcripts remain separate; replacement takeover adds worktree/context artifacts around this
/// core brief. The box-handoff hook injects it with authoritative in-box git status.
pub fn prepare_handoff(name: &str, from: Option<&str>, target: &str) -> Result<PathBuf, String> {
    prepare_handoff_for(name, name, from, target, None, None)
}

/// Replacement-box variant of [`prepare_handoff`]. The source supplies the digest, while the
/// pending filename is addressed to the destination vm id so its first SessionStart consumes it.
pub(crate) fn prepare_handoff_for(
    source_name: &str,
    destination_name: &str,
    from: Option<&str>,
    target: &str,
    native_context: Option<&str>,
    store_override: Option<&Path>,
) -> Result<PathBuf, String> {
    let name = source_name;
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    if !valid_name(destination_name) {
        return Err("invalid destination box name".into());
    }
    if !valid_runtime(target) {
        return Err(format!("unsupported handoff runtime {target:?}"));
    }
    let source = from
        .filter(|a| valid_runtime(a))
        .map(str::to_string)
        .unwrap_or_else(|| agent_for_box(name));
    let digest = session_digest(name).ok_or_else(|| format!("no such box: {name}"))?;
    let store = store_override
        .map(Path::to_path_buf)
        .or_else(|| store_for_box(name))
        .ok_or("can't locate the box's shared store")?;
    let dir = store.join("handoffs");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;

    let mut brief = format!(
        "# Skein cross-agent handoff\n\n- source box: `{name}`\n- destination box: `{destination_name}`\n- from: `{source}`\n- to: `{target}`\n- branch: `{}`\n- state: `{}`\n- prepared: `{}`\n\nContinue the existing work in this replacement sandbox. Its commits and working tree were restored from the source snapshot; inspect them before editing and do not redo completed work. The source box remains intact as rollback.\n",
        digest.branch,
        digest.state,
        Utc::now().to_rfc3339()
    );
    // Written for a box whose credential is scoped, which is the default. The SSH advice this
    // replaced is not merely stale — it would send an agent chasing `ssh-add -l` against a socket
    // that is deliberately no longer a socket, and read as a broken box rather than a scoped one.
    brief.push_str(
        "\n## Git authentication\n\nYou can **push to your own repo**, and **read** public repos plus any private repo this fleet's GitHub App is installed on. Your credentials are placed for you; there is nothing to set up, and no SSH agent (remotes are rewritten to HTTPS automatically, so `git@github.com:…` remotes keep working).\n\n\
         `gh` holds your own repo's token, so `gh pr create` and `gh pr comment` work here. Against any other repo `gh` is unauthenticated — use `git` for reads, which is credentialed separately.\n\n\
         A push to a repo that is not yours is refused by GitHub — that is deliberate, not a misconfiguration, and no amount of retrying or re-authenticating will change it. If you genuinely need to write to another repo, ask for it:\n\n\
         ```\n/boxes/.skein/box-session.sh --request-write \"$SKEIN_BOX\" <owner/name> \"<why>\"\n```\n\n\
         That files a request for this fleet's owner to approve in the cockpit. It grants nothing by itself; once approved, the access appears within a minute and expires by default. Never copy a private key into the box.\n",
    );
    if let Some(task) = current_task(name) {
        brief.push_str(&format!("\n## Active objective\n\n{task}\n"));
    }
    if let Some(blocked) = digest.blocked_on {
        brief.push_str(&format!("\n## Blocked on\n\n{blocked}\n"));
    } else if let Some(last) = digest.last_message {
        brief.push_str(&format!("\n## Previous agent's last message\n\n{last}\n"));
    }
    if let Some(journal) = digest.journal {
        brief.push_str(&format!("\n## Journal tail\n\n{journal}\n"));
    }
    if !digest.commits.is_empty() {
        brief.push_str("\n## Recent branch commits\n");
        for commit in digest.commits.iter().take(20) {
            brief.push_str(&format!("\n- {commit}"));
        }
        brief.push('\n');
    }
    if let Some(d) = digest.diff {
        brief.push_str(&format!(
            "\n## Change summary\n\n{} files, +{} / -{} lines.\n",
            d.files, d.ins, d.del
        ));
    }
    let files = changed_files(name);
    if !files.is_empty() {
        brief.push_str("\nChanged files reported to Skein:\n");
        for file in files.iter().take(120) {
            brief.push_str(&format!("\n- `{file}`"));
        }
        brief.push('\n');
    }

    if let Some(context) = native_context.filter(|text| !text.trim().is_empty()) {
        brief.push_str("\n## Bounded native conversation export\n\nThis is continuity context, not a native resumable session. Provider-specific metadata and tool state may be omitted.\n\n");
        brief.push_str(context.trim());
        brief.push('\n');
    }

    let durable = dir.join(format!("{destination_name}.md"));
    write_atomic(&durable, &dir, brief.as_bytes())?;
    let pending = dir.join(format!("{destination_name}.{target}.pending.md"));
    write_atomic(&pending, &dir, brief.as_bytes())?;
    Ok(pending)
}
