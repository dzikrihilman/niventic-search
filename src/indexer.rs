use std::collections::HashMap;
use std::env;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Represents an indexed application entry.
#[derive(Debug, Clone)]
pub struct AppEntry {
    /// Display name (derived from the .lnk filename or exe name)
    pub name: String,
    /// Full path to the executable or shortcut
    pub target_path: String,
    /// The .lnk path (used for launching via shortcut)
    pub lnk_path: Option<String>,
}

/// Scan known Windows directories for application shortcuts and executables.
/// Returns a deduplicated list of AppEntry sorted by name.
pub fn scan_apps() -> Vec<AppEntry> {
    let mut entries: Vec<AppEntry> = Vec::new();

    // Directories to scan for .lnk shortcuts
    let mut search_dirs: Vec<PathBuf> = Vec::new();

    // Common Start Menu locations
    // User's Start Menu
    if let Ok(appdata) = env::var("APPDATA") {
        let user_start = PathBuf::from(&appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs");
        search_dirs.push(user_start);
    }

    // All Users / ProgramData Start Menu
    if let Ok(programdata) = env::var("PROGRAMDATA") {
        let common_start = PathBuf::from(&programdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs");
        search_dirs.push(common_start);
    }

    // Scan each directory recursively for .lnk files
    for dir in &search_dirs {
        if dir.exists() {
            scan_directory_recursive(dir, &mut entries);
        }
    }

    // Scan desktop shortcuts and executable files to catch apps not exposed in Start Menu.
    scan_desktop_entries(&mut entries);

    // Fallback source for UWP/Store apps via AppsFolder IDs.
    scan_uwp_apps(&mut entries);

    // Deduplicate by name (keep the first occurrence)
    let mut seen: HashMap<String, bool> = HashMap::new();
    entries.retain(|e| {
        let key = e.name.to_lowercase();
        if seen.contains_key(&key) {
            false
        } else {
            seen.insert(key, true);
            true
        }
    });

    // Sort alphabetically
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    eprintln!("[niventic] Indexed {} applications", entries.len());
    entries
}

/// Scan user/public desktop for .lnk and .exe entries.
/// This is intentionally non-recursive to avoid expensive deep filesystem traversal.
fn scan_desktop_entries(entries: &mut Vec<AppEntry>) {
    let mut desktop_dirs: Vec<PathBuf> = Vec::new();

    if let Some(user_desktop) = dirs::desktop_dir() {
        desktop_dirs.push(user_desktop);
    }

    if let Ok(public_dir) = env::var("PUBLIC") {
        desktop_dirs.push(PathBuf::from(public_dir).join("Desktop"));
    }

    for dir in desktop_dirs {
        if !dir.exists() {
            continue;
        }

        let read_dir = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }

            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();

            if ext == "lnk" {
                if let Some(app) = parse_lnk_file(&path) {
                    entries.push(app);
                }
                continue;
            }

            if ext == "exe" {
                if let Some(name) = path.file_stem().map(|s| s.to_string_lossy().to_string()) {
                    entries.push(AppEntry {
                        name,
                        target_path: path.to_string_lossy().to_string(),
                        lnk_path: None,
                    });
                }
            }
        }
    }
}

/// Scan UWP and Start apps using PowerShell `Get-StartApps`.
/// We map AppID to `shell:AppsFolder\\<AppID>` so launch uses the shell route.
fn scan_uwp_apps(entries: &mut Vec<AppEntry>) {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-StartApps | ForEach-Object {\"$($_.Name)`t$($_.AppID)\"}",
        ])
        // Avoid flashing a console window when background refresh runs.
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    let output = match output {
        Ok(out) if out.status.success() => out,
        _ => return,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut parts = trimmed.splitn(2, '\t');
        let name = parts.next().unwrap_or("").trim();
        let app_id = parts.next().unwrap_or("").trim();

        if name.is_empty() || app_id.is_empty() {
            continue;
        }

        // Skip noise entries similar to shortcut filtering.
        if is_noise_entry_name(name) {
            continue;
        }

        entries.push(AppEntry {
            name: name.to_string(),
            target_path: format!("shell:AppsFolder\\{}", app_id),
            lnk_path: None,
        });
    }
}

/// Recursively scan a directory for .lnk files and parse them.
fn scan_directory_recursive(dir: &Path, entries: &mut Vec<AppEntry>) {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };

    for entry in read_dir.flatten() {
        let path = entry.path();

        if path.is_dir() {
            scan_directory_recursive(&path, entries);
            continue;
        }

        if let Some(ext) = path.extension() {
            if ext.to_ascii_lowercase() == "lnk" {
                if let Some(app) = parse_lnk_file(&path) {
                    entries.push(app);
                }
            }
        }
    }
}

/// Parse a single .lnk file and extract the app name and target path.
fn parse_lnk_file(lnk_path: &Path) -> Option<AppEntry> {
    // Derive display name from the .lnk filename (without extension)
    let name = lnk_path.file_stem()?.to_string_lossy().to_string();

    // Skip common non-app shortcuts
    if is_noise_entry_name(&name) {
        return None;
    }

    // Try to resolve the target path from the .lnk file
    let target_path = resolve_lnk_target(lnk_path).unwrap_or_default();

    Some(AppEntry {
        name,
        target_path,
        lnk_path: Some(lnk_path.to_string_lossy().to_string()),
    })
}

/// Filter out obvious non-launcher shortcuts while preserving legit app names.
/// Example: keep "IObit Uninstaller", but skip "Uninstall Google Chrome".
fn is_noise_entry_name(name: &str) -> bool {
    let lower = name.to_lowercase();

    // Treat uninstall as a word token only, so "uninstaller" still passes.
    let has_uninstall_token = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| token == "uninstall");

    has_uninstall_token
        || lower.contains("readme")
        || lower.contains("help")
        || lower.contains("license")
        || lower.contains("changelog")
        || lower.contains("release notes")
}

/// Attempt to read and resolve the target executable path from a .lnk file.
fn resolve_lnk_target(lnk_path: &Path) -> Option<String> {
    // Try with default Windows encoding first
    let shell_link = lnk::ShellLink::open(lnk_path).ok()?;

    // Try link_info -> local_base_path first (most reliable)
    if let Some(link_info) = shell_link.link_info() {
        if let Some(base_path) = link_info.local_base_path() {
            return Some(base_path.to_string());
        }
        if let Some(base_path_unicode) = link_info.local_base_path_unicode() {
            return Some(base_path_unicode.to_string());
        }
    }

    // Fallback: try relative_path or working_dir
    if let Some(rel_path) = shell_link.relative_path() {
        return Some(rel_path.to_string());
    }

    None
}

/// Perform a fuzzy (case-insensitive substring) search over the app entries.
pub fn search<'a>(entries: &'a [AppEntry], query: &'a str) -> Vec<&'a AppEntry> {
    if query.is_empty() {
        return vec![];
    }

    let q = query.to_lowercase();
    let mut results: Vec<(&AppEntry, usize)> = entries
        .iter()
        .filter_map(|e| {
            let name_lower = e.name.to_lowercase();
            if name_lower.contains(&q) {
                // Score: prefer entries where the match starts earlier
                let score = name_lower.find(&q).unwrap_or(999);
                Some((e, score))
            } else {
                None
            }
        })
        .collect();

    // Sort by match quality (earlier match = better)
    results.sort_by_key(|(_, score)| *score);

    // Limit to 15 results for performance
    results.into_iter().take(15).map(|(e, _)| e).collect()
}
