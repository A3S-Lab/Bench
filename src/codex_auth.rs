use anyhow::{Context, Result};
use serde_json::Value;
use std::cmp::Reverse;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

pub const AUTH_SOURCE_ENV: &str = "A3S_BENCH_CODEX_AUTH_FILE";

const HOME_DIRECTORY: &str = "codex-homes";
const HOME_PREFIX: &str = "a3s-codex-home-";
const OWNER_MARKER: &str = ".a3s-bench-codex-home";
const OWNER_MARKER_CONTENT: &[u8] = b"a3s-bench/codex-home/v1\n";
const MAX_AUTH_BYTES: u64 = 1024 * 1024;
const STALE_HOME_AGE: Duration = Duration::from_secs(24 * 60 * 60);
static HOME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct PrivateCodexHome {
    path: PathBuf,
    codex_path: PathBuf,
    secrets: Vec<SecretBuffer>,
    cleaned: bool,
    cleanup_on_drop: bool,
}

impl PrivateCodexHome {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn codex_path(&self) -> &Path {
        &self.codex_path
    }

    pub fn prepare_for_container_copy(&self) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            // This is a short-lived benchmark-owned copy below an owner-only
            // parent. Docker Desktop does not consistently preserve its uid
            // when archiving into a volume, so make the isolated copy usable
            // regardless of the uid assigned inside the task container.
            for path in [&self.path, &self.codex_path] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777))?;
            }
            std::fs::set_permissions(
                self.codex_path.join("auth.json"),
                std::fs::Permissions::from_mode(0o666),
            )?;
        }
        Ok(())
    }

    pub fn redact(&self, bytes: &[u8]) -> Vec<u8> {
        redact_bytes(bytes, &self.secrets)
    }

    /// Remove the benchmark-owned home.  Callers should invoke this before
    /// releasing any container cleanup guard; Drop retries it best-effort.
    pub fn cleanup(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        let result = match owned_home(self.path()) {
            Ok(true) => crate::state_fs::remove_tree(self.path()),
            Ok(false) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(error),
        };
        if result.is_ok() {
            self.cleaned = true;
        }
        result
    }

    pub(crate) fn retain_for_stale_recovery(&mut self) {
        self.cleanup_on_drop = false;
    }
}

impl Drop for PrivateCodexHome {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = self.cleanup();
        }
    }
}

struct SecretBuffer(Vec<u8>);

impl SecretBuffer {
    fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SecretBuffer {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

pub fn stage(state_root: &Path, source_override: Option<&Path>) -> Result<PrivateCodexHome> {
    let source = match source_override {
        Some(path) => path.to_path_buf(),
        None => match std::env::var_os(AUTH_SOURCE_ENV) {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            Some(_) => anyhow::bail!("{AUTH_SOURCE_ENV} must name an auth.json file"),
            None => resolve_default_source()?,
        },
    };
    stage_from_source(state_root, &source)
}

/// Explicit-source seam for tests and controlled installations.  It never
/// consults CODEX_HOME, HOME, or any ambient credential configuration.
pub fn stage_from_source(state_root: &Path, source: &Path) -> Result<PrivateCodexHome> {
    let parent = state_root.join(HOME_DIRECTORY);
    crate::state_fs::secure_directory(&parent)?;
    cleanup_stale_homes(&parent)?;
    let auth_bytes = read_auth_defensively(source)?;
    let mut secrets = extract_secret_buffers(auth_bytes.as_slice())?;
    let path = create_home(&parent)?;
    let codex_path = path.join(".codex");
    let result = (|| -> Result<()> {
        crate::state_fs::secure_atomic_write(&path.join(OWNER_MARKER), OWNER_MARKER_CONTENT)?;
        crate::state_fs::create_secure_directory_exclusive(&codex_path)?;
        let auth_path = codex_path.join("auth.json");
        create_auth_copy(&auth_path, auth_bytes.as_slice())?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = crate::state_fs::remove_tree(&path);
        return Err(error).context("could not stage private Codex authentication");
    }
    secrets.sort_by_key(|secret| Reverse(secret.0.len()));
    Ok(PrivateCodexHome {
        path,
        codex_path,
        secrets,
        cleaned: false,
        cleanup_on_drop: true,
    })
}

pub fn resolve_default_source() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME").filter(|home| !home.is_empty()) {
        return Ok(PathBuf::from(home).join("auth.json"));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        return Ok(PathBuf::from(home).join(".codex/auth.json"));
    }
    anyhow::bail!(
        "Codex file authentication is unavailable; set {AUTH_SOURCE_ENV} to an auth.json file"
    )
}

pub fn cleanup_stale_homes(parent: &Path) -> Result<()> {
    cleanup_stale_homes_at(parent, SystemTime::now(), STALE_HOME_AGE)
}

fn cleanup_stale_homes_at(parent: &Path, now: SystemTime, max_age: Duration) -> Result<()> {
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name
            .strip_prefix(HOME_PREFIX)
            .is_some_and(|suffix| !suffix.is_empty())
        {
            continue;
        }
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() || !owned_home(&path)? {
            continue;
        }
        let age = now
            .duration_since(metadata.modified().unwrap_or(now))
            .unwrap_or_default();
        if age > max_age {
            crate::state_fs::remove_tree(&path)?;
        }
    }
    Ok(())
}

