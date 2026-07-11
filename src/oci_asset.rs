use crate::asset::{self, LocalAgentAsset};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn resolve(reference: &str, state_root: &Path) -> Result<LocalAgentAsset> {
    anyhow::ensure!(!reference.is_empty(), "OCI Agent Asset reference is empty");
    match resolve_docker_image(reference, state_root) {
        Ok(asset) => Ok(asset),
        Err(docker_error) => resolve_oras_with("oras", reference, state_root).with_context(|| {
            format!(
                "OCI reference is neither a Docker-compatible Agent image nor a pullable ORAS Agent artifact; Docker resolution failed: {docker_error:#}"
            )
        }),
    }
}

fn resolve_docker_image(reference: &str, state_root: &Path) -> Result<LocalAgentAsset> {
    let image_id = match docker_output(&["image", "inspect", "--format", "{{.Id}}", reference]) {
        Ok(value) => value,
        Err(_) => {
            let output = Command::new("docker")
                .args(["pull", reference])
                .output()
                .context("could not start Docker OCI pull")?;
            anyhow::ensure!(
                output.status.success(),
                "could not pull Docker-compatible OCI Agent Asset {reference:?}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            docker_output(&["image", "inspect", "--format", "{{.Id}}", reference])?
        }
    };
    validate_digest(&image_id)?;
    if let Some(asset) = load_cache(state_root, &image_id)? {
        return Ok(asset);
    }
    let staging = staging(state_root, "asset-image")?;
    crate::state_fs::create_secure_directory_exclusive(&staging.join(".a3s"))?;
    let container = match docker_output(&["create", reference, "/run.sh"]) {
        Ok(container) => container,
        Err(error) => return cleanup_error(&staging, error),
    };
    let extraction = (|| -> Result<()> {
        docker_copy(
            &container,
            "/.a3s/asset.acl",
            &staging.join(".a3s/asset.acl"),
        )?;
        let entrypoint = asset::load_manifest_entrypoint(&staging.join(".a3s/asset.acl"))?;
        let file = safe_entrypoint_file(&entrypoint)?;
        docker_copy(&container, &format!("/{file}"), &staging.join(file))?;
        let _ = docker_copy(&container, "/agent.md", &staging.join("agent.md"));
        Ok(())
    })();
    let _ = Command::new("docker")
        .args(["rm", "-f", &container])
        .output();
    if let Err(error) = extraction {
        return cleanup_error(&staging, error);
    }
    publish_cache(state_root, staging, &image_id)
}

fn resolve_oras_with(program: &str, reference: &str, state_root: &Path) -> Result<LocalAgentAsset> {
    let digest = command_output(program, &["resolve", reference])?;
    validate_digest(&digest)?;
    if let Some(asset) = load_cache(state_root, &digest)? {
        return Ok(asset);
    }
    let staging = staging(state_root, "asset-oras")?;
    let output = Command::new(program)
        .args(["pull", "--output"])
        .arg(&staging)
        .arg(reference)
        .output()
        .with_context(|| format!("could not run {program} pull"))?;
    if !output.status.success() {
        let error = anyhow::anyhow!(
            "ORAS pull failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return cleanup_error(&staging, error);
    }
    if let Err(error) = crate::state_fs::validate_tree(&staging) {
        return cleanup_error(&staging, error);
    }
    let validation = (|| -> Result<()> {
        let manifest = staging.join(".a3s/asset.acl");
        let entrypoint = asset::load_manifest_entrypoint(&manifest)
            .context("ORAS artifact is not an A3S Agent Asset")?;
        let file = safe_entrypoint_file(&entrypoint)?;
        anyhow::ensure!(
            staging.join(file).is_file(),
            "ORAS Agent Asset entrypoint is missing: {file}"
        );
        Ok(())
    })();
    if let Err(error) = validation {
        return cleanup_error(&staging, error);
    }
    publish_cache(state_root, staging, &digest)
}

fn publish_cache(state_root: &Path, staging: PathBuf, digest: &str) -> Result<LocalAgentAsset> {
    crate::state_fs::secure_atomic_write(
        &staging.join(".complete"),
        format!("{digest}\n").as_bytes(),
    )?;
    if let Err(error) = crate::state_fs::sync_tree(&staging) {
        return cleanup_error(&staging, error);
    }
    let cache = cache_path(state_root, digest)?;
    match std::fs::rename(&staging, &cache) {
        Ok(()) => {}
        Err(_) if valid_cache(&cache, digest)? => std::fs::remove_dir_all(&staging)?,
        Err(error) => return cleanup_error(&staging, error.into()),
    }
    #[cfg(unix)]
    std::fs::File::open(state_root.join("assets"))?.sync_all()?;
    asset::load_directory(&cache, digest.to_owned())
}

fn load_cache(state_root: &Path, digest: &str) -> Result<Option<LocalAgentAsset>> {
    let cache = cache_path(state_root, digest)?;
    if valid_cache(&cache, digest)? {
        return Ok(Some(asset::load_directory(&cache, digest.to_owned())?));
    }
    if real_directory(&cache)? {
        std::fs::remove_dir_all(cache)?;
    }
    Ok(None)
}

fn valid_cache(path: &Path, digest: &str) -> Result<bool> {
    if !real_directory(path)? {
        return Ok(false);
    }
    crate::state_fs::validate_tree(path)?;
    let marker = path.join(".complete");
    let Some(bytes) =
        crate::state_fs::read_optional_regular_file(&marker, "OCI Asset cache marker")?
    else {
        return Ok(false);
    };
    Ok(std::str::from_utf8(&bytes)?.trim() == digest)
}

fn staging(state_root: &Path, purpose: &str) -> Result<PathBuf> {
    let assets = state_root.join("assets");
    crate::state_fs::create_unique_staging_directory(&assets, purpose)
}

fn cache_path(state_root: &Path, digest: &str) -> Result<PathBuf> {
    validate_digest(digest)?;
    Ok(state_root
        .join("assets")
        .join(digest.trim_start_matches("sha256:")))
}

fn validate_digest(digest: &str) -> Result<()> {
    let value = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow::anyhow!("OCI resolver returned a non-sha256 digest"))?;
    anyhow::ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "OCI resolver returned an invalid sha256 digest"
    );
    Ok(())
}

