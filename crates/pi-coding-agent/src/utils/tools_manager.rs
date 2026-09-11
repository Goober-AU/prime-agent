//! Port of packages/coding-agent/src/utils/tools-manager.ts

use std::path::{Path, PathBuf};

use base64::Engine as _;
use futures_util::StreamExt;

use super::child_process::{spawn_sync_hidden, SpawnOptions};

const NETWORK_TIMEOUT_MS: u64 = 10_000;
const DOWNLOAD_TIMEOUT_MS: u64 = 120_000;
const COMMAND_TIMEOUT_MS: u64 = 5_000;
const RIPGREP_INSTALL_URL: &str = "https://github.com/BurntSushi/ripgrep#installation";

pub type ManagedTool = &'static str;

pub const FD: ManagedTool = "fd";
pub const RG: ManagedTool = "rg";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolUnavailableReason {
    Offline,
    ManualInstallRequired,
    UnsupportedPlatform,
    DownloadFailed,
}

impl ToolUnavailableReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolUnavailableReason::Offline => "offline",
            ToolUnavailableReason::ManualInstallRequired => "manual_install_required",
            ToolUnavailableReason::UnsupportedPlatform => "unsupported_platform",
            ToolUnavailableReason::DownloadFailed => "download_failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolAvailableResult {
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct ToolUnavailableResult {
    pub reason: ToolUnavailableReason,
    pub platform: String,
    pub architecture: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ToolEnsureResult {
    Available(ToolAvailableResult),
    Unavailable(ToolUnavailableResult),
}

impl ToolEnsureResult {
    pub fn status(&self) -> &'static str {
        match self {
            ToolEnsureResult::Available(_) => "available",
            ToolEnsureResult::Unavailable(_) => "unavailable",
        }
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            ToolEnsureResult::Available(result) => Some(&result.path),
            ToolEnsureResult::Unavailable(_) => None,
        }
    }
}

fn is_offline_mode_enabled() -> bool {
    match std::env::var("PI_OFFLINE") {
        Ok(value) if !value.is_empty() => {
            value == "1" || value.to_lowercase() == "true" || value.to_lowercase() == "yes"
        }
        _ => false,
    }
}

pub struct ToolConfig {
    pub name: &'static str,
    pub repo: &'static str,
    pub binary_name: &'static str,
    pub system_binary_names: &'static [&'static str],
    pub tag_prefix: &'static str,
}

pub fn tool_config(tool: ManagedTool) -> Option<&'static ToolConfig> {
    match tool {
        FD => Some(&FD_CONFIG),
        RG => Some(&RG_CONFIG),
        _ => None,
    }
}

static FD_CONFIG: ToolConfig = ToolConfig {
    name: "fd",
    repo: "sharkdp/fd",
    binary_name: "fd",
    system_binary_names: &["fd", "fdfind"],
    tag_prefix: "v",
};

static RG_CONFIG: ToolConfig = ToolConfig {
    name: "ripgrep",
    repo: "BurntSushi/ripgrep",
    binary_name: "rg",
    system_binary_names: &[],
    tag_prefix: "",
};

fn arch_str_for(architecture: &str) -> Option<&'static str> {
    match architecture {
        "arm64" => Some("aarch64"),
        "x64" => Some("x86_64"),
        _ => None,
    }
}

/// `getAssetName(version, plat, architecture)` for the fd tool.
pub fn fd_asset_name(version: &str, plat: &str, architecture: &str) -> Option<String> {
    match plat {
        "darwin" => {
            let arch = arch_str_for(architecture)?;
            Some(format!("fd-v{}-{}-apple-darwin.tar.gz", version, arch))
        }
        "linux" => {
            let arch = arch_str_for(architecture)?;
            Some(format!("fd-v{}-{}-unknown-linux-gnu.tar.gz", version, arch))
        }
        "win32" => {
            let arch = arch_str_for(architecture)?;
            Some(format!("fd-v{}-{}-pc-windows-msvc.zip", version, arch))
        }
        _ => None,
    }
}

