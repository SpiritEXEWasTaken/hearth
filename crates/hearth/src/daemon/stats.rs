//! Host statistics from /proc and statvfs. No root needed.

use hearth_core::model::{DiskStats, HostStats, MemoryStats};
use std::collections::HashSet;
use std::fs;
use std::time::Duration;

/// Filesystems worth showing as disks.
const REAL_FS: &[&str] = &[
    "ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs", "vfat", "exfat", "ntfs", "ntfs3", "fuseblk",
    "bcachefs", "jfs", "reiserfs", "nfs", "nfs4", "cifs", "smb3",
];

pub async fn collect() -> HostStats {
    let cpu1 = read("/proc/stat").as_deref().and_then(parse_cpu_times);
    tokio::time::sleep(Duration::from_millis(250)).await;
    let cpu2 = read("/proc/stat").as_deref().and_then(parse_cpu_times);
    let cpu_percent = match (cpu1, cpu2) {
        (Some((i1, t1)), Some((i2, t2))) if t2 > t1 => {
            let busy = (t2 - t1).saturating_sub(i2.saturating_sub(i1));
            (busy as f64 * 100.0 / (t2 - t1) as f64 * 10.0).round() / 10.0
        }
        _ => 0.0,
    };
    HostStats {
        hostname: read("/proc/sys/kernel/hostname")
            .unwrap_or_default()
            .trim()
            .to_string(),
        os: read("/etc/os-release")
            .as_deref()
            .and_then(parse_os_release)
            .unwrap_or_else(|| "Linux".into()),
        kernel: read("/proc/sys/kernel/osrelease")
            .unwrap_or_default()
            .trim()
            .to_string(),
        arch: std::env::consts::ARCH.to_string(),
        uptime_secs: read("/proc/uptime")
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .map(|f| f as u64)
            .unwrap_or(0),
        load: read("/proc/loadavg")
            .as_deref()
            .map(parse_loadavg)
            .unwrap_or_default(),
        cpu_count: std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1),
        cpu_percent,
        memory: read("/proc/meminfo")
            .as_deref()
            .map(parse_meminfo)
            .unwrap_or_default(),
        memory_pressure_avg60: read("/proc/pressure/memory")
            .as_deref()
            .and_then(parse_psi_some_avg60),
        oom_kills: read("/proc/vmstat").as_deref().and_then(parse_oom_kills),
        disks: read("/proc/self/mounts")
            .as_deref()
            .map(disks)
            .unwrap_or_default(),
    }
}

fn read(path: &str) -> Option<String> {
    fs::read_to_string(path).ok()
}

pub fn parse_os_release(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim().trim_matches('"').to_string())
}

pub fn parse_loadavg(text: &str) -> [f64; 3] {
    let mut it = text.split_whitespace().map(|s| s.parse().unwrap_or(0.0));
    [
        it.next().unwrap_or(0.0),
        it.next().unwrap_or(0.0),
        it.next().unwrap_or(0.0),
    ]
}

/// (idle + iowait, total) jiffies from the aggregate `cpu` line.
pub fn parse_cpu_times(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|s| s.parse().ok())
        .collect();
    if v.len() < 4 {
        return None;
    }
    // guest/guest_nice (9, 10) are already counted in user/nice.
    let total: u64 = v.iter().take(8).sum();
    let idle = v[3] + v.get(4).copied().unwrap_or(0);
    Some((idle, total))
}

pub fn parse_meminfo(text: &str) -> MemoryStats {
    let get = |key: &str| -> u64 {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    };
    let total = get("MemTotal");
    let available = get("MemAvailable");
    let swap_total = get("SwapTotal");
    let swap_free = get("SwapFree");
    MemoryStats {
        total_bytes: total,
        available_bytes: available,
        used_bytes: total.saturating_sub(available),
        swap_total_bytes: swap_total,
        swap_used_bytes: swap_total.saturating_sub(swap_free),
    }
}

pub fn parse_psi_some_avg60(text: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.starts_with("some "))?;
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix("avg60="))
        .and_then(|v| v.parse().ok())
}

