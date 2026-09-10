use crate::task::RoleResources;
use std::path::Path;

pub fn work_docker_args(resources: RoleResources) -> Vec<String> {
    vec![
        "--pids-limit".into(),
        "512".into(),
        "--memory".into(),
        resources.memory_bytes.to_string(),
        "--cpus".into(),
        resources.cpu_limit.to_string(),
    ]
}

pub fn judge_docker_args(resources: RoleResources) -> Vec<String> {
    vec![
        "--memory".into(),
        resources.memory_bytes.to_string(),
        "--cpus".into(),
        resources.cpu_limit.to_string(),
    ]
}

pub const READ_ONLY_JUDGE_TMPFS: &[&str] = &[];

/// Detect host DNS servers suitable for injection into Docker containers.
///
/// On hosts where `systemd-resolved` is active, `/etc/resolv.conf` is a
/// symlink to the stub resolver at `127.0.0.53`, which is only reachable
/// from the host loopback interface.  Docker Desktop's VPNKit forwarder
/// runs inside a QEMU VM and cannot reach `127.0.0.53`, so every container
/// on the default bridge network loses DNS resolution.
///
/// This function reads `/etc/resolv.conf` first.  If every `nameserver`
/// entry is a loopback address (127.0.0.0/8), it falls back to
/// `/run/systemd/resolve/resolv.conf`, which contains the real upstream
/// DNS servers.  The result is deduplicated while preserving order.
fn detect_host_dns() -> Vec<String> {
    let mut servers = read_nameservers(Path::new("/etc/resolv.conf"));
    if servers.iter().all(|s| is_loopback(s)) {
        let real = read_nameservers(Path::new("/run/systemd/resolve/resolv.conf"));
        if !real.is_empty() {
            servers = real;
        }
    }
    dedup_preserve_order(servers)
}

/// Read `nameserver` entries from a resolv.conf file, skipping comments.
fn read_nameservers(path: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let line = line.split('#').next()?.trim();
            let mut parts = line.split_whitespace();
            if parts.next() == Some("nameserver") {
                parts.next().map(str::to_owned)
            } else {
                None
            }
        })
        .collect()
}

/// Returns `true` if the address is in the 127.0.0.0/8 loopback range.
fn is_loopback(addr: &str) -> bool {
    addr.strip_prefix("127.").is_some_and(|rest| {
        rest.split('.')
            .next()
            .is_some_and(|octet| octet.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Remove duplicates while preserving the original ordering.
fn dedup_preserve_order(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect()
}

/// Build the flat `["--dns", addr, "--dns", addr, …]` argument vector from
/// the detected host DNS servers, suitable for appending to a
/// `docker run` / `docker create` command.
pub fn host_dns_args() -> Vec<String> {
    let mut args = Vec::new();
    for server in detect_host_dns() {
        args.push("--dns".into());
        args.push(server);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_profiles_apply_locked_resources_without_duplicate_limits() {
        let work = work_docker_args(RoleResources {
            cpu_limit: 6,
            memory_bytes: 12_345,
        });
        assert!(work.windows(2).any(|pair| pair == ["--memory", "12345"]));
        assert!(work.windows(2).any(|pair| pair == ["--cpus", "6"]));
        assert!(!work.iter().any(|value| value == "--tmpfs"));

        let judge = judge_docker_args(RoleResources {
            cpu_limit: 3,
            memory_bytes: 54_321,
        });
        assert!(judge.windows(2).any(|pair| pair == ["--memory", "54321"]));
        assert!(judge.windows(2).any(|pair| pair == ["--cpus", "3"]));
        assert!(!judge.iter().any(|value| value == "--tmpfs"));
        assert!(READ_ONLY_JUDGE_TMPFS.is_empty());
    }

    #[test]
    fn read_nameservers_parses_standard_resolv_conf() {
        let dir = std::env::temp_dir().join("a3s_dns_test_resolv");
        let _ = std::fs::create_dir(&dir);
        let path = dir.join("resolv.conf");
        std::fs::write(
            &path,
            "# comment\nnameserver 10.1.7.5\nnameserver 10.1.7.6 # inline\n",
        )
        .unwrap();
        let servers = read_nameservers(&path);
        assert_eq!(servers, vec!["10.1.7.5", "10.1.7.6"]);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn is_loopback_recognizes_127_addresses() {
        assert!(is_loopback("127.0.0.53"));
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("127.255.255.255"));
        assert!(!is_loopback("10.1.7.5"));
        assert!(!is_loopback("192.168.65.7"));
        assert!(!is_loopback("::1"));
    }

    #[test]
    fn dedup_preserves_order() {
        assert_eq!(
            dedup_preserve_order(vec![
                "10.1.7.5".into(),
                "10.1.7.6".into(),
                "10.1.7.5".into(),
            ]),
            vec!["10.1.7.5", "10.1.7.6"]
        );
    }

    #[test]
    fn host_dns_args_nonempty_on_real_machine() {
        let args = host_dns_args();
        // On any machine with a working /etc/resolv.conf this should be non-empty.
        // If it's empty, DNS detection is broken.
        assert!(
            !args.is_empty(),
            "host_dns_args returned empty on a real machine"
        );
        // Verify the "--dns" prefix pattern.
        assert!(args.len().is_multiple_of(2));
        for chunk in args.chunks(2) {
            assert_eq!(chunk[0], "--dns");
            assert!(!chunk[1].is_empty());
        }
    }
}
