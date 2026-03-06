use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default = "default_run_at_startup")]
    pub run_at_startup: bool,
    pub hotkey: HotkeyConfig,
    #[serde(default)]
    pub appearance: AppearanceConfig,
    #[serde(default)]
    pub quick_access: Vec<QuickAccessItem>,
}

fn default_run_at_startup() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HotkeyConfig {
    pub modifier: String,
    pub key: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppearanceConfig {
    pub width: u32,
    pub height: u32,
    pub font: String,
    pub background: String,
    pub border_radius: f32,
    pub border_width: f32,
    pub border_color: String,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
}

fn default_opacity() -> f32 {
    0.9
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct QuickAccessItem {
    pub name: String,
    pub path: String,
    pub icon: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            run_at_startup: false,
            hotkey: HotkeyConfig {
                modifier: "Alt".to_string(),
                key: "Space".to_string(),
            },
            appearance: AppearanceConfig::default(),
            quick_access: QuickAccessItem::defaults(),
        }
    }
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            width: 800,
            height: 500,
            font: "JetBrainsMono Nerd Font".to_string(),
            background: "#2d2d30".to_string(),
            border_radius: 14.0,
            border_width: 0.5,
            border_color: "#a9a9a9".to_string(),
            opacity: 0.9,
        }
    }
}

impl QuickAccessItem {
    pub fn defaults() -> Vec<Self> {
        vec![
            QuickAccessItem {
                name: "Browser".to_string(),
                path: "https://".to_string(),
                icon: "world.svg".to_string(),
            },
            QuickAccessItem {
                name: "Terminal".to_string(),
                path: "wt".to_string(),
                icon: "terminal.svg".to_string(),
            },
            QuickAccessItem {
                name: "Files".to_string(),
                path: "explorer".to_string(),
                icon: "folder-open.svg".to_string(),
            },
            QuickAccessItem {
                name: "Settings".to_string(),
                path: "ms-settings:".to_string(),
                icon: "settings-2.svg".to_string(),
            },
            QuickAccessItem {
                name: "Code".to_string(),
                path: "code".to_string(),
                icon: "code.svg".to_string(),
            },
        ]
    }
}

/// Returns the path to the config directory: %APPDATA%/niventic/
pub fn config_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("niventic")
}

/// Returns the path to the icons directory: %APPDATA%/niventic/icons/
pub fn icons_dir() -> PathBuf {
    config_dir().join("icons")
}

/// Returns the path to the config file: %APPDATA%/niventic/config.toml
fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Load the configuration from disk, or create a default one if it doesn't exist.
pub fn load_config() -> AppConfig {
    let path = config_path();

    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str::<AppConfig>(&contents) {
                Ok(mut config) => {
                    // Populate defaults if quick_access is empty
                    if config.quick_access.is_empty() {
                        config.quick_access = QuickAccessItem::defaults();
                    }
                    return config;
                }
                Err(e) => {
                    eprintln!("[niventic] Failed to parse config: {e}. Using defaults.");
                }
            },
            Err(e) => {
                eprintln!("[niventic] Failed to read config file: {e}. Using defaults.");
            }
        }
    } else {
        let config = AppConfig::default();
        save_config(&config);
        eprintln!("[niventic] Created default config at: {}", path.display());
        return config;
    }

    AppConfig::default()
}

/// Save configuration to disk.
pub fn save_config(config: &AppConfig) {
    let dir = config_dir();
    if fs::create_dir_all(&dir).is_ok() {
        let toml_str = toml::to_string_pretty(config).unwrap_or_default();
        let _ = fs::write(config_path(), toml_str);
        eprintln!("[niventic] Config saved.");
    }
}

/// Parse the modifier string into Win32 HOT_KEY_MODIFIERS flags.
pub fn parse_modifier(modifier_str: &str) -> HOT_KEY_MODIFIERS {
    let mut flags = HOT_KEY_MODIFIERS(0);
    for part in modifier_str.split('+') {
        match part.trim().to_lowercase().as_str() {
            "alt" => flags |= MOD_ALT,
            "ctrl" | "control" => flags |= MOD_CONTROL,
            "shift" => flags |= MOD_SHIFT,
            "win" | "super" | "meta" => flags |= MOD_WIN,
            other => eprintln!("[niventic] Unknown modifier: {other}"),
        }
    }
    flags |= MOD_NOREPEAT;
    flags
}

/// Parse a key name string into a Win32 VIRTUAL_KEY code.
pub fn parse_key(key_str: &str) -> u32 {
    match key_str.trim().to_lowercase().as_str() {
        "space" => VK_SPACE.0 as u32,
        "enter" | "return" => VK_RETURN.0 as u32,
        "tab" => VK_TAB.0 as u32,
        "escape" | "esc" => VK_ESCAPE.0 as u32,
        "backspace" => VK_BACK.0 as u32,
        "delete" | "del" => VK_DELETE.0 as u32,
        "insert" | "ins" => VK_INSERT.0 as u32,
        "home" => VK_HOME.0 as u32,
        "end" => VK_END.0 as u32,
        "pageup" => VK_PRIOR.0 as u32,
        "pagedown" => VK_NEXT.0 as u32,
        "f1" => VK_F1.0 as u32,
        "f2" => VK_F2.0 as u32,
        "f3" => VK_F3.0 as u32,
        "f4" => VK_F4.0 as u32,
        "f5" => VK_F5.0 as u32,
        "f6" => VK_F6.0 as u32,
        "f7" => VK_F7.0 as u32,
        "f8" => VK_F8.0 as u32,
        "f9" => VK_F9.0 as u32,
        "f10" => VK_F10.0 as u32,
        "f11" => VK_F11.0 as u32,
        "f12" => VK_F12.0 as u32,
        s if s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() => {
            s.to_uppercase().as_bytes()[0] as u32
        }
        s if s.len() == 1 && s.as_bytes()[0].is_ascii_digit() => s.as_bytes()[0] as u32,
        other => {
            eprintln!("[niventic] Unknown key: {other}. Defaulting to Space.");
            VK_SPACE.0 as u32
        }
    }
}