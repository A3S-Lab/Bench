use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const PACKAGE_ENV: &str = "A3S_BENCH_CODEX_PACKAGE";
pub const CACHE_METADATA_SCHEMA: &str = "a3s.bench.codex-package-cache.v1";

const MANIFEST_FILE: &str = "codex-package.json";
const PACKAGE_CACHE_DIRECTORY: &str = "codex-packages";
const PACKAGE_METADATA_DIRECTORY: &str = "codex-package-metadata";
const CODE_MODE_HOST_FILE: &str = "codex-code-mode-host";
const RIPGREP_FILE: &str = "rg";
const BWRAP_FILE: &str = "bwrap";
const OFFICIAL_ZSH_FILE: &str = "codex-resources/zsh/bin/zsh";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_PACKAGE_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;
const NORMALIZED_EXECUTABLE_MODE: u32 = 0o555;
const NORMALIZED_NONEXECUTABLE_MODE: u32 = 0o444;

/// The standalone Codex package manifest is an official Codex file.  A3S
/// cache identity is deliberately kept out of this type and out of the
/// source manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodexPackageManifest {
    pub layout_version: u32,
    pub version: String,
    #[serde(rename = "target")]
    pub target_triple: String,
    pub variant: String,
    pub entrypoint: String,
    pub resources_dir: String,
    pub path_dir: String,
}

#[derive(Debug, Clone)]
pub struct CachedCodexPackage {
    pub root: PathBuf,
    pub manifest: CodexPackageManifest,
    pub reported_version: String,
    pub(crate) artifact_set_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexPackagePaths {
    pub entrypoint: String,
    pub entrypoint_dir: String,
    pub code_mode_host: String,
    pub resources_dir: String,
    pub path_dir: String,
    pub bwrap: String,
}

impl CachedCodexPackage {
    pub fn artifact_set_digest(&self) -> &str {
        &self.artifact_set_digest
    }

    pub fn target_triple(&self) -> &str {
        &self.manifest.target_triple
    }

    pub fn container_paths(&self) -> Result<CodexPackagePaths> {
        validate_manifest(&self.manifest)?;
        package_paths(&self.manifest)
    }

    pub fn verify_for_mount(&self) -> Result<()> {
        validate_manifest(&self.manifest)?;
        anyhow::ensure!(
            self.reported_version == format!("codex-cli {}", self.manifest.version)
                && !self.reported_version.trim().is_empty()
                && !self.reported_version.chars().any(char::is_control),
            "Codex package reported version is invalid"
        );
        validate_digest(&self.artifact_set_digest)?;
        let file_metadata =
            verify_package_tree_allow_mode_drift(&self.root, &self.manifest, None, false)?;
        let artifact_set_digest = calculate_artifact_set_digest(&self.manifest, &file_metadata)?;
        anyhow::ensure!(
            artifact_set_digest == self.artifact_set_digest,
            "Codex package artifact-set digest does not match the locked package"
        );
        verify_sealed_tree(&self.root).context("Codex package cache is not sealed read-only")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheMetadata {
    schema: String,
    manifest: CodexPackageManifest,
    reported_version: String,
    files: BTreeMap<String, ArtifactFile>,
    artifact_set_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactFile {
    file_type: String,
    mode: u32,
    size: u64,
    content_sha256: String,
}

#[derive(Serialize)]
struct ArtifactIdentity<'a> {
    manifest: &'a CodexPackageManifest,
    files: &'a BTreeMap<String, ArtifactFile>,
}

pub fn prepare(state_root: &Path, task_platform: Option<&str>) -> Result<CachedCodexPackage> {
    let source = discover_source()?;
    prepare_from_path(state_root, &source, task_platform)
}

/// Prepare a package from an explicit path. This is also the test seam: it
/// never consults PATH or a user's home directory.
pub fn prepare_from_path(
    state_root: &Path,
    source: &Path,
    task_platform: Option<&str>,
) -> Result<CachedCodexPackage> {
    let source = real_directory(source).context("Codex package source is unavailable")?;
    let manifest = read_manifest(&source)?;
    validate_manifest(&manifest)?;
    validate_platform(&manifest.target_triple, task_platform)?;
    let file_metadata = verify_package_tree(&source, &manifest, None, true)
        .context("Codex package source failed verification")?;
    let artifact_set_digest = calculate_artifact_set_digest(&manifest, &file_metadata)?;
    let reported_version = format!("codex-cli {}", manifest.version);
    let metadata = CacheMetadata {
        schema: CACHE_METADATA_SCHEMA.into(),
        manifest: manifest.clone(),
        reported_version: reported_version.clone(),
        files: file_metadata.clone(),
        artifact_set_digest: artifact_set_digest.clone(),
    };
    validate_cache_metadata(&metadata)?;

    let destination = cache_path(state_root, &artifact_set_digest)?;
    let metadata_path = metadata_path(state_root, &artifact_set_digest)?;
    let cache_root = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Codex package cache has no parent"))?;
    let metadata_root = metadata_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Codex package metadata cache has no parent"))?;
    crate::state_fs::secure_directory(cache_root)?;
    crate::state_fs::secure_directory(metadata_root)?;

    if real_directory_if_present(&destination)? {
        verify_cached_root(&destination, &metadata)?;
        reseal_cached_tree(&destination)?;
        let cached = verify_cached_root_sealed(&destination, &metadata)?;
        write_metadata_if_absent(&metadata_path, &metadata)?;
        return Ok(cached);
    }
    anyhow::ensure!(
        !metadata_path.exists(),
        "Codex package cache metadata exists without its package"
    );

    let staging = crate::state_fs::create_unique_staging_directory(cache_root, "codex-package")?;
    let publish = (|| -> Result<()> {
        copy_package_tree(&source, &staging, &manifest, &file_metadata)?;
        verify_package_tree(&source, &manifest, Some(&file_metadata), true)
            .context("Codex package source changed while it was being staged")?;
        verify_package_tree(&staging, &manifest, Some(&file_metadata), false)
            .context("staged Codex package failed verification")?;
        crate::state_fs::sync_tree(&staging)?;
        reseal_cached_tree(&staging)?;
        verify_package_tree(&staging, &manifest, Some(&file_metadata), false)
            .context("sealed staged Codex package failed verification")?;
        match std::fs::rename(&staging, &destination) {
            Ok(()) => Ok(()),
            Err(_) if real_directory_if_present(&destination)? => Ok(()),
            Err(error) => Err(error).context("could not publish Codex package cache"),
        }
    })();
    if staging.exists() {
        let _ = crate::state_fs::remove_tree(&staging);
    }
    publish?;

    verify_cached_root(&destination, &metadata)?;
    reseal_cached_tree(&destination)?;
    let cached = verify_cached_root_sealed(&destination, &metadata)?;
    write_metadata_if_absent(&metadata_path, &metadata)?;
    Ok(cached)
}

pub fn load_cached(
    state_root: &Path,
    product: &crate::lock::CandidateProductLock,
    task_platform: Option<&str>,
) -> Result<CachedCodexPackage> {
    let target_triple = product.target_triple.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "legacy native Codex CandidateLock has no package target; regenerate it for containerized Codex"
        )
    })?;
    let artifact_set_digest = product.artifact_set_digest.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "legacy native Codex CandidateLock has no package digest; regenerate it for containerized Codex"
        )
    })?;
    let destination = validated_cached_root(state_root, artifact_set_digest)?;
    let metadata_path = metadata_path(state_root, artifact_set_digest)?;
    let metadata_bytes = crate::state_fs::read_regular_file(
        &metadata_path,
        "verified Codex package cache metadata",
    )
    .with_context(|| {
        format!(
            "verified Codex package {artifact_set_digest} is unavailable; regenerate the CandidateLock"
        )
    })?;
    let metadata: CacheMetadata =
        serde_json::from_slice(&metadata_bytes).context("invalid Codex package cache metadata")?;
    validate_cache_metadata(&metadata)?;
    anyhow::ensure!(
        metadata.manifest.target_triple == target_triple,
        "cached Codex package target does not match CandidateLock"
    );
    anyhow::ensure!(
        metadata.artifact_set_digest == artifact_set_digest,
        "cached Codex package digest does not match CandidateLock"
    );
    validate_platform(&metadata.manifest.target_triple, task_platform)?;
    let reported = product.version.trim();
    anyhow::ensure!(
        !reported.is_empty(),
        "locked Codex product version is empty"
    );
    anyhow::ensure!(
        reported == metadata.reported_version,
        "locked Codex product version does not match the prepared package"
    );
    verify_cached_root(&destination, &metadata)
        .context("locked Codex package is unavailable or corrupt")?;
    reseal_cached_tree(&destination)?;
    verify_cached_root_sealed(&destination, &metadata)
        .context("locked Codex package is unavailable, corrupt, or not sealed")
}

