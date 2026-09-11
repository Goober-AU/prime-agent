//! Port of packages/coding-agent/src/utils/version-check.ts

use std::time::Duration;

use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::update_source::{PRIME_AGENT_UPDATE_RELEASE_URL, PRIME_AGENT_UPDATE_REPOSITORY_URL};

const DEFAULT_PRIME_AGENT_DOWNLOAD_BASE_URL: &str =
    "https://github.com/telemusai/prime-agent/releases/latest/download";
const MAIN_VERSION_MANIFEST_PATH: &str = "main.json";
const DEFAULT_VERSION_CHECK_TIMEOUT_MS: u64 = 10000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestPiRelease {
    pub version: String,
    pub package_name: String,
    pub install_spec: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedVersion {
    pub major: i64,
    pub minor: i64,
    pub patch: i64,
    pub prerelease: Option<String>,
}

fn compare_prerelease_identifiers(left_prerelease: &str, right_prerelease: &str) -> i64 {
    let left_identifiers: Vec<&str> = left_prerelease.split('.').collect();
    let right_identifiers: Vec<&str> = right_prerelease.split('.').collect();
    let length = left_identifiers.len().max(right_identifiers.len());

    for index in 0..length {
        let left = left_identifiers.get(index).copied();
        let right = right_identifiers.get(index).copied();
        if left == right {
            continue;
        }
        let Some(left) = left else { return -1 };
        let Some(right) = right else { return 1 };

        let left_is_numeric = is_all_digits(left);
        let right_is_numeric = is_all_digits(right);
        if left_is_numeric && right_is_numeric {
            let left_number = strip_leading_zeros(left);
            let right_number = strip_leading_zeros(right);
            if left_number.len() != right_number.len() {
                return left_number.len() as i64 - right_number.len() as i64;
            }
            if left_number != right_number {
                return if left_number < right_number { -1 } else { 1 };
            }
            continue;
        }
        if left_is_numeric {
            return -1;
        }
        if right_is_numeric {
            return 1;
        }
        if left != right {
            return if left < right { -1 } else { 1 };
        }
    }

    0
}

fn is_all_digits(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
}

fn strip_leading_zeros(value: &str) -> &str {
    let trimmed = value.trim_start_matches('0');
    if trimmed.is_empty() {
        "0"
    } else {
        trimmed
    }
}

fn parse_package_version(version: &str) -> Option<ParsedVersion> {
    // /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+.*)?$/
    let trimmed = version.trim();
    let body = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let (core, rest) = split_once_any(body, &['-', '+']);
    let core_parts: Vec<&str> = core.split('.').collect();
    if core_parts.len() != 3 {
        return None;
    }
    let mut numbers: Vec<i64> = Vec::new();
    for part in &core_parts {
        if !is_all_digits(part) {
            return None;
        }
        numbers.push(part.parse().ok()?);
    }

    let prerelease = match rest {
        None => None,
        Some((separator, tail)) => {
            if separator == '+' {
                None
            } else {
                let identifier = tail.split('+').next().unwrap_or(tail);
                if identifier.is_empty()
                    || !identifier
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
                {
                    return None;
                }
                Some(identifier.to_string())
            }
        }
    };

    Some(ParsedVersion {
        major: numbers[0],
        minor: numbers[1],
        patch: numbers[2],
        prerelease,
    })
}

fn split_once_any<'a>(value: &'a str, separators: &[char]) -> (&'a str, Option<(char, &'a str)>) {
    match value.find(|character| separators.contains(&character)) {
        Some(index) => {
            let separator = value[index..].chars().next().expect("separator char");
            (&value[..index], Some((separator, &value[index + 1..])))
        }
        None => (value, None),
    }
}

pub fn compare_package_versions(left_version: &str, right_version: &str) -> Option<i64> {
    let left = parse_package_version(left_version)?;
    let right = parse_package_version(right_version)?;

    if left.major != right.major {
        return Some(left.major - right.major);
    }
    if left.minor != right.minor {
        return Some(left.minor - right.minor);
    }
    if left.patch != right.patch {
        return Some(left.patch - right.patch);
    }
    if left.prerelease == right.prerelease {
        return Some(0);
    }
    let Some(left_prerelease) = left.prerelease else {
        return Some(1);
    };
    let Some(right_prerelease) = right.prerelease else {
        return Some(-1);
    };
    Some(compare_prerelease_identifiers(&left_prerelease, &right_prerelease))
}

