//! Version ranges as written in `dependencies` and on the command line.
//!
//! The syntax is the common subset of npm and Cargo ranges: `*`, exact versions, `^1.2`,
//! `~1.2.3`, wildcards (`1.x`) and comparators (`>=1.0.0 <2.0.0`, joined by spaces or
//! commas). A bare version such as `1.2.3` means exactly that version. Alternatives (`||`)
//! and hyphen ranges (`1.0.0 - 2.0.0`) are not supported.

use semver::{Version, VersionReq};

use crate::error::{Result, err};

/// Parses a range, normalising it to what the `semver` crate understands.
pub fn parse(range: &str) -> Result<VersionReq> {
    let trimmed = range.trim();
    if trimmed.is_empty() || trimmed == "latest" {
        return Ok(VersionReq::STAR);
    }
    if trimmed.contains("||") || trimmed.contains(" - ") {
        return Err(err(format!(
            "version range \"{range}\" uses \"||\" or a hyphen range, which ctx does not support"
        )));
    }

    // Re-attach operators that were written with a space ("> 1.0.0"), then treat every
    // whitespace- or comma-separated piece as one comparator.
    let mut comparators: Vec<String> = Vec::new();
    let mut pending_operator = String::new();
    for piece in trimmed
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|piece| !piece.is_empty())
    {
        if piece.chars().all(|c| "<>=~^".contains(c)) {
            pending_operator.push_str(piece);
            continue;
        }
        let piece = format!("{pending_operator}{piece}");
        pending_operator.clear();
        // npm reads a bare version as "exactly this"; the semver crate would read it as "^".
        let starts_with_digit = piece.chars().next().is_some_and(|c| c.is_ascii_digit());
        comparators.push(if starts_with_digit {
            format!("={piece}")
        } else {
            piece
        });
    }
    if !pending_operator.is_empty() || comparators.is_empty() {
        return Err(err(format!("invalid version range \"{range}\"")));
    }

    VersionReq::parse(&comparators.join(", "))
        .map_err(|error| err(format!("invalid version range \"{range}\": {error}")))
}

/// Whether `version` satisfies `range`. Pre-releases only match ranges that name one.
pub fn matches(range: &VersionReq, version: &Version) -> bool {
    range.matches(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepts(range: &str, version: &str) -> bool {
        matches(&parse(range).unwrap(), &Version::parse(version).unwrap())
    }

    #[test]
    fn star_and_empty_match_any_stable_version() {
        for range in ["*", "", "latest"] {
            assert!(accepts(range, "0.0.1"));
            assert!(accepts(range, "12.4.0"));
            assert!(!accepts(range, "1.0.0-rc.1"));
        }
    }

    #[test]
    fn bare_version_is_exact() {
        assert!(accepts("1.2.3", "1.2.3"));
        assert!(!accepts("1.2.3", "1.2.4"));
        assert!(!accepts("1.2.3", "1.3.0"));
    }

    #[test]
    fn partial_bare_version_matches_the_series() {
        assert!(accepts("1.2", "1.2.9"));
        assert!(!accepts("1.2", "1.3.0"));
        assert!(accepts("1", "1.9.9"));
        assert!(!accepts("1", "2.0.0"));
    }

    #[test]
    fn caret_tilde_and_wildcards() {
        assert!(accepts("^1.2.3", "1.9.0"));
        assert!(!accepts("^1.2.3", "2.0.0"));
        assert!(accepts("~1.2.3", "1.2.9"));
        assert!(!accepts("~1.2.3", "1.3.0"));
        assert!(accepts("1.x", "1.5.0"));
        assert!(!accepts("1.x", "2.0.0"));
    }

    #[test]
    fn comparators_joined_by_space_or_comma() {
        for range in [">=1.0.0 <2.0.0", ">=1.0.0, <2.0.0", ">= 1.0.0 < 2.0.0"] {
            assert!(accepts(range, "1.5.0"), "{range}");
            assert!(!accepts(range, "2.0.0"), "{range}");
            assert!(!accepts(range, "0.9.0"), "{range}");
        }
    }

    #[test]
    fn prerelease_must_be_named() {
        assert!(accepts("^2.0.0-rc.1", "2.0.0-rc.2"));
        assert!(accepts("2.0.0-rc.1", "2.0.0-rc.1"));
        assert!(!accepts("^1.0.0", "2.0.0-rc.1"));
    }

    #[test]
    fn rejects_unsupported_and_malformed_ranges() {
        for range in [
            "^1.0.0 || ^2.0.0",
            "1.0.0 - 2.0.0",
            "not a range",
            ">=",
            "^",
        ] {
            assert!(parse(range).is_err(), "{range}");
        }
    }
}
