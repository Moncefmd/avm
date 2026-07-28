use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::thread;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_LENGTH, HeaderMap, HeaderValue, LINK, RETRY_AFTER, USER_AGENT,
};
use semver::Version;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use url::Url;

use crate::error::{AvmError, Result};
use crate::version;

pub const DEFAULT_API_URL: &str = "https://api.github.com/repos/argoproj/argo-cd/releases";
pub const MAX_BINARY_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_API_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 1024 * 1024;
const MAX_ERROR_RESPONSE_BYTES: u64 = 16 * 1024;
const API_VERSION: &str = "2022-11-28";
const MAX_RETRIES: usize = 3;

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

#[derive(Debug, Eq, PartialEq)]
pub struct Download {
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone)]
pub struct GitHubClient {
    api: Client,
    download: Client,
    api_url: String,
    api_origin: Url,
}

impl GitHubClient {
    pub fn from_env() -> Result<Self> {
        let api_url =
            std::env::var("AVM_GITHUB_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_owned());
        let token = std::env::var("AVM_GITHUB_TOKEN")
            .ok()
            .or_else(|| std::env::var("GH_TOKEN").ok())
            .or_else(|| std::env::var("GITHUB_TOKEN").ok());
        Self::new(api_url, token.as_deref())
    }

    pub fn new(api_url: impl Into<String>, token: Option<&str>) -> Result<Self> {
        let api_url = api_url.into().trim_end_matches('/').to_owned();
        validate_network_url(&api_url)?;
        let api_origin = Url::parse(&api_url)
            .map_err(|error| AvmError::Message(format!("invalid network URL: {error}")))?;
        if !api_origin.username().is_empty()
            || api_origin.password().is_some()
            || api_origin.query().is_some()
            || api_origin.fragment().is_some()
        {
            return Err(AvmError::Message(
                "the GitHub API base URL cannot contain credentials, a query, or a fragment"
                    .to_owned(),
            ));
        }

        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&format!("avm/{}", env!("CARGO_PKG_VERSION")))
                .map_err(|error| AvmError::Message(error.to_string()))?,
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(API_VERSION),
        );