fn create_home(parent: &Path) -> Result<PathBuf> {
    for _ in 0..32 {
        let sequence = HOME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!("{HOME_PREFIX}{}-{sequence}", std::process::id()));
        match crate::state_fs::create_secure_directory_exclusive(&path) {
            Ok(()) => return Ok(path),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    anyhow::bail!("could not allocate a private Codex home")
}

fn read_auth_defensively(source: &Path) -> Result<SecretBuffer> {
    let metadata = std::fs::symlink_metadata(source).map_err(|_| {
        anyhow::anyhow!("Codex auth.json is unavailable or is not a private regular file")
    })?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Codex auth.json is unavailable or is not a private regular file"
    );
    anyhow::ensure!(
        metadata.len() <= MAX_AUTH_BYTES,
        "Codex auth.json exceeds the permitted size"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            metadata.nlink() == 1,
            "Codex auth.json must not be a hard link"
        );
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(no_follow_flag());
    }
    let mut file = options
        .open(source)
        .map_err(|_| anyhow::anyhow!("Codex auth.json could not be opened safely"))?;
    let opened = file.metadata()?;
    anyhow::ensure!(
        opened.is_file() && opened.len() <= MAX_AUTH_BYTES,
        "Codex auth.json changed while it was being opened"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            opened.dev() == metadata.dev() && opened.ino() == metadata.ino(),
            "Codex auth.json changed while it was being opened"
        );
        anyhow::ensure!(
            opened.nlink() == 1,
            "Codex auth.json must not be a hard link"
        );
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.by_ref()
        .take(MAX_AUTH_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let final_metadata = file.metadata()?;
    anyhow::ensure!(
        bytes.len() as u64 == final_metadata.len()
            && bytes.len() as u64 <= MAX_AUTH_BYTES
            && final_metadata.is_file(),
        "Codex auth.json changed while it was being copied"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            final_metadata.dev() == metadata.dev()
                && final_metadata.ino() == metadata.ino()
                && final_metadata.nlink() == 1,
            "Codex auth.json changed while it was being copied"
        );
    }
    let document: Value = serde_json::from_slice(&bytes)
        .context("Codex auth.json must contain a valid JSON object")?;
    anyhow::ensure!(
        document.is_object(),
        "Codex auth.json must contain a valid JSON object"
    );
    Ok(SecretBuffer::new(bytes))
}

