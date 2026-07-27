// `--remote` (SSH) support. Historically Unix-only (hence the module name);
// now cross-platform on the *local/controlling* side -- see the module-level
// doc comment in `unix.rs` for the platform split (remote *targets* are
// still assumed Unix-like unconditionally, that part hasn't changed).
mod unix;

pub(crate) use unix::*;

// Cloud relay transport (Socket.IO-based). Platform-independent, unlike
// `unix` above, so it is registered unconditionally.
pub(crate) mod cloud;

// Optional P2P data-channel upgrade for `cloud`'s byte-relay path (Task 5,
// spelflow-tanuki-terminal-stability-tasks.md). Platform-independent, same
// as `cloud` above.
pub(crate) mod webrtc_p2p;

/// Extracts a top-level `--cloud <target_token_id>` flag from `args`,
/// analogous to [`extract_remote_args`]'s `--remote` handling but for the
/// cloud relay transport instead of SSH. Unlike `--remote`, this has no
/// platform gate: `crate::remote::cloud` builds and works identically on
/// Windows and Unix, so `--cloud` is recognized and usable on every
/// platform this crate targets.
pub(crate) fn extract_cloud_args(args: &[String]) -> Result<(Vec<String>, Option<String>), String> {
    let mut cleaned = Vec::with_capacity(args.len());
    if let Some(program) = args.first() {
        cleaned.push(program.clone());
    }

    let mut cloud_target = None;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            cleaned.extend_from_slice(&args[index..]);
            break;
        }
        if arg == "--cloud" {
            if cloud_target.is_some() {
                return Err("--cloud can only be specified once".to_string());
            }
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --cloud".to_string());
            };
            cloud_target = Some(validate_cloud_target(value)?.to_owned());
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--cloud=") {
            if cloud_target.is_some() {
                return Err("--cloud can only be specified once".to_string());
            }
            cloud_target = Some(validate_cloud_target(value)?.to_owned());
            index += 1;
            continue;
        }

        cleaned.push(arg.clone());
        index += 1;
    }

    Ok((cleaned, cloud_target))
}

fn validate_cloud_target(target: &str) -> Result<&str, String> {
    if target.is_empty() {
        return Err("missing value for --cloud".to_string());
    }
    if target.starts_with('-') {
        return Err("--cloud target must not start with '-'".to_string());
    }
    Ok(target)
}

pub(crate) fn print_remote_error_hint(err: &std::io::Error, target: &str) {
    if is_remote_auth_error(err) {
        eprintln!(
            "hint: verify SSH access first with `{}`.",
            ssh_check_command(target)
        );
        eprintln!(
            "hint: if your SSH key has a passphrase, load it into ssh-agent with `ssh-add` before running `tanuki --remote`."
        );
    }
}

fn is_remote_auth_error(err: &std::io::Error) -> bool {
    let message = err.to_string();
    message.contains("Permission denied")
        && (message.contains("(publickey")
            || message.contains("(keyboard-interactive")
            || message.contains("(password"))
}

fn ssh_check_command(target: &str) -> String {
    format!("ssh {}", shell_quote(target))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_auth_error_matches_ssh_auth_denied() {
        let err = std::io::Error::other(
            "remote platform detection failed: user@host: Permission denied (publickey).",
        );

        assert!(is_remote_auth_error(&err));
    }

    #[test]
    fn remote_auth_error_matches_keyboard_interactive_denied() {
        let err = std::io::Error::other(
            "remote server status failed: user@host: Permission denied (keyboard-interactive).",
        );

        assert!(is_remote_auth_error(&err));
    }

    #[test]
    fn remote_auth_error_ignores_non_auth_errors() {
        let err = std::io::Error::other("remote platform detection failed: unsupported platform");

        assert!(!is_remote_auth_error(&err));
    }

    #[test]
    fn ssh_check_command_quotes_remote_target() {
        assert_eq!(ssh_check_command("host name"), "ssh 'host name'");
    }
}
