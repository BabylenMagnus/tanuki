use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STAGED_CLIPBOARD_IMAGE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub(crate) struct StagedClipboardImage {
    pub(crate) path: PathBuf,
    pub(crate) paste_text: String,
}

pub(crate) fn stage(
    client_id: u64,
    extension: &str,
    data: &[u8],
) -> io::Result<StagedClipboardImage> {
    let extension = sanitize_extension(extension);
    let dir = ensure_staging_dir()?;
    cleanup_stale(&dir);

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);

    for attempt in 0..100 {
        let path = dir.join(format!(
            "client-{client_id}-clipboard-{unique}-{attempt}.{extension}"
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        restrict_file_options(&mut options);
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        file.write_all(data)?;
        return Ok(StagedClipboardImage {
            paste_text: path.to_string_lossy().into_owned(),
            path,
        });
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to allocate unique clipboard image staging path",
    ))
}

/// Longest file name (in bytes) the server keeps; longer names are truncated
/// before the extension so the name still fits common filesystem limits.
const MAX_STAGED_FILE_NAME_BYTES: usize = 128;

/// Stages a file from a client into a per-upload directory under
/// `tanuki-files-<uid>` in the temp dir and returns its absolute path.
///
/// Unlike staged clipboard images, these files are deliberately not tied to
/// the client connection (`tanuki send` disconnects right after sending); they
/// are removed by age only, see [`cleanup_stale`].
pub(crate) fn stage_file(client_id: u64, name: &str, data: &[u8]) -> io::Result<PathBuf> {
    stage_file_in(&ensure_files_staging_dir()?, client_id, name, data)
}

fn stage_file_in(dir: &Path, client_id: u64, name: &str, data: &[u8]) -> io::Result<PathBuf> {
    cleanup_stale(dir);
    let name = sanitize_file_name(name);
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);

    for attempt in 0..100 {
        // One directory per upload keeps the original file name intact
        // without ever overwriting an earlier upload of the same name.
        let upload_dir = dir.join(format!("client-{client_id}-{unique}-{attempt}"));
        match fs::create_dir(&upload_dir) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
        restrict_dir_permissions(&upload_dir)?;

        let path = upload_dir.join(&name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        restrict_file_options(&mut options);
        let write = options
            .open(&path)
            .and_then(|mut file| file.write_all(data));
        if let Err(err) = write {
            let _ = fs::remove_dir_all(&upload_dir);
            return Err(err);
        }
        return Ok(path);
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to allocate unique file staging directory",
    ))
}

/// Reduces a client-supplied file name to a single safe path component.
///
/// The name comes from another machine (possibly Windows), so both `/` and
/// `\` count as separators and only the last component is kept. Control
/// characters and characters that are invalid on common filesystems are
/// replaced, and whitespace becomes `_` because the staged path is pasted
/// into a shell prompt unquoted.
fn sanitize_file_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut cleaned: String = last
        .chars()
        .map(|ch| {
            if ch.is_control()
                || ch.is_whitespace()
                || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*')
            {
                '_'
            } else {
                ch
            }
        })
        .collect();
    // No hidden files, and `.` / `..` must never survive.
    let trimmed = cleaned.trim_start_matches('.');
    if trimmed.len() != cleaned.len() {
        cleaned = trimmed.to_owned();
    }
    if cleaned.is_empty() {
        return "file".to_owned();
    }
    truncate_file_name(&cleaned, MAX_STAGED_FILE_NAME_BYTES)
}

fn truncate_file_name(name: &str, max_bytes: usize) -> String {
    if name.len() <= max_bytes {
        return name.to_owned();
    }
    let (stem, extension) = match name.rfind('.') {
        // Keep a plausible extension; a "extension" that is itself huge is just more stem.
        Some(dot) if name.len() - dot <= 16 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    let mut end = max_bytes.saturating_sub(extension.len());
    while end > 0 && !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{extension}", &stem[..end])
}

pub(crate) fn remove_files(paths: Vec<PathBuf>) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn sanitize_extension(extension: &str) -> &'static str {
    if extension.eq_ignore_ascii_case("png") {
        "png"
    } else if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
        "jpg"
    } else if extension.eq_ignore_ascii_case("gif") {
        "gif"
    } else if extension.eq_ignore_ascii_case("webp") {
        "webp"
    } else if extension.eq_ignore_ascii_case("bmp") {
        "bmp"
    } else {
        "png"
    }
}

fn staging_dir() -> PathBuf {
    staging_dir_named("tanuki-clipboard-images")
}

fn files_staging_dir() -> PathBuf {
    staging_dir_named("tanuki-files")
}

fn staging_dir_named(prefix: &str) -> PathBuf {
    #[cfg(unix)]
    let user_id = unsafe { libc::geteuid() };
    #[cfg(windows)]
    let user_id = std::process::id();
    std::env::temp_dir().join(format!("{prefix}-{user_id}"))
}