fn extract_secret_buffers(document: &[u8]) -> Result<Vec<SecretBuffer>> {
    let value: Value = serde_json::from_slice(document)
        .context("Codex auth.json must contain a valid JSON object")?;
    anyhow::ensure!(
        value.is_object(),
        "Codex auth.json must contain a valid JSON object"
    );
    let mut values = vec![SecretBuffer::new(document.to_vec())];
    fn visit(value: &Value, values: &mut Vec<SecretBuffer>) {
        match value {
            Value::String(value) if !value.is_empty() => {
                values.push(SecretBuffer::new(value.as_bytes().to_vec()))
            }
            Value::Array(values_array) => {
                for value in values_array {
                    visit(value, values);
                }
            }
            Value::Object(values_object) => {
                for value in values_object.values() {
                    visit(value, values);
                }
            }
            _ => {}
        }
    }
    visit(&value, &mut values);
    values.sort_by_key(|secret| Reverse(secret.0.len()));
    values.dedup_by(|left, right| left.0 == right.0);
    Ok(values)
}

fn create_auth_copy(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn owned_home(path: &Path) -> Result<bool> {
    let name_is_owned = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(HOME_PREFIX));
    if !name_is_owned {
        return Ok(false);
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Codex home has no parent"))?;
        let parent_metadata = std::fs::symlink_metadata(parent)?;
        if metadata.uid() != parent_metadata.uid() {
            return Ok(false);
        }
    }
    let marker = crate::state_fs::read_optional_regular_file(
        &path.join(OWNER_MARKER),
        "Codex home ownership marker",
    )?;
    Ok(marker.as_deref() == Some(OWNER_MARKER_CONTENT))
}

fn redact_bytes(bytes: &[u8], secrets: &[SecretBuffer]) -> Vec<u8> {
    if secrets.is_empty() {
        return bytes.to_vec();
    }
    let mut output = bytes.to_vec();
    for secret in secrets {
        if secret.0.is_empty() {
            continue;
        }
        let mut redacted = Vec::with_capacity(output.len());
        let mut cursor = 0;
        while let Some(relative) = output[cursor..]
            .windows(secret.0.len())
            .position(|window| window == secret.0.as_slice())
        {
            let start = cursor + relative;
            redacted.extend_from_slice(&output[cursor..start]);
            redacted.extend_from_slice(b"[redacted]");
            cursor = start + secret.0.len();
        }
        redacted.extend_from_slice(&output[cursor..]);
        output = redacted;
    }
    output
}