pub fn is_newer_package_version(candidate_version: &str, current_version: &str) -> bool {
    if let Some(comparison) = compare_package_versions(candidate_version, current_version) {
        return comparison > 0;
    }
    candidate_version.trim() != current_version.trim()
}

pub fn is_main_build_update_available(candidate_version: &str, current_version: &str) -> bool {
    if let Some(current) = parse_package_version(current_version) {
        if current
            .prerelease
            .as_deref()
            .map(|prerelease| prerelease.starts_with("telemusai.main."))
            .unwrap_or(false)
        {
            return is_newer_package_version(candidate_version, current_version);
        }
    }
    normalize_release_version(candidate_version) != normalize_release_version(current_version)
}

fn get_prime_agent_download_base_url() -> String {
    let configured = std::env::var("PRIME_AGENT_DOWNLOAD_BASE_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let base = configured.unwrap_or_else(|| DEFAULT_PRIME_AGENT_DOWNLOAD_BASE_URL.to_string());
    base.trim_end_matches('/').to_string()
}

fn normalize_release_version(version: &str) -> String {
    let trimmed = version.trim();
    trimmed.strip_prefix('v').unwrap_or(trimmed).to_string()
}

fn resolve_release_url(base_url: &str, path_or_url: &str) -> Option<String> {
    let trimmed = path_or_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let base = reqwest::Url::parse(&format!("{}/", base_url)).ok()?;
    let url = base.join(trimmed).ok()?;
    match url.scheme() {
        "https" | "http" => Some(url.to_string()),
        _ => None,
    }
}

pub async fn get_latest_pi_release(
    current_version: &str,
    timeout_ms: Option<u64>,
) -> Option<LatestPiRelease> {
    if std::env::var("PI_SKIP_VERSION_CHECK").is_ok() || std::env::var("PI_OFFLINE").is_ok() {
        return None;
    }

    let base_url = get_prime_agent_download_base_url();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(
            timeout_ms.unwrap_or(DEFAULT_VERSION_CHECK_TIMEOUT_MS),
        ))
        .build()
        .ok()?;
    let response = client
        .get(format!("{}/{}", base_url, MAIN_VERSION_MANIFEST_PATH))
        .header("User-Agent", get_pi_user_agent(current_version))
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }

    let data: serde_json::Value = response.json().await.ok()?;
    let Some(object) = data.as_object() else {
        return None;
    };
    if object.get("channel").and_then(|value| value.as_str()) != Some("main") {
        return None;
    }
    let Some(version_value) = object.get("version").and_then(|value| value.as_str()) else {
        return None;
    };
    if parse_package_version(version_value).is_none() {
        return None;
    }
    if object.get("package").and_then(|value| value.as_str()) != Some("prime-agent") {
        return None;
    }
    let Some(tarball) = object.get("tarball").and_then(|value| value.as_str()) else {
        return None;
    };

    let version = normalize_release_version(version_value);
    let install_spec = resolve_release_url(&base_url, tarball)?;
    let expected_suffix = format!("/prime-agent-{}.tgz", version);
    let parsed_spec = reqwest::Url::parse(&install_spec).ok()?;
    if !parsed_spec.path().ends_with(&expected_suffix) {
        return None;
    }
    let configured_base = std::env::var("PRIME_AGENT_DOWNLOAD_BASE_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if configured_base.is_none()
        && install_spec
            != format!(
                "{}/releases/download/v{}/prime-agent-{}.tgz",
                PRIME_AGENT_UPDATE_REPOSITORY_URL, version, version
            )
    {
        return None;
    }

    Some(LatestPiRelease {
        version,
        package_name: object
            .get("package")
            .and_then(|value| value.as_str())
            .unwrap_or("prime-agent")
            .to_string(),
        install_spec,
    })
}

pub async fn get_latest_pi_version(current_version: &str, timeout_ms: Option<u64>) -> Option<String> {
    get_latest_pi_release(current_version, timeout_ms)
        .await
        .map(|release| release.version)
}

pub async fn check_for_new_pi_version(current_version: &str) -> Option<String> {
    let latest_version = get_latest_pi_version(current_version, None).await;
    // A main build may have a lower semver than an installed stable or custom build.
    match latest_version {
        Some(latest) if is_main_build_update_available(&latest, current_version) => Some(latest),
        _ => None,
    }
}

