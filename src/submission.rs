use crate::task::SubmissionPolicy;
use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::path::{Path, PathBuf};

pub fn project(source: &Path, destination: &Path, policy: &SubmissionPolicy) -> Result<()> {
    let include = compile_patterns(&policy.include)?;
    let exclude = compile_patterns(&policy.exclude)?;
    let files = collect_terminal_files(source, policy, &include, &exclude)?;
    crate::state_fs::secure_directory(destination)?;
    for (relative, _) in files {
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            crate::state_fs::secure_directory(parent)?;
        }
        std::fs::copy(source.join(&relative), &target)
            .with_context(|| format!("could not project submission file {}", relative.display()))?;
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

fn collect_terminal_files(
    root: &Path,
    policy: &SubmissionPolicy,
    include: &GlobSet,
    exclude: &GlobSet,
) -> Result<Vec<(PathBuf, u64)>> {
    struct VisitState<'a> {
        files: &'a mut Vec<(PathBuf, u64)>,
        total: u64,
        limit_reached: bool,
    }
    fn visit(
        root: &Path,
        directory: &Path,
        policy: &SubmissionPolicy,
        include: &GlobSet,
        exclude: &GlobSet,
        state: &mut VisitState,
    ) -> Result<()> {
        for entry in std::fs::read_dir(directory)? {
            if state.limit_reached {
                return Ok(());
            }
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let relative = entry.path().strip_prefix(root)?.to_path_buf();
            let normalized = normalize(&relative)?;
            if kind.is_dir() {
                anyhow::ensure!(
                    normalized.split('/').count() <= 64,
                    "terminal path is too deep"
                );
                visit(root, &entry.path(), policy, include, exclude, state)?;
            } else if kind.is_file() {
                // Only count files that match the submission include/exclude policy,
                // so build artifacts and caches outside the submission scope don't
                // inflate the total.
                if !include.is_match(&normalized)
                    || exclude.is_match(&normalized)
                    || reserved(&normalized)
                {
                    continue;
                }
                let metadata = entry.metadata()?;
                if metadata.len() > policy.max_file_bytes {
                    continue;
                }
                let new_total = state
                    .total
                    .checked_add(metadata.len())
                    .ok_or_else(|| anyhow::anyhow!("terminal size overflow"))?;
                if new_total > policy.max_total_bytes || state.files.len() >= policy.max_files {
                    state.limit_reached = true;
                    return Ok(());
                }
                state.total = new_total;
                state.files.push((relative, metadata.len()));
            } else {
                // Skip special files (sockets, FIFOs, device nodes) left
                // behind by the candidate's runtime instead of failing.
                eprintln!(
                    "skipping special file in terminal workspace: {}",
                    relative.display()
                );
                continue;
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    let mut state = VisitState {
        files: &mut files,
        total: 0,
        limit_reached: false,
    };
    visit(root, root, policy, include, exclude, &mut state)?;
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
        project(
            source.path(),
            output.path(),
            &policy(&["real"], &["ignored-link"]),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(output.path().join("real")).unwrap(),
            "data"
        );
        assert!(!output.path().join("ignored-link").exists());
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_are_copied_as_independent_regular_files() {
        use std::os::unix::fs::MetadataExt;

        let source = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("first"), "shared").unwrap();
        std::fs::hard_link(source.path().join("first"), source.path().join("second")).unwrap();

        project(source.path(), output.path(), &policy(&["**"], &[])).unwrap();

        assert_eq!(
            std::fs::read_to_string(output.path().join("first")).unwrap(),
            "shared"
        );
        assert_eq!(
            std::fs::read_to_string(output.path().join("second")).unwrap(),
            "shared"
        );
        assert_ne!(
            std::fs::metadata(output.path().join("first"))
                .unwrap()
                .ino(),
            std::fs::metadata(output.path().join("second"))
                .unwrap()
                .ino()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn case_distinct_paths_are_both_projected_on_linux() {
        let source = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("README"), "upper").unwrap();
        std::fs::write(source.path().join("readme"), "lower").unwrap();

        project(source.path(), output.path(), &policy(&["**"], &[])).unwrap();

        assert_eq!(
            std::fs::read_to_string(output.path().join("README")).unwrap(),
            "upper"
        );
        assert_eq!(
            std::fs::read_to_string(output.path().join("readme")).unwrap(),
            "lower"
        );
    }

    #[test]
    fn oversized_workspace_is_truncated_not_aborted() {
        let source = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();

        // Create files that match the include pattern but exceed total size.
        let _ = std::fs::write(source.path().join("a.txt"), vec![b'x'; 600]);
        let _ = std::fs::write(source.path().join("b.txt"), vec![b'y'; 600]);

        // max_total_bytes = 1024, so only the first file fits.
        let small_policy = SubmissionPolicy {
            include: vec!["**".into()],
            exclude: vec![],
            max_files: 100,
            max_total_bytes: 1024,
            max_file_bytes: 1024,
        };

        project(source.path(), output.path(), &small_policy).unwrap();

        // At least one file should have been projected; the function must not
        // have returned an error.
        let projected: Vec<_> = std::fs::read_dir(output.path()).unwrap().collect();
        assert!(!projected.is_empty());
    }

    #[test]
    fn build_artifacts_outside_submission_scope_do_not_count_toward_limits() {
        let source = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();

        // A large file that would exceed limits but is excluded by the policy.
        std::fs::create_dir_all(source.path().join("target")).unwrap();
        let _ = std::fs::write(source.path().join("target/big.bin"), vec![b'z'; 10_000]);

        // A small submission file that should pass.
        let _ = std::fs::write(source.path().join("output.txt"), "result");

        let small_policy = SubmissionPolicy {
            include: vec!["output.txt".into()],
            exclude: vec![],
            max_files: 100,
            max_total_bytes: 1024,
            max_file_bytes: 1024,
        };

        project(source.path(), output.path(), &small_policy).unwrap();
        assert_eq!(
            std::fs::read_to_string(output.path().join("output.txt")).unwrap(),
            "result"
        );
        assert!(!output.path().join("target").exists());
    }
}
