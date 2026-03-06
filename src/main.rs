mod config;
mod hotkey;
mod icons;
mod indexer;

use slint::{Model, ModelRc, PhysicalPosition, SharedString, VecModel};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::rc::Rc;
use std::cell::RefCell;

slint::include_modules!();

/// Off-screen position to simulate hiding the window
const OFF_SCREEN: PhysicalPosition = PhysicalPosition::new(-9999, -9999);

/// Calculate center position dynamically based on primary monitor and window size
fn center_position(win_w: i32, win_h: i32) -> PhysicalPosition {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    let (screen_w, screen_h) = unsafe {
        (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
    };
    PhysicalPosition::new(
        (screen_w - win_w) / 2,
        (screen_h - win_h) / 3, // 1/3 from top, more natural for a launcher
    )
}

fn main() -> Result<(), slint::PlatformError> {
    // 1. Load user configuration
    let app_config = config::load_config();
    let modifiers = config::parse_modifier(&app_config.hotkey.modifier);
    let vk_code = config::parse_key(&app_config.hotkey.key);

    eprintln!(
        "[niventic] Hotkey configured: {} + {} (mod=0x{:X}, vk=0x{:X})",
        app_config.hotkey.modifier, app_config.hotkey.key, modifiers.0, vk_code
    );

    // 2. Scan installed applications at startup
    let all_apps = indexer::scan_apps();

    // 3. Create the Slint window
    let main_window = AppWindow::new()?;

    // Store config in shared Rc<RefCell> for callbacks
    let app_config_rc = Rc::new(RefCell::new(app_config));

    // Bind appearance config to Slint properties
    {
        let cfg = app_config_rc.borrow();
        bind_config_strings(&main_window, &cfg);
        apply_appearance(&main_window, &cfg.appearance);

        // Bind quick access entries
        let qa_entries: Vec<QuickAccessEntry> = cfg.quick_access.iter().map(|qa| {
            QuickAccessEntry {
                name: SharedString::from(&qa.name),
                path: SharedString::from(&qa.path),
                icon: SharedString::from(&qa.icon),
            }
        }).collect();
        let qa_model = Rc::new(VecModel::from(qa_entries));
        main_window.set_cfg_quick_access(ModelRc::from(qa_model));
    }

    // Prevent close from quitting the event loop
    main_window.window().on_close_requested(|| {
        slint::CloseRequestResponse::HideWindow
    });

    // Shared visibility flag
    let is_visible = std::rc::Rc::new(std::cell::Cell::new(true));

    // 4. Start global hotkey listener in background thread
    let hotkey_rx = hotkey::start_listener(modifiers, vk_code);

    // 5. Handle Escape key: move window off-screen
    let window_weak = main_window.as_weak();
    let vis = is_visible.clone();
    main_window.on_escape_pressed(move || {
        if let Some(w) = window_weak.upgrade() {
            w.window().set_position(OFF_SCREEN);
            vis.set(false);
            eprintln!("[niventic] Window hidden (Escape)");
        }
    });

    // 6. Handle search text changes: filter real apps
    let window_weak = main_window.as_weak();
    let apps_for_search = all_apps.clone();
    let icon_cache = std::rc::Rc::new(std::cell::RefCell::new(icons::IconCache::new()));
    let icon_cache_search = icon_cache.clone();
    main_window.on_search_changed(move |query| {
        if let Some(w) = window_weak.upgrade() {
            let results = indexer::search(&apps_for_search, &query);
            let mut cache = icon_cache_search.borrow_mut();
            let slint_results: Vec<SearchResult> = results
                .iter()
                .map(|app| {
                    let icon_img = cache.get_slint_image_with_fallback(
                        &app.target_path,
                        app.lnk_path.as_deref(),
                    );
                    let has_icon = icon_img.size().width > 0;
                    SearchResult {
                        name: SharedString::from(app.name.as_str()),
                        path: SharedString::from(app.target_path.as_str()),
                        icon: SharedString::from(guess_icon(&app.name)),
                        icon_image: icon_img,
                        has_icon,
                    }
                })
                .collect();
            let model = std::rc::Rc::new(VecModel::from(slint_results));
            w.set_results(ModelRc::from(model));
        }
    });

    // 7. Handle item activation (Enter or click): launch the app
    let apps_for_launch = all_apps.clone();
    let window_weak = main_window.as_weak();
    let vis = is_visible.clone();
    main_window.on_item_activated(move |index| {
        if let Some(w) = window_weak.upgrade() {
            let query = w.get_search_text().to_string();
            let results = indexer::search(&apps_for_launch, &query);
            if let Some(app) = results.get(index as usize) {
                eprintln!("[niventic] Launching: {} ({})", app.name, app.target_path);
                launch_app(app);

                // Hide the window after launching
                w.window().set_position(OFF_SCREEN);
                vis.set(false);
                w.set_search_text(SharedString::from(""));
                w.set_selected_index(0);
                let model = std::rc::Rc::new(VecModel::<SearchResult>::default());
                w.set_results(ModelRc::from(model));
            }
        }
    });

    // 7b. Handle quick access button clicks
    let window_weak = main_window.as_weak();
    let vis = is_visible.clone();
    let config_for_qa = app_config_rc.clone();
    main_window.on_quick_access_activated(move |name| {
        eprintln!("[niventic] Quick access: {}", name);

        // Find path from config
        let cfg = config_for_qa.borrow();
        let path = cfg.quick_access.iter()
            .find(|qa| qa.name.to_lowercase() == name.as_str())
            .map(|qa| qa.path.clone());

        if let Some(path) = path {
            eprintln!("[niventic] Quick access launching: {}", path);
            let _ = Command::new("cmd")
                .raw_arg(format!("/C start \"\" \"{}\"", path))
                .creation_flags(0x08000000)
                .spawn();

            // Hide the window after launching
            if let Some(w) = window_weak.upgrade() {
                w.window().set_position(OFF_SCREEN);
                vis.set(false);
                w.set_search_text(SharedString::from(""));
                w.set_selected_index(0);
                let model = Rc::new(VecModel::<SearchResult>::default());
                w.set_results(ModelRc::from(model));
            }
        } else {
            eprintln!("[niventic] No quick access match for: {}", name);
        }
    });

    // 7c. Handle save settings
    let window_weak = main_window.as_weak();
    let config_for_save = app_config_rc.clone();
    main_window.on_save_settings(move || {
        if let Some(w) = window_weak.upgrade() {
            let mut cfg = config_for_save.borrow_mut();
            cfg.appearance.width = w.get_cfg_width().to_string().parse().unwrap_or(800);
            cfg.appearance.height = w.get_cfg_height().to_string().parse().unwrap_or(500);
            cfg.appearance.font = w.get_cfg_font().to_string();
            cfg.appearance.background = w.get_cfg_bg().to_string();
            cfg.appearance.border_radius = w.get_cfg_border_radius().to_string().parse().unwrap_or(14.0);
            cfg.appearance.border_width = w.get_cfg_border_width().to_string().parse().unwrap_or(0.5);
            cfg.appearance.border_color = w.get_cfg_border_color().to_string();

            // Save quick access from model
            let qa_model = w.get_cfg_quick_access();
            let mut qa_items = Vec::new();
            for i in 0..qa_model.row_count() {
                let item = qa_model.row_data(i).unwrap();
                qa_items.push(config::QuickAccessItem {
                    name: item.name.to_string(),
                    path: item.path.to_string(),
                    icon: item.icon.to_string(),
                });
            }
            cfg.quick_access = qa_items;

            config::save_config(&cfg);
            apply_appearance(&w, &cfg.appearance);
            eprintln!("[niventic] Settings saved!");
            w.set_show_settings(false);
        }
    });

    // 8. Poll hotkey events using a Slint timer
    let window_weak = main_window.as_weak();
    let vis = is_visible.clone();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(50),
        move || {
            if let Ok(hotkey::HotkeyEvent::Toggle) = hotkey_rx.try_recv() {
                if let Some(w) = window_weak.upgrade() {
                    if vis.get() {
                        w.window().set_position(OFF_SCREEN);
                        vis.set(false);
                        eprintln!("[niventic] Window hidden");
                    } else {
                        // Reset search state when showing
                        w.set_search_text(SharedString::from(""));
                        w.set_selected_index(0);
                        let model = std::rc::Rc::new(VecModel::<SearchResult>::default());
                        w.set_results(ModelRc::from(model));

                        let sz = w.window().size();
                        w.window().set_position(center_position(
                            sz.width as i32,
                            sz.height as i32,
                        ));
                        vis.set(true);

                        // Force foreground focus (Windows blocks bg windows from stealing focus)
                        unsafe {
                            use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow};
                            use windows::core::w;
                            if let Ok(hwnd) = FindWindowW(None, w!("Niventic Launcher")) {
                                let _ = SetForegroundWindow(hwnd);
                            }
                        }

                        w.invoke_focus_search();
                        eprintln!("[niventic] Window shown");
                    }
                }
            }
        },
    );

    // 9. Show the window centered and run the event loop
    main_window.show()?;

    // Set position and focus after a brief delay so window().size() is accurate
    let window_weak = main_window.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(100), move || {
        if let Some(w) = window_weak.upgrade() {
            let sz = w.window().size();
            w.window().set_position(center_position(sz.width as i32, sz.height as i32));
            w.invoke_focus_search();
        }
    });

    slint::run_event_loop()?;

    Ok(())
}