fn safe_entrypoint_file(entrypoint: &str) -> Result<&str> {
    let file = entrypoint.split(':').next().unwrap_or(entrypoint);
    anyhow::ensure!(
        !file.is_empty() && !file.starts_with('/') && !file.contains(".."),
        "unsafe OCI Agent entrypoint"
    );
    Ok(file)
}

fn real_directory(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "OCI Agent cache path is not a real directory: {}",
                path.display()
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn cleanup_error<T>(staging: &Path, error: anyhow::Error) -> Result<T> {
    let _ = std::fs::remove_dir_all(staging);
    Err(error)
}

fn command_output(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("could not run {program}"))?;
    anyhow::ensure!(
        output.status.success(),
        "{program} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn docker_output(args: &[&str]) -> Result<String> {
    command_output("docker", args)
}

fn docker_copy(container: &str, source: &str, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        crate::state_fs::secure_directory(parent)?;
    }
    let output = Command::new("docker")
        .arg("cp")
        .arg(format!("{container}:{source}"))
        .arg(destination)
        .output()
        .context("could not start Docker OCI extraction")?;
    anyhow::ensure!(
        output.status.success(),
        "OCI Agent Asset is missing {source}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_only_complete_sha256_digests() {
        assert!(validate_digest(&format!("sha256:{}", "a".repeat(64))).is_ok());
        for value in ["sha256:abc", "sha512:abc", "sha256:not-hex"] {
            assert!(validate_digest(value).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn generic_oras_artifact_resolves_without_a_docker_image() {
        use std::os::unix::fs::PermissionsExt;

        let state = tempfile::tempdir().unwrap();
        let tool = state.path().join("fake-oras");
        std::fs::write(
            &tool,
            format!(
                "#!/bin/sh\nset -eu\nif [ \"$1\" = resolve ]; then echo sha256:{}; exit 0; fi\n[ \"$1\" = pull ]\nout=$3\nmkdir -p \"$out/.a3s\"\nprintf '%s\\n' 'version = \"a3s.asset.v1\"' 'category = \"agent\"' 'source {{ entrypoint = \"run.sh\" }}' > \"$out/.a3s/asset.acl\"\nprintf '%s\\n' '#!/bin/sh' 'exit 0' > \"$out/run.sh\"\n",
                "b".repeat(64)
            ),
        )
        .unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
        let asset =
            resolve_oras_with(tool.to_str().unwrap(), "registry/agent:test", state.path()).unwrap();
        assert_eq!(asset.entrypoint, "run.sh");
        assert_eq!(asset.identity, format!("sha256:{}", "b".repeat(64)));
    }

    #[cfg(unix)]
    #[test]
    fn cache_validation_rejects_symlink_marker() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let outside = root.path().join("outside");
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(&outside, "sha256:fake\n").unwrap();
        symlink(&outside, cache.join(".complete")).unwrap();
        assert!(valid_cache(&cache, "sha256:fake").is_err());
    }
}
