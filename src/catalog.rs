use std::time::Duration;

use crate::error::{AvmError, Result};
use crate::github::GitHubClient;
use crate::release::Release;
use crate::store::Store;
use crate::version::{self, Selector};

const RELEASE_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);

pub(crate) fn release_for_selector(
    store: &Store,
    remote: &GitHubClient,
    selector: &Selector,
    refresh: bool,
) -> Result<Release> {
    match selector {
        Selector::LatestStable => available_releases(store, remote, false, refresh)?
            .into_iter()
            .max_by_key(Release::parsed_version)
            .ok_or_else(|| AvmError::ReleaseNotFound(version::STABLE.to_owned())),
        Selector::Exact(version) => remote.release(version),
        Selector::Major { major } => available_releases(store, remote, false, refresh)?
            .into_iter()
            .filter(|release| {
                release
                    .parsed_version()
                    .is_some_and(|version| version.major == *major)
            })
            .max_by_key(Release::parsed_version)
            .ok_or_else(|| AvmError::ReleaseNotFound(format!("v{major}.x"))),
        Selector::Minor { major, minor } => available_releases(store, remote, false, refresh)?
            .into_iter()
            .filter(|release| {
                release
                    .parsed_version()
                    .is_some_and(|version| version.major == *major && version.minor == *minor)
            })
            .max_by_key(Release::parsed_version)
            .ok_or_else(|| AvmError::ReleaseNotFound(format!("v{major}.{minor}.x"))),
    }
}

pub(crate) fn available_releases(
    store: &Store,
    remote: &GitHubClient,
    include_prerelease: bool,
    refresh: bool,
) -> Result<Vec<Release>> {
    let mut cache_is_invalid = false;
    let cached = if refresh {
        None
    } else {
        match store.load_release_cache(remote.source(), Some(RELEASE_CACHE_TTL)) {
            Ok(cached) => cached,
            Err(AvmError::InvalidCache(reason)) => {
                eprintln!("warning: ignoring invalid release cache: {reason}");
                cache_is_invalid = true;
                None
            }
            Err(error) => return Err(error),
        }
    };
    let mut releases = match cached {
        Some(releases) => releases,
        None => match remote.releases() {
            Ok(releases) => {
                if let Err(error) = store.write_release_cache(remote.source(), &releases) {
                    eprintln!("warning: could not update release cache: {error}");
                }
                releases
            }
            Err(error) => {
                let stale = if cache_is_invalid {
                    None
                } else {
                    match store.load_release_cache(remote.source(), None) {
                        Ok(stale) => stale,
                        Err(AvmError::InvalidCache(reason)) => {
                            eprintln!("warning: ignoring invalid release cache: {reason}");
                            None
                        }
                        Err(cache_error) => return Err(cache_error),
                    }
                };
                if let Some(stale) = stale {
                    eprintln!("warning: {error}; using stale cached release metadata");
                    stale
                } else {
                    return Err(error);
                }
            }
        },
    };
    releases.retain(|release| {
        release.parsed_version().is_some()
            && !release.draft
            && (include_prerelease || !release.is_prerelease())
    });
    releases.sort_by_key(|release| std::cmp::Reverse(release.parsed_version()));
    Ok(releases)
}

pub(crate) fn release_matches_query(release: &Release, query: &str) -> bool {
    match Selector::parse(query) {
        Ok(Selector::LatestStable) => !release.is_prerelease(),
        Ok(Selector::Major { major }) => release
            .parsed_version()
            .is_some_and(|version| version.major == major),
        Ok(Selector::Minor { major, minor }) => release
            .parsed_version()
            .is_some_and(|version| version.major == major && version.minor == minor),
        Ok(Selector::Exact(version)) => release.tag_name == version,
        Err(_) => release
            .tag_name
            .to_ascii_lowercase()
            .contains(&query.to_ascii_lowercase()),
    }
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
    fn stable_queries_use_domain_prerelease_status() {
        assert!(release_matches_query(
            &release("v3.4.5", false),
            version::STABLE
        ));
        assert!(!release_matches_query(
            &release("v3.5.0-rc.1", false),
            version::STABLE
        ));
        assert!(!release_matches_query(
            &release("v3.4.5", true),
            version::STABLE
        ));
    }
}
