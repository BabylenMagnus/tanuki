//! `tanuki send`: uploads local files to a Tanuki server (usually a `--remote`
//! target reached through the SSH bridge) and reports where they were staged.
//!
//! This is a one-shot protocol client, not a terminal client. It connects in
//! the direct-attach launch mode and never attaches to a terminal: an app-mode
//! client would become the server's foreground client and resize the running
//! session, while a pending direct-attach client leaves the live session alone.

use std::io::{self, Read as _};
use std::path::Path;
use std::time::Duration;

use crate::ipc::Transport;
use crate::protocol::{
    self, ClientKeybindings, ClientLaunchMode, ClientMessage, RenderEncoding, ServerMessage,
    MAX_CLIPBOARD_FILE_PAYLOAD, MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION,
};

/// Longest wait for any single server reply. It has to cover a cold ssh
/// connection (TCP, key exchange, auth) inside the first read, and staging up
/// to `MAX_CLIPBOARD_FILE_PAYLOAD` bytes on the server.
const REPLY_TIMEOUT: Duration = Duration::from_secs(300);

/// A local file read into memory, ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalFile {
    pub(crate) name: String,
    pub(crate) data: Vec<u8>,
}

/// What the server did with one uploaded file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StagedFile {
    pub(crate) name: String,
    /// The staged path on the server, or the reason staging failed.
    pub(crate) result: Result<String, String>,
}

/// Reads every path up front so a bad argument fails before any ssh work.
pub(crate) fn read_local_files(paths: &[String]) -> io::Result<Vec<LocalFile>> {
    paths.iter().map(|path| read_local_file(path)).collect()
}

fn read_local_file(path: &str) -> io::Result<LocalFile> {
    let path = Path::new(path);
    let display = path.display();
    let metadata = std::fs::metadata(path)
        .map_err(|err| io::Error::new(err.kind(), format!("{display}: {err}")))?;
    if !metadata.is_file() {
        return Err(io::Error::other(format!("{display}: not a regular file")));
    }
    if metadata.len() > MAX_CLIPBOARD_FILE_PAYLOAD as u64 {
        return Err(io::Error::other(format!(
            "{display}: {} bytes exceeds the {} byte limit",
            metadata.len(),
            MAX_CLIPBOARD_FILE_PAYLOAD
        )));
    }

    // The size can change between the check above and the read; enforce the
    // limit on what is actually read too.
    let mut data = Vec::with_capacity(metadata.len() as usize);
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(MAX_CLIPBOARD_FILE_PAYLOAD as u64 + 1)
                .read_to_end(&mut data)
        })
        .map_err(|err| io::Error::new(err.kind(), format!("{display}: {err}")))?;
    if data.len() > MAX_CLIPBOARD_FILE_PAYLOAD {
        return Err(io::Error::other(format!(
            "{display}: grew past the {MAX_CLIPBOARD_FILE_PAYLOAD} byte limit while reading"
        )));
    }

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    Ok(LocalFile { name, data })
}

/// Connects to the server's client socket at `socket` and uploads `files`.
pub(crate) fn send_files(socket: &Path, files: Vec<LocalFile>) -> io::Result<Vec<StagedFile>> {
    let mut stream = Transport::Local(crate::ipc::connect_local_stream(socket)?);
    stream.set_nonblocking(false)?;
    // Not every transport supports a recv timeout (see `Transport::set_recv_timeout`);
    // a missing timeout only means a hung server is waited on for longer.
    let _ = stream.set_recv_timeout(Some(REPLY_TIMEOUT));
    send_files_over(&mut stream, files)
}

/// The protocol conversation, independent of how the stream was reached.
pub(crate) fn send_files_over<S: io::Read + io::Write>(
    stream: &mut S,
    files: Vec<LocalFile>,
) -> io::Result<Vec<StagedFile>> {
    handshake(stream)?;

    let mut staged = Vec::with_capacity(files.len());
    for file in files {
        let name = file.name.clone();
        protocol::write_message(
            stream,
            &ClientMessage::ClipboardFile {
                name: file.name,
                data: file.data,
                paste: false,
            },
        )
        .map_err(|err| io::Error::other(format!("failed to send {name}: {err}")))?;
        staged.push(read_reply(stream, name)?);
    }

    // Best effort: the server also notices the closed connection.
    let _ = protocol::write_message(stream, &ClientMessage::Detach);
    Ok(staged)
}

