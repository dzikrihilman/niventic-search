use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

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
    let lower = name.to_lowercase();
    if lower.contains("uninstall")
        || lower.contains("readme")
        || lower.contains("help")
        || lower.contains("license")
        || lower.contains("changelog")
        || lower.contains("release notes")
    {
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