/// Launch an application using its .lnk shortcut or target path.
fn launch_app(app: &indexer::AppEntry) {
    let path_to_launch = app
        .lnk_path
        .as_deref()
        .unwrap_or(&app.target_path);

    if path_to_launch.is_empty() {
        eprintln!("[niventic] No path to launch for: {}", app.name);
        return;
    }

    let result = Command::new("cmd")
        .args(["/C", "start", "", path_to_launch])
        .spawn();

    match result {
        Ok(_) => eprintln!("[niventic] Successfully launched: {}", app.name),
        Err(e) => eprintln!("[niventic] Failed to launch {}: {e}", app.name),
    }
}

/// Guess an emoji icon based on the app name.
fn guess_icon(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("code") || n.contains("studio") || n.contains("ide") {
        "💻"
    } else if n.contains("terminal") || n.contains("cmd") || n.contains("powershell") || n.contains("console") {
        "🖥️"
    } else if n.contains("firefox") || n.contains("chrome") || n.contains("edge") || n.contains("browser") || n.contains("opera") || n.contains("brave") {
        "🌐"
    } else if n.contains("explorer") || n.contains("file") || n.contains("folder") {
        "📁"
    } else if n.contains("calc") {
        "🧮"
    } else if n.contains("note") || n.contains("text") || n.contains("edit") || n.contains("word") {
        "📝"
    } else if n.contains("task") || n.contains("monitor") {
        "📊"
    } else if n.contains("settings") || n.contains("config") || n.contains("control") || n.contains("panel") {
        "⚙️"
    } else if n.contains("mail") || n.contains("outlook") || n.contains("thunder") {
        "📧"
    } else if n.contains("music") || n.contains("spotify") || n.contains("audio") || n.contains("sound") {
        "🎵"
    } else if n.contains("photo") || n.contains("image") || n.contains("paint") || n.contains("gimp") {
        "🖼️"
    } else if n.contains("video") || n.contains("player") || n.contains("vlc") || n.contains("media") {
        "🎬"
    } else if n.contains("game") || n.contains("steam") || n.contains("epic") {
        "🎮"
    } else if n.contains("discord") || n.contains("slack") || n.contains("teams") || n.contains("chat") || n.contains("telegram") {
        "💬"
    } else if n.contains("git") || n.contains("github") {
        "🔧"
    } else if n.contains("store") || n.contains("shop") {
        "🛒"
    } else {
        "📦"
    }
}

