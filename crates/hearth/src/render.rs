//! Human-readable output. (`--json` bypasses all of this.)

use hearth_core::model::{
    AppDetail, AppState, AppSummary, HostStats, ImageVersion, LogLine, Status, TokenInfo,
};

pub fn bytes(n: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let f = n as f64;
    if f >= GIB {
        format!("{:.1} GiB", f / GIB)
    } else if f >= MIB {
        format!("{:.0} MiB", f / MIB)
    } else {
        format!("{:.0} KiB", f / 1024.0)
    }
}

fn duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

fn short_digest(d: &str) -> String {
    let hexpart = d.split_once(':').map(|(_, h)| h).unwrap_or(d);
    format!("@{}", &hexpart[..hexpart.len().min(12)])
}

/// Prints rows as aligned columns; the first row is the header.
/// "5 min ago" for a Unix time.
pub fn ago(t: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format!("{} ago", duration(now.saturating_sub(t).max(0) as u64))
}

pub fn table(rows: &[Vec<String>]) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|i| {
            rows.iter()
                .map(|r| r.get(i).map_or(0, |c| c.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    for r in rows {
        let line: Vec<String> = r
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i + 1 == r.len() {
                    c.clone()
                } else {
                    format!("{c:<w$}", w = widths[i])
                }
            })
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}

pub fn host(h: &HostStats) {
    let m = &h.memory;
    let pct = (m.used_bytes * 100).checked_div(m.total_bytes).unwrap_or(0);
    println!(
        "{} · {} · up {} · {} CPUs",
        h.hostname,
        h.os,
        duration(h.uptime_secs),
        h.cpu_count
    );
    println!(
        "CPU {:.0}% · load {:.2} {:.2} {:.2} · memory {} used of {} ({pct}%), {} available",
        h.cpu_percent,
        h.load[0],
        h.load[1],
        h.load[2],
        bytes(m.used_bytes),
        bytes(m.total_bytes),
        bytes(m.available_bytes)
    );
    let mut extra = Vec::new();
    if m.swap_total_bytes > 0 {
        extra.push(format!(
            "swap {} of {}",
            bytes(m.swap_used_bytes),
            bytes(m.swap_total_bytes)
        ));
    }
    if let Some(p) = h.memory_pressure_avg60 {
        extra.push(format!("memory pressure {p:.2}%"));
    }
    if let Some(o) = h.oom_kills {
        extra.push(format!("OOM kills since boot: {o}"));
    }
    if !extra.is_empty() {
        println!("{}", extra.join(" · "));
    }
    for d in &h.disks {
        let pct = (d.used_bytes * 100).checked_div(d.total_bytes).unwrap_or(0);
        println!(
            "disk {}: {} used of {} ({pct}%), {} free [{}]",
            d.mount,
            bytes(d.used_bytes),
            bytes(d.total_bytes),
            bytes(d.available_bytes),
            d.fs_type
        );
    }
}

fn state_str(a: &AppSummary) -> String {
    match a.state {
        AppState::Running => "running",
        AppState::Partial => "partial",
        AppState::Stopped => "stopped",
        AppState::Unhealthy => "unhealthy",
    }
    .to_string()
}

fn version_str(i: &ImageVersion) -> String {
    i.running_version
        .clone()
        .or_else(|| i.running_digest.as_deref().map(short_digest))
        .unwrap_or_else(|| "?".into())
}

fn latest_str(i: &ImageVersion) -> String {
    match &i.latest {
        None => "not checked".into(),
        Some(l) if l.error.is_some() && l.digest.is_none() => "check failed".into(),
        Some(l) if !l.update_available => "up to date".into(),
        Some(l) => l
            .version
            .clone()
            .or_else(|| l.digest.as_deref().map(short_digest))
            .unwrap_or_else(|| "?".into()),
    }
}

fn notes_str(images: &[ImageVersion]) -> String {
    let mut notes = Vec::new();
    for i in images {
        if let Some(l) = &i.latest {
            if l.update_available {
                notes.push(format!("{}: update", i.service));
            }
            if let Some(t) = &l.newer_in_track {
                notes.push(format!("{}: newer tag {t}", i.service));
            }
            if let Some(t) = &l.newer_major {
                notes.push(format!("{}: new major {t}", i.service));
            }
        }
    }
    notes.join(", ")
}

/// The "main" image of an app for one-line display: the first service's.
fn main_image(a: &AppSummary) -> Option<&ImageVersion> {
    a.images.first()
}

/// The main app list (apps with a web page) and, below it, the services and programs that
/// aren't on the dashboard.
pub fn apps(apps: &[AppSummary]) {
    let (main, other): (Vec<&AppSummary>, Vec<&AppSummary>) = apps.iter().partition(|a| a.dashboard);
    app_table(&main);
    if !other.is_empty() {
        println!();
        println!("Not on the dashboard (no web page):");
        app_table(&other);
    }
}

fn app_table(apps: &[&AppSummary]) {
    let mut rows = vec![vec![
        "APP".into(),
        "KIND".into(),
        "STATE".into(),
        "HEALTH".into(),
        "MEMORY".into(),
        "RUNNING".into(),
        "LATEST".into(),
        "NOTES".into(),
    ]];
    for a in apps {
        let img = main_image(a);
        let mut where_ = String::new();
        if let Some(ep) = &a.endpoint
            && *ep != hearth_core::model::DockerEndpoint::Root
        {
            where_ = format!(" [{}]", ep.label());
        }
        if let Some(u) = a.unit.as_ref().and_then(|u| u.user.as_ref()) {
            where_ = format!(" [user {u}]");
        }
        rows.push(vec![
            format!("{}{where_}", a.id),
            a.kind.as_str().into(),
            state_str(a),
            a.health.clone().unwrap_or_else(|| "-".into()),
            a.memory_bytes.map(bytes).unwrap_or_else(|| "-".into()),
            img.map(version_str).unwrap_or_else(|| "-".into()),
            img.map(latest_str).unwrap_or_else(|| "-".into()),
            notes_str(&a.images),
        ]);
    }
    table(&rows);
}

pub fn status(s: &Status) {
    host(&s.host);
    println!();
    apps(&s.apps);
    for w in &s.warnings {
        eprintln!("warning: {w}");
    }
}

pub fn app_detail(d: &AppDetail) {
    let a = &d.summary;
    println!("{} ({})", a.title, a.id);
    println!(
        "  state:      {}{}",
        state_str(a),
        a.health.as_deref().map(|h| format!(", {h}")).unwrap_or_default()
    );
    println!(
        "  managed by: {:?}{}",
        a.managed_by,
        if a.read_only { " (read-only in Hearth)" } else { "" }
    );
    println!("  kind:       {}", a.kind.as_str());
    if let Some(ep) = &a.endpoint {
        println!("  docker:     {}", ep.label());
    }
    if let Some(u) = &a.unit {
        println!(
            "  unit:       {}{} ({}, {}/{})",
            u.name,
            u.user
                .as_ref()
                .map(|u| format!(" (user {u})"))
                .unwrap_or_default(),
            u.unit_file_state,
            u.active_state,
            u.sub_state
        );
    }
    println!("  dashboard:  {}", if a.dashboard { "yes" } else { "no" });
    if let Some(m) = a.memory_bytes {
        println!("  memory:     {}", bytes(m));
    }
    if let Some(u) = &a.web_url {
        println!("  web UI:     {u}");
    }
    if let Some(w) = &d.working_dir {
        println!("  directory:  {w}");
    }
    if let Some(s) = &d.store_app_id {
        println!("  store app:  {s}");
    }
    if !d.external_variables.is_empty() {
        println!(
            "  needs vars: {} (plain `docker compose` can't run this file on its own)",
            d.external_variables.join(", ")
        );
    }
    if !d.missing_devices.is_empty() {
        println!("  missing devices on this host: {}", d.missing_devices.join(", "));
    }
    println!();
    let mut rows = vec![vec![
        "CONTAINER".into(),
        "SERVICE".into(),
        "STATE".into(),
        "MEMORY".into(),
        "RESTARTS".into(),
        "STATUS".into(),
    ]];
    for c in &a.containers {
        rows.push(vec![
            c.name.clone(),
            c.service.clone().unwrap_or_else(|| "-".into()),
            c.state.clone(),
            c.memory_bytes.map(bytes).unwrap_or_else(|| "-".into()),
            c.restart_count
                .map(|r| r.to_string())
                .unwrap_or_else(|| "-".into()),
            c.status.clone(),
        ]);
    }
    table(&rows);
    println!();
    let mut rows = vec![vec![
        "SERVICE".into(),
        "IMAGE".into(),
        "RUNNING".into(),
        "LATEST".into(),
        "NOTES".into(),
    ]];
    for i in &a.images {
        rows.push(vec![
            i.service.clone(),
            i.reference.clone(),
            version_str(i),
            latest_str(i),
            notes_str(std::slice::from_ref(i)),
        ]);
        if let Some(e) = i.latest.as_ref().and_then(|l| l.error.as_ref()) {
            rows.push(vec![String::new(), format!("  check error: {e}")]);
        }
    }
    table(&rows);
}

pub fn logs(lines: &[LogLine]) {
    let width = lines.iter().map(|l| l.service.len()).max().unwrap_or(0);
    for l in lines {
        let mark = if l.stream == "stderr" { "!" } else { " " };
        println!("{:<width$} {mark}| {}", l.service, l.line);
    }
}

pub fn tokens(list: &[TokenInfo]) {
    if list.is_empty() {
        println!("No tokens.");
        return;
    }
    let mut rows = vec![vec![
        "ID".into(),
        "NAME".into(),
        "SCOPES".into(),
        "LAST USED".into(),
    ]];
    for t in list {
        rows.push(vec![
            t.id.clone(),
            t.name.clone(),
            t.scopes.join(" "),
            t.last_used_at
                .map(|_| "yes".to_string())
                .unwrap_or_else(|| "never".into()),
        ]);
    }
    table(&rows);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(bytes(1536 * 1024 * 1024), "1.5 GiB");
        assert_eq!(bytes(300 * 1024 * 1024), "300 MiB");
        assert_eq!(bytes(434176), "424 KiB");
        assert_eq!(duration(3 * 86400 + 4 * 3600), "3d 4h");
        assert_eq!(duration(125), "2m");
        assert_eq!(short_digest("sha256:0123456789abcdef"), "@0123456789ab");
    }
}