fn validated_cached_root(state_root: &Path, artifact_set_digest: &str) -> Result<PathBuf> {
    let cache_root = canonical_real_directory(
        &state_root.join(PACKAGE_CACHE_DIRECTORY),
        "Codex package cache parent",
    )?;
    let destination = canonical_real_directory(
        &cache_root.join(digest_hex(artifact_set_digest)?),
        "Codex package cache digest root",
    )?;
    anyhow::ensure!(
        destination.parent() == Some(cache_root.as_path()),
        "Codex package cache digest root is outside its validated parent"
    );
    Ok(destination)
}

pub fn cache_path(state_root: &Path, artifact_set_digest: &str) -> Result<PathBuf> {
    Ok(state_root
        .join(PACKAGE_CACHE_DIRECTORY)
        .join(digest_hex(artifact_set_digest)?))
}

pub fn metadata_path(state_root: &Path, artifact_set_digest: &str) -> Result<PathBuf> {
    Ok(state_root
        .join(PACKAGE_METADATA_DIRECTORY)
        .join(format!("{}.json", digest_hex(artifact_set_digest)?)))
}

pub(crate) fn calculate_artifact_set_digest(
    manifest: &CodexPackageManifest,
    files: &BTreeMap<String, ArtifactFile>,
) -> Result<String> {
    let bytes = serde_json::to_vec(&ArtifactIdentity { manifest, files })?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

pub fn validate_platform(target_triple: &str, task_platform: Option<&str>) -> Result<()> {
    anyhow::ensure!(
        target_triple.contains("-linux-") || target_triple.starts_with("linux-"),
        "Codex package target must be a Linux target triple"
    );
    let Some(task_platform) = task_platform else {
        return Ok(());
    };
    let task_platform = canonical_platform(task_platform)?;
    let target_arch = target_triple
        .split('-')
        .next()
        .ok_or_else(|| anyhow::anyhow!("Codex package target triple is invalid"))?;
    let target_arch = match target_arch {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        value => value,
    };
    let expected_arch = task_platform
        .split_once('/')
        .map(|(_, arch)| arch)
        .expect("canonical platform contains an architecture");
    anyhow::ensure!(
        target_arch == expected_arch,
        "Codex package target {target_triple:?} is incompatible with Task work platform {task_platform:?}"
    );
    Ok(())
}

fn discover_source() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(PACKAGE_ENV) {
        return Ok(PathBuf::from(path));
    }
    if let Some(binary) = std::env::var_os("A3S_BENCH_CODEX_BIN") {
        if let Some(path) = package_ancestor(Path::new(&binary)) {
            return Ok(path);
        }
    }
    let path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .flat_map(|directory| {
            [directory.join("codex"), directory.join("codex.cmd")]
                .into_iter()
                .filter(|candidate| candidate.is_file())
                .collect::<Vec<_>>()
        })
        .find_map(|binary| package_ancestor(&binary));
    path.ok_or_else(|| {
        anyhow::anyhow!(
            "containerized Codex requires a complete installed package; set {PACKAGE_ENV}"
        )
    })
}

