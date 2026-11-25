//! User lookups from /etc/passwd and /etc/group (static binaries can't rely on NSS).

use std::fs;

pub fn uid_of(user: &str) -> Option<u32> {
    let text = fs::read_to_string("/etc/passwd").ok()?;
    passwd_uid(&text, user)
}

/// The user's primary group id.
pub fn primary_gid_of(user: &str) -> Option<u32> {
    let text = fs::read_to_string("/etc/passwd").ok()?;
    passwd_gid(&text, user)
}

fn passwd_gid(text: &str, user: &str) -> Option<u32> {
    text.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == user).then_some(())?;
        f.nth(2)?.parse().ok()
    })
}

fn passwd_uid(text: &str, user: &str) -> Option<u32> {
    text.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == user).then_some(())?;
        f.nth(1)?.parse().ok()
    })
}

#[cfg(test)]
fn group_gid(text: &str, group: &str) -> Option<u32> {
    text.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == group).then_some(())?;
        f.nth(1)?.parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_passwd_and_group() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\nhearth:x:998:998::/var/lib/hearth:/usr/sbin/nologin\n";
        assert_eq!(passwd_uid(passwd, "hearth"), Some(998));
        assert_eq!(passwd_uid(passwd, "root"), Some(0));
        assert_eq!(passwd_uid(passwd, "nobody"), None);
        assert_eq!(passwd_gid(passwd, "hearth"), Some(998));
        let group = "root:x:0:\nhearth:x:997:alice\n";
        assert_eq!(group_gid(group, "hearth"), Some(997));
    }
}
