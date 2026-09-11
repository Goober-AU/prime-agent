//! Port of packages/coding-agent/src/utils/git.ts

use std::path::{Path, PathBuf};

use super::child_process::{spawn_sync_hidden, SpawnOptions};

/// Parsed git URL information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSource {
    /// Always "git" for git sources
    pub kind: String,
    /// Clone URL (always valid for git clone, without ref suffix)
    pub repo: String,
    /// Git host domain (e.g., "github.com")
    pub host: String,
    /// Repository path (e.g., "user/repo")
    pub path: String,
    /// Git ref (branch, tag, commit) if specified
    pub reference: Option<String>,
    /// True if ref was specified (package won't be auto-updated)
    pub pinned: bool,
}

fn split_ref(url: &str) -> (String, Option<String>) {
    if let Some(captures) = scp_like_parts(url) {
        let (host, path_with_maybe_ref) = captures;
        let ref_separator = path_with_maybe_ref.find('@');
        let Some(ref_separator) = ref_separator else {
            return (url.to_string(), None);
        };
        let repo_path = &path_with_maybe_ref[..ref_separator];
        let reference = &path_with_maybe_ref[ref_separator + 1..];
        if repo_path.is_empty() || reference.is_empty() {
            return (url.to_string(), None);
        }
        return (
            format!("git@{}:{}", host, repo_path),
            Some(reference.to_string()),
        );
    }

    if url.contains("://") {
        if let Ok(parsed) = reqwest::Url::parse(url) {
            let path_with_maybe_ref = parsed.path().trim_start_matches('/').to_string();
            let ref_separator = path_with_maybe_ref.find('@');
            let Some(ref_separator) = ref_separator else {
                return (url.to_string(), None);
            };
            let repo_path = &path_with_maybe_ref[..ref_separator];
            let reference = &path_with_maybe_ref[ref_separator + 1..];
            if repo_path.is_empty() || reference.is_empty() {
                return (url.to_string(), None);
            }
            let mut rebuilt = parsed.clone();
            rebuilt.set_path(&format!("/{}", repo_path));
            let repo = rebuilt.to_string().trim_end_matches('/').to_string();
            return (repo, Some(reference.to_string()));
        }
        return (url.to_string(), None);
    }

    let Some(slash_index) = url.find('/') else {
        return (url.to_string(), None);
    };
    let host = &url[..slash_index];
    let path_with_maybe_ref = &url[slash_index + 1..];
    let Some(ref_separator) = path_with_maybe_ref.find('@') else {
        return (url.to_string(), None);
    };
    let repo_path = &path_with_maybe_ref[..ref_separator];
    let reference = &path_with_maybe_ref[ref_separator + 1..];
    if repo_path.is_empty() || reference.is_empty() {
        return (url.to_string(), None);
    }
    (format!("{}/{}", host, repo_path), Some(reference.to_string()))
}

