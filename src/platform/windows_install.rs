//! Native Windows self-update installer.
//!
//! Replaces the old `tanuki update` path of shelling out to
//! `powershell -ExecutionPolicy Bypass -Command "irm ... | iex"`, which
//! Windows Defender's real-time protection blocks as a malware
//! download-cradle pattern (confirmed via `Get-MpThreatDetection` matching
//! that exact command line to two failed `tanuki update` attempts).
//!
//! This module reproduces `website/install.ps1`'s on-disk layout and
//! semantics in-process, so existing installs (already using that layout)
//! keep working: a versioned release directory tree under
//! `%USERPROFILE%\.tanuki\packages\standalone`, a `current` junction, and a
//! PATH-visible junction under `%LOCALAPPDATA%\Programs\Tanuki\bin` (or the
//! `TANUKI_INSTALL_DIR`/`TANUKI_HOME` overrides). `install.ps1` remains the
//! reference implementation for this layout for the first-install bootstrap
//! case; keep both in sync by hand if the layout ever changes.

use std::env;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const WINDOWS_TARGET_TRIPLE: &str = "x86_64-pc-windows-msvc";
const DEFAULT_RETAINED_RELEASES: usize = 3;
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const LOCK_MAX_WAIT: Duration = Duration::from_secs(60);

/// Resolved install layout, matching `install.ps1`'s env-var resolution
/// exactly (`TANUKI_HOME`/`TANUKI_INSTALL_DIR` overrides).
pub(crate) struct TanukiInstallPaths {
    pub(crate) standalone_root: PathBuf,
    pub(crate) releases_dir: PathBuf,
    pub(crate) current_dir: PathBuf,
    pub(crate) lock_path: PathBuf,
    pub(crate) visible_bin_dir: PathBuf,
    allow_legacy_bin_migration: bool,
}

impl TanukiInstallPaths {
    pub(crate) fn resolve() -> Result<Self, String> {
        let tanuki_home = match env::var_os("TANUKI_HOME").filter(|value| !value.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => {
                let profile = env::var_os("USERPROFILE")
                    .ok_or("USERPROFILE is not set; cannot locate Tanuki install")?;
                PathBuf::from(profile).join(".tanuki")
            }
        };
        let standalone_root = tanuki_home.join("packages").join("standalone");
        let releases_dir = standalone_root.join("releases");
        let current_dir = standalone_root.join("current");
        let lock_path = standalone_root.join("install.lock");

        let (visible_bin_dir, allow_legacy_bin_migration) =
            match env::var_os("TANUKI_INSTALL_DIR").filter(|value| !value.is_empty()) {
                Some(dir) => (PathBuf::from(dir), false),
                None => {
                    let local_app_data = env::var_os("LOCALAPPDATA")
                        .ok_or("LOCALAPPDATA is not set; cannot locate Tanuki install")?;
                    let default_dir = PathBuf::from(local_app_data)
                        .join("Programs")
                        .join("Tanuki")
                        .join("bin");
                    (default_dir, true)
                }
            };

        Ok(Self {
            standalone_root,
            releases_dir,
            current_dir,
            lock_path,
            visible_bin_dir,
            allow_legacy_bin_migration,
        })
    }

    pub(crate) fn installed_tanuki_exe_path(&self) -> PathBuf {
        self.visible_bin_dir.join("tanuki.exe")
    }
}

