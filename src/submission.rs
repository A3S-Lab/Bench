use crate::task::SubmissionPolicy;
use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn project(source: &Path, destination: &Path, policy: &SubmissionPolicy) -> Result<()> {
    let include = compile_patterns(&policy.include)?;
    let exclude = compile_patterns(&policy.exclude)?;
    let files = collect_terminal_files(source, policy)?;
    crate::state_fs::secure_directory(destination)?;
    for (relative, _) in files {
        let normalized = normalize(&relative)?;
        if !include.is_match(&normalized) || exclude.is_match(&normalized) || reserved(&normalized)
        {
            continue;
        }
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            crate::state_fs::secure_directory(parent)?;
        }
        std::fs::copy(source.join(&relative), &target)
            .with_context(|| format!("could not project submission file {normalized:?}"))?;
        crate::state_fs::set_owner_only_file(&target, false)?;
    }
    Ok(())
}

pub fn validate_policy(policy: &SubmissionPolicy) -> Result<()> {
    compile_patterns(&policy.include)?;
    compile_patterns(&policy.exclude)?;
    anyhow::ensure!(
        policy.max_files > 0,
        "submission max_files must be positive"
    );
    anyhow::ensure!(
        policy.max_file_bytes > 0 && policy.max_total_bytes >= policy.max_file_bytes,
        "submission byte limits are invalid"
    );
    Ok(())
}

fn collect_terminal_files(root: &Path, policy: &SubmissionPolicy) -> Result<Vec<(PathBuf, u64)>> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut Vec<(PathBuf, u64)>,
        seen_case: &mut HashSet<String>,
        policy: &SubmissionPolicy,
        total: &mut u64,
    ) -> Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                eprintln!("warning: terminal workspace contains a symlink, skipping: {}", entry.path().display());
                continue;
            }
            let relative = entry.path().strip_prefix(root)?.to_path_buf();
            let normalized = normalize(&relative)?;
            if !seen_case.insert(normalized.to_lowercase()) {
                eprintln!("warning: terminal workspace contains a case-colliding path, skipping: {}", entry.path().display());
                continue;
            }
            if kind.is_dir() {
                anyhow::ensure!(
                    normalized.split('/').count() <= 64,
                    "terminal path is too deep"
                );
                visit(root, &entry.path(), files, seen_case, policy, total)?;
            } else if kind.is_file() {
                let metadata = entry.metadata()?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    anyhow::ensure!(
                        metadata.nlink() == 1,
                        "terminal workspace contains a hard link"
                    );
                }
                anyhow::ensure!(
                    metadata.len() <= policy.max_file_bytes,
                    "terminal file exceeds size limit"
                );
                *total = total
                    .checked_add(metadata.len())
                    .ok_or_else(|| anyhow::anyhow!("terminal size overflow"))?;
                anyhow::ensure!(
                    *total <= policy.max_total_bytes,
                    "terminal workspace exceeds total size limit"
                );
                files.push((relative, metadata.len()));
                anyhow::ensure!(
                    files.len() <= policy.max_files,
                    "terminal workspace exceeds file-count limit"
                );
            } else {
                anyhow::bail!("terminal workspace contains a special file");
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    let mut seen_case = HashSet::new();
    let mut total = 0;
    visit(root, root, &mut files, &mut seen_case, policy, &mut total)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn compile_patterns(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let pattern = validate_pattern(pattern)?;
        if pattern == "." || pattern == "**" {
            add_glob(&mut builder, "**")?;
            continue;
        }
        let has_glob = pattern
            .bytes()
            .any(|byte| matches!(byte, b'*' | b'?' | b'[' | b']'));
        if has_glob {
            add_glob(&mut builder, pattern)?;
            if !pattern.contains('/') {
                add_glob(&mut builder, &format!("**/{pattern}"))?;
            }
        } else {
            add_glob(&mut builder, pattern)?;
            add_glob(&mut builder, &format!("{pattern}/**"))?;
        }
    }
    Ok(builder.build()?)
}

fn add_glob(builder: &mut GlobSetBuilder, pattern: &str) -> Result<()> {
    builder.add(
        GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(false)
            .build()?,
    );
    Ok(())
}

fn validate_pattern(pattern: &str) -> Result<&str> {
    let pattern = pattern.trim_end_matches('/');
    anyhow::ensure!(!pattern.is_empty(), "submission pattern is empty");
    anyhow::ensure!(
        !pattern.starts_with('/') && !pattern.contains('\\'),
        "submission pattern is not relative POSIX syntax"
    );
    anyhow::ensure!(
        pattern
            .split('/')
            .all(|segment| segment != ".." && !segment.is_empty()),
        "submission pattern contains an unsafe path segment"
    );
    Ok(pattern)
}

fn normalize(path: &Path) -> Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("submission path is not UTF-8"))?
        .replace(std::path::MAIN_SEPARATOR, "/");
    anyhow::ensure!(
        !value.is_empty()
            && !value.starts_with('/')
            && value
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != ".."),
        "submission contains an unsafe path"
    );
    Ok(value)
}

fn reserved(path: &str) -> bool {
    path == ".a3s/bench" || path.starts_with(".a3s/bench/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(include: &[&str], exclude: &[&str]) -> SubmissionPolicy {
        SubmissionPolicy {
            include: include.iter().map(|value| (*value).into()).collect(),
            exclude: exclude.iter().map(|value| (*value).into()).collect(),
            max_files: 100,
            max_total_bytes: 1024 * 1024,
            max_file_bytes: 1024 * 1024,
        }
    }

    #[test]
    fn projects_includes_then_excludes_and_reserved_state() {
        let source = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("src")).unwrap();
        std::fs::create_dir_all(source.path().join(".a3s/bench")).unwrap();
        std::fs::write(source.path().join("src/main.rs"), "main").unwrap();
        std::fs::write(source.path().join("src/debug.log"), "log").unwrap();
        std::fs::write(source.path().join(".a3s/bench/secret"), "secret").unwrap();
        project(source.path(), output.path(), &policy(&["src/"], &["*.log"])).unwrap();
        assert!(output.path().join("src/main.rs").is_file());
        assert!(!output.path().join("src/debug.log").exists());
        assert!(!output.path().join(".a3s").exists());
    }

    #[test]
    fn empty_include_projects_nothing_and_parent_patterns_are_rejected() {
        let source = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("answer"), "42").unwrap();
        project(source.path(), output.path(), &policy(&[], &[])).unwrap();
        assert!(std::fs::read_dir(output.path()).unwrap().next().is_none());
        assert!(compile_patterns(&["../secret".into()]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_terminal_types_are_rejected_even_when_excluded() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("real"), "data").unwrap();
        symlink(
            source.path().join("real"),
            source.path().join("ignored-link"),
        )
        .unwrap();
        // Symlinks are now skipped (not rejected), so project should succeed.
        // The symlink is excluded from the submission but the real file is included.
        project(
            source.path(),
            output.path(),
            &policy(&["real"], &["ignored-link"])
        ).unwrap();
        assert!(output.path().join("real").is_file());
    }
}
