pub const WORK_DOCKER_LIMITS: &[&str] = &[
    "--pids-limit",
    "512",
    "--memory",
    "8g",
    "--cpus",
    "4",
    "--tmpfs",
    "/tmp:rw,noexec,nosuid,size=1g",
];

pub const JUDGE_DOCKER_LIMITS: &[&str] = &["--pids-limit", "256", "--memory", "16g", "--cpus", "2"];

pub const READ_ONLY_JUDGE_TMPFS: &[&str] = &[
    "--pids-limit",
    "256",
    "--memory",
    "16g",
    "--cpus",
    "2",
    "--tmpfs",
    "/tmp:rw,exec,nosuid,size=4g",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_profiles_are_closed_and_distinct() {
        assert!(WORK_DOCKER_LIMITS
            .windows(2)
            .any(|pair| pair == ["--memory", "8g"]));
        assert!(JUDGE_DOCKER_LIMITS
            .windows(2)
            .any(|pair| pair == ["--memory", "16g"]));
        assert!(!JUDGE_DOCKER_LIMITS.contains(&"--tmpfs"));
        assert!(READ_ONLY_JUDGE_TMPFS.contains(&"--tmpfs"));
        assert_ne!(WORK_DOCKER_LIMITS, JUDGE_DOCKER_LIMITS);
    }
}
