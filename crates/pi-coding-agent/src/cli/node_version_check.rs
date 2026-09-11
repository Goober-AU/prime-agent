//! Port of packages/coding-agent/src/cli/node-version-check.ts
//!
//! Keep this module and its constant imports Node-20-safe for the versions it rejects.

// TODO(slice): `crate::utils::update_source::PRIME_AGENT_UPDATE_RELEASE_URL` is a
// separate slice (ca-utils) and is not landed, so the same literal is repeated
// here; the TypeScript imports it from ../utils/update-source.js.
const PRIME_AGENT_UPDATE_RELEASE_URL: &str = "https://github.com/telemusai/prime-agent/releases/latest";

const MIN_NODE_VERSION_PARTS: [i64; 3] = [22, 8, 0];
pub const MIN_NODE_VERSION: &str = "22.8.0";

/// Callback surface of the TypeScript `NodeVersionGuardIO`.
pub struct NodeVersionGuardIo<'a> {
    pub version: String,
    pub log: &'a dyn Fn(&str),
    pub exit: &'a dyn Fn(i32),
}

struct ParsedNodeVersion {
    parts: [i64; 3],
    prerelease: bool,
}

/// `^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$`
fn parse_version(version: &str) -> Option<ParsedNodeVersion> {
    let trimmed = version.strip_prefix('v').unwrap_or(version);
    let (core, rest) = match trimmed.find(['-', '+']) {
        Some(index) => (&trimmed[..index], &trimmed[index..]),
        None => (trimmed, ""),
    };
    let mut numbers = [0i64; 3];
    let mut parts = core.split('.');
    for slot in numbers.iter_mut() {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse::<i64>().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }

    let mut prerelease = false;
    if !rest.is_empty() {
        let (suffix, plus) = match rest.find('+') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        if !suffix.is_empty() {
            if !suffix.starts_with('-') || suffix.len() == 1 {
                return None;
            }
            if !suffix[1..]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
            {
                return None;
            }
            prerelease = true;
        }
        if !plus.is_empty()
            && (plus.len() == 1
                || !plus[1..]
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-'))
        {
            return None;
        }
    }

    Some(ParsedNodeVersion { parts: numbers, prerelease })
}

fn is_supported_node_version(version: &ParsedNodeVersion) -> bool {
    for index in 0..MIN_NODE_VERSION_PARTS.len() {
        let part = version.parts[index];
        let minimum_part = MIN_NODE_VERSION_PARTS[index];
        if part != minimum_part {
            return part > minimum_part;
        }
    }
    !version.prerelease
}

/// `process.versions.bun`: the port is not Bun, so this is always false.
fn is_bun_runtime() -> bool {
    false
}

pub fn assert_node_version(io: &NodeVersionGuardIo<'_>) -> bool {
    // Bun ships its own runtime; its node-compat version is unrelated to the user's Node.
    if is_bun_runtime() {
        return true;
    }

    match parse_version(&io.version) {
        Some(version) if is_supported_node_version(&version) => true,
        None => true,
        Some(_) => {
            (io.log)(&format!(
                "prime-agent requires Node {} or newer, but the active Node is v{}.",
                MIN_NODE_VERSION, io.version
            ));
            (io.log)("");
            (io.log)(&format!(
                "  1. Install Node {}+ (e.g. \"nvm install 22 && nvm use 22\", or from https://nodejs.org)",
                MIN_NODE_VERSION
            ));
            (io.log)("  2. Reinstall prime-agent under that Node so the command resolves to it:");
            (io.log)(&format!("     {}", PRIME_AGENT_UPDATE_RELEASE_URL));
            (io.exit)(1);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn run(version: &str) -> (bool, Vec<String>, RefCell<Vec<i32>>) {
        let log = RefCell::new(Vec::new());
        let exits = RefCell::new(Vec::new());
        let log_fn = |message: &str| log.borrow_mut().push(message.to_string());
        let exit_fn = |code: i32| exits.borrow_mut().push(code);
        let io = NodeVersionGuardIo { version: version.to_string(), log: &log_fn, exit: &exit_fn };
        let result = assert_node_version(&io);
        (result, log.into_inner(), exits)
    }

    #[test]
    fn minimum_version_constant_matches_the_parts() {
        assert_eq!(MIN_NODE_VERSION, MIN_NODE_VERSION_PARTS.map(|part| part.to_string()).join("."));
    }

    #[test]
    fn parses_versions_like_the_regex() {
        assert_eq!(parse_version("22.8.0").map(|v| v.parts), Some([22, 8, 0]));
        assert_eq!(parse_version("v22.8.0").map(|v| v.parts), Some([22, 8, 0]));
        assert_eq!(parse_version("22.8").map(|v| v.parts), None);
        assert_eq!(parse_version("v22.8.0-rc.1").map(|v| v.prerelease), Some(true));
        assert_eq!(parse_version("22.8.0+build").map(|v| v.prerelease), Some(false));
        assert_eq!(parse_version("22.8.0-rc.1+build").map(|v| v.prerelease), Some(true));
        assert_eq!(parse_version("nope").map(|v| v.parts), None);
        assert_eq!(parse_version("22.8.0.1").map(|v| v.parts), None);
    }

    #[test]
    fn accepts_the_minimum_and_newer_versions() {
        let (result, log, exits) = run("22.8.0");
        assert!(result);
        assert!(log.is_empty());
        assert!(exits.borrow().is_empty());

        let (result, _, _) = run("23.0.0");
        assert!(result);
        let (result, _, _) = run("22.9.0");
        assert!(result);
        let (result, _, _) = run("v22.10.1");
        assert!(result);
    }

    #[test]
    fn rejects_older_versions_and_exits_with_one() {
        let (result, log, exits) = run("20.11.0");
        assert!(!result);
        assert_eq!(exits.borrow().as_slice(), &[1]);
        assert_eq!(
            log[0],
            "prime-agent requires Node 22.8.0 or newer, but the active Node is v20.11.0."
        );
        assert_eq!(log[1], "");
        assert_eq!(
            log[2],
            "  1. Install Node 22.8.0+ (e.g. \"nvm install 22 && nvm use 22\", or from https://nodejs.org)"
        );
        assert_eq!(log[3], "  2. Reinstall prime-agent under that Node so the command resolves to it:");
        assert_eq!(log[4], "     https://github.com/telemusai/prime-agent/releases/latest");
        assert_eq!(log.len(), 5);
    }

    #[test]
    fn rejects_the_minimum_prerelease() {
        let (result, _, exits) = run("22.8.0-rc.1");
        assert!(!result);
        assert_eq!(exits.borrow().as_slice(), &[1]);
    }

    #[test]
    fn ignores_unparseable_versions() {
        let (result, log, exits) = run("not-a-version");
        assert!(result);
        assert!(log.is_empty());
        assert!(exits.borrow().is_empty());
    }
}