/// `getAssetName(version, plat, architecture)` for the ripgrep tool.
pub fn rg_asset_name(version: &str, plat: &str, architecture: &str) -> Option<String> {
    match plat {
        "darwin" => {
            let arch = arch_str_for(architecture)?;
            Some(format!("ripgrep-{}-{}-apple-darwin.tar.gz", version, arch))
        }
        "linux" => {
            if architecture == "arm64" {
                return Some(format!("ripgrep-{}-aarch64-unknown-linux-gnu.tar.gz", version));
            }
            if architecture == "x64" {
                return Some(format!("ripgrep-{}-x86_64-unknown-linux-musl.tar.gz", version));
            }
            None
        }
        "win32" => {
            let arch = arch_str_for(architecture)?;
            Some(format!("ripgrep-{}-{}-pc-windows-msvc.zip", version, arch))
        }
        _ => None,
    }
}

pub fn get_asset_name(tool: ManagedTool, version: &str, plat: &str, architecture: &str) -> Option<String> {
    match tool {
        FD => fd_asset_name(version, plat, architecture),
        RG => rg_asset_name(version, plat, architecture),
        _ => None,
    }
}

/// `getBinDir()` from config.ts: `<agentDir>/bin`.
///
/// config.ts belongs to another slice; this minimal local definition keeps the
/// dependency explicit until `pi_coding_agent::config::get_bin_dir` exists.
pub fn get_bin_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("PI_BIN_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let agent_dir = std::env::var("PI_CODING_AGENT_DIR")
        .or_else(|_| std::env::var("PRIME_AGENT_CODING_AGENT_DIR"))
        .unwrap_or_else(|_| {
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| ".".to_string());
            format!("{}{}{}", home, std::path::MAIN_SEPARATOR, ".prime/agent")
        });
    PathBuf::from(agent_dir).join("bin")
}

/// `APP_NAME` from config.ts, used for the GitHub User-Agent header.
pub fn app_name() -> String {
    std::env::var("PI_CONFIG_NAME").unwrap_or_else(|_| "pi".to_string())
}

// Check that a command both launches and reports a successful version.
pub fn command_works(cmd: &str) -> bool {
    let options = SpawnOptions {
        capture_stdout: true,
        capture_stderr: true,
        ..Default::default()
    };
    let result = spawn_sync_hidden_with_timeout(cmd, &["--version".to_string()], options, COMMAND_TIMEOUT_MS);
    match result {
        Some(output) => output.status.success(),
        None => false,
    }
}

fn spawn_sync_hidden_with_timeout(
    command: &str,
    args: &[String],
    options: SpawnOptions,
    timeout_ms: u64,
) -> Option<std::process::Output> {
    let handle = std::thread::spawn({
        let command = command.to_string();
        let args = args.to_vec();
        move || spawn_sync_hidden(&command, &args, options)
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while !handle.is_finished() {
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    handle.join().ok()?.ok()
}

// Get the path to a tool (system-wide or in our tools dir)
pub fn get_tool_path(tool: ManagedTool) -> Option<String> {
    let config = tool_config(tool)?;
    let tools_dir = get_bin_dir();

    // Check our tools directory first
    let local_path = tools_dir.join(format!(
        "{}{}",
        config.binary_name,
        if super::pi_user_agent::process_platform() == "win32" {
            ".exe"
        } else {
            ""
        }
    ));
    let local_path_string = local_path.to_string_lossy().to_string();
    if local_path.exists() && command_works(&local_path_string) {
        return Some(local_path_string);
    }

    // Check system PATH - if found, just return the command name (it's in PATH)
    let system_binary_names: Vec<&str> = if config.system_binary_names.is_empty() {
        vec![config.binary_name]
    } else {
        config.system_binary_names.to_vec()
    };
    for system_binary_name in system_binary_names {
        if command_works(system_binary_name) {
            return Some(system_binary_name.to_string());
        }
    }

    None
}

// Fetch latest release version from GitHub
async fn get_latest_version(repo: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(NETWORK_TIMEOUT_MS))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(format!("https://api.github.com/repos/{}/releases/latest", repo))
        .header("User-Agent", format!("{}-coding-agent", app_name()))
        .send()
        .await
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        return Err(format!("GitHub API error: {}", response.status().as_u16()));
    }

    let data: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    let tag_name = data
        .get("tag_name")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "GitHub API error: missing tag_name".to_string())?;
    Ok(tag_name.strip_prefix('v').unwrap_or(tag_name).to_string())
}

