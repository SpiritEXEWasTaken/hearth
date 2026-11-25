//! Tag tracks: which other tags count as "the same line" as the one you run.
//!
//! A tag like `v3.4.1` has the shape (prefix `v`, 3 numbers, no suffix). Only tags with the same
//! shape are compared, so `v3.4.1` is never "upgraded" to `v3.5.0-rc1`, `latest` or `v3`.
//! Suffixes are compared by pattern with digits masked, so linuxserver-style tags like
//! `10.11.2-ubu2404-ls45` and `10.11.3-ubu2404-ls46` are on the same track.
//!
//! Floating tags (`latest`, `stable`, `v3`) have no newer tag to suggest on the same track:
//! updates for them come from digest comparison. A newer *major* (`v4` while on `v3`) is
//! reported separately and is never applied automatically.

use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagTrack {
    prefix: String,
    nums: Vec<u64>,
    suffix: String,
    suffix_pattern: String,
    suffix_nums: Vec<u64>,
}

/// Words that mark pre-releases. Tags containing them are only compared with tags that also do.
const PRERELEASE_MARKERS: &[&str] = &["rc", "beta", "alpha", "dev", "nightly", "pre", "snapshot", "test"];

impl TagTrack {
    /// Parses a versioned tag. Returns `None` for non-version tags such as `latest` or `stable`.
    pub fn parse(tag: &str) -> Option<TagTrack> {
        let (prefix, rest) = match tag.strip_prefix(['v', 'V']) {
            Some(r) => ("v", r),
            None => ("", tag),
        };
        let mut nums = Vec::new();
        let mut i = 0;
        let bytes = rest.as_bytes();
        loop {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == start || i - start > 12 {
                return if nums.is_empty() {
                    None
                } else {
                    Some(Self::build(prefix, nums, &rest[start..]))
                };
            }
            nums.push(rest[start..i].parse().ok()?);
            if nums.len() == 4
                || i >= bytes.len()
                || bytes[i] != b'.'
                || !bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
            {
                break;
            }
            i += 1; // skip '.'
        }
        Some(Self::build(prefix, nums, &rest[i..]))
    }

    fn build(prefix: &str, nums: Vec<u64>, suffix: &str) -> TagTrack {
        let mut pattern = String::with_capacity(suffix.len());
        let mut suffix_nums = Vec::new();
        let mut cur = String::new();
        for c in suffix.chars() {
            if c.is_ascii_digit() {
                cur.push(c);
            } else {
                if !cur.is_empty() {
                    pattern.push('#');
                    suffix_nums.push(cur.parse().unwrap_or(0));
                    cur.clear();
                }
                pattern.push(c);
            }
        }
        if !cur.is_empty() {
            pattern.push('#');
            suffix_nums.push(cur.parse().unwrap_or(0));
        }
        TagTrack {
            prefix: prefix.to_string(),
            nums,
            suffix: suffix.to_string(),
            suffix_pattern: pattern,
            suffix_nums,
        }
    }

    pub fn major(&self) -> u64 {
        self.nums.first().copied().unwrap_or(0)
    }

    pub fn is_prerelease(&self) -> bool {
        let s = self.suffix.to_ascii_lowercase();
        PRERELEASE_MARKERS.iter().any(|m| s.contains(m))
    }

    /// Same prefix, same number of version components and the same suffix pattern.
    pub fn same_shape(&self, other: &TagTrack) -> bool {
        self.prefix == other.prefix
            && self.nums.len() == other.nums.len()
            && self.suffix_pattern == other.suffix_pattern
    }

    fn cmp_version(&self, other: &TagTrack) -> Ordering {
        self.nums
            .cmp(&other.nums)
            .then_with(|| self.suffix_nums.cmp(&other.suffix_nums))
    }
}

/// What the tag list says about the tag you run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackReport {
    /// The newest tag with the same shape and the same major version, if newer than yours.
    pub newer_in_track: Option<String>,
    /// The newest tag with the same shape and a higher major version.
    pub newer_major: Option<String>,
}

