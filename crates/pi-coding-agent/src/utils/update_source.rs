//! Port of packages/coding-agent/src/utils/update-source.ts

pub const PRIME_AGENT_UPDATE_REPOSITORY: &str = "telemusai/prime-agent";
pub const PRIME_AGENT_UPDATE_REPOSITORY_URL: &str = "https://github.com/telemusai/prime-agent";
pub const PRIME_AGENT_UPDATE_RELEASE_URL: &str =
    "https://github.com/telemusai/prime-agent/releases/latest";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_derive_from_the_repository() {
        assert_eq!(
            PRIME_AGENT_UPDATE_REPOSITORY_URL,
            format!("https://github.com/{}", PRIME_AGENT_UPDATE_REPOSITORY)
        );
        assert_eq!(
            PRIME_AGENT_UPDATE_RELEASE_URL,
            format!("{}/releases/latest", PRIME_AGENT_UPDATE_REPOSITORY_URL)
        );
    }
}