/// Installs an already-downloaded, checksum-verified `tanuki.exe` and
/// returns the path users invoke as `tanuki`. `version_identity` is the
/// release version string (e.g. `"0.1.4"`), used only for release-dir
/// naming — never compared to decide whether to skip work (see
/// `stage_and_place_release`'s file-existence guard).
pub(crate) fn install_windows_update(
    downloaded_file: &Path,
    version_identity: &str,
) -> Result<PathBuf, String> {
    let paths = TanukiInstallPaths::resolve()?;
    fs::create_dir_all(&paths.releases_dir)
        .map_err(|err| format!("failed to create {}: {err}", paths.releases_dir.display()))?;

    let release_dir = paths.releases_dir.join(release_dir_name(version_identity));

    with_install_lock(&paths.lock_path, || {
        remove_stale_staging_dirs(&paths.releases_dir);
        stage_and_place_release(&release_dir, &paths.releases_dir, downloaded_file)?;

        set_managed_junction(&paths.current_dir, &release_dir, &paths.releases_dir, false)?;
        set_managed_junction(
            &paths.visible_bin_dir,
            &release_dir,
            &paths.standalone_root,
            paths.allow_legacy_bin_migration,
        )?;

        // Smoke test runs unconditionally, including when staging was a
        // no-op (release dir already existed) -- mirrors install.ps1's own
        // post-link check and guards against the exact bug class in Claude
        // Code's own Windows installer once: a "looks current, skip" path
        // that never actually confirmed the target ran.
        verify_installed_binary(&paths.visible_bin_dir)?;
        prune_old_releases(&paths.releases_dir, &release_dir, DEFAULT_RETAINED_RELEASES);
        Ok(())
    })?;

    update_user_path(&paths.visible_bin_dir)?;

    Ok(paths.installed_tanuki_exe_path())
}

fn release_dir_name(version_identity: &str) -> String {
    format!(
        "{}-{WINDOWS_TARGET_TRIPLE}",
        sanitize_version_identity(version_identity)
    )
}

fn sanitize_version_identity(identity: &str) -> String {
    identity
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Install lock
// ---------------------------------------------------------------------------

/// Exclusive-share lock file, matching `install.ps1`'s
/// `[System.IO.FileShare]::None` open + 250ms spin-retry
/// (`Invoke-WithInstallLock`). Capped at `LOCK_MAX_WAIT` rather than
/// retrying forever, to fail loudly instead of hanging indefinitely if a
/// prior installer crashed while holding the lock.
fn with_install_lock<T>(
    lock_path: &Path,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }

    let mut waited = Duration::ZERO;
    let lock_file = loop {
        match OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .share_mode(0) // exclusive -- no other process may open this file concurrently
            .open(lock_path)
        {
            Ok(file) => break file,
            Err(err) if is_sharing_violation(&err) && waited < LOCK_MAX_WAIT => {
                std::thread::sleep(LOCK_RETRY_INTERVAL);
                waited += LOCK_RETRY_INTERVAL;
            }
            Err(err) => {
                return Err(format!(
                    "failed to acquire install lock at {}: {err}",
                    lock_path.display()
                ))
            }
        }
    };

    let result = f();
    drop(lock_file);
    result
}

fn is_sharing_violation(err: &io::Error) -> bool {
    // ERROR_SHARING_VIOLATION (32) / ERROR_ACCESS_DENIED (5) -- another
    // installer instance currently holds the lock.
    matches!(err.raw_os_error(), Some(32) | Some(5))
}

// ---------------------------------------------------------------------------
// Staging + atomic placement
// ---------------------------------------------------------------------------

