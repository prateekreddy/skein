//! Removing a credential file from the sandbox: the sweep of a previous build's leftovers.

/// The shell that takes it away again. `-f`, so a file already gone is success.
pub(super) fn forget_credential_script(file: &str) -> String {
    format!("rm -f {file}")
}