/// Keeps the constant reachable for callers that mirror the TypeScript exports.
pub fn default_download_base_url() -> &'static str {
    let _ = PRIME_AGENT_UPDATE_RELEASE_URL;
    DEFAULT_PRIME_AGENT_DOWNLOAD_BASE_URL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_package_versions() {
        assert!(compare_package_versions("0.70.6", "0.70.5").unwrap() > 0);
        assert_eq!(compare_package_versions("0.70.5", "0.70.5").unwrap(), 0);
        assert!(compare_package_versions("0.70.4", "0.70.5").unwrap() < 0);
        assert!(compare_package_versions("0.70.5-beta.10.1.abcdef0", "0.70.5-beta.9.1.1234567").unwrap() > 0);
        assert!(!is_newer_package_version("0.70.5", "0.70.5"));
        assert!(is_newer_package_version("0.70.6", "0.70.5"));
    }

    #[test]
    fn parses_versions_and_rejects_garbage() {
        let parsed = parse_package_version("v1.2.3").unwrap();
        assert_eq!((parsed.major, parsed.minor, parsed.patch), (1, 2, 3));
        assert_eq!(parsed.prerelease, None);
        assert_eq!(
            parse_package_version("1.2.3-beta.4.5.abcdef").unwrap().prerelease,
            Some("beta.4.5.abcdef".to_string())
        );
        assert!(parse_package_version("invalid").is_none());
        assert!(parse_package_version("1.2").is_none());
    }

    #[test]
    fn moves_stable_installs_to_main_builds() {
        assert!(is_main_build_update_available(
            "1.2.4-telemusai.main.10.abcdef012",
            "1.2.4"
        ));
        assert!(!is_main_build_update_available(
            "1.2.4-telemusai.main.10.abcdef012",
            "1.2.4-telemusai.main.10.abcdef012"
        ));
        assert!(!is_main_build_update_available(
            "1.2.4-telemusai.main.10.abcdef012",
            "1.2.4-telemusai.main.11.123456789"
        ));
        assert!(is_main_build_update_available(
            "1.2.4-telemusai.main.10.abcdef012",
            "1.2.4-telemusai.main.9.123456789"
        ));
    }

    #[test]
    fn resolves_release_urls_against_the_base() {
        assert_eq!(
            resolve_release_url("https://github.com/x/y", "releases/v1/a.tgz"),
            Some("https://github.com/x/y/releases/v1/a.tgz".to_string())
        );
        assert_eq!(
            resolve_release_url("https://github.com/x/y", "https://cdn.example.com/a.tgz"),
            Some("https://cdn.example.com/a.tgz".to_string())
        );
        assert_eq!(resolve_release_url("https://github.com/x/y", ""), None);
        assert_eq!(resolve_release_url("https://github.com/x/y", "file:///tmp/a.tgz"), None);
    }

    #[test]
    fn normalizes_release_versions() {
        assert_eq!(normalize_release_version("v1.2.4"), "1.2.4");
        assert_eq!(normalize_release_version(" 1.2.4 "), "1.2.4");
    }

    #[test]
    fn base_url_defaults_and_environment_override() {
        let expected = "https://github.com/telemusai/prime-agent/releases/latest/download";
        assert_eq!(default_download_base_url(), expected);
        std::env::remove_var("PRIME_AGENT_DOWNLOAD_BASE_URL");
        assert_eq!(get_prime_agent_download_base_url(), expected);
        std::env::set_var("PRIME_AGENT_DOWNLOAD_BASE_URL", "http://127.0.0.1:18188/");
        assert_eq!(get_prime_agent_download_base_url(), "http://127.0.0.1:18188");
        std::env::remove_var("PRIME_AGENT_DOWNLOAD_BASE_URL");
    }

    #[tokio::test]
    async fn skips_fetching_when_version_checks_are_disabled() {
        std::env::set_var("PI_SKIP_VERSION_CHECK", "1");
        assert!(get_latest_pi_version("1.2.3", None).await.is_none());
        std::env::remove_var("PI_SKIP_VERSION_CHECK");

        std::env::set_var("PI_OFFLINE", "1");
        assert!(get_latest_pi_release("1.2.3", Some(10)).await.is_none());
        std::env::remove_var("PI_OFFLINE");
    }
}