fn package_ancestor(path: &Path) -> Option<PathBuf> {
    let absolute = path.canonicalize().ok()?;
    for ancestor in absolute.ancestors() {
        let manifest = ancestor.join(MANIFEST_FILE);
        if std::fs::symlink_metadata(&manifest)
            .ok()
            .is_some_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn read_manifest(root: &Path) -> Result<CodexPackageManifest> {
    let bytes = read_package_file(&root.join(MANIFEST_FILE), "Codex package manifest")?;
    serde_json::from_slice(&bytes).context("invalid official codex-package.json")
}

fn validate_manifest(manifest: &CodexPackageManifest) -> Result<()> {
    anyhow::ensure!(
        manifest.layout_version == 1,
        "unsupported Codex package layoutVersion {}",
        manifest.layout_version
    );
    anyhow::ensure!(
        !manifest.version.trim().is_empty() && !manifest.version.chars().any(char::is_control),
        "Codex package version is invalid"
    );
    anyhow::ensure!(
        !manifest.target_triple.trim().is_empty()
            && manifest
                .target_triple
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_')),
        "Codex package target triple is invalid"
    );
    validate_platform(&manifest.target_triple, None)?;
    anyhow::ensure!(
        manifest.variant == "codex",
        "Codex package variant must be \"codex\""
    );
    anyhow::ensure!(
        manifest.entrypoint == "bin/codex"
            && manifest.resources_dir == "codex-resources"
            && manifest.path_dir == "codex-path",
        "Codex package manifest does not use the official standalone layout"
    );
    validate_relative_path(&manifest.entrypoint)?;
    validate_relative_path(&manifest.resources_dir)?;
    validate_relative_path(&manifest.path_dir)?;
    anyhow::ensure!(
        manifest.entrypoint != MANIFEST_FILE
            && manifest.resources_dir != MANIFEST_FILE
            && manifest.path_dir != MANIFEST_FILE,
        "Codex package layout must not point at codex-package.json"
    );
    let required = required_files(manifest)?;
    anyhow::ensure!(
        !required.is_empty(),
        "Codex package layout has no required files"
    );
    Ok(())
}

fn required_files(manifest: &CodexPackageManifest) -> Result<BTreeSet<String>> {
    let paths = package_paths(manifest)?;
    let ripgrep = join_package_path(Some(Path::new(&paths.path_dir)), RIPGREP_FILE)?;
    let required = [paths.entrypoint, paths.code_mode_host, ripgrep, paths.bwrap]
        .into_iter()
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        required.len() == 4,
        "Codex package layout points multiple required files at one path"
    );
    Ok(required)
}

fn allowed_files(manifest: &CodexPackageManifest) -> Result<BTreeSet<String>> {
    let mut files = required_files(manifest)?;
    // Codex 0.147 ships this helper inside its resources directory.  It is
    // official, but it is not named by codex-package.json, so allow only this
    // exact path and bind it into the artifact identity when present.
    if manifest.resources_dir == "codex-resources" {
        files.insert(OFFICIAL_ZSH_FILE.into());
    }
    Ok(files)
}

fn package_paths(manifest: &CodexPackageManifest) -> Result<CodexPackagePaths> {
    let entrypoint = manifest.entrypoint.clone();
    let entrypoint_parent = Path::new(&entrypoint).parent();
    let entrypoint_dir = match entrypoint_parent {
        Some(parent) if !parent.as_os_str().is_empty() => relative_path(parent)?,
        _ => String::new(),
    };
    let code_mode_host = join_package_path(entrypoint_parent, CODE_MODE_HOST_FILE)?;
    let resources_dir = manifest.resources_dir.clone();
    let path_dir = manifest.path_dir.clone();
    let bwrap = join_package_path(Some(Path::new(&resources_dir)), BWRAP_FILE)?;
    Ok(CodexPackagePaths {
        entrypoint,
        entrypoint_dir,
        code_mode_host,
        resources_dir,
        path_dir,
        bwrap,
    })
}

fn verify_package_tree(
    root: &Path,
    manifest: &CodexPackageManifest,
    expected_files: Option<&BTreeMap<String, ArtifactFile>>,
    allow_source_convenience_symlink: bool,
) -> Result<BTreeMap<String, ArtifactFile>> {
    verify_package_tree_inner(
        root,
        manifest,
        expected_files,
        allow_source_convenience_symlink,
        false,
    )
}

fn verify_package_tree_allow_mode_drift(
    root: &Path,
    manifest: &CodexPackageManifest,
    expected_files: Option<&BTreeMap<String, ArtifactFile>>,
    allow_source_convenience_symlink: bool,
) -> Result<BTreeMap<String, ArtifactFile>> {
    verify_package_tree_inner(
        root,
        manifest,
        expected_files,
        allow_source_convenience_symlink,
        true,
    )
}

fn verify_package_tree_inner(
    root: &Path,
    manifest: &CodexPackageManifest,
    expected_files: Option<&BTreeMap<String, ArtifactFile>>,
    allow_source_convenience_symlink: bool,
    allow_mode_drift: bool,
) -> Result<BTreeMap<String, ArtifactFile>> {
    ensure_real_directory(root, "Codex package root")?;
    validate_manifest(manifest)?;
    let actual_manifest = read_manifest(root)?;
    anyhow::ensure!(
        actual_manifest == *manifest,
        "Codex package manifest changed during verification"
    );
    let manifest_size = package_file_size(&root.join(MANIFEST_FILE), "Codex package manifest")?;
    anyhow::ensure!(
        manifest_size <= MAX_MANIFEST_BYTES,
        "Codex package manifest exceeds the permitted size"
    );
    let files = collect_files(root, manifest, allow_source_convenience_symlink)?;
    let allowed = allowed_files(manifest)?;
    anyhow::ensure!(
        files.iter().all(|path| {
            relative_path(path)
                .map(|path| path == MANIFEST_FILE || allowed.contains(&path))
                .unwrap_or(false)
        }),
        "Codex package contains an extra file"
    );
    let mut total_size = manifest_size;
    let mut file_metadata = BTreeMap::new();
    let present = files
        .iter()
        .map(|path| relative_path(path))
        .collect::<Result<BTreeSet<_>>>()?;
    for relative in allowed
        .into_iter()
        .filter(|relative| present.contains(relative))
    {
        let path = root.join(&relative);
        let metadata = stream_artifact_file(&path, &mut total_size, !allow_mode_drift)
            .with_context(|| format!("could not verify Codex package file {relative:?}"))?;
        let metadata = if allow_mode_drift {
            ArtifactFile {
                mode: NORMALIZED_EXECUTABLE_MODE,
                ..metadata
            }
        } else {
            metadata
        };
        file_metadata.insert(relative, metadata);
    }
    validate_file_metadata(manifest, &file_metadata)?;
    if let Some(expected_files) = expected_files {
        anyhow::ensure!(
            &file_metadata == expected_files,
            "Codex package file metadata changed during verification"
        );
    }
    if !allow_mode_drift {
        anyhow::ensure!(
            !crate::state_fs::is_executable(&root.join(MANIFEST_FILE))?,
            "Codex package manifest must not be executable"
        );
    }
    Ok(file_metadata)
}

fn validate_file_metadata(
    manifest: &CodexPackageManifest,
    files: &BTreeMap<String, ArtifactFile>,
) -> Result<()> {
    let allowed = allowed_files(manifest)?;
    anyhow::ensure!(
        files.keys().all(|path| allowed.contains(path)),
        "Codex package contains an unapproved artifact path"
    );
    for (path, file) in files {
        validate_relative_path(path)?;
        anyhow::ensure!(
            path != MANIFEST_FILE && file.file_type == "regular",
            "Codex package artifact {path:?} is not a regular file"
        );
        anyhow::ensure!(
            file.mode == NORMALIZED_EXECUTABLE_MODE,
            "Codex package artifact {path:?} has an invalid normalized mode"
        );
        anyhow::ensure!(
            file.size <= MAX_PACKAGE_FILE_BYTES,
            "Codex package artifact {path:?} exceeds the permitted size"
        );
        validate_digest(&file.content_sha256)
            .with_context(|| format!("invalid digest for package file {path:?}"))?;
    }
    anyhow::ensure!(
        files.keys().any(|path| path == &manifest.entrypoint),
        "Codex package entrypoint is missing"
    );
    for required in required_files(manifest)? {
        anyhow::ensure!(
            files.contains_key(&required),
            "Codex package is missing required file {required:?}"
        );
    }
    Ok(())
}

fn copy_package_tree(
    source: &Path,
    destination: &Path,
    manifest: &CodexPackageManifest,
    files: &BTreeMap<String, ArtifactFile>,
) -> Result<()> {
    let manifest_bytes = serde_json::to_vec(manifest)?;
    crate::state_fs::secure_atomic_write(&destination.join(MANIFEST_FILE), &manifest_bytes)?;
    for relative in files.keys() {
        let source_file = source.join(relative);
        let destination_file = destination.join(relative);
        if let Some(parent) = destination_file.parent() {
            crate::state_fs::secure_directory(parent)?;
        }
        copy_package_file(
            &source_file,
            &destination_file,
            files
                .get(relative)
                .ok_or_else(|| anyhow::anyhow!("missing package metadata for {relative:?}"))?,
        )
        .with_context(|| format!("could not stage Codex package file {relative:?}"))?;
    }
    anyhow::ensure!(
        read_manifest(destination)? == *manifest,
        "Codex package manifest changed while it was being copied"
    );
    Ok(())
}

fn verify_cached_root(root: &Path, metadata: &CacheMetadata) -> Result<CachedCodexPackage> {
    validate_cache_metadata(metadata)?;
    let file_metadata = verify_package_tree_allow_mode_drift(
        root,
        &metadata.manifest,
        Some(&metadata.files),
        false,
    )?;
    let artifact_set_digest = calculate_artifact_set_digest(&metadata.manifest, &file_metadata)?;
    anyhow::ensure!(
        artifact_set_digest == metadata.artifact_set_digest,
        "Codex package artifact-set digest does not match cache metadata"
    );
    Ok(CachedCodexPackage {
        root: root.to_path_buf(),
        manifest: metadata.manifest.clone(),
        reported_version: metadata.reported_version.clone(),
        artifact_set_digest,
    })
}

fn verify_cached_root_sealed(root: &Path, metadata: &CacheMetadata) -> Result<CachedCodexPackage> {
    let cached = verify_cached_root(root, metadata)?;
    verify_sealed_tree(root)?;
    Ok(cached)
}

fn reseal_cached_tree(root: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        // Apply the final normalized modes directly. A two-pass 0500 -> 0555
        // transition lets concurrent preparers observe a transient mode and
        // reject an otherwise valid shared cache.
        set_sealed_unix_modes(root)?;
    }
    #[cfg(not(unix))]
    crate::state_fs::seal_tree_read_only(root)?;
    Ok(())
}

