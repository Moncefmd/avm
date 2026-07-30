use semver::Version;

use crate::error::{AvmError, Result};

pub const STABLE: &str = "stable";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Selector {
    LatestStable,
    Major { major: u64 },
    Minor { major: u64, minor: u64 },
    Exact(String),
}

impl Selector {
    pub fn parse(input: &str) -> Result<Self> {
        if input == STABLE {
            return Ok(Self::LatestStable);
        }

        if let Ok(exact) = normalize(input) {
            return Ok(Self::Exact(exact));
        }

        if input.is_empty() || input.trim() != input || input.starts_with('v') {
            return Err(AvmError::InvalidSelector {
                input: input.to_owned(),
            });
        }

        let components = input.split('.').collect::<Vec<_>>();
        if !(1..=2).contains(&components.len())
            || components.iter().any(|component| {
                component.is_empty()
                    || component.starts_with('0') && component.len() > 1
                    || !component.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            return Err(AvmError::InvalidSelector {
                input: input.to_owned(),
            });
        }

        let major = components[0]
            .parse()
            .map_err(|_| AvmError::InvalidSelector {
                input: input.to_owned(),
            })?;
        let Some(minor) = components.get(1) else {
            return Ok(Self::Major { major });
        };
        let minor = minor.parse().map_err(|_| AvmError::InvalidSelector {
            input: input.to_owned(),
        })?;

        Ok(Self::Minor { major, minor })
    }
}

pub fn normalize(input: &str) -> Result<String> {
    let raw = input.strip_prefix('v').unwrap_or(input);
    if raw.is_empty() || raw.trim() != raw {
        return Err(AvmError::InvalidVersion {
            input: input.to_owned(),
        });
    }

    let parsed = Version::parse(raw).map_err(|_| AvmError::InvalidVersion {
        input: input.to_owned(),
    })?;

    Ok(format!("v{parsed}"))
}

pub fn parse_tag(tag: &str) -> Option<Version> {
    let raw = tag.strip_prefix('v')?;
    Version::parse(raw).ok()
}

#[cfg(test)]
pub fn sort_tags_desc(tags: &mut [String]) {
    tags.sort_by(|a, b| match (parse_tag(a), parse_tag(b)) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.cmp(b),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_supported_forms() {
        assert_eq!(normalize("3.4.5").unwrap(), "v3.4.5");
        assert_eq!(normalize("v3.4.5").unwrap(), "v3.4.5");
        assert_eq!(normalize("v3.5.0-rc1").unwrap(), "v3.5.0-rc1");
    }

    #[test]
    fn rejects_path_traversal_and_non_versions() {
        for value in [
            "../../Documents",
            "v3",
            " v3.4.5",
            "V3.4.5",
            "",
            "stable/../x",
        ] {
            assert!(normalize(value).is_err(), "{value} should be rejected");
        }
    }

    #[test]
    fn sorts_semantically_newest_first() {
        let mut tags = vec![
            "v2.9.9".to_owned(),
            "v3.1.0-rc1".to_owned(),
            "v3.0.0".to_owned(),
            "v2.10.0".to_owned(),
        ];
        sort_tags_desc(&mut tags);
        assert_eq!(tags, vec!["v3.1.0-rc1", "v3.0.0", "v2.10.0", "v2.9.9"]);
    }

    #[test]
    fn parses_management_selectors() {
        assert_eq!(Selector::parse("stable").unwrap(), Selector::LatestStable);
        assert_eq!(Selector::parse("3").unwrap(), Selector::Major { major: 3 });
        assert_eq!(
            Selector::parse("3.4").unwrap(),
            Selector::Minor { major: 3, minor: 4 }
        );
        assert_eq!(
            Selector::parse("3.4.5").unwrap(),
            Selector::Exact("v3.4.5".to_owned())
        );
        assert_eq!(
            Selector::parse("v3.4.5").unwrap(),
            Selector::Exact("v3.4.5".to_owned())
        );
    }

    #[test]
    fn rejects_ambiguous_or_malformed_selectors() {
        for value in [
            "v3", "v3.4", "3.x", "03", "03.4", "3.04", "3.4.5.6", " stable",
        ] {
            assert!(
                Selector::parse(value).is_err(),
                "{value} should be rejected"
            );
        }
    }
}