fn ensure_staging_dir() -> io::Result<PathBuf> {
    ensure_dir(staging_dir(), "clipboard image")
}

fn ensure_files_staging_dir() -> io::Result<PathBuf> {
    ensure_dir(files_staging_dir(), "file")
}

fn ensure_dir(dir: PathBuf, what: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(&dir)?;
    let metadata = fs::metadata(&dir)?;
    if !metadata.is_dir() {
        return Err(io::Error::other(format!(
            "{what} staging path is not a directory: {}",
            dir.display()
        )));
    }
    restrict_dir_permissions(&dir)?;
    Ok(dir)
}

#[cfg(unix)]
fn restrict_file_options(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.mode(0o600);
}

#[cfg(windows)]
fn restrict_file_options(_options: &mut fs::OpenOptions) {}

#[cfg(unix)]
fn restrict_dir_permissions(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(windows)]
fn restrict_dir_permissions(_dir: &Path) -> io::Result<()> {
    Ok(())
}

fn cleanup_stale(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified.elapsed().unwrap_or_default() > STAGED_CLIPBOARD_IMAGE_MAX_AGE {
            // Staged images are plain files; staged uploads are per-upload directories.
            if metadata.is_dir() {
                let _ = fs::remove_dir_all(path);
            } else {
                let _ = fs::remove_file(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_extension_accepts_known_image_extensions() {
        assert_eq!(sanitize_extension("PNG"), "png");
        assert_eq!(sanitize_extension("jpeg"), "jpg");
        assert_eq!(sanitize_extension("webp"), "webp");
        assert_eq!(sanitize_extension("sh"), "png");
    }

    #[test]
    fn sanitize_file_name_keeps_only_the_last_component() {
        assert_eq!(sanitize_file_name("report.pdf"), "report.pdf");
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("C:\\Users\\me\\a.txt"), "a.txt");
        assert_eq!(sanitize_file_name("dir/sub\\b.log"), "b.log");
    }

    #[test]
    fn sanitize_file_name_never_yields_dot_names_or_empty() {
        assert_eq!(sanitize_file_name(".."), "file");
        assert_eq!(sanitize_file_name("."), "file");
        assert_eq!(sanitize_file_name(""), "file");
        assert_eq!(sanitize_file_name("dir/"), "file");
        assert_eq!(sanitize_file_name(".bashrc"), "bashrc");
        assert_eq!(sanitize_file_name("...x"), "x");
    }

    #[test]
    fn sanitize_file_name_replaces_spaces_control_and_reserved_chars() {
        assert_eq!(sanitize_file_name("my file (1).png"), "my_file_(1).png");
        assert_eq!(sanitize_file_name("a\tb\nc.txt"), "a_b_c.txt");
        assert_eq!(sanitize_file_name("a<b>:\"c|d?e*.txt"), "a_b__c_d_e_.txt");
        assert_eq!(sanitize_file_name("отчёт.pdf"), "отчёт.pdf");
    }

    #[test]
    fn sanitize_file_name_truncates_long_names_but_keeps_extension() {
        let long = format!("{}.tar.gz", "a".repeat(400));
        let cleaned = sanitize_file_name(&long);
        assert!(cleaned.len() <= MAX_STAGED_FILE_NAME_BYTES);
        assert!(cleaned.ends_with(".gz"));

        // Multi-byte characters must not be split mid-character.
        let multibyte = format!("{}.txt", "я".repeat(200));
        let cleaned = sanitize_file_name(&multibyte);
        assert!(cleaned.len() <= MAX_STAGED_FILE_NAME_BYTES);
        assert!(cleaned.ends_with(".txt"));
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "tanuki-stage-test-{label}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn stage_file_writes_bytes_under_the_staging_dir_with_the_clean_name() {
        let dir = TempDir::new("write");

        let path = stage_file_in(&dir.0, 7, "../My Report.pdf", b"pdf-bytes").unwrap();

        assert!(path.starts_with(&dir.0));
        assert_eq!(path.file_name().unwrap(), "My_Report.pdf");
        assert_eq!(fs::read(&path).unwrap(), b"pdf-bytes");
    }

    #[test]
    fn stage_file_never_overwrites_an_earlier_upload_of_the_same_name() {
        let dir = TempDir::new("nooverwrite");

        let first = stage_file_in(&dir.0, 1, "a.txt", b"one").unwrap();
        let second = stage_file_in(&dir.0, 1, "a.txt", b"two").unwrap();

        assert_ne!(first, second);
        assert_eq!(fs::read(&first).unwrap(), b"one");
        assert_eq!(fs::read(&second).unwrap(), b"two");
    }
}