#[cfg(unix)]
fn set_sealed_unix_modes(root: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fn visit(root: &Path, path: &Path) -> Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Codex package sealed tree contains a non-directory"
        );
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let entry_path = entry.path();
            let kind = entry.file_type()?;
            anyhow::ensure!(
                !kind.is_symlink(),
                "Codex package sealed tree contains a symlink"
            );
            if kind.is_dir() {
                visit(root, &entry_path)?;
            } else if kind.is_file() {
                let relative = relative_path(entry_path.strip_prefix(root)?)?;
                let mode = if relative == MANIFEST_FILE {
                    NORMALIZED_NONEXECUTABLE_MODE
                } else {
                    NORMALIZED_EXECUTABLE_MODE
                };
                std::fs::set_permissions(&entry_path, std::fs::Permissions::from_mode(mode))?;
            } else {
                anyhow::bail!("Codex package sealed tree contains a special file");
            }
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555))?;
        Ok(())
    }

    visit(root, root)
}

#[cfg(unix)]
fn verify_sealed_tree(root: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fn visit(root: &Path, path: &Path) -> Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Codex package sealed tree root must be a real directory"
        );
        anyhow::ensure!(
            metadata.permissions().mode() & 0o7777 == 0o555,
            "Codex package directory is not exactly mode 0555"
        );
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let entry_path = entry.path();
            let kind = entry.file_type()?;
            anyhow::ensure!(
                !kind.is_symlink(),
                "Codex package sealed tree contains a symlink"
            );
            if kind.is_dir() {
                visit(root, &entry_path)?;
            } else if kind.is_file() {
                let metadata = std::fs::symlink_metadata(&entry_path)?;
                let relative = relative_path(entry_path.strip_prefix(root)?)?;
                let expected = if relative == MANIFEST_FILE {
                    NORMALIZED_NONEXECUTABLE_MODE
                } else {
                    NORMALIZED_EXECUTABLE_MODE
                };
                anyhow::ensure!(
                    metadata.permissions().mode() & 0o7777 == expected,
                    "Codex package file {relative:?} is not sealed with its required mode"
                );
            } else {
                anyhow::bail!("Codex package sealed tree contains a special file");
            }
        }
        Ok(())
    }

    visit(root, root)
}