fn handshake<S: io::Read + io::Write>(stream: &mut S) -> io::Result<()> {
    let hello = ClientMessage::Hello {
        version: PROTOCOL_VERSION,
        cols: 80,
        rows: 24,
        cell_width_px: 0,
        cell_height_px: 0,
        requested_encoding: RenderEncoding::TerminalAnsi,
        keybindings: ClientKeybindings::Server,
        launch_mode: ClientLaunchMode::TerminalAttach,
    };
    protocol::write_message(stream, &hello)
        .map_err(|err| io::Error::other(format!("failed to send hello: {err}")))?;

    match protocol::read_message::<_, ServerMessage>(stream, MAX_FRAME_SIZE)
        .map_err(|err| io::Error::other(format!("failed to read welcome: {err}")))?
    {
        ServerMessage::Welcome { error: None, .. } => Ok(()),
        ServerMessage::Welcome {
            error: Some(error), ..
        } => Err(io::Error::other(format!(
            "server rejected the connection: {error}"
        ))),
        _ => Err(io::Error::other("expected Welcome from the server")),
    }
}

/// Reads server messages until the reply for `name` arrives; everything else
/// (notifications, config reloads) is irrelevant to a one-shot upload.
fn read_reply<S: io::Read>(stream: &mut S, name: String) -> io::Result<StagedFile> {
    loop {
        let message: ServerMessage = protocol::read_message(stream, MAX_GRAPHICS_FRAME_SIZE)
            .map_err(|err| {
                io::Error::other(format!("lost connection while sending {name}: {err}"))
            })?;
        match message {
            ServerMessage::FileStaged {
                name: reply_name,
                path,
                error,
            } if reply_name == name => {
                let result = match (path, error) {
                    (Some(path), _) => Ok(path),
                    (None, Some(error)) => Err(error),
                    (None, None) => Err("server reported no path and no error".to_owned()),
                };
                return Ok(StagedFile { name, result });
            }
            ServerMessage::ServerShutdown { reason } => {
                return Err(io::Error::other(format!(
                    "server is shutting down{}",
                    reason.map(|r| format!(": {r}")).unwrap_or_default()
                )));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn framed(message: &ServerMessage) -> Vec<u8> {
        let mut bytes = Vec::new();
        protocol::write_message(&mut bytes, message).unwrap();
        bytes
    }

    /// A stream that plays back canned server bytes and records what was written.
    struct Scripted {
        incoming: Cursor<Vec<u8>>,
        written: Vec<u8>,
    }

    impl io::Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.incoming.read(buf)
        }
    }

    impl io::Write for Scripted {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn welcome_ok() -> ServerMessage {
        ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        }
    }

    fn scripted(messages: &[ServerMessage]) -> Scripted {
        Scripted {
            incoming: Cursor::new(messages.iter().flat_map(framed).collect()),
            written: Vec::new(),
        }
    }

    fn file(name: &str, data: &[u8]) -> LocalFile {
        LocalFile {
            name: name.to_owned(),
            data: data.to_vec(),
        }
    }

    #[test]
    fn send_reports_the_staged_path_and_sends_hello_then_file_then_detach() {
        let mut stream = scripted(&[
            welcome_ok(),
            ServerMessage::FileStaged {
                name: "a.txt".into(),
                path: Some("/tmp/tanuki-files-1/x/a.txt".into()),
                error: None,
            },
        ]);

        let staged = send_files_over(&mut stream, vec![file("a.txt", b"hello")]).unwrap();

        assert_eq!(
            staged,
            vec![StagedFile {
                name: "a.txt".into(),
                result: Ok("/tmp/tanuki-files-1/x/a.txt".into()),
            }]
        );

        let mut sent = stream.written.as_slice();
        match protocol::read_message::<_, ClientMessage>(&mut sent, MAX_GRAPHICS_FRAME_SIZE)
            .unwrap()
        {
            ClientMessage::Hello {
                version,
                launch_mode,
                ..
            } => {
                assert_eq!(version, PROTOCOL_VERSION);
                // Must not be an app client: that would steal foreground and resize the session.
                assert_eq!(launch_mode, ClientLaunchMode::TerminalAttach);
            }
            other => panic!("expected Hello first, got {other:?}"),
        }
        match protocol::read_message::<_, ClientMessage>(&mut sent, MAX_GRAPHICS_FRAME_SIZE)
            .unwrap()
        {
            ClientMessage::ClipboardFile { name, data, paste } => {
                assert_eq!(name, "a.txt");
                assert_eq!(data, b"hello");
                assert!(!paste, "send must not paste into a pane");
            }
            other => panic!("expected ClipboardFile, got {other:?}"),
        }
        assert!(matches!(
            protocol::read_message::<_, ClientMessage>(&mut sent, MAX_GRAPHICS_FRAME_SIZE).unwrap(),
            ClientMessage::Detach
        ));
    }

    #[test]
    fn send_skips_unrelated_server_messages_while_waiting_for_the_reply() {
        let mut stream = scripted(&[
            welcome_ok(),
            ServerMessage::ReloadSoundConfig,
            ServerMessage::MouseCapture { enabled: true },
            ServerMessage::FileStaged {
                name: "other.txt".into(),
                path: Some("/tmp/other".into()),
                error: None,
            },
            ServerMessage::FileStaged {
                name: "a.txt".into(),
                path: Some("/tmp/a".into()),
                error: None,
            },
        ]);

        let staged = send_files_over(&mut stream, vec![file("a.txt", b"x")]).unwrap();

        assert_eq!(staged[0].result, Ok("/tmp/a".to_owned()));
    }

    #[test]
    fn send_surfaces_a_server_side_staging_error_per_file() {
        let mut stream = scripted(&[
            welcome_ok(),
            ServerMessage::FileStaged {
                name: "a.txt".into(),
                path: None,
                error: Some("No space left on device".into()),
            },
            ServerMessage::FileStaged {
                name: "b.txt".into(),
                path: Some("/tmp/b".into()),
                error: None,
            },
        ]);

        let staged =
            send_files_over(&mut stream, vec![file("a.txt", b"x"), file("b.txt", b"y")]).unwrap();

        assert_eq!(staged[0].result, Err("No space left on device".to_owned()));
        assert_eq!(staged[1].result, Ok("/tmp/b".to_owned()));
    }

    #[test]
    fn send_fails_clearly_when_the_server_rejects_the_handshake() {
        let mut stream = scripted(&[ServerMessage::Welcome {
            version: 99,
            encoding: RenderEncoding::SemanticFrame,
            error: Some("client version 19 is older than server version 99".into()),
        }]);

        let err = send_files_over(&mut stream, vec![file("a.txt", b"x")]).unwrap_err();

        assert!(err.to_string().contains("older than server version"));
    }

    #[test]
    fn send_fails_when_the_connection_drops_before_the_reply() {
        let mut stream = scripted(&[welcome_ok()]);

        let err = send_files_over(&mut stream, vec![file("a.txt", b"x")]).unwrap_err();

        assert!(err.to_string().contains("lost connection"));
    }

    #[test]
    fn send_fails_when_the_server_shuts_down() {
        let mut stream = scripted(&[
            welcome_ok(),
            ServerMessage::ServerShutdown {
                reason: Some("live update".into()),
            },
        ]);

        let err = send_files_over(&mut stream, vec![file("a.txt", b"x")]).unwrap_err();

        assert!(err.to_string().contains("live update"));
    }

    #[test]
    fn read_local_files_uses_the_file_name_and_rejects_missing_and_directories() {
        let dir = std::env::temp_dir().join(format!("tanuki-send-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("note.txt");
        std::fs::write(&path, b"content").unwrap();

        let files = read_local_files(&[path.to_string_lossy().into_owned()]).unwrap();
        assert_eq!(files, vec![file("note.txt", b"content")]);

        assert!(
            read_local_files(&[dir.join("missing.txt").to_string_lossy().into_owned()]).is_err()
        );
        let err = read_local_files(&[dir.to_string_lossy().into_owned()]).unwrap_err();
        assert!(err.to_string().contains("not a regular file"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