pub fn parse_oom_kills(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix("oom_kill "))
        .and_then(|v| v.trim().parse().ok())
}

/// Real filesystems from /proc/self/mounts, one entry per device (first mount wins).
pub fn parse_mounts(text: &str) -> Vec<(String, String, String)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 3 {
            continue;
        }
        let (dev, mount, fs) = (f[0], unescape(f[1]), f[2]);
        if !REAL_FS.contains(&fs) || !seen.insert(dev.to_string()) {
            continue;
        }
        // Skip container/snap internals that happen to be real filesystems.
        if mount.starts_with("/var/lib/docker") || mount.starts_with("/snap/") || mount.starts_with("/run/") {
            continue;
        }
        out.push((dev.to_string(), mount, fs.to_string()));
    }
    out
}

/// /proc/mounts escapes spaces etc. as octal (`\040`).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let n = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(n as char);
            i += 4;
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

fn disks(mounts: &str) -> Vec<DiskStats> {
    parse_mounts(mounts)
        .into_iter()
        .filter_map(|(device, mount, fs_type)| {
            let st = rustix::fs::statvfs(mount.as_str()).ok()?;
            let frsize = st.f_frsize;
            let total = st.f_blocks * frsize;
            if total == 0 {
                return None;
            }
            let free = st.f_bfree * frsize;
            Some(DiskStats {
                mount,
                device,
                fs_type,
                total_bytes: total,
                used_bytes: total.saturating_sub(free),
                available_bytes: st.f_bavail * frsize,
            })
        })
        .collect()
}

/// Memory use of a container: its cgroup's `memory.current`, found via its main process.
pub fn container_memory(pid: u64) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    let cg = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = cg.lines().find_map(|l| l.strip_prefix("0::"))?;
    let v = fs::read_to_string(format!("/sys/fs/cgroup{}/memory.current", path.trim())).ok()?;
    v.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo() {
        let m = parse_meminfo(
            "MemTotal:        8003584 kB\nMemFree:  100 kB\nMemAvailable:    3815424 kB\nSwapTotal:       1048572 kB\nSwapFree:         524286 kB\n",
        );
        assert_eq!(m.total_bytes, 8003584 * 1024);
        assert_eq!(m.available_bytes, 3815424 * 1024);
        assert_eq!(m.used_bytes, (8003584 - 3815424) * 1024);
        assert_eq!(m.swap_used_bytes, 524286 * 1024);
    }

    #[test]
    fn cpu_times() {
        let (idle, total) =
            parse_cpu_times("cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3 4\n").unwrap_or_default();
        assert_eq!((idle, total), (850, 1000));
    }

    #[test]
    fn psi_oom_load_os() {
        assert_eq!(
            parse_psi_some_avg60(
                "some avg10=0.00 avg60=1.25 avg300=0.10 total=1\nfull avg10=0 avg60=0 avg300=0 total=0\n"
            ),
            Some(1.25)
        );
        assert_eq!(parse_oom_kills("pgfault 1\noom_kill 3\n"), Some(3));
        assert_eq!(parse_loadavg("0.52 0.40 0.33 2/900 1234\n"), [0.52, 0.40, 0.33]);
        assert_eq!(
            parse_os_release("NAME=\"Debian GNU/Linux\"\nPRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\n")
                .as_deref(),
            Some("Debian GNU/Linux 13 (trixie)")
        );
    }

    #[test]
    fn mounts_keep_real_fs_once_per_device() {
        let m = "/dev/nvme0n1p2 / ext4 rw 0 0\nproc /proc proc rw 0 0\n/dev/nvme0n1p2 /var/lib/docker ext4 rw 0 0\n/dev/nvme0n1p1 /boot/efi vfat rw 0 0\noverlay /var/lib/docker/overlay2/x/merged overlay rw 0 0\n/dev/sdb1 /mnt/My\\040Disk ext4 rw 0 0\ntmpfs /run tmpfs rw 0 0\n";
        let got: Vec<String> = parse_mounts(m).into_iter().map(|(_, mount, _)| mount).collect();
        assert_eq!(got, vec!["/", "/boot/efi", "/mnt/My Disk"]);
    }
}