#[cfg(not(unix))]
fn verify_sealed_tree(root: &Path) -> Result<()> {
    fn visit(path: &Path) -> Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Codex package sealed tree root must be a real directory"
        );
        anyhow::ensure!(
            metadata.permissions().readonly(),
            "Codex package directory is not read-only"
        );
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let entry_path = entry.path();
            let kind = entry.file_type()?;
            anyhow::ensure!(
                !kind.is_symlink(),
                "Codex package sealed tree contains a symlink"
            );
            if kind.is_dir() {
                visit(&entry_path)?;
            } else if kind.is_file() {
                anyhow::ensure!(
                    std::fs::symlink_metadata(&entry_path)?
                        .permissions()
                        .readonly(),
                    "Codex package file is not read-only"
                );
            } else {
                anyhow::bail!("Codex package sealed tree contains a special file");
            }
        }
        Ok(())
    }

    visit(root)
}

fn validate_cache_metadata(metadata: &CacheMetadata) -> Result<()> {
    anyhow::ensure!(
        metadata.schema == CACHE_METADATA_SCHEMA,
        "unsupported Codex package cache metadata schema {:?}",
        metadata.schema
    );
    validate_manifest(&metadata.manifest)?;
    anyhow::ensure!(
        metadata.reported_version == format!("codex-cli {}", metadata.manifest.version)
            && !metadata.reported_version.trim().is_empty()
            && !metadata.reported_version.chars().any(char::is_control),
        "Codex package reported version is invalid"
    );
    validate_file_metadata(&metadata.manifest, &metadata.files)?;
    anyhow::ensure!(
        calculate_artifact_set_digest(&metadata.manifest, &metadata.files)?
            == metadata.artifact_set_digest,
        "Codex package cache artifact-set digest does not match its file hashes"
    );
    validate_digest(&metadata.artifact_set_digest)?;
    Ok(())
}

fn collect_files(
    root: &Path,
    manifest: &CodexPackageManifest,
    allow_source_convenience_symlink: bool,
) -> Result<Vec<PathBuf>> {
    fn visit(
        root: &Path,
        directory: &Path,
        manifest: &CodexPackageManifest,
        allow_source_convenience_symlink: bool,
        output: &mut Vec<PathBuf>,
    ) -> Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(root)?.to_path_buf();
            let relative_name = relative_path(&relative)?;
            validate_relative_path(&relative_name)?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                anyhow::ensure!(
                    allow_source_convenience_symlink
                        && relative_name == "codex"
                        && manifest.entrypoint == "bin/codex",
                    "Codex package contains an unapproved symlink"
                );
                validate_source_convenience_symlink(&path, manifest)?;
            } else if kind.is_dir() {
                visit(
                    root,
                    &path,
                    manifest,
                    allow_source_convenience_symlink,
                    output,
                )?;
            } else if kind.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    anyhow::ensure!(
                        entry.metadata()?.nlink() == 1,
                        "Codex package contains a hard link"
                    );
                }
                output.push(relative);
            } else {
                anyhow::bail!("Codex package contains a special file");
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(
        root,
        root,
        manifest,
        allow_source_convenience_symlink,
        &mut files,
    )?;
    files.sort();
    let allowed = allowed_files(manifest)?;
    for path in &files {
        let relative = relative_path(path)?;
        anyhow::ensure!(
            relative == MANIFEST_FILE || allowed.contains(&relative),
            "Codex package contains an extra file {relative:?}"
        );
    }
    Ok(files)
}

fn validate_source_convenience_symlink(path: &Path, manifest: &CodexPackageManifest) -> Result<()> {
    let target = std::fs::read_link(path).with_context(|| {
        format!(
            "could not read Codex package convenience symlink {}",
            path.display()
        )
    })?;
    let target = relative_path(&target)?;
    validate_relative_path(&target)
        .context("Codex package convenience symlink target is unsafe")?;
    anyhow::ensure!(
        target == "bin/codex" && manifest.entrypoint == "bin/codex",
        "Codex package convenience symlink must be codex -> bin/codex"
    );
    Ok(())
}

fn relative_path(path: &Path) -> Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Codex package path is not UTF-8"))?
        .replace(std::path::MAIN_SEPARATOR, "/");
    Ok(value)
}

fn validate_relative_path(path: &str) -> Result<()> {
    anyhow::ensure!(
        !path.is_empty()
            && path.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-')
            })
            && !Path::new(path).is_absolute()
            && path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "Codex package path {path:?} is unsafe"
    );
    Ok(())
}

fn join_package_path(parent: Option<&Path>, child: &str) -> Result<String> {
    let path = parent
        .map(|parent| parent.join(child))
        .unwrap_or_else(|| PathBuf::from(child));
    let value = relative_path(&path)?;
    validate_relative_path(&value)?;
    Ok(value)
}

fn validate_digest(value: &str) -> Result<()> {
    let hex = value
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow::anyhow!("digest must use sha256"))?;
    anyhow::ensure!(
        hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "digest must contain exactly 64 hexadecimal characters"
    );
    Ok(())
}

fn digest_hex(value: &str) -> Result<&str> {
    let hex = value
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow::anyhow!("Codex artifact digest must use sha256"))?;
    anyhow::ensure!(
        hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid Codex artifact-set digest"
    );
    Ok(hex)
}

fn read_package_file(path: &Path, kind: &str) -> Result<Vec<u8>> {
    let metadata = checked_path_metadata(path, kind)?;
    anyhow::ensure!(
        metadata.len() <= MAX_MANIFEST_BYTES,
        "{kind} exceeds the permitted size"
    );
    let mut file = open_regular_file(path, &metadata, kind)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let final_metadata = file.metadata()?;
    ensure_same_file(&metadata, &final_metadata, kind)?;
    anyhow::ensure!(
        bytes.len() as u64 == final_metadata.len() && bytes.len() as u64 <= MAX_MANIFEST_BYTES,
        "{kind} changed while it was being read"
    );
    Ok(bytes)
}

fn package_file_size(path: &Path, kind: &str) -> Result<u64> {
    Ok(checked_path_metadata(path, kind)?.len())
}

fn checked_path_metadata(path: &Path, kind: &str) -> Result<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("{kind} is unavailable at {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "{kind} must be a real regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(metadata.nlink() == 1, "{kind} must not be a hard link");
    }
    Ok(metadata)
}