#[cfg(unix)]
fn no_follow_flag() -> i32 {
    #[cfg(target_os = "linux")]
    {
        0o400000
    }
    #[cfg(not(target_os = "linux"))]
    {
        0x0100
    }
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|cause| cause.kind() == std::io::ErrorKind::NotFound)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn copies_private_auth_and_removes_the_home_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fake-auth.json");
        let sentinel = br#"{"access_token":"fake-auth-sentinel","nested":["public","fake-auth-sentinel-extra"]}"#;
        std::fs::write(&source, sentinel).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        let state = root.path().join("state");
        let home_path;
        {
            let home = stage_from_source(&state, &source).unwrap();
            home_path = home.path().to_path_buf();
            assert_eq!(
                std::fs::read(home.path().join(".codex/auth.json")).unwrap(),
                sentinel
            );
            assert_eq!(
                std::fs::metadata(home.path().join(".codex/auth.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                home.redact(b"prefix fake-auth-sentinel suffix"),
                b"prefix [redacted] suffix"
            );
            assert_eq!(home.redact(sentinel), b"[redacted]");
        }
        assert!(!home_path.exists());
        assert!(source.exists());
    }

    #[test]
    fn container_copy_is_uid_independent_without_changing_the_source() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fake-auth.json");
        std::fs::write(&source, r#"{"access_token":"fake"}"#).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        let home = stage_from_source(&root.path().join("state"), &source).unwrap();

        home.prepare_for_container_copy().unwrap();

        assert_eq!(
            std::fs::metadata(home.path()).unwrap().permissions().mode() & 0o777,
            0o777
        );
        assert_eq!(
            std::fs::metadata(home.codex_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o777
        );
        assert_eq!(
            std::fs::metadata(home.codex_path().join("auth.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o666
        );
        assert_eq!(
            std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn stale_cleanup_uses_the_parent_marker_after_codex_contents_change() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fake-auth.json");
        std::fs::write(&source, r#"{"access_token":"fake"}"#).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        let state = root.path().join("state");
        let home = stage_from_source(&state, &source).unwrap();
        let home_path = home.path().to_path_buf();
        std::fs::remove_file(home.codex_path().join("auth.json")).unwrap();
        std::fs::write(home.codex_path().join("refresh-cache"), b"mutated").unwrap();
        std::fs::File::open(&home_path)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(1))
            .unwrap();

        cleanup_stale_homes_at(
            &state.join(HOME_DIRECTORY),
            SystemTime::now() + Duration::from_secs(2),
            Duration::ZERO,
        )
        .unwrap();

        assert!(!home_path.exists());
    }

    #[test]
    fn accepts_group_readable_auth_but_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fake-auth.json");
        std::fs::write(&source, r#"{"access_token":"fake"}"#).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o640)).unwrap();
        let staged = stage_from_source(&root.path().join("state"), &source).unwrap();
        drop(staged);
        let link = root.path().join("link-auth.json");
        symlink(&source, &link).unwrap();
        assert!(stage_from_source(&root.path().join("state"), &link).is_err());
    }

    #[test]
    fn cleans_only_stale_owned_homes() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join(HOME_DIRECTORY);
        crate::state_fs::secure_directory(&parent).unwrap();

        let owned = parent.join(format!("{HOME_PREFIX}owned"));
        crate::state_fs::create_secure_directory_exclusive(&owned).unwrap();
        crate::state_fs::secure_atomic_write(&owned.join(OWNER_MARKER), OWNER_MARKER_CONTENT)
            .unwrap();

        let wrong_marker = parent.join(format!("{HOME_PREFIX}wrong-marker"));
        crate::state_fs::create_secure_directory_exclusive(&wrong_marker).unwrap();
        crate::state_fs::secure_atomic_write(&wrong_marker.join(OWNER_MARKER), b"not-owned\n")
            .unwrap();

        let other_prefix = parent.join("codex-home-owned");
        crate::state_fs::create_secure_directory_exclusive(&other_prefix).unwrap();
        crate::state_fs::secure_atomic_write(
            &other_prefix.join(OWNER_MARKER),
            OWNER_MARKER_CONTENT,
        )
        .unwrap();

        cleanup_stale_homes_at(
            &parent,
            SystemTime::now() + Duration::from_secs(1),
            Duration::ZERO,
        )
        .unwrap();
        assert!(!owned.exists());
        assert!(wrong_marker.exists());
        assert!(other_prefix.exists());
    }

    fn create_stage_cleanup_fixtures(root: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        use std::os::unix::fs::symlink;

        let parent = root.join(HOME_DIRECTORY);
        crate::state_fs::secure_directory(&parent).unwrap();

        let stale_owned = parent.join(format!("{HOME_PREFIX}stale-owned"));
        crate::state_fs::create_secure_directory_exclusive(&stale_owned).unwrap();
        crate::state_fs::secure_atomic_write(&stale_owned.join(OWNER_MARKER), OWNER_MARKER_CONTENT)
            .unwrap();
        let stale_time = SystemTime::now()
            .checked_sub(STALE_HOME_AGE + Duration::from_secs(1))
            .unwrap();
        std::fs::File::open(&stale_owned)
            .unwrap()
            .set_modified(stale_time)
            .unwrap();

        let fresh_owned = parent.join(format!("{HOME_PREFIX}fresh-owned"));
        crate::state_fs::create_secure_directory_exclusive(&fresh_owned).unwrap();
        crate::state_fs::secure_atomic_write(&fresh_owned.join(OWNER_MARKER), OWNER_MARKER_CONTENT)
            .unwrap();

        let non_owned = parent.join(format!("{HOME_PREFIX}non-owned"));
        crate::state_fs::create_secure_directory_exclusive(&non_owned).unwrap();
        crate::state_fs::secure_atomic_write(&non_owned.join(OWNER_MARKER), b"not-owned\n")
            .unwrap();

        let symlink_target = root.join("symlink-target");
        std::fs::create_dir(&symlink_target).unwrap();
        std::fs::write(symlink_target.join("sentinel"), b"untouched").unwrap();
        let symlink_home = parent.join(format!("{HOME_PREFIX}symlink"));
        symlink(&symlink_target, &symlink_home).unwrap();

        (
            stale_owned,
            fresh_owned,
            non_owned,
            symlink_home,
            symlink_target,
        )
    }

    fn assert_stage_cleanup_survivors(
        fresh_owned: &Path,
        non_owned: &Path,
        symlink_home: &Path,
        symlink_target: &Path,
    ) {
        assert!(fresh_owned.exists());
        assert_eq!(
            std::fs::read(fresh_owned.join(OWNER_MARKER)).unwrap(),
            OWNER_MARKER_CONTENT
        );
        assert!(non_owned.exists());
        assert_eq!(
            std::fs::read(non_owned.join(OWNER_MARKER)).unwrap(),
            b"not-owned\n"
        );
        assert!(std::fs::symlink_metadata(symlink_home)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read(symlink_target.join("sentinel")).unwrap(),
            b"untouched"
        );
    }

    #[test]
    fn cleans_stale_owned_home_before_missing_auth_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let (stale_owned, fresh_owned, non_owned, symlink_home, symlink_target) =
            create_stage_cleanup_fixtures(&state);
        let source = root.path().join("missing-auth.json");

        assert!(stage_from_source(&state, &source).is_err());
        assert!(!stale_owned.exists());
        assert_stage_cleanup_survivors(&fresh_owned, &non_owned, &symlink_home, &symlink_target);
    }

    #[test]
    fn cleans_stale_owned_home_before_invalid_auth_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let (stale_owned, fresh_owned, non_owned, symlink_home, symlink_target) =
            create_stage_cleanup_fixtures(&state);
        let source = root.path().join("invalid-auth.json");
        std::fs::write(&source, b"not valid json").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();

        assert!(stage_from_source(&state, &source).is_err());
        assert!(!stale_owned.exists());
        assert_stage_cleanup_survivors(&fresh_owned, &non_owned, &symlink_home, &symlink_target);
    }

    #[test]
    fn uses_only_the_explicit_auth_source_override() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fake-auth.json");
        let auth = br#"{"refresh_token":"explicit-fake-auth"}"#;
        std::fs::write(&source, auth).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        let previous = std::env::var_os(AUTH_SOURCE_ENV);
        std::env::set_var(AUTH_SOURCE_ENV, &source);
        let staged = stage(root.path(), None).unwrap();
        assert_eq!(
            std::fs::read(staged.path().join(".codex/auth.json")).unwrap(),
            auth
        );
        drop(staged);
        if let Some(previous) = previous {
            std::env::set_var(AUTH_SOURCE_ENV, previous);
        } else {
            std::env::remove_var(AUTH_SOURCE_ENV);
        }
    }

    #[test]
    fn rejects_invalid_json_and_explicit_cleanup_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fake-auth.json");
        std::fs::write(&source, b"[]").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(stage_from_source(&root.path().join("state"), &source).is_err());

        std::fs::write(&source, br#"{"access_token":"cleanup-sentinel"}"#).unwrap();
        let mut staged = stage_from_source(&root.path().join("state"), &source).unwrap();
        let path = staged.path().to_path_buf();
        staged.cleanup().unwrap();
        staged.cleanup().unwrap();
        assert!(!path.exists());
    }
}
