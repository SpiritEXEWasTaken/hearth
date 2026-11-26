//! "Is a reboot needed?" Hearth never reboots by itself; it only says so, and why.

use hearth_core::model::RebootStatus;
use std::cmp::Ordering;
use std::path::Path;

/// Written by the helper when PackageKit reports that an update needs a system restart. It's
/// under /run, so a reboot clears it.
pub const MARKER: &str = "/run/hearth-reboot-required";

pub fn status() -> RebootStatus {
    let mut reasons = Vec::new();
    let running = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if !running.is_empty() {
        let installed = installed_kernels();
        if let Some(r) = kernel_reason(&running, &installed) {
            reasons.push(r);
        }
    }
    if Path::new("/run/reboot-required").exists() {
        let pkgs = std::fs::read_to_string("/run/reboot-required.pkgs").unwrap_or_default();
        let pkgs: Vec<&str> = pkgs.split_whitespace().collect();
        reasons.push(if pkgs.is_empty() {
            "the package manager asks for a reboot".to_string()
        } else {
            format!("the package manager asks for a reboot ({})", pkgs.join(", "))
        });
    }
    if let Ok(s) = std::fs::read_to_string(MARKER) {
        for l in s.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if !reasons.iter().any(|r| r == l) {
                reasons.push(l.to_string());
            }
        }
    }
    RebootStatus {
        needed: !reasons.is_empty(),
        reasons,
    }
}

/// Installed kernel versions: module folders with a `modules.dep`, plus `/boot/vmlinuz-<version>`
/// (the module folders can be hidden by a sandbox; Arch's /boot names carry no version).
fn installed_kernels() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for dir in ["/usr/lib/modules", "/lib/modules"] {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if e.path().join("modules.dep").is_file() {
                out.push(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    for e in std::fs::read_dir("/boot").into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(v) = name.strip_prefix("vmlinuz-")
            && v.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            out.push(v.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Records that an update asked for a restart (shown until the next boot).
pub fn mark(reason: &str) {
    let mut s = std::fs::read_to_string(MARKER).unwrap_or_default();
    if !s.lines().any(|l| l == reason) {
        s.push_str(reason);
        s.push('\n');
        let _ = std::fs::write(MARKER, s);
    }
}

/// Compares the running kernel with the installed ones of the same flavour (`amd64`,
/// `cloud-amd64`, `cachyos`, `lts`…), since a machine can have several kernel packages.
pub fn kernel_reason(running: &str, installed: &[String]) -> Option<String> {
    if installed.is_empty() {
        return None;
    }
    let flavour = kernel_flavour(running);
    let newest = installed
        .iter()
        .filter(|k| kernel_flavour(k) == flavour)
        .max_by(|a, b| version_cmp(a, b));
    match newest {
        None if !installed.iter().any(|k| k == running) => {
            Some(format!("the running kernel {running} is no longer installed"))
        }
        Some(n) if version_cmp(n, running) == Ordering::Greater => {
            Some(format!("kernel {n} is installed, {running} is running"))
        }
        Some(_) if !installed.iter().any(|k| k == running) => {
            Some(format!("the running kernel {running} is no longer installed"))
        }
        _ => None,
    }
}

/// The parts after the version number, with digits removed: `6.12.116+deb13-cloud-amd64` →
/// `cloud-amd`, `6.12.5-2-cachyos-lts` → `-cachyos-lts`, `6.11.4-301.fc41.x86_64` → `.fc.x_`.
fn kernel_flavour(k: &str) -> String {
    match k.split_once('-') {
        Some((_, rest)) => rest.chars().filter(|c| !c.is_ascii_digit()).collect(),
        None => String::new(),
    }
}

/// Natural order: digit runs compare as numbers (`6.12.10` > `6.12.9`).
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n1 = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    n1.push(c);
                    x.next();
                }
                let mut n2 = String::new();
                while let Some(d) = y.peek().copied().filter(char::is_ascii_digit) {
                    n2.push(d);
                    y.next();
                }
                let (t1, t2) = (n1.trim_start_matches('0'), n2.trim_start_matches('0'));
                let o = t1.len().cmp(&t2.len()).then_with(|| t1.cmp(t2));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                if c != d {
                    return c.cmp(&d);
                }
                x.next();
                y.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn newer_kernel_of_the_same_flavour_needs_a_reboot() {
        let r = kernel_reason(
            "6.12.116+deb13-amd64",
            &v(&["6.12.116+deb13-amd64", "6.12.118+deb13-amd64"]),
        );
        assert_eq!(
            r.as_deref(),
            Some("kernel 6.12.118+deb13-amd64 is installed, 6.12.116+deb13-amd64 is running")
        );
    }

    #[test]
    fn up_to_date_kernel_needs_nothing() {
        assert_eq!(
            kernel_reason("6.12.118+deb13-amd64", &v(&["6.12.118+deb13-amd64"])),
            None
        );
        // 6.12.10 is newer than 6.12.9 (not a string comparison)
        assert_eq!(
            kernel_reason(
                "6.12.10-1-cachyos",
                &v(&["6.12.9-1-cachyos", "6.12.10-1-cachyos"])
            ),
            None
        );
    }

    #[test]
    fn other_flavours_are_not_compared() {
        // An LTS kernel next to the main one isn't a reason to reboot.
        assert_eq!(
            kernel_reason(
                "6.12.5-2-cachyos",
                &v(&["6.12.5-2-cachyos", "6.6.70-1-cachyos-lts"])
            ),
            None
        );
        assert_eq!(
            kernel_reason(
                "6.6.70-1-cachyos-lts",
                &v(&["6.12.5-2-cachyos", "6.6.70-1-cachyos-lts"])
            ),
            None
        );
    }

    #[test]
    fn removed_running_kernel_needs_a_reboot() {
        // Arch removes the old modules on upgrade.
        let r = kernel_reason("6.12.4-1-cachyos", &v(&["6.12.5-2-cachyos"]));
        assert!(r.is_some_and(|r| r.contains("6.12.5-2-cachyos is installed")));
        let r = kernel_reason("6.12.4-arch1-1", &v(&["6.12.4-1-cachyos"]));
        assert_eq!(
            r.as_deref(),
            Some("the running kernel 6.12.4-arch1-1 is no longer installed")
        );
    }

    #[test]
    fn natural_version_order() {
        assert_eq!(version_cmp("6.12.10", "6.12.9"), Ordering::Greater);
        assert_eq!(version_cmp("1.0", "1.0"), Ordering::Equal);
        assert_eq!(version_cmp("1.0", "1.0.1"), Ordering::Less);
    }
}