        if let Some(token) = token.filter(|token| !token.is_empty()) {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| AvmError::Message("GitHub token contains invalid bytes".to_owned()))?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }

        let api = Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(api_redirect_policy(api_origin.clone()))
            .build()
            .map_err(AvmError::http)?;

        let download = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(15 * 60))
            .redirect(download_redirect_policy())
            .build()
            .map_err(AvmError::http)?;

        Ok(Self {
            api,
            download,
            api_url,
            api_origin,
        })
    }

    pub fn source(&self) -> &str {
        &self.api_url
    }

    pub fn release(&self, tag: &str) -> Result<Release> {
        let tag = version::normalize(tag)?;
        let url = format!("{}/tags/{tag}", self.api_url);
        self.get_release(&url, &tag)
    }

    fn get_release(&self, url: &str, label: &str) -> Result<Release> {
        match self.send_api_get(url) {
            Ok(response) => decode_json(response),
            Err(AvmError::Api { status: 404, .. }) => {
                Err(AvmError::ReleaseNotFound(label.to_owned()))
            }
            Err(error) => Err(error),
        }
    }

    pub fn releases(&self) -> Result<Vec<Release>> {
        let mut releases = Vec::new();
        let mut next = Some(format!("{}?per_page=100", self.api_url));
        let mut pages = 0usize;

        while let Some(url) = next {
            pages += 1;
            if pages > 100 {
                return Err(AvmError::Message(
                    "GitHub pagination exceeded the 100-page safety limit".to_owned(),
                ));
            }

            let response = self.send_api_get(&url)?;
            next = next_link(response.headers());
            let mut page: Vec<Release> = decode_json(response)?;
            let page_was_empty = page.is_empty();
            releases.append(&mut page);
            if page_was_empty {
                break;
            }
        }

        Ok(releases)
    }

    pub fn expected_checksum(&self, release: &Release, asset_name: &str) -> Result<Option<String>> {
        let asset = release
            .asset(asset_name)
            .ok_or_else(|| AvmError::AssetNotFound {
                version: release.tag_name.clone(),
                asset: asset_name.to_owned(),
            })?;

        if let Some(digest) = asset.digest.as_deref() {
            return Ok(Some(parse_api_digest(digest, &release.tag_name)?));
        }

        let mut checksum_assets: Vec<&Asset> = release
            .assets
            .iter()
            .filter(|candidate| {
                candidate.name == "cli_checksums.txt"
                    || (candidate.name.starts_with("argocd-")
                        && candidate.name.ends_with("-checksums.txt"))
            })
            .collect();
        checksum_assets.sort_by_key(|candidate| candidate.name != "cli_checksums.txt");

        let Some(checksum_asset) = checksum_assets.first() else {
            return Ok(None);
        };

        let manifest =
            self.download_text(&checksum_asset.browser_download_url, MAX_CHECKSUM_BYTES)?;
        parse_checksum_manifest(&manifest, asset_name, &release.tag_name)
    }

    pub fn download_to(&self, url: &str, path: &Path) -> Result<Download> {
        validate_network_url(url)?;
        let mut response = self.send_get(&self.download, url)?;

        if let Some(length) = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            && length > MAX_BINARY_BYTES
        {
            return Err(AvmError::DownloadTooLarge);
        }

        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(|error| AvmError::io(format!("create {}", path.display()), error))?;
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 128 * 1024];

        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|error| AvmError::network_io("reading the download", error))?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(read as u64);
            if total > MAX_BINARY_BYTES {
                return Err(AvmError::DownloadTooLarge);
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .map_err(|error| AvmError::io(format!("write {}", path.display()), error))?;
        }

        file.sync_all()
            .map_err(|error| AvmError::io(format!("sync {}", path.display()), error))?;

        let digest = hasher.finalize();
        Ok(Download {
            bytes: total,
            sha256: hex_digest(&digest),
        })
    }

    fn download_text(&self, url: &str, limit: u64) -> Result<String> {
        validate_network_url(url)?;
        let response = self.send_get(&self.download, url)?;
        let mut bytes = Vec::new();
        response
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| AvmError::network_io("reading the checksum manifest", error))?;
        if bytes.len() as u64 > limit {
            return Err(AvmError::MalformedChecksum(
                "checksum manifest is unexpectedly large".to_owned(),
            ));
        }
        String::from_utf8(bytes)
            .map_err(|_| AvmError::MalformedChecksum("manifest is not UTF-8".to_owned()))
    }

    fn send_get(&self, client: &Client, url: &str) -> Result<Response> {
        validate_network_url(url)?;
        let mut last_error = None;

        for attempt in 0..MAX_RETRIES {
            match client.get(url).send() {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response)
                    if retryable_status(response.status()) && attempt + 1 < MAX_RETRIES =>
                {
                    let delay = retry_delay(&response, attempt);
                    last_error = Some(api_error(response));
                    thread::sleep(delay);
                }
                Ok(response) => return Err(api_error(response)),
                Err(error)
                    if (error.is_timeout() || error.is_connect()) && attempt + 1 < MAX_RETRIES =>
                {
                    last_error = Some(AvmError::http(error));
                    thread::sleep(Duration::from_millis(250 * (1 << attempt)));
                }
                Err(error) => return Err(AvmError::http(error)),
            }
        }

        Err(last_error.unwrap_or_else(|| AvmError::Message("request failed".to_owned())))
    }

    fn send_api_get(&self, url: &str) -> Result<Response> {
        let parsed = Url::parse(url)
            .map_err(|error| AvmError::Message(format!("invalid network URL: {error}")))?;
        if !same_origin(&self.api_origin, &parsed) {
            return Err(AvmError::Message(format!(
                "refusing a cross-origin GitHub API URL: {}",
                sanitized_url(url)
            )));
        }
        self.send_get(&self.api, url)
    }
}

fn api_redirect_policy(origin: Url) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.error("too many redirects");
        }
        if is_secure_or_loopback(attempt.url()) && same_origin(&origin, attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("refusing a cross-origin or insecure API redirect")
        }
    })
}

fn download_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.error("too many redirects");
        }
        if is_secure_or_loopback(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("refusing an insecure HTTP redirect")
        }
    })
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn validate_network_url(raw: &str) -> Result<()> {
    let url = Url::parse(raw)
        .map_err(|error| AvmError::Message(format!("invalid network URL: {error}")))?;
    if is_secure_or_loopback(&url) {
        Ok(())
    } else {
        Err(AvmError::Message(format!(
            "refusing insecure network URL: {}",
            sanitized_url(raw)
        )))
    }
}

pub fn sanitized_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return "<invalid URL>".to_owned();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn is_secure_or_loopback(url: &Url) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    if url.scheme() != "http" {
        return false;
    }
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
}

fn retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn retry_delay(response: &Response, attempt: usize) -> Duration {
    let header_delay = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds.min(5)));
    header_delay.unwrap_or_else(|| Duration::from_millis(250 * (1 << attempt)))
}

fn api_error(mut response: Response) -> AvmError {
    let status = response.status();
    let reset = response
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .map(|value| format!(" (reset epoch: {value})"))
        .unwrap_or_default();
    let is_rate_limited = status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN
            && response
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|value| value.to_str().ok())
                == Some("0"));

    if is_rate_limited {
        return AvmError::RateLimited { reset };
    }

    let mut bytes = Vec::new();
    let message = match response
        .by_ref()
        .take(MAX_ERROR_RESPONSE_BYTES)
        .read_to_end(&mut bytes)
    {
        Ok(_) if !bytes.is_empty() => String::from_utf8_lossy(&bytes).into_owned(),
        _ => status
            .canonical_reason()
            .unwrap_or("request failed")
            .to_owned(),
    };
    let message: String = message.chars().take(500).collect();
    AvmError::Api {
        status: status.as_u16(),
        message,
    }
}