// Download a file from URL
async fn download_file(url: &str, dest: &Path) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(DOWNLOAD_TIMEOUT_MS))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client.get(url).send().await.map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        return Err(format!("Failed to download: {}", response.status().as_u16()));
    }

    let mut file = tokio::fs::File::create(dest).await.map_err(|error| error.to_string())?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub fn find_binary_recursively(root_dir: &Path, binary_file_name: &str) -> Option<PathBuf> {
    let mut stack: Vec<PathBuf> = vec![root_dir.to_path_buf()];

    while let Some(current_dir) = stack.pop() {
        let entries = match std::fs::read_dir(&current_dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let full_path = current_dir.join(entry.file_name());
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_file() && entry.file_name().to_string_lossy() == binary_file_name {
                return Some(full_path);
            }
            if file_type.is_dir() {
                stack.push(full_path);
            }
        }
    }

    None
}

#[derive(Debug)]
pub struct UnsupportedToolPlatformError(pub String);

impl std::fmt::Display for UnsupportedToolPlatformError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// `extract-zip` equivalent for the archive formats used by this module.
fn extract_zip_archive(archive_path: &Path, extract_dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive_path).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let name = match entry.enclosed_name() {
            Some(name) => name.to_path_buf(),
            None => continue,
        };
        let out_path = extract_dir.join(name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|error| error.to_string())?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let mut out_file = std::fs::File::create(&out_path).map_err(|error| error.to_string())?;
        std::io::copy(&mut entry, &mut out_file).map_err(|error| error.to_string())?;
    }
    Ok(())
}

// Download and install a tool
async fn download_tool(tool: ManagedTool) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let config = tool_config(tool).ok_or_else(|| format!("Unknown tool: {}", tool))?;

    let plat = super::pi_user_agent::process_platform();
    let architecture = super::pi_user_agent::process_arch();
    let tools_dir = get_bin_dir();

    if get_asset_name(tool, "VERSION", plat, architecture).is_none() {
        return Err(Box::new(UnsupportedToolPlatformError(format!(
            "Unsupported platform: {}/{}",
            plat, architecture
        ))));
    }

    // Get latest version and the matching platform asset.
    let version = get_latest_version(config.repo).await?;
    let asset_name = get_asset_name(tool, &version, plat, architecture).ok_or_else(|| {
        Box::new(UnsupportedToolPlatformError(format!(
            "Unsupported platform: {}/{}",
            plat, architecture
        ))) as Box<dyn std::error::Error + Send + Sync>
    })?;

    // Create tools directory
    std::fs::create_dir_all(&tools_dir)?;

    let download_url = format!(
        "https://github.com/{}/releases/download/{}{}/{}",
        config.repo, config.tag_prefix, version, asset_name
    );
    let archive_path = tools_dir.join(&asset_name);
    let binary_ext = if plat == "win32" { ".exe" } else { "" };
    let binary_path = tools_dir.join(format!("{}{}", config.binary_name, binary_ext));

    // Download
    download_file(&download_url, &archive_path).await?;

    // Extract into a unique temp directory. fd and rg downloads can run concurrently
    // during startup, so sharing a fixed directory causes races.
    let extract_dir = tools_dir.join(format!(
        "extract_tmp_{}_{}_{}_{}",
        config.binary_name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0),
        short_random_suffix()
    ));
    std::fs::create_dir_all(&extract_dir)?;

    let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = (|| {
        if asset_name.ends_with(".tar.gz") {
            let extract_result = spawn_sync_hidden(
                "tar",
                &[
                    "xzf".to_string(),
                    archive_path.to_string_lossy().to_string(),
                    "-C".to_string(),
                    extract_dir.to_string_lossy().to_string(),
                ],
                SpawnOptions {
                    capture_stdout: true,
                    capture_stderr: true,
                    ..Default::default()
                },
            );
            match extract_result {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    let err_msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    let err_msg = if err_msg.is_empty() {
                        "unknown error".to_string()
                    } else {
                        err_msg
                    };
                    return Err(format!("Failed to extract {}: {}", asset_name, err_msg).into());
                }
                Err(error) => {
                    return Err(format!("Failed to extract {}: {}", asset_name, error).into());
                }
            }
        } else if asset_name.ends_with(".zip") {
            extract_zip_archive(&archive_path, &extract_dir)?;
        } else {
            return Err(format!("Unsupported archive format: {}", asset_name).into());
        }

        // Find the binary in extracted files. Some archives contain files directly
        // at root, others nest under a versioned subdirectory.
        let binary_file_name = format!("{}{}", config.binary_name, binary_ext);
        let stripped = asset_name
            .strip_suffix(".tar.gz")
            .or_else(|| asset_name.strip_suffix(".zip"))
            .unwrap_or(&asset_name);
        let extracted_dir = extract_dir.join(stripped);
        let candidates = vec![
            extracted_dir.join(&binary_file_name),
            extract_dir.join(&binary_file_name),
        ];
        let mut extracted_binary = candidates.into_iter().find(|candidate| candidate.exists());

        if extracted_binary.is_none() {
            extracted_binary = find_binary_recursively(&extract_dir, &binary_file_name);
        }

        match extracted_binary {
            Some(extracted_binary) => {
                let _ = std::fs::remove_file(&binary_path);
                std::fs::rename(&extracted_binary, &binary_path)?;
            }
            None => {
                return Err(format!(
                    "Binary not found in archive: expected {} under {}",
                    binary_file_name,
                    extract_dir.to_string_lossy()
                )
                .into());
            }
        }

        // Make executable (Unix only)
        if plat != "win32" {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        if !command_works(&binary_path.to_string_lossy()) {
            let _ = std::fs::remove_file(&binary_path);
            return Err(format!("Installed {} binary failed its version check", config.name).into());
        }

        Ok(())
    })();

    // Cleanup
    let _ = std::fs::remove_file(&archive_path);
    let _ = std::fs::remove_dir_all(&extract_dir);
    result?;

    Ok(binary_path.to_string_lossy().to_string())
}