fn open_regular_file(path: &Path, metadata: &std::fs::Metadata, kind: &str) -> Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(no_follow_flag());
    }
    let file = options
        .open(path)
        .with_context(|| format!("{kind} could not be opened safely"))?;
    let opened = file.metadata()?;
    ensure_same_file(metadata, &opened, kind)?;
    Ok(file)
}

fn ensure_same_file(
    expected: &std::fs::Metadata,
    actual: &std::fs::Metadata,
    kind: &str,
) -> Result<()> {
    anyhow::ensure!(
        actual.is_file() && actual.len() == expected.len(),
        "{kind} changed while it was being opened"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            actual.dev() == expected.dev() && actual.ino() == expected.ino(),
            "{kind} changed while it was being opened"
        );
        anyhow::ensure!(actual.nlink() == 1, "{kind} must not be a hard link");
    }
    Ok(())
}

fn normalized_mode(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 != 0 {
            NORMALIZED_EXECUTABLE_MODE
        } else {
            NORMALIZED_NONEXECUTABLE_MODE
        }
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        NORMALIZED_NONEXECUTABLE_MODE
    }
}

fn stream_artifact_file(
    path: &Path,
    total_size: &mut u64,
    require_executable: bool,
) -> Result<ArtifactFile> {
    let metadata = checked_path_metadata(path, "Codex package artifact")?;
    anyhow::ensure!(
        metadata.len() <= MAX_PACKAGE_FILE_BYTES,
        "Codex package artifact exceeds the permitted size"
    );
    let mode = normalized_mode(&metadata);
    if require_executable {
        anyhow::ensure!(
            mode == NORMALIZED_EXECUTABLE_MODE,
            "Codex package required file is not executable"
        );
    }
    let mut file = open_regular_file(path, &metadata, "Codex package artifact")?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| anyhow::anyhow!("Codex package artifact size overflowed"))?;
        anyhow::ensure!(
            size <= MAX_PACKAGE_FILE_BYTES,
            "Codex package artifact exceeds the permitted size"
        );
        hasher.update(&buffer[..count]);
    }
    let final_metadata = file.metadata()?;
    ensure_same_file(&metadata, &final_metadata, "Codex package artifact")?;
    anyhow::ensure!(
        size == final_metadata.len(),
        "Codex package artifact changed while it was being read"
    );
    *total_size = total_size
        .checked_add(size)
        .ok_or_else(|| anyhow::anyhow!("Codex package size overflowed"))?;
    anyhow::ensure!(
        *total_size <= MAX_PACKAGE_BYTES,
        "Codex package exceeds the permitted size"
    );
    Ok(ArtifactFile {
        file_type: "regular".into(),
        mode: if require_executable {
            mode
        } else {
            NORMALIZED_EXECUTABLE_MODE
        },
        size,
        content_sha256: format!("sha256:{:x}", hasher.finalize()),
    })
}

fn copy_package_file(source: &Path, destination: &Path, expected: &ArtifactFile) -> Result<()> {
    let metadata = checked_path_metadata(source, "Codex package source file")?;
    anyhow::ensure!(
        metadata.len() == expected.size && normalized_mode(&metadata) == expected.mode,
        "Codex package source file metadata does not match its verified identity"
    );
    let mut source_file = open_regular_file(source, &metadata, "Codex package source file")?;
    #[cfg(not(unix))]
    {
        let _ = expected;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o700);
    }
    let mut destination_file = options.open(destination)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let count = source_file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| anyhow::anyhow!("Codex package file size overflowed"))?;
        anyhow::ensure!(
            size <= MAX_PACKAGE_FILE_BYTES,
            "Codex package file is oversized"
        );
        hasher.update(&buffer[..count]);
        destination_file.write_all(&buffer[..count])?;
    }
    let final_source_metadata = source_file.metadata()?;
    ensure_same_file(
        &metadata,
        &final_source_metadata,
        "Codex package source file",
    )?;
    anyhow::ensure!(
        size == expected.size
            && format!("sha256:{:x}", hasher.finalize()) == expected.content_sha256,
        "Codex package source file content does not match its verified identity"
    );
    destination_file.sync_all()?;
    drop(destination_file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(expected.mode))?;
    }
    Ok(())
}

fn write_metadata_if_absent(path: &Path, metadata: &CacheMetadata) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(metadata)?;
    if metadata_matches(path, metadata)? {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Codex package metadata path has no parent"))?;
    crate::state_fs::secure_directory(parent)?;
    for attempt in 0..32 {
        let temporary = path.with_file_name(format!(
            ".{}-tmp-{}-{attempt}",
            path.file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| anyhow::anyhow!("Codex package metadata path is not UTF-8"))?,
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };
        let result = (|| -> Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            match std::fs::hard_link(&temporary, path) {
                Ok(()) => {
                    std::fs::remove_file(&temporary)?;
                    crate::state_fs::set_owner_only_file(path, true)?;
                    File::open(parent)?.sync_all()?;
                    Ok(())
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let _ = std::fs::remove_file(&temporary);
                    ensure_metadata_matches(path, metadata)
                }
                Err(error) => Err(error.into()),
            }
        })();
        if temporary.exists() {
            let _ = std::fs::remove_file(&temporary);
        }
        return result;
    }
    anyhow::bail!("could not allocate an atomic Codex package metadata file")
}

fn metadata_matches(path: &Path, metadata: &CacheMetadata) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            ensure_metadata_matches(path, metadata)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn ensure_metadata_matches(path: &Path, metadata: &CacheMetadata) -> Result<()> {
    let existing = crate::state_fs::read_regular_file(path, "Codex package cache metadata")?;
    let existing: CacheMetadata =
        serde_json::from_slice(&existing).context("invalid Codex package cache metadata")?;
    anyhow::ensure!(
        existing == *metadata,
        "Codex package cache metadata does not match the verified package"
    );
    Ok(())
}

fn ensure_real_directory(path: &Path, kind: &str) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "{kind} must be a real directory"
    );
    Ok(())
}

fn canonical_real_directory(path: &Path, kind: &str) -> Result<PathBuf> {
    ensure_real_directory(path, kind)?;
    path.canonicalize()
        .with_context(|| format!("could not canonicalize {kind}"))
}

fn real_directory(path: &Path) -> Result<PathBuf> {
    canonical_real_directory(path, "Codex package source")
}