fn remove_stale_staging_dirs(releases_dir: &Path) {
    let Ok(entries) = fs::read_dir(releases_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(".staging.") {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Idempotent: skips download+place if `release_dir\tanuki.exe` already
/// exists on disk. This check is deliberately a real filesystem check, never
/// a cached/compared version string -- the exact guard against the bug
/// class documented for Claude Code's own Windows installer, where a
/// version-equality short-circuit once skipped the file copy without
/// verifying the target actually existed.
fn stage_and_place_release(
    release_dir: &Path,
    releases_dir: &Path,
    downloaded_file: &Path,
) -> Result<(), String> {
    if release_dir.join("tanuki.exe").is_file() {
        return Ok(());
    }

    if release_dir.exists() {
        fs::remove_dir_all(release_dir).map_err(|err| {
            format!(
                "failed to clear stale release dir {}: {err}",
                release_dir.display()
            )
        })?;
    }

    let release_name = release_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("release");
    let staging_dir = releases_dir.join(format!(".staging.{release_name}.{}", std::process::id()));
    fs::create_dir_all(&staging_dir)
        .map_err(|err| format!("failed to create staging dir: {err}"))?;
    fs::copy(downloaded_file, staging_dir.join("tanuki.exe")).map_err(|err| {
        let _ = fs::remove_dir_all(&staging_dir);
        format!("failed to stage downloaded binary: {err}")
    })?;

    // Atomic rename within the same volume, matching install.ps1's
    // `Move-Item` of the staging dir to the final release dir.
    fs::rename(&staging_dir, release_dir).map_err(|err| {
        let _ = fs::remove_dir_all(&staging_dir);
        format!("failed to place release: {err}")
    })
}

// ---------------------------------------------------------------------------
// Junction management (ports install.ps1's Set-ManagedJunction)
// ---------------------------------------------------------------------------

fn set_managed_junction(
    link_path: &Path,
    target_path: &Path,
    managed_target_prefix: &Path,
    allow_legacy_bin_migration: bool,
) -> Result<(), String> {
    // `Path::exists()` follows the reparse point and reports `false` for a
    // junction whose target has been removed (e.g. a pruned release dir).
    // `symlink_metadata` sees the reparse point entry itself, dangling or
    // not, so use it to decide whether anything is here at all.
    let link_metadata = fs::symlink_metadata(link_path).ok();
    if let Some(metadata) = link_metadata {
        // A dangling reparse point resolves `Path::exists()` to `false`.
        // `junction::exists`/`get_target` both gate internally on
        // `Path::exists()` too, so they misreport a dangling junction as
        // "not found" rather than erroring -- work around that by only
        // trusting them when the target still resolves, and otherwise
        // falling back to the raw file-type bit (a reparse point that
        // isn't a real file or a non-empty directory can only be a stale
        // junction of ours, since nothing else places one at a managed
        // install path).
        let target_resolves = link_path.exists();
        let is_junction = if target_resolves {
            match junction::exists(link_path) {
                Ok(is_junction) => is_junction,
                // `junction::exists` opens the path as a reparse point and
                // reads its reparse data; a plain directory that resolves
                // fine via `Path::exists()` still fails that read with
                // ERROR_NOT_A_REPARSE_POINT (raw os error 4390) instead of
                // `Ok(false)`. Treat that specific error as "not a
                // junction" so plain directories fall through to the
                // `link_path.is_dir()` branch below, instead of hard-erroring
                // out of every update whenever `current`/the bin dir was
                // ever a real directory (e.g. from before junction-based
                // installs existed).
                Err(err) if err.raw_os_error() == Some(4390) => false,
                Err(err) => {
                    return Err(format!("failed to inspect {}: {err}", link_path.display()))
                }
            }
        } else {
            metadata.file_type().is_symlink()
        };

        if is_junction {
            if target_resolves {
                let existing_target = junction::get_target(link_path).map_err(|err| {
                    format!(
                        "failed to read junction target at {}: {err}",
                        link_path.display()
                    )
                })?;
                if !path_starts_with_ignore_case(&existing_target, managed_target_prefix) {
                    return Err(format!(
                        "refusing to retarget junction at {} because it is not managed by this installer",
                        link_path.display()
                    ));
                }
                if paths_equal_ignore_case(&existing_target, target_path) {
                    return Ok(()); // already correct -- no-op
                }
            }
            junction::delete(link_path).map_err(|err| {
                format!(
                    "failed to remove stale junction at {}: {err}",
                    link_path.display()
                )
            })?;
            // `junction::delete` only clears the reparse-point tag (via
            // FSCTL_DELETE_REPARSE_POINT) -- the now-plain, empty directory
            // it leaves behind is still on disk. `junction::create` calls
            // `fs::create_dir` on the link path first, which fails with
            // "already exists" (os error 183) unless that leftover
            // directory is removed too. This is not a dangling-target edge
            // case: it fires on every retarget to a different release.
            fs::remove_dir(link_path).map_err(|err| {
                format!(
                    "failed to remove leftover directory at {} after clearing its junction tag: {err}",
                    link_path.display()
                )
            })?;
        } else if link_path.is_dir() {
            let has_entries = fs::read_dir(link_path)
                .map_err(|err| format!("failed to read {}: {err}", link_path.display()))?
                .next()
                .is_some();
            if has_entries {
                if allow_legacy_bin_migration && is_legacy_bin_dir(link_path)? {
                    migrate_legacy_bin_dir(link_path)?;
                } else {
                    return Err(format!(
                        "refusing to replace non-empty directory at {} with a junction",
                        link_path.display()
                    ));
                }
            } else {
                fs::remove_dir(link_path).map_err(|err| {
                    format!(
                        "failed to remove empty directory at {}: {err}",
                        link_path.display()
                    )
                })?;
            }
        } else {
            return Err(format!(
                "refusing to replace file at {} with a junction",
                link_path.display()
            ));
        }
    }

    if let Some(parent) = link_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    junction::create(target_path, link_path).map_err(|err| {
        format!(
            "failed to create junction at {}: {err}",
            link_path.display()
        )
    })
}

fn is_legacy_bin_dir(path: &Path) -> Result<bool, String> {
    let entries: Vec<_> = fs::read_dir(path)
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?
        .filter_map(|entry| entry.ok())
        .collect();
    if entries.iter().any(|entry| entry.path().is_dir()) {
        return Ok(false);
    }
    Ok(entries
        .iter()
        .any(|entry| entry.file_name().eq_ignore_ascii_case("tanuki.exe")))
}

fn migrate_legacy_bin_dir(path: &Path) -> Result<(), String> {
    let base_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("bin");
    let legacy_path = path.with_file_name(format!("{base_name}.legacy.{}", random_suffix()));
    fs::rename(path, &legacy_path)
        .map_err(|err| format!("failed to move legacy bin directory aside: {err}"))?;
    eprintln!(
        "Moved legacy Tanuki bin directory to {}.",
        legacy_path.display()
    );
    Ok(())
}

fn random_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}-{:x}", std::process::id(), nanos)
}

fn normalize_for_compare(path: &Path) -> String {
    path.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

fn paths_equal_ignore_case(a: &Path, b: &Path) -> bool {
    normalize_for_compare(a) == normalize_for_compare(b)
}

fn path_starts_with_ignore_case(path: &Path, prefix: &Path) -> bool {
    normalize_for_compare(path).starts_with(&normalize_for_compare(prefix))
}

// ---------------------------------------------------------------------------
// Post-link smoke test + retention
// ---------------------------------------------------------------------------

/// Runs unconditionally after every install (see call site) -- mirrors
/// install.ps1's post-link `tanuki.exe --version` check, which runs before
/// pruning old releases so a bad install never destroys the last-known-good
/// version first.
fn verify_installed_binary(visible_bin_dir: &Path) -> Result<(), String> {
    let tanuki_exe = visible_bin_dir.join("tanuki.exe");
    let status = Command::new(&tanuki_exe)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|err| {
            format!(
                "installed Tanuki command failed verification: {} --version ({err})",
                tanuki_exe.display()
            )
        })?;
    if !status.success() {
        return Err(format!(
            "installed Tanuki command failed verification: {} --version",
            tanuki_exe.display()
        ));
    }
    Ok(())
}

fn prune_old_releases(releases_dir: &Path, current_release_dir: &Path, keep: usize) {
    if keep == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(releases_dir) else {
        return;
    };

    let mut dirs: Vec<(PathBuf, SystemTime)> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with(".staging."))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((entry.path(), modified))
        })
        .collect();
    dirs.sort_by_key(|entry| std::cmp::Reverse(entry.1));

    let current = current_release_dir
        .canonicalize()
        .unwrap_or_else(|_| current_release_dir.to_path_buf());
    let mut kept = 0usize;
    for (dir, _) in dirs {
        let dir_canon = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if dir_canon == current {
            kept += 1;
            continue;
        }
        if kept < keep {
            kept += 1;
            continue;
        }
        let _ = fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------------------
// PATH update (registry write + WM_SETTINGCHANGE broadcast)
// ---------------------------------------------------------------------------

/// Reproduces install.ps1's
/// `[Environment]::SetEnvironmentVariable("Path", ..., "User")`, which
/// writes `HKCU\Environment\Path` *and* internally broadcasts
/// `WM_SETTINGCHANGE` -- `winreg` only does the registry half, so the
/// broadcast is reproduced explicitly below.
fn update_user_path(visible_bin_dir: &Path) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;

    let bin_dir = visible_bin_dir
        .to_string_lossy()
        .trim_end_matches('\\')
        .to_string();

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let env_key = hkcu
        .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
        .map_err(|err| format!("failed to open HKCU\\Environment: {err}"))?;

    let current_path: String = env_key.get_value("Path").unwrap_or_default();
    let new_path = prepend_path_entry(&current_path, &bin_dir);

    if new_path != current_path {
        env_key
            .set_value("Path", &new_path)
            .map_err(|err| format!("failed to update PATH in HKCU\\Environment: {err}"))?;
        broadcast_environment_change();
    }

    // Also update the *current* process's view of PATH, matching
    // install.ps1's `$env:Path = ...` -- low-stakes since `tanuki update`
    // itself doesn't need to re-resolve `tanuki` on PATH mid-run, but cheap
    // to keep for parity.
    if let Ok(process_path) = env::var("Path") {
        let updated_process_path = prepend_path_entry(&process_path, &bin_dir);
        if updated_process_path != process_path {
            env::set_var("Path", updated_process_path);
        }
    }

    Ok(())
}