fn decode_json<T: DeserializeOwned>(response: Response) -> Result<T> {
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .take(MAX_API_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| AvmError::network_io("reading the GitHub API response", error))?;
    if bytes.len() as u64 > MAX_API_RESPONSE_BYTES {
        return Err(AvmError::Api {
            status,
            message: "response exceeded the 32 MiB safety limit".to_owned(),
        });
    }
    serde_json::from_slice(&bytes).map_err(|error| AvmError::Api {
        status,
        message: format!("invalid JSON response: {error}"),
    })
}

fn next_link(headers: &HeaderMap) -> Option<String> {
    let links = headers.get(LINK)?.to_str().ok()?;
    links.split(',').find_map(|part| {
        let mut sections = part.trim().split(';');
        let url = sections.next()?.trim();
        let is_next = sections.any(|section| section.trim() == r#"rel="next""#);
        is_next.then(|| {
            url.strip_prefix('<')
                .and_then(|value| value.strip_suffix('>'))
                .unwrap_or(url)
                .to_owned()
        })
    })
}

fn parse_api_digest(digest: &str, release: &str) -> Result<String> {
    let Some(value) = digest.strip_prefix("sha256:") else {
        return Err(AvmError::MalformedChecksum(release.to_owned()));
    };
    validate_sha256(value)
        .then(|| value.to_ascii_lowercase())
        .ok_or_else(|| AvmError::MalformedChecksum(release.to_owned()))
}

pub fn parse_checksum_manifest(
    manifest: &str,
    asset_name: &str,
    release: &str,
) -> Result<Option<String>> {
    let mut matched = None;

    for line in manifest.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let checksum = fields
            .next()
            .ok_or_else(|| AvmError::MalformedChecksum(release.to_owned()))?;
        let filename = fields
            .next()
            .ok_or_else(|| AvmError::MalformedChecksum(release.to_owned()))?
            .trim_start_matches('*');
        if fields.next().is_some() || !validate_sha256(checksum) {
            return Err(AvmError::MalformedChecksum(release.to_owned()));
        }

        if filename == asset_name {
            if matched.is_some() {
                return Err(AvmError::ChecksumConflict {
                    version: release.to_owned(),
                    asset: asset_name.to_owned(),
                });
            }
            matched = Some(checksum.to_ascii_lowercase());
        }
    }

    Ok(matched)
}

fn validate_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checksum_manifest_strictly() {
        let checksum = "a".repeat(64);
        let manifest = format!("{checksum}  argocd-linux-amd64\n");
        assert_eq!(
            parse_checksum_manifest(&manifest, "argocd-linux-amd64", "v3.4.5").unwrap(),
            Some(checksum)
        );
    }

    #[test]
    fn ignores_other_valid_manifest_entries() {
        let checksum = "b".repeat(64);
        let manifest = format!("{checksum}  argocd-darwin-amd64\n");
        assert_eq!(
            parse_checksum_manifest(&manifest, "argocd-linux-amd64", "v3.4.5").unwrap(),
            None
        );
    }

    #[test]
    fn rejects_duplicate_or_malformed_entries() {
        let checksum = "c".repeat(64);
        let duplicate = format!("{checksum}  argocd-linux-amd64\n{checksum}  argocd-linux-amd64\n");
        assert!(parse_checksum_manifest(&duplicate, "argocd-linux-amd64", "v3.4.5").is_err());
        assert!(parse_checksum_manifest("not-a-checksum file\n", "file", "v3.4.5").is_err());
    }

    #[test]
    fn extracts_next_pagination_link() {
        let mut headers = HeaderMap::new();
        headers.insert(
            LINK,
            HeaderValue::from_static(
                "<https://api.github.test/releases?page=2>; rel=\"next\", \
                 <https://api.github.test/releases?page=9>; rel=\"last\"",
            ),
        );
        assert_eq!(
            next_link(&headers).as_deref(),
            Some("https://api.github.test/releases?page=2")
        );
    }

    #[test]
    fn strips_secrets_from_urls() {
        assert_eq!(
            sanitized_url("https://user:secret@example.test/file?token=secret#fragment"),
            "https://example.test/file"
        );
    }

    #[test]
    fn rejects_cross_origin_pagination_links() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"[{"tag_name":"v1.0.0"}]"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nLink: <http://127.0.0.1:1/releases?page=2>; \
                 rel=\"next\"\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });

        let client =
            GitHubClient::new(format!("http://{address}/releases"), Some("sentinel")).unwrap();
        let error = client.releases().unwrap_err();
        assert!(error.to_string().contains("cross-origin"));
        server.join().unwrap();
    }
}
