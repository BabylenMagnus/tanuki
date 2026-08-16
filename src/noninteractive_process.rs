use std::{ffi::OsStr, process::Command};

/// Builds a subprocess whose stdio is controlled by the caller and which never opens a Windows console.
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    crate::platform::configure_background_command(&mut command);
    command
}

pub(crate) fn curl_command() -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = command("curl");
    // Windows' bundled curl uses the Schannel TLS backend, which mishandles
    // GitHub's TLS 1.3 post-handshake session tickets (misreported as a
    // renegotiation) and drops the connection with "(52) Empty reply from
    // server". Pinning TLS 1.2 avoids the broken code path; unaffected on
    // other platforms since they use OpenSSL/LibreSSL/etc.
    #[cfg(windows)]
    cmd.args(["--tlsv1.2", "--tls-max", "1.2"]);
    cmd
}