fn short_random_suffix() -> String {
    let bytes: [u8; 6] = uuid::Uuid::new_v4().into_bytes()[..6].try_into().unwrap();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// Termux package names for tools
pub fn termux_package(tool: ManagedTool) -> String {
    match tool {
        FD => "fd".to_string(),
        RG => "ripgrep".to_string(),
        other => other.to_string(),
    }
}

pub fn get_ripgrep_install_hint(platform_name: &str) -> String {
    match platform_name {
        "darwin" => "Install it with: brew install ripgrep".to_string(),
        "linux" => format!(
            "Install it with your package manager (for example, sudo apt install ripgrep or sudo dnf install ripgrep). See {}",
            RIPGREP_INSTALL_URL
        ),
        "win32" => "Install it with: winget install BurntSushi.ripgrep.MSVC".to_string(),
        "android" => "Install it with: pkg install ripgrep".to_string(),
        _ => format!("Install ripgrep manually: {}", RIPGREP_INSTALL_URL),
    }
}

pub fn format_missing_ripgrep_message(result: &ToolUnavailableResult) -> String {
    let reason = match result.reason {
        ToolUnavailableReason::Offline => {
            "Automatic installation was skipped because PI_OFFLINE is enabled.".to_string()
        }
        ToolUnavailableReason::ManualInstallRequired => {
            "Prime Agent cannot install this helper automatically in Termux.".to_string()
        }
        ToolUnavailableReason::UnsupportedPlatform => format!(
            "Automatic installation is unavailable for {}/{}.",
            result.platform, result.architecture
        ),
        ToolUnavailableReason::DownloadFailed => {
            let detail = result
                .detail
                .as_ref()
                .map(|detail| detail.split_whitespace().collect::<Vec<&str>>().join(" "))
                .filter(|detail| !detail.is_empty());
            match detail {
                Some(detail) => format!("Prime Agent could not install it automatically: {}", detail),
                None => "Prime Agent could not install it automatically.".to_string(),
            }
        }
    };

    [
        "ripgrep (rg) is an optional search helper. Without it, model-run file searches may be slower or fail; Prime Agent and subagents remain available.".to_string(),
        reason,
        get_ripgrep_install_hint(&result.platform),
    ]
    .join("\n")
}

// Ensure a tool is available, downloading if necessary, and retain why provisioning failed.
pub async fn ensure_tool_with_status(tool: ManagedTool, silent: bool) -> ToolEnsureResult {
    if let Some(existing_path) = get_tool_path(tool) {
        return ToolEnsureResult::Available(ToolAvailableResult { path: existing_path });
    }

    let config = match tool_config(tool) {
        Some(config) => config,
        None => {
            return ToolEnsureResult::Unavailable(ToolUnavailableResult {
                reason: ToolUnavailableReason::UnsupportedPlatform,
                platform: super::pi_user_agent::process_platform().to_string(),
                architecture: super::pi_user_agent::process_arch().to_string(),
                detail: Some(format!("Unknown tool: {}", tool)),
            })
        }
    };
    let platform_name = super::pi_user_agent::process_platform();
    let architecture = super::pi_user_agent::process_arch();

    if is_offline_mode_enabled() {
        if !silent {
            println!(
                "\u{1b}[33m{} not found. Offline mode enabled, skipping download.\u{1b}[39m",
                config.name
            );
        }
        return ToolEnsureResult::Unavailable(ToolUnavailableResult {
            reason: ToolUnavailableReason::Offline,
            platform: platform_name.to_string(),
            architecture: architecture.to_string(),
            detail: None,
        });
    }

    // On Android/Termux, Linux binaries don't work due to Bionic libc incompatibility.
    // Users must install via pkg.
    if platform_name == "android" {
        let pkg_name = termux_package(tool);
        if !silent {
            println!(
                "\u{1b}[33m{} not found. Install with: pkg install {}\u{1b}[39m",
                config.name, pkg_name
            );
        }
        return ToolEnsureResult::Unavailable(ToolUnavailableResult {
            reason: ToolUnavailableReason::ManualInstallRequired,
            platform: platform_name.to_string(),
            architecture: architecture.to_string(),
            detail: None,
        });
    }

    // Tool not found - download it
    if !silent {
        println!("\u{1b}[2m{} not found. Downloading...\u{1b}[22m", config.name);
    }

    match download_tool(tool).await {
        Ok(path) => {
            if !silent {
                println!("\u{1b}[2m{} installed to {}\u{1b}[22m", config.name, path);
            }
            ToolEnsureResult::Available(ToolAvailableResult { path })
        }
        Err(error) => {
            let message = error.to_string();
            if !silent {
                println!("\u{1b}[33mFailed to download {}: {}\u{1b}[39m", config.name, message);
            }
            let reason = if error.downcast_ref::<UnsupportedToolPlatformError>().is_some() {
                ToolUnavailableReason::UnsupportedPlatform
            } else {
                ToolUnavailableReason::DownloadFailed
            };
            ToolEnsureResult::Unavailable(ToolUnavailableResult {
                reason,
                platform: platform_name.to_string(),
                architecture: architecture.to_string(),
                detail: Some(message),
            })
        }
    }
}

// Compatibility wrapper for callers that only need the resolved executable path.
pub async fn ensure_tool(tool: ManagedTool, silent: bool) -> Option<String> {
    let result = ensure_tool_with_status(tool, silent).await;
    match result {
        ToolEnsureResult::Available(result) => Some(result.path),
        ToolEnsureResult::Unavailable(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names_match_the_typescript_tables() {
        assert_eq!(
            fd_asset_name("9.0.0", "darwin", "arm64").unwrap(),
            "fd-v9.0.0-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            fd_asset_name("9.0.0", "linux", "x64").unwrap(),
            "fd-v9.0.0-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            fd_asset_name("9.0.0", "win32", "x64").unwrap(),
            "fd-v9.0.0-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(fd_asset_name("9.0.0", "linux", "ia32"), None);
        assert_eq!(fd_asset_name("9.0.0", "freebsd", "x64"), None);

        assert_eq!(
            rg_asset_name("14.1.0", "linux", "arm64").unwrap(),
            "ripgrep-14.1.0-aarch64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            rg_asset_name("14.1.0", "linux", "x64").unwrap(),
            "ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            rg_asset_name("14.1.0", "win32", "arm64").unwrap(),
            "ripgrep-14.1.0-aarch64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn offline_mode_detection_matches_the_typescript() {
        std::env::remove_var("PI_OFFLINE");
        assert!(!is_offline_mode_enabled());
        std::env::set_var("PI_OFFLINE", "1");
        assert!(is_offline_mode_enabled());
        std::env::set_var("PI_OFFLINE", "TRUE");
        assert!(is_offline_mode_enabled());
        std::env::set_var("PI_OFFLINE", "yes");
        assert!(is_offline_mode_enabled());
        std::env::set_var("PI_OFFLINE", "0");
        assert!(!is_offline_mode_enabled());
        std::env::remove_var("PI_OFFLINE");
    }

    #[test]
    fn ripgrep_install_hints_match_the_typescript() {
        assert_eq!(get_ripgrep_install_hint("darwin"), "Install it with: brew install ripgrep");
        assert_eq!(
            get_ripgrep_install_hint("win32"),
            "Install it with: winget install BurntSushi.ripgrep.MSVC"
        );
        assert_eq!(get_ripgrep_install_hint("android"), "Install it with: pkg install ripgrep");
        assert!(get_ripgrep_install_hint("linux").contains("sudo apt install ripgrep"));
        assert!(get_ripgrep_install_hint("solaris").starts_with("Install ripgrep manually: "));
    }

    #[test]
    fn missing_ripgrep_message_uses_the_reason() {
        let offline = ToolUnavailableResult {
            reason: ToolUnavailableReason::Offline,
            platform: "linux".to_string(),
            architecture: "x64".to_string(),
            detail: None,
        };
        let message = format_missing_ripgrep_message(&offline);
        assert!(message.starts_with("ripgrep (rg) is an optional search helper."));
        assert!(message.contains("PI_OFFLINE is enabled."));
        assert!(message.ends_with(&get_ripgrep_install_hint("linux")));

        let failed = ToolUnavailableResult {
            reason: ToolUnavailableReason::DownloadFailed,
            platform: "linux".to_string(),
            architecture: "x64".to_string(),
            detail: Some("  network\n  down  ".to_string()),
        };
        assert!(format_missing_ripgrep_message(&failed).contains("could not install it automatically: network down"));

        let failed_no_detail = ToolUnavailableResult {
            detail: None,
            ..failed.clone()
        };
        assert!(format_missing_ripgrep_message(&failed_no_detail)
            .contains("Prime Agent could not install it automatically.\n"));
    }

    #[test]
    fn find_binary_recursively_walks_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("fd-v1.0.0-x86_64-pc-windows-msvc");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("fd.exe"), b"binary").unwrap();
        let found = find_binary_recursively(dir.path(), "fd.exe").unwrap();
        assert_eq!(found, nested.join("fd.exe"));
        assert!(find_binary_recursively(dir.path(), "missing.exe").is_none());
    }

    #[test]
    fn termux_package_names_match_the_typescript() {
        assert_eq!(termux_package(FD), "fd");
        assert_eq!(termux_package(RG), "ripgrep");
    }

    #[tokio::test]
    async fn offline_mode_skips_the_download() {
        std::env::set_var("PI_OFFLINE", "1");
        std::env::set_var("PI_BIN_DIR", tempfile::tempdir().unwrap().path().to_string_lossy().to_string());
        let result = ensure_tool_with_status(RG, true).await;
        std::env::remove_var("PI_OFFLINE");
        std::env::remove_var("PI_BIN_DIR");
        match result {
            ToolEnsureResult::Unavailable(unavailable) => {
                assert_eq!(unavailable.reason, ToolUnavailableReason::Offline)
            }
            other => panic!("expected unavailable, got {:?}", other.status()),
        }
    }
}