fn prepend_path_entry(path_value: &str, entry: &str) -> String {
    let needle = entry.trim_end_matches('\\');
    let mut segments: Vec<&str> = vec![entry];
    segments.extend(
        path_value
            .split(';')
            .filter(|segment| !segment.is_empty())
            .filter(|segment| !segment.trim_end_matches('\\').eq_ignore_ascii_case(needle)),
    );
    segments.join(";")
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn broadcast_environment_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };

    let param = wide_null("Environment");
    unsafe {
        let mut result: usize = 0;
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            param.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            &mut result,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tanuki-windows-install-tests-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sanitize_version_identity_replaces_disallowed_chars() {
        assert_eq!(sanitize_version_identity("0.1.4"), "0.1.4");
        assert_eq!(
            sanitize_version_identity("0.1.4-preview.7+abc"),
            "0.1.4-preview.7-abc"
        );
    }

    #[test]
    fn release_dir_name_appends_target_triple() {
        assert_eq!(release_dir_name("0.1.4"), "0.1.4-x86_64-pc-windows-msvc");
    }

    #[test]
    fn prepend_path_entry_dedupes_case_insensitively_and_moves_to_front() {
        let result = prepend_path_entry(
            r"C:\Windows;C:\Users\user\AppData\Local\Programs\Tanuki\bin\;C:\Tools",
            r"C:\Users\user\AppData\Local\Programs\Tanuki\bin",
        );
        assert_eq!(
            result,
            r"C:\Users\user\AppData\Local\Programs\Tanuki\bin;C:\Windows;C:\Tools"
        );
    }

    #[test]
    fn prepend_path_entry_is_a_no_op_when_already_first() {
        let path = r"C:\Users\user\AppData\Local\Programs\Tanuki\bin;C:\Windows";
        assert_eq!(
            prepend_path_entry(path, r"C:\Users\user\AppData\Local\Programs\Tanuki\bin"),
            path
        );
    }

    #[test]
    fn stage_and_place_release_skips_when_binary_already_exists() {
        let root = unique_temp_dir("stage-skip");
        let releases_dir = root.join("releases");
        let release_dir = releases_dir.join("0.1.4-x86_64-pc-windows-msvc");
        fs::create_dir_all(&release_dir).unwrap();
        fs::write(release_dir.join("tanuki.exe"), b"existing").unwrap();

        let downloaded = root.join("downloaded.exe");
        fs::write(&downloaded, b"new-version").unwrap();

        stage_and_place_release(&release_dir, &releases_dir, &downloaded).unwrap();

        // Existing file untouched -- idempotent skip via real Path::exists()
        // check, not a version comparison.
        assert_eq!(
            fs::read(release_dir.join("tanuki.exe")).unwrap(),
            b"existing"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stage_and_place_release_places_new_release() {
        let root = unique_temp_dir("stage-place");
        let releases_dir = root.join("releases");
        fs::create_dir_all(&releases_dir).unwrap();
        let release_dir = releases_dir.join("0.1.4-x86_64-pc-windows-msvc");

        let downloaded = root.join("downloaded.exe");
        fs::write(&downloaded, b"new-version").unwrap();

        stage_and_place_release(&release_dir, &releases_dir, &downloaded).unwrap();

        assert_eq!(
            fs::read(release_dir.join("tanuki.exe")).unwrap(),
            b"new-version"
        );
        // No leftover staging directory.
        let leftover_staging = fs::read_dir(&releases_dir)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(".staging."));
        assert!(!leftover_staging);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_stale_staging_dirs_deletes_only_staging_prefixed_dirs() {
        let root = unique_temp_dir("stale-staging");
        fs::create_dir_all(root.join(".staging.0.1.4-x86_64-pc-windows-msvc.1234")).unwrap();
        fs::create_dir_all(root.join("0.1.3-x86_64-pc-windows-msvc")).unwrap();

        remove_stale_staging_dirs(&root);

        assert!(!root
            .join(".staging.0.1.4-x86_64-pc-windows-msvc.1234")
            .exists());
        assert!(root.join("0.1.3-x86_64-pc-windows-msvc").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_old_releases_keeps_current_and_most_recent() {
        let root = unique_temp_dir("prune");
        let dirs: Vec<PathBuf> = (0..5)
            .map(|i| {
                let dir = root.join(format!("release-{i}"));
                fs::create_dir_all(&dir).unwrap();
                // Stagger mtimes so ordering is deterministic.
                std::thread::sleep(Duration::from_millis(10));
                fs::write(dir.join("marker"), b"x").unwrap();
                dir
            })
            .collect();

        // Treat the *oldest* dir as "current" -- must survive pruning
        // regardless of its recency.
        let current = dirs[0].clone();
        prune_old_releases(&root, &current, 2);

        let remaining: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();

        assert!(
            remaining.contains(&"release-0".to_string()),
            "{remaining:?}"
        );
        assert!(
            remaining.contains(&"release-4".to_string()),
            "{remaining:?}"
        );
        assert!(
            remaining.contains(&"release-3".to_string()),
            "{remaining:?}"
        );
        assert!(
            !remaining.contains(&"release-1".to_string()),
            "{remaining:?}"
        );
        assert!(
            !remaining.contains(&"release-2".to_string()),
            "{remaining:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn set_managed_junction_retargets_existing_valid_junction() {
        // Reproduces the real `tanuki update` failure: retargeting an
        // existing, valid managed junction to a different release dir
        // (i.e. every ordinary version update).
        let root = unique_temp_dir("junction-retarget");
        let old_target = root.join("release-0.1.5");
        fs::create_dir_all(&old_target).unwrap();
        let link_path = root.join("current");
        junction::create(&old_target, &link_path).unwrap();

        let new_target = root.join("release-0.1.6");
        fs::create_dir_all(&new_target).unwrap();

        set_managed_junction(&link_path, &new_target, &root, false)
            .expect("retargets an existing valid junction to a new release");
        assert_eq!(junction::get_target(&link_path).unwrap(), new_target);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn set_managed_junction_recreates_dangling_junction() {
        let root = unique_temp_dir("junction-dangling");
        let old_target = root.join("old-release");
        fs::create_dir_all(&old_target).unwrap();
        let link_path = root.join("current");
        junction::create(&old_target, &link_path).unwrap();

        // Simulate a pruned release: the junction entry still exists on
        // disk, but the directory it points at is gone, so `Path::exists()`
        // (which follows the reparse point) reports `false` for it.
        fs::remove_dir_all(&old_target).unwrap();
        assert!(!link_path.exists());
        assert!(fs::symlink_metadata(&link_path).is_ok());

        let new_target = root.join("new-release");
        fs::create_dir_all(&new_target).unwrap();

        set_managed_junction(&link_path, &new_target, &root, false)
            .expect("recreates a dangling junction instead of erroring");
        assert_eq!(junction::get_target(&link_path).unwrap(), new_target);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn set_managed_junction_replaces_plain_directory_not_created_as_junction() {
        // Reproduces the real `tanuki update` failure: `current` is a plain
        // (non-reparse-point) directory that fully resolves via
        // `Path::exists()`, so `junction::exists` doesn't short-circuit on a
        // missing path -- it opens the directory as a reparse point and
        // fails to read reparse data with ERROR_NOT_A_REPARSE_POINT instead
        // of returning `Ok(false)`. That must be treated as "not a
        // junction," not propagated as a hard error.
        let root = unique_temp_dir("junction-plain-dir");
        let link_path = root.join("current");
        fs::create_dir_all(&link_path).unwrap();

        let target = root.join("release-0.1.8");
        fs::create_dir_all(&target).unwrap();

        set_managed_junction(&link_path, &target, &root, false)
            .expect("replaces a plain, non-reparse-point directory with a junction");
        assert_eq!(junction::get_target(&link_path).unwrap(), target);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn set_managed_junction_refuses_to_replace_plain_file() {
        let root = unique_temp_dir("junction-refuse-file");
        let target = root.join("target-release");
        fs::create_dir_all(&target).unwrap();
        let link_path = root.join("link");
        fs::write(&link_path, b"not a junction").unwrap();

        let result = set_managed_junction(&link_path, &target, &root, false);
        assert!(result.is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn tanuki_install_paths_resolve_honors_overrides() {
        // Safety: env var mutation races with other tests running in
        // parallel threads within this process; scope this test's env vars
        // to values unlikely to collide, and always restore afterward.
        let previous_home = env::var_os("TANUKI_HOME");
        let previous_install_dir = env::var_os("TANUKI_INSTALL_DIR");

        let fake_home = unique_temp_dir("install-paths-home");
        let fake_install_dir = unique_temp_dir("install-paths-bin");
        env::set_var("TANUKI_HOME", &fake_home);
        env::set_var("TANUKI_INSTALL_DIR", &fake_install_dir);

        let paths = TanukiInstallPaths::resolve().unwrap();
        assert_eq!(
            paths.standalone_root,
            fake_home.join("packages").join("standalone")
        );
        assert_eq!(paths.visible_bin_dir, fake_install_dir);

        match previous_home {
            Some(value) => env::set_var("TANUKI_HOME", value),
            None => env::remove_var("TANUKI_HOME"),
        }
        match previous_install_dir {
            Some(value) => env::set_var("TANUKI_INSTALL_DIR", value),
            None => env::remove_var("TANUKI_INSTALL_DIR"),
        }

        let _ = fs::remove_dir_all(&fake_home);
        let _ = fs::remove_dir_all(&fake_install_dir);
    }
}
