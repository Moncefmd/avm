use semver::Version;
use serde::{Deserialize, Serialize};

use crate::version;

pub const MAX_BINARY_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn parsed_version(&self) -> Option<Version> {
        version::parse_tag(&self.tag_name)
    }

    pub fn is_prerelease(&self) -> bool {
        self.prerelease
            || self
                .parsed_version()
                .is_some_and(|version| !version.pre.is_empty())
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag_name: &str, prerelease: bool) -> Release {
        Release {
            tag_name: tag_name.to_owned(),
            draft: false,
            prerelease,
            assets: Vec::new(),
        }
    }

    #[test]
    fn prerelease_status_uses_metadata_and_semver() {
        assert!(!release("v3.4.5", false).is_prerelease());
        assert!(release("v3.4.5", true).is_prerelease());
        assert!(release("v3.5.0-rc.1", false).is_prerelease());
    }
}