/// Bind config string values to Slint properties (for settings UI editing).
fn bind_config_strings(w: &AppWindow, cfg: &config::AppConfig) {
    w.set_cfg_width(SharedString::from(cfg.appearance.width.to_string()));
    w.set_cfg_height(SharedString::from(cfg.appearance.height.to_string()));
    w.set_cfg_font(SharedString::from(&cfg.appearance.font));
    w.set_cfg_bg(SharedString::from(&cfg.appearance.background));
    w.set_cfg_border_radius(SharedString::from(format!("{}", cfg.appearance.border_radius)));
    w.set_cfg_border_width(SharedString::from(format!("{}", cfg.appearance.border_width)));
    w.set_cfg_border_color(SharedString::from(&cfg.appearance.border_color));
}

/// Apply typed appearance values to Slint properties (for live rendering).
fn apply_appearance(w: &AppWindow, appearance: &config::AppearanceConfig) {
    w.set_app_bg(slint::Brush::from(parse_hex_color(&appearance.background)));
    w.set_app_border_radius(appearance.border_radius);
    w.set_app_border_width(appearance.border_width);
    w.set_app_border_color(slint::Brush::from(parse_hex_color(&appearance.border_color)));
    w.set_app_font(SharedString::from(&appearance.font));
}

/// Parse a hex color string like "#2d2d30" to slint::Color.
fn parse_hex_color(hex: &str) -> slint::Color {
    let hex = hex.trim_start_matches('#');
    if hex.len() >= 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
        slint::Color::from_rgb_u8(r, g, b)
    } else {
        slint::Color::from_rgb_u8(45, 45, 48)
    }
}