fn real_directory_if_present(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Codex package cache path is not a real directory"
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn canonical_platform(value: &str) -> Result<String> {
    let mut parts = value.split('/');
    let os = parts.next().unwrap_or_default();
    let architecture = parts.next().unwrap_or_default();
    anyhow::ensure!(
        os == "linux" && !architecture.is_empty() && parts.next().is_none(),
        "Task work platform must use linux/architecture"
    );
    let architecture = match architecture {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        value => value,
    };
    Ok(format!("{os}/{architecture}"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    fn fake_manifest() -> CodexPackageManifest {
        CodexPackageManifest {
            layout_version: 1,
            version: "1.2.3".into(),
            target_triple: "x86_64-unknown-linux-musl".into(),
            variant: "codex".into(),
            entrypoint: "bin/codex".into(),
            resources_dir: "codex-resources".into(),
            path_dir: "codex-path".into(),
        }
    }

    fn write_fake_package(root: &Path) {
        let manifest = fake_manifest();
        for relative in required_files(&manifest).unwrap() {
            let path = root.join(&relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let contents = if relative == manifest.entrypoint {
                "#!/bin/sh\nprintf 'codex-cli 1.2.3\\n'\n"
            } else {
                "#!/bin/sh\nexit 0\n"
            };
            std::fs::write(&path, contents).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(
            root.join(MANIFEST_FILE),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn locked_product(package: &CachedCodexPackage) -> crate::lock::CandidateProductLock {
        crate::lock::CandidateProductLock {
            name: "codex-cli".into(),
            version: package.reported_version.clone(),
            target_triple: Some(package.manifest.target_triple.clone()),
            artifact_set_digest: Some(package.artifact_set_digest.clone()),
        }
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn parses_official_manifest_without_a3s_fields() {
        let manifest: CodexPackageManifest = serde_json::from_str(
            r#"{
                "layoutVersion": 1,
                "version": "0.147.0",
                "target": "x86_64-unknown-linux-musl",
                "variant": "codex",
                "entrypoint": "bin/codex",
                "resourcesDir": "codex-resources",
                "pathDir": "codex-path"
            }"#,
        )
        .unwrap();
        assert_eq!(manifest.layout_version, 1);
        assert_eq!(manifest.target_triple, "x86_64-unknown-linux-musl");
        assert!(serde_json::from_str::<CodexPackageManifest>(
            r#"{
                "layoutVersion": 1,
                "version": "0.147.0",
                "target": "x86_64-unknown-linux-musl",
                "variant": "codex",
                "entrypoint": "bin/codex",
                "resourcesDir": "codex-resources",
                "pathDir": "codex-path",
                "schema": "a3s.bench.codex-package.v1"
            }"#
        )
        .is_err());
    }

    #[test]
    fn prepares_and_reuses_complete_package_with_separate_metadata() {
        let source = tempfile::tempdir().unwrap();
        write_fake_package(source.path());
        let original_manifest: CodexPackageManifest =
            serde_json::from_slice(&std::fs::read(source.path().join(MANIFEST_FILE)).unwrap())
                .unwrap();
        let state = tempfile::tempdir().unwrap();
        let first = prepare_from_path(state.path(), source.path(), Some("linux/amd64")).unwrap();
        let second = prepare_from_path(state.path(), source.path(), Some("linux/amd64")).unwrap();
        assert_eq!(first.root, second.root);
        assert_eq!(first.reported_version, "codex-cli 1.2.3");
        let cached_manifest: CodexPackageManifest =
            serde_json::from_slice(&std::fs::read(first.root.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(cached_manifest, original_manifest);
        assert!(!first.root.join("codex-package-cache.json").exists());
        assert!(metadata_path(state.path(), first.artifact_set_digest())
            .unwrap()
            .is_file());
        assert_eq!(
            std::fs::metadata(first.root.join("bin/codex"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
    }

    #[cfg(unix)]
    #[test]
    fn load_cached_reseals_writable_cache_drift() {
        let source = tempfile::tempdir().unwrap();
        write_fake_package(source.path());
        let state = tempfile::tempdir().unwrap();
        let prepared = prepare_from_path(state.path(), source.path(), None).unwrap();
        let product = locked_product(&prepared);

        std::fs::set_permissions(&prepared.root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(
            prepared.root.join("bin"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::set_permissions(
            prepared.root.join("bin/codex"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::fs::set_permissions(
            prepared.root.join(MANIFEST_FILE),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();

        let loaded = load_cached(state.path(), &product, None).unwrap();
        assert_eq!(loaded.root, prepared.root.canonicalize().unwrap());
        assert_eq!(mode(&loaded.root), 0o555);
        assert_eq!(mode(&loaded.root.join("bin")), 0o555);
        assert_eq!(mode(&loaded.root.join("bin/codex")), 0o555);
        assert_eq!(mode(&loaded.root.join(MANIFEST_FILE)), 0o444);
    }

    #[cfg(unix)]
    #[test]
    fn load_cached_rejects_symlinked_or_non_directory_cache_parent_and_root() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().unwrap();
        write_fake_package(source.path());
        let state = tempfile::tempdir().unwrap();
        let prepared = prepare_from_path(state.path(), source.path(), None).unwrap();
        let product = locked_product(&prepared);
        let cache_parent = state.path().join(PACKAGE_CACHE_DIRECTORY);
        let digest_root = prepared.root.clone();

        let real_digest_root = cache_parent.join("real-digest-root");
        std::fs::rename(&digest_root, &real_digest_root).unwrap();
        symlink(&real_digest_root, &digest_root).unwrap();
        assert!(load_cached(state.path(), &product, None).is_err());
        std::fs::remove_file(&digest_root).unwrap();
        std::fs::rename(&real_digest_root, &digest_root).unwrap();

        let digest_backup = cache_parent.join("digest-backup");
        std::fs::rename(&digest_root, &digest_backup).unwrap();
        std::fs::write(&digest_root, b"not a directory").unwrap();
        assert!(load_cached(state.path(), &product, None).is_err());
        std::fs::remove_file(&digest_root).unwrap();
        std::fs::rename(&digest_backup, &digest_root).unwrap();

        let real_cache_parent = state.path().join("real-cache-parent");
        std::fs::rename(&cache_parent, &real_cache_parent).unwrap();
        symlink(&real_cache_parent, &cache_parent).unwrap();
        assert!(load_cached(state.path(), &product, None).is_err());
        std::fs::remove_file(&cache_parent).unwrap();
        std::fs::rename(&real_cache_parent, &cache_parent).unwrap();

        let parent_backup = state.path().join("cache-parent-backup");
        std::fs::rename(&cache_parent, &parent_backup).unwrap();
        std::fs::write(&cache_parent, b"not a directory").unwrap();
        assert!(load_cached(state.path(), &product, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn verify_for_mount_rejects_post_load_content_and_mode_tampering() {
        let source = tempfile::tempdir().unwrap();
        write_fake_package(source.path());
        let state = tempfile::tempdir().unwrap();
        let prepared = prepare_from_path(state.path(), source.path(), None).unwrap();
        let product = locked_product(&prepared);
        let loaded = load_cached(state.path(), &product, None).unwrap();

        std::fs::set_permissions(&loaded.root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(loaded.verify_for_mount().is_err());
        std::fs::set_permissions(&loaded.root, std::fs::Permissions::from_mode(0o500)).unwrap();

        let manifest = loaded.root.join(MANIFEST_FILE);
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(loaded.verify_for_mount().is_err());
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o400)).unwrap();

        let artifact = loaded.root.join("bin/codex");
        std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(loaded.verify_for_mount().is_err());
        std::fs::write(&artifact, b"tampered").unwrap();
        assert!(loaded.verify_for_mount().is_err());
    }

    #[test]
    fn equivalent_manifest_formatting_reuses_artifact_cache() {
        let formatted_source = tempfile::tempdir().unwrap();
        write_fake_package(formatted_source.path());

        let reordered_source = tempfile::tempdir().unwrap();
        write_fake_package(reordered_source.path());
        std::fs::write(
            reordered_source.path().join(MANIFEST_FILE),
            r#"{"pathDir":"codex-path","resourcesDir":"codex-resources","entrypoint":"bin/codex","variant":"codex","target":"x86_64-unknown-linux-musl","version":"1.2.3","layoutVersion":1}"#,
        )
        .unwrap();

        let state = tempfile::tempdir().unwrap();
        let formatted = prepare_from_path(state.path(), formatted_source.path(), None).unwrap();
        let reordered = prepare_from_path(state.path(), reordered_source.path(), None).unwrap();

        assert_eq!(
            formatted.artifact_set_digest(),
            reordered.artifact_set_digest()
        );
        assert_eq!(formatted.root, reordered.root);
    }

    #[test]
    fn accepts_official_root_convenience_symlink_without_caching_or_identity() {
        let linked_source = tempfile::tempdir().unwrap();
        write_fake_package(linked_source.path());
        std::os::unix::fs::symlink("bin/codex", linked_source.path().join("codex")).unwrap();

        let plain_source = tempfile::tempdir().unwrap();
        write_fake_package(plain_source.path());
        let state = tempfile::tempdir().unwrap();

        let linked = prepare_from_path(state.path(), linked_source.path(), None).unwrap();
        let plain = prepare_from_path(state.path(), plain_source.path(), None).unwrap();

        assert_eq!(linked.artifact_set_digest(), plain.artifact_set_digest());
        let error = std::fs::symlink_metadata(linked.root.join("codex")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn rejects_malicious_source_symlinks() {
        for (name, target) in [
            ("codex", "/bin/codex"),
            ("codex", "../bin/codex"),
            ("codex", "bin/../bin/codex"),
            ("codex", "bin/other"),
            ("other", "bin/codex"),
            ("bin/codex-link", "../bin/codex"),
        ] {
            let source = tempfile::tempdir().unwrap();
            write_fake_package(source.path());
            std::os::unix::fs::symlink(target, source.path().join(name)).unwrap();

            assert!(
                verify_package_tree(source.path(), &fake_manifest(), None, true).is_err(),
                "accepted symlink {name:?} -> {target:?}"
            );
        }
    }

    #[test]
    fn rejects_symlink_in_cached_package() {
        let cache = tempfile::tempdir().unwrap();
        write_fake_package(cache.path());
        let manifest = fake_manifest();
        let file_hashes = verify_package_tree(cache.path(), &manifest, None, false).unwrap();
        std::os::unix::fs::symlink("bin/codex", cache.path().join("codex")).unwrap();

        assert!(verify_package_tree(cache.path(), &manifest, Some(&file_hashes), false).is_err());
    }

    #[test]
    fn rejects_tampered_cached_artifact_and_invalid_layout() {
        let source = tempfile::tempdir().unwrap();
        write_fake_package(source.path());
        let state = tempfile::tempdir().unwrap();
        let first = prepare_from_path(state.path(), source.path(), None).unwrap();
        let executable = first.root.join("bin/codex");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&executable, "tampered").unwrap();
        assert!(prepare_from_path(state.path(), source.path(), None).is_err());

        let manifest_path = source.path().join(MANIFEST_FILE);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["resourcesDir"] = serde_json::Value::String("../outside".into());
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(prepare_from_path(state.path(), source.path(), None).is_err());
    }

    #[test]
    fn rejects_platform_mismatch_and_missing_layout_artifacts() {
        let source = tempfile::tempdir().unwrap();
        write_fake_package(source.path());
        let manifest_path = source.path().join(MANIFEST_FILE);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["target"] = serde_json::Value::String("aarch64-unknown-linux-musl".into());
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let state = tempfile::tempdir().unwrap();
        assert!(prepare_from_path(state.path(), source.path(), Some("linux/amd64")).is_err());

        manifest["target"] = serde_json::Value::String("x86_64-unknown-linux-musl".into());
        let bwrap = package_paths(&fake_manifest()).unwrap().bwrap;
        std::fs::remove_file(source.path().join(bwrap)).unwrap();
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(prepare_from_path(state.path(), source.path(), None).is_err());
    }

    #[test]
    fn concurrent_preparation_converges() {
        let source = Arc::new(tempfile::tempdir().unwrap());
        write_fake_package(source.path());
        let state = Arc::new(tempfile::tempdir().unwrap());
        let mut threads = Vec::new();
        for _ in 0..2 {
            let source = Arc::clone(&source);
            let state = Arc::clone(&state);
            threads.push(std::thread::spawn(move || {
                prepare_from_path(state.path(), source.path(), None)
                    .unwrap()
                    .root
            }));
        }
        let paths = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(paths[0], paths[1]);
    }
}