/// Compares `current` against the registry's tag list.
pub fn analyse(current: &str, tags: &[String]) -> TrackReport {
    let Some(cur) = TagTrack::parse(current) else {
        return TrackReport::default();
    };
    let cur_pre = cur.is_prerelease();
    let mut best_same: Option<(TagTrack, &str)> = None;
    let mut best_major: Option<(TagTrack, &str)> = None;
    for tag in tags {
        let Some(t) = TagTrack::parse(tag) else { continue };
        if !cur.same_shape(&t) || t.is_prerelease() != cur_pre || t.cmp_version(&cur) != Ordering::Greater {
            continue;
        }
        let slot = if t.major() == cur.major() {
            &mut best_same
        } else {
            &mut best_major
        };
        if slot
            .as_ref()
            .is_none_or(|(b, _)| t.cmp_version(b) == Ordering::Greater)
        {
            *slot = Some((t, tag.as_str()));
        }
    }
    // A single-number tag like `v3` has no "newer in track" (v3 moves itself).
    let newer_in_track = if cur.nums.len() > 1 {
        best_same.map(|(_, s)| s.to_string())
    } else {
        None
    };
    TrackReport {
        newer_in_track,
        newer_major: best_major.map(|(_, s)| s.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn floating_tags_have_no_track() {
        assert!(TagTrack::parse("latest").is_none());
        assert!(TagTrack::parse("stable").is_none());
        assert_eq!(analyse("latest", &tags(&["1.0", "2.0"])), TrackReport::default());
    }

    #[test]
    fn semver_patch_and_minor_within_major() {
        let r = analyse(
            "v3.4.1",
            &tags(&[
                "v3.4.1",
                "v3.4.2",
                "v3.5.0",
                "v3.5.0-rc1",
                "v4.0.0",
                "v3",
                "latest",
                "3.6.0",
            ]),
        );
        assert_eq!(r.newer_in_track.as_deref(), Some("v3.5.0"));
        assert_eq!(r.newer_major.as_deref(), Some("v4.0.0"));
    }

    #[test]
    fn never_suggests_older_or_equal() {
        let r = analyse("12.4.0", &tags(&["12.2.0", "12.4.0", "11.9.9"]));
        assert_eq!(r, TrackReport::default());
    }

    #[test]
    fn major_only_tag_reports_new_major_only() {
        let r = analyse("v3", &tags(&["v2", "v3", "v4", "v3.4.2"]));
        assert_eq!(r.newer_in_track, None);
        assert_eq!(r.newer_major.as_deref(), Some("v4"));
    }

    #[test]
    fn linuxserver_style_suffixes() {
        let r = analyse(
            "10.11.2-ubu2404-ls45",
            &tags(&[
                "10.11.2-ubu2404-ls45",
                "10.11.2-ubu2404-ls46",
                "10.11.3-ubu2404-ls47",
                "10.11.3",
                "nightly-ubu2404",
            ]),
        );
        assert_eq!(r.newer_in_track.as_deref(), Some("10.11.3-ubu2404-ls47"));
    }

    #[test]
    fn prereleases_only_compare_with_prereleases() {
        let r = analyse("2.0.0", &tags(&["2.1.0-beta1"]));
        assert_eq!(r, TrackReport::default());
        let r = analyse("2.1.0-beta1", &tags(&["2.1.0-beta2", "2.1.0"]));
        assert_eq!(r.newer_in_track.as_deref(), Some("2.1.0-beta2"));
    }

    #[test]
    fn postgres_style_compound_tag() {
        let cur = "14-vectorchord0.4.3-pgvectors0.2.0";
        let r = analyse(
            cur,
            &tags(&[
                cur,
                "15-vectorchord0.4.3-pgvectors0.2.0",
                "14-vectorchord0.5.0-pgvectors0.2.0",
            ]),
        );
        // Same major (14) with newer extension versions is on-track; 15 is a new major.
        assert_eq!(
            r.newer_in_track, None,
            "single-number major has no in-track suggestion"
        );
        assert_eq!(
            r.newer_major.as_deref(),
            Some("15-vectorchord0.4.3-pgvectors0.2.0")
        );
    }

    #[test]
    fn v_prefix_is_part_of_the_shape() {
        let r = analyse("v1.2", &tags(&["1.3", "v1.3"]));
        assert_eq!(r.newer_in_track.as_deref(), Some("v1.3"));
    }
}