/// `/^git@([^:]+):(.+)$/`
fn scp_like_parts(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("git@")?;
    let colon = rest.find(':')?;
    let host = &rest[..colon];
    let path = &rest[colon + 1..];
    if host.is_empty() || host.contains(':') || path.is_empty() {
        return None;
    }
    Some((host.to_string(), path.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostedInfo {
    domain: String,
    user: String,
    project: String,
    committish: Option<String>,
}

/// The hosted-git-info surface this module uses: known hosts only, `.git`
/// stripped from the project, optional `#committish`.
fn hosted_git_info_from_url(candidate: &str) -> Option<HostedInfo> {
    let (without_committish, committish) = match candidate.find('#') {
        Some(index) => (
            &candidate[..index],
            Some(candidate[index + 1..].to_string()),
        ),
        None => (candidate, None),
    };

    let (host, path) = if let Some((host, path)) = scp_like_parts(without_committish) {
        (host, path)
    } else {
        let lowered = without_committish.to_lowercase();
        let mut rest = without_committish;
        for scheme in ["https://", "http://", "ssh://", "git://", "git+https://", "git+ssh://"] {
            if lowered.starts_with(scheme) {
                rest = &without_committish[scheme.len()..];
                break;
            }
        }
        // Strip an optional `user@` credential prefix.
        if let Some(at_index) = rest.find('@') {
            let after_at = &rest[at_index + 1..];
            if after_at.contains('/') && !after_at.starts_with('/') {
                rest = after_at;
            }
        }
        let Some(slash_index) = rest.find('/') else {
            return None;
        };
        (rest[..slash_index].to_string(), rest[slash_index + 1..].to_string())
    };

    let host_lower = host.to_lowercase();
    let domain = match host_lower.as_str() {
        "github.com" | "www.github.com" | "gist.github.com" => host_lower.clone(),
        "gitlab.com" | "www.gitlab.com" => "gitlab.com".to_string(),
        "bitbucket.org" | "www.bitbucket.org" => "bitbucket.org".to_string(),
        _ => return None,
    };

    let path = path.trim_start_matches('/');
    let mut segments = path.splitn(2, '/');
    let user = segments.next().unwrap_or_default().to_string();
    let project_raw = segments.next().unwrap_or_default();
    if user.is_empty() || project_raw.is_empty() {
        return None;
    }
    let project = project_raw
        .split('/')
        .next()
        .unwrap_or(project_raw)
        .trim_end_matches(".git")
        .to_string();

    Some(HostedInfo {
        domain,
        user,
        project,
        committish,
    })
}

fn parse_generic_git_url(url: &str) -> Option<GitSource> {
    let (repo_without_ref, reference) = split_ref(url);
    let mut repo = repo_without_ref.clone();
    let mut host = String::new();
    let mut path = String::new();

    if let Some((scp_host, scp_path)) = scp_like_parts(&repo_without_ref) {
        host = scp_host;
        path = scp_path;
    } else if repo_without_ref.starts_with("https://")
        || repo_without_ref.starts_with("http://")
        || repo_without_ref.starts_with("ssh://")
        || repo_without_ref.starts_with("git://")
    {
        let parsed = reqwest::Url::parse(&repo_without_ref).ok()?;
        host = parsed.host_str().unwrap_or_default().to_string();
        path = parsed.path().trim_start_matches('/').to_string();
    } else {
        let slash_index = repo_without_ref.find('/')?;
        host = repo_without_ref[..slash_index].to_string();
        path = repo_without_ref[slash_index + 1..].to_string();
        if !host.contains('.') && host != "localhost" {
            return None;
        }
        repo = format!("https://{}", repo_without_ref);
    }

    let normalized_path = path
        .trim_end_matches(".git")
        .trim_start_matches('/')
        .to_string();
    if host.is_empty() || normalized_path.is_empty() || normalized_path.split('/').count() < 2 {
        return None;
    }

    Some(GitSource {
        kind: "git".to_string(),
        repo,
        host,
        path: normalized_path,
        pinned: reference.is_some(),
        reference,
    })
}

/// Parse git source into a GitSource.
///
/// Rules:
/// - With git: prefix, accept all historical shorthand forms.
/// - Without git: prefix, only accept explicit protocol URLs.
pub fn parse_git_url(source: &str) -> Option<GitSource> {
    let trimmed = source.trim();
    let has_git_prefix = trimmed.starts_with("git:");
    let url = if has_git_prefix {
        trimmed[4..].trim().to_string()
    } else {
        trimmed.to_string()
    };

    if !has_git_prefix && !is_protocol_url(&url) {
        return None;
    }

    let (split_repo, split_ref_value) = split_ref(&url);

    let mut hosted_candidates: Vec<String> = Vec::new();
    if let Some(reference) = &split_ref_value {
        hosted_candidates.push(format!("{}#{}", split_repo, reference));
    }
    hosted_candidates.push(url.clone());
    for candidate in hosted_candidates {
        if let Some(info) = hosted_git_info_from_url(&candidate) {
            if split_ref_value.is_some() && info.project.contains('@') {
                continue;
            }
            let use_https_prefix = !split_repo.starts_with("http://")
                && !split_repo.starts_with("https://")
                && !split_repo.starts_with("ssh://")
                && !split_repo.starts_with("git://")
                && !split_repo.starts_with("git@");
            let reference = info.committish.clone().or_else(|| split_ref_value.clone());
            return Some(GitSource {
                kind: "git".to_string(),
                repo: if use_https_prefix {
                    format!("https://{}", split_repo)
                } else {
                    split_repo.clone()
                },
                host: info.domain.clone(),
                path: format!("{}/{}", info.user, info.project).trim_end_matches(".git").to_string(),
                pinned: reference.is_some(),
                reference,
            });
        }
    }

    let mut https_candidates: Vec<String> = Vec::new();
    if let Some(reference) = &split_ref_value {
        https_candidates.push(format!("https://{}#{}", split_repo, reference));
    }
    https_candidates.push(format!("https://{}", url));
    for candidate in https_candidates {
        if let Some(info) = hosted_git_info_from_url(&candidate) {
            if split_ref_value.is_some() && info.project.contains('@') {
                continue;
            }
            let reference = info.committish.clone().or_else(|| split_ref_value.clone());
            return Some(GitSource {
                kind: "git".to_string(),
                repo: format!("https://{}", split_repo),
                host: info.domain.clone(),
                path: format!("{}/{}", info.user, info.project).trim_end_matches(".git").to_string(),
                pinned: reference.is_some(),
                reference,
            });
        }
    }

    parse_generic_git_url(&url)
}

/// `/^(https?|ssh|git):\/\//i`
fn is_protocol_url(url: &str) -> bool {
    let lowered = url.to_lowercase();
    lowered.starts_with("http://")
        || lowered.starts_with("https://")
        || lowered.starts_with("ssh://")
        || lowered.starts_with("git://")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPaths {
    pub repo_dir: String,
    pub common_git_dir: String,
    pub head_path: String,
}

/// Find git metadata paths by walking up from cwd.
/// Handles both regular git repos (.git is a directory) and worktrees (.git is a file).
pub fn find_git_paths(cwd: &str) -> Option<GitPaths> {
    let mut dir = PathBuf::from(cwd);
    loop {
        let git_path = dir.join(".git");
        if git_path.exists() {
            let Ok(stat) = std::fs::metadata(&git_path) else {
                return None;
            };
            if stat.is_file() {
                let Ok(content) = std::fs::read_to_string(&git_path) else {
                    return None;
                };
                let content = content.trim();
                if let Some(rest) = content.strip_prefix("gitdir: ") {
                    let git_dir = resolve_path(&dir, rest.trim());
                    let head_path = git_dir.join("HEAD");
                    if !head_path.exists() {
                        return None;
                    }
                    let common_dir_path = git_dir.join("commondir");
                    let common_git_dir = if common_dir_path.exists() {
                        let Ok(common) = std::fs::read_to_string(&common_dir_path) else {
                            return None;
                        };
                        resolve_path(&git_dir, common.trim())
                    } else {
                        git_dir.clone()
                    };
                    return Some(GitPaths {
                        repo_dir: dir.to_string_lossy().to_string(),
                        common_git_dir: common_git_dir.to_string_lossy().to_string(),
                        head_path: head_path.to_string_lossy().to_string(),
                    });
                }
            } else if stat.is_dir() {
                let head_path = git_path.join("HEAD");
                if !head_path.exists() {
                    return None;
                }
                return Some(GitPaths {
                    repo_dir: dir.to_string_lossy().to_string(),
                    common_git_dir: git_path.to_string_lossy().to_string(),
                    head_path: head_path.to_string_lossy().to_string(),
                });
            }
        }
        let Some(parent) = dir.parent().map(Path::to_path_buf) else {
            return None;
        };
        if parent == dir {
            return None;
        }
        dir = parent;
    }
}

fn resolve_path(base: &Path, candidate: &str) -> PathBuf {
    let candidate_path = Path::new(candidate);
    if candidate_path.is_absolute() {
        candidate_path.to_path_buf()
    } else {
        base.join(candidate_path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitContext {
    pub repo_url: Option<String>,
    pub commit: Option<String>,
    pub branch: Option<String>,
}

pub fn git_contexts_equal(a: &GitContext, b: &GitContext) -> bool {
    a.repo_url == b.repo_url && a.commit == b.commit && a.branch == b.branch
}

fn run_git(cwd: &str, args: &[&str]) -> Option<String> {
    let mut full_args: Vec<String> = vec!["--no-optional-locks".to_string()];
    full_args.extend(args.iter().map(|arg| arg.to_string()));
    let result = spawn_sync_hidden(
        "git",
        &full_args,
        SpawnOptions {
            cwd: Some(cwd.to_string()),
            capture_stdout: true,
            capture_stderr: false,
            ..Default::default()
        },
    );
    let Ok(output) = result else {
        return None;
    };
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

pub fn capture_git_context(cwd: &str) -> Option<GitContext> {
    let commit = run_git(cwd, &["rev-parse", "HEAD"]);
    let branch = run_git(cwd, &["branch", "--show-current"]);
    let remote = run_git(cwd, &["remote", "get-url", "origin"]);
    if commit.is_none() && branch.is_none() && remote.is_none() {
        return None;
    }

    let mut context = GitContext::default();
    if let Some(remote) = &remote {
        context.repo_url = Some(
            parse_git_url(remote)
                .map(|source| source.repo)
                .unwrap_or_else(|| remote.clone()),
        );
    }
    context.commit = commit;
    context.branch = branch;
    Some(context)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_protocol_urls() {
        let result = parse_git_url("https://github.com/user/repo").unwrap();
        assert_eq!(result.host, "github.com");
        assert_eq!(result.path, "user/repo");
        assert_eq!(result.repo, "https://github.com/user/repo");
        assert!(!result.pinned);
    }

    #[test]
    fn parses_protocol_urls_with_a_ref() {
        let result = parse_git_url("https://github.com/user/repo@v1.0.0").unwrap();
        assert_eq!(result.host, "github.com");
        assert_eq!(result.path, "user/repo");
        assert_eq!(result.reference.as_deref(), Some("v1.0.0"));
        assert_eq!(result.repo, "https://github.com/user/repo");
        assert!(result.pinned);
    }

    #[test]
    fn parses_shorthand_only_with_the_git_prefix() {
        let scp = parse_git_url("git:git@github.com:user/repo").unwrap();
        assert_eq!(scp.host, "github.com");
        assert_eq!(scp.path, "user/repo");
        assert_eq!(scp.repo, "git@github.com:user/repo");

        let shorthand = parse_git_url("git:github.com/user/repo").unwrap();
        assert_eq!(shorthand.host, "github.com");
        assert_eq!(shorthand.repo, "https://github.com/user/repo");

        let with_ref = parse_git_url("git:git@github.com:user/repo@v1.0.0").unwrap();
        assert_eq!(with_ref.host, "github.com");
        assert_eq!(with_ref.path, "user/repo");
        assert_eq!(with_ref.reference.as_deref(), Some("v1.0.0"));
        assert_eq!(with_ref.repo, "git@github.com:user/repo");
        assert!(with_ref.pinned);
    }

    #[test]
    fn rejects_unsupported_forms_without_the_git_prefix() {
        assert!(parse_git_url("git@github.com:user/repo").is_none());
        assert!(parse_git_url("github.com/user/repo").is_none());
        assert!(parse_git_url("user/repo").is_none());
    }

    #[test]
    fn parses_generic_unknown_hosts_with_the_git_prefix() {
        let result = parse_git_url("git:example.com/team/repo").unwrap();
        assert_eq!(result.host, "example.com");
        assert_eq!(result.path, "team/repo");
        assert_eq!(result.repo, "https://example.com/team/repo");
    }

    #[test]
    fn splits_scp_like_and_url_refs() {
        assert_eq!(
            split_ref("git@github.com:user/repo@main"),
            ("git@github.com:user/repo".to_string(), Some("main".to_string()))
        );
        assert_eq!(
            split_ref("https://github.com/user/repo@main"),
            ("https://github.com/user/repo".to_string(), Some("main".to_string()))
        );
        assert_eq!(split_ref("git@github.com:user/repo"), ("git@github.com:user/repo".to_string(), None));
    }

    #[test]
    fn finds_git_paths_for_a_regular_repository() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let nested = dir.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();

        let paths = find_git_paths(nested.to_str().unwrap()).unwrap();
        assert_eq!(paths.repo_dir, dir.path().to_string_lossy());
        assert_eq!(paths.common_git_dir, git_dir.to_string_lossy());
        assert_eq!(paths.head_path, git_dir.join("HEAD").to_string_lossy());
    }

    #[test]
    fn finds_git_paths_for_a_worktree_file() {
        let dir = tempfile::tempdir().unwrap();
        let real_git_dir = dir.path().join("real-git-dir");
        std::fs::create_dir_all(&real_git_dir).unwrap();
        std::fs::write(real_git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", real_git_dir.to_string_lossy()),
        )
        .unwrap();

        let paths = find_git_paths(worktree.to_str().unwrap()).unwrap();
        assert_eq!(paths.repo_dir, worktree.to_string_lossy());
        assert_eq!(paths.common_git_dir, real_git_dir.to_string_lossy());
    }

    #[test]
    fn returns_none_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("no-git-here");
        std::fs::create_dir_all(&nested).unwrap();
        // A missing HEAD inside a .git directory is a hard failure, not a search miss.
        let broken = dir.path().join("broken");
        std::fs::create_dir_all(broken.join(".git")).unwrap();
        assert!(find_git_paths(broken.to_str().unwrap()).is_none());
    }

    #[test]
    fn compares_git_contexts() {
        const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
        const SHA2: &str = "89abcdef0123456789abcdef0123456789abcdef";
        let base = GitContext {
            repo_url: None,
            commit: Some(SHA.to_string()),
            branch: Some("main".to_string()),
        };
        assert!(git_contexts_equal(&base, &base.clone()));
        let other = GitContext {
            commit: Some(SHA2.to_string()),
            ..base.clone()
        };
        assert!(!git_contexts_equal(&base, &other));
        let no_branch = GitContext {
            branch: None,
            ..base.clone()
        };
        assert!(!git_contexts_equal(&base, &no_branch));
    }

    #[test]
    fn captures_context_from_a_real_repository() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let mut full: Vec<String> = vec!["-C".to_string(), dir.path().to_string_lossy().to_string()];
            full.extend(args.iter().map(|arg| arg.to_string()));
            spawn_sync_hidden(
                "git",
                &full,
                SpawnOptions {
                    capture_stdout: true,
                    capture_stderr: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        if !run(&["init", "-q", "-b", "main"]).status.success() {
            return;
        }
        run(&["config", "user.email", "t@t.co"]);
        run(&["config", "user.name", "t"]);
        run(&[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ]);
        std::fs::write(dir.path().join("file.txt"), "init\n").unwrap();
        run(&["add", "-A"]);
        if !run(&["commit", "-q", "-m", "init"]).status.success() {
            return;
        }

        let context = capture_git_context(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(context.branch.as_deref(), Some("main"));
        assert_eq!(context.repo_url.as_deref(), Some("https://github.com/acme/widgets.git"));
        assert_eq!(context.commit.as_deref().map(|sha| sha.len()), Some(40));
    }

    #[test]
    fn returns_none_outside_a_git_repo_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(capture_git_context(dir.path().to_str().unwrap()).is_none());
    }
}
