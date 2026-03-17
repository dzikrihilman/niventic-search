#![windows_subsystem = "windows"]

mod config;
mod hotkey;
mod icons;
mod indexer;

use slint::{Model, ModelRc, PhysicalPosition, SharedString, VecModel};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::rc::Rc;
use std::cell::RefCell;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

slint::include_modules!();

const INDEX_REFRESH_INTERVAL: Duration = Duration::from_secs(45);
const CREATE_NO_WINDOW: u32 = 0x08000000;

fn hide_from_taskbar(hwnd: windows::Win32::Foundation::HWND) {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowLongW, SetWindowLongW, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
            SetWindowPos, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_FRAMECHANGED, SWP_NOACTIVATE,
        };
        let mut style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        style |= WS_EX_TOOLWINDOW.0 as i32;
        style &= !(WS_EX_APPWINDOW.0 as i32);
        SetWindowLongW(hwnd, GWL_EXSTYLE, style);
        let _ = SetWindowPos(
            hwnd,
            None,
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE,
        );
    }
}

/// Load a Quick Access icon as a `slint::Image`. It first checks if the name matches a bundled 
/// default icon. If so, it loads it from embedded bytes. Otherwise, it attempts to load from 
/// the user's `icons_dir`.
fn load_qa_icon(name: &str) -> slint::Image {
    match name {
        "world.svg" => slint::Image::load_from_svg_data(include_bytes!("../ui/assets/world.svg")).unwrap_or_default(),
        "terminal.svg" => slint::Image::load_from_svg_data(include_bytes!("../ui/assets/terminal.svg")).unwrap_or_default(),
        "folder-open.svg" => slint::Image::load_from_svg_data(include_bytes!("../ui/assets/folder-open.svg")).unwrap_or_default(),
        "settings-2.svg" => slint::Image::load_from_svg_data(include_bytes!("../ui/assets/settings-2.svg")).unwrap_or_default(),
        "code.svg" => slint::Image::load_from_svg_data(include_bytes!("../ui/assets/code.svg")).unwrap_or_default(),
        _ => {
            let icon_path = config::icons_dir().join(name);
            slint::Image::load_from_path(&icon_path).unwrap_or_default()
        }
    }
}

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
    let all_apps = Arc::new(Mutex::new(indexer::scan_apps()));

    // Async refresh channel + throttle state for re-indexing while app is running.
    let (index_update_tx, index_update_rx) = mpsc::channel::<Vec<indexer::AppEntry>>();
    let index_refresh_in_progress = Arc::new(AtomicBool::new(false));
    let last_index_refresh_at = Rc::new(RefCell::new(Instant::now()));

    // 3. Create the Slint window
    let main_window = AppWindow::new()?;

    // Store config in shared Rc<RefCell> for callbacks
    let app_config_rc = Rc::new(RefCell::new(app_config));

    // Bind appearance config to Slint properties
    {
        let cfg = app_config_rc.borrow();
        bind_config_strings(&main_window, &cfg);
        apply_appearance(&main_window, &cfg.appearance);

        // Bind quick access entries with loaded icons
        let qa_entries: Vec<QuickAccessEntry> = cfg.quick_access.iter().map(|qa| {
            let icon_img = load_qa_icon(&qa.icon);
            QuickAccessEntry {
                name: SharedString::from(&qa.name),
                path: SharedString::from(&qa.path),
                icon: SharedString::from(&qa.icon),
                icon_image: icon_img,
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

    // System Tray Setup
    use tray_icon::{TrayIconBuilder, menu::{Menu, MenuItem, PredefinedMenuItem}};
    let tray_menu = Menu::new();
    let show_i = MenuItem::new("Show Niventic", true, None);
    let settings_i = MenuItem::new("Settings", true, None);
    let quit_i = MenuItem::new("Quit", true, None);

    let _ = tray_menu.append_items(&[
        &show_i,
        &PredefinedMenuItem::separator(),
        &settings_i,
        &PredefinedMenuItem::separator(),
        &quit_i,
    ]);

    let _tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip("Niventic Launcher")
        .with_icon(generate_tray_icon())
        .build()
        .unwrap_or_else(|e| {
            eprintln!("[niventic] Failed to build tray icon: {}", e);
            // Return a dummy if it fails (not possible since it's inside unwrap_or_else, wait we can just handle error gracefully by dropping)
            panic!("Tray builder failed");
        });

    let tray_event_receiver = tray_icon::TrayIconEvent::receiver();
    let tray_menu_receiver = tray_icon::menu::MenuEvent::receiver();

    // 4. Start global hotkey listener in background thread
    let hotkey_rx = hotkey::start_listener(modifiers, vk_code);

    // 5. Handle Escape key: move window off-screen
    let window_weak = main_window.as_weak();
    let vis = is_visible.clone();
    main_window.on_escape_pressed(move || {
        if let Some(w) = window_weak.upgrade() {
            let _ = w.hide();
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
            let apps_guard = match apps_for_search.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    eprintln!("[niventic] Failed to lock app index for search: {e}");
                    return;
                }
            };

            let results = indexer::search(&apps_guard, &query);
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
                        icon: SharedString::from(""),
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
            let selected_app = {
                let apps_guard = match apps_for_launch.lock() {
                    Ok(guard) => guard,
                    Err(e) => {
                        eprintln!("[niventic] Failed to lock app index for launch: {e}");
                        return;
                    }
                };
                let results = indexer::search(&apps_guard, &query);
                results.get(index as usize).map(|app| (*app).clone())
            };

            if let Some(app) = selected_app {
                eprintln!("[niventic] Launching: {} ({})", app.name, app.target_path);
                launch_app(&app);

                // Hide the window after launching
                let _ = w.hide();
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
        let clicked_name = name.trim();
        let cfg = config_for_qa.borrow();
        let path = cfg.quick_access.iter()
            .find(|qa| qa.name.trim().eq_ignore_ascii_case(clicked_name))
            .map(|qa| qa.path.clone());

        if let Some(path) = path {
            eprintln!("[niventic] Quick access launching: {}", path);
            if let Err(e) = launch_target_hidden(&path) {
                eprintln!("[niventic] Failed quick access launch '{}': {e}", path);
            }

            // Hide the window after launching
            if let Some(w) = window_weak.upgrade() {
                let _ = w.hide();
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
            cfg.run_at_startup = w.get_cfg_run_at_startup();
            cfg.appearance.width = w.get_cfg_width().to_string().parse().unwrap_or(800);
            cfg.appearance.height = w.get_cfg_height().to_string().parse().unwrap_or(500);
            cfg.appearance.font = w.get_cfg_font().to_string();
            cfg.appearance.background = w.get_cfg_bg().to_string();
            cfg.appearance.border_radius = w.get_cfg_border_radius().to_string().parse().unwrap_or(14.0);
            cfg.appearance.border_width = w.get_cfg_border_width().to_string().parse().unwrap_or(0.5);
            cfg.appearance.border_color = w.get_cfg_border_color().to_string();
            cfg.appearance.opacity = w.get_cfg_opacity().to_string().parse().unwrap_or(0.9);

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

            // Save hotkey config
            cfg.hotkey.modifier = w.get_cfg_hotkey_modifier().to_string();
            cfg.hotkey.key = w.get_cfg_hotkey_key().to_string();

            config::save_config(&cfg);
            apply_appearance(&w, &cfg.appearance);
            
            // Apply Run at Startup registry changes
            if let Err(e) = apply_run_at_startup(cfg.run_at_startup) {
                eprintln!("[niventic] Failed to set run-at-startup registry: {e}");
            }

            eprintln!("[niventic] Settings saved!");
            w.set_show_settings(false);
        }
    });

    // 7d. Handle color picker
    let window_weak = main_window.as_weak();
    main_window.on_pick_color(move |which| {
        if let Some(w) = window_weak.upgrade() {
            if let Some(color) = open_color_dialog() {
                let hex = format!("#{:02x}{:02x}{:02x}", color.0, color.1, color.2);
                if which == "bg" {
                    w.set_cfg_bg(SharedString::from(&hex));
                    w.set_app_bg(slint::Brush::from(slint::Color::from_rgb_u8(color.0, color.1, color.2)));
                } else if which == "border" {
                    w.set_cfg_border_color(SharedString::from(&hex));
                    w.set_app_border_color(slint::Brush::from(slint::Color::from_rgb_u8(color.0, color.1, color.2)));
                }
            }
        }
    });

    // 7e. Handle file picker for Quick Access path
    let window_weak = main_window.as_weak();
    main_window.on_pick_qa_path(move |idx| {
        if let Some(w) = window_weak.upgrade() {
            let dialog = rfd::FileDialog::new().set_title("Select Application or File");
            if let Some(path) = dialog.pick_file() {
                let qa_model = w.get_cfg_quick_access();
                if let Some(mut item) = qa_model.row_data(idx as usize) {
                    item.path = SharedString::from(path.to_string_lossy().as_ref());
                    qa_model.set_row_data(idx as usize, item);
                }
            }
        }
    });

    // 7f. Handle file picker for Quick Access icon (copies to app icons dir)
    let window_weak = main_window.as_weak();
    main_window.on_pick_qa_icon(move |idx| {
        if let Some(w) = window_weak.upgrade() {
            let dialog = rfd::FileDialog::new()
                .set_title("Select Icon Image")
                .add_filter("Images", &["svg", "png", "ico", "jpg", "jpeg"]);
            if let Some(src_path) = dialog.pick_file() {
                // Copy icon to app icons directory
                let icons_dir = config::icons_dir();
                if let Err(e) = std::fs::create_dir_all(&icons_dir) {
                    eprintln!("[niventic] Failed to create icons dir: {e}");
                    return;
                }
                if let Some(file_name) = src_path.file_name() {
                    let dest = icons_dir.join(file_name);
                    if let Err(e) = std::fs::copy(&src_path, &dest) {
                        eprintln!("[niventic] Failed to copy icon: {e}");
                        return;
                    }
                    // Store just the filename (will be resolved from icons_dir at load time)
                    let name_str = file_name.to_string_lossy().to_string();
                    let qa_model = w.get_cfg_quick_access();
                    if let Some(mut item) = qa_model.row_data(idx as usize) {
                        item.icon = SharedString::from(&name_str);
                        item.icon_image = load_qa_icon(&name_str);
                        qa_model.set_row_data(idx as usize, item);
                    }
                    eprintln!("[niventic] Icon copied to: {}", dest.display());
                }
            }
        }
    });

    // 8. Poll hotkey events using a Slint timer
    let window_weak = main_window.as_weak();
    let vis = is_visible.clone();
    let timer = slint::Timer::default();

    let apps_for_refresh = all_apps.clone();
    let index_update_tx_timer = index_update_tx.clone();
    let index_refresh_in_progress_timer = index_refresh_in_progress.clone();
    let last_index_refresh_at_timer = last_index_refresh_at.clone();

    // Track when window was last shown (grace period for focus check)
    let shown_at: Rc<RefCell<Option<std::time::Instant>>> = Rc::new(RefCell::new(None));
    let shown_at_clone = shown_at.clone();

    // Cache our HWND (found lazily on first use via raw_window_handle)
    let our_hwnd: Rc<RefCell<Option<windows::Win32::Foundation::HWND>>> = Rc::new(RefCell::new(None));

    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(50),
        move || {
            let schedule_index_refresh_if_stale = || {
                let refresh_due = last_index_refresh_at_timer.borrow().elapsed() >= INDEX_REFRESH_INTERVAL;
                if refresh_due
                    && index_refresh_in_progress_timer
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    let tx = index_update_tx_timer.clone();
                    std::thread::spawn(move || {
                        let refreshed = indexer::scan_apps();
                        let _ = tx.send(refreshed);
                    });
                }
            };

            // Apply background re-index result if available.
            match index_update_rx.try_recv() {
                Ok(new_apps) => {
                    let count = new_apps.len();
                    if let Ok(mut apps) = apps_for_refresh.lock() {
                        *apps = new_apps;
                        *last_index_refresh_at_timer.borrow_mut() = Instant::now();
                        eprintln!("[niventic] App index refreshed: {} entries", count);
                    } else {
                        eprintln!("[niventic] Failed to update app index after refresh");
                    }
                    index_refresh_in_progress_timer.store(false, Ordering::Release);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {}
            }

            // === Tray Icon & Menu Events ===
            if let Ok(event) = tray_event_receiver.try_recv() {
                match event {
                    tray_icon::TrayIconEvent::Click { button: tray_icon::MouseButton::Left, .. } => {
                        schedule_index_refresh_if_stale();
                        if let Some(w) = window_weak.upgrade() {
                            let sz = w.window().size();
                            w.window().set_position(center_position(sz.width as i32, sz.height as i32));
                            // Apply tool window style BEFORE show to prevent taskbar flash
                            let hwnd = { *our_hwnd.borrow() };
                            if let Some(our) = hwnd { hide_from_taskbar(our); }
                            let _ = w.show();
                            vis.set(true);
                            *shown_at_clone.borrow_mut() = Some(std::time::Instant::now());
                            if let Some(our) = hwnd {
                                unsafe {
                                    let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(our);
                                }
                            }
                            w.invoke_focus_search();
                        }
                    }
                    _ => {}
                }
            }
            if let Ok(event) = tray_menu_receiver.try_recv() {
                if event.id == show_i.id() {
                    schedule_index_refresh_if_stale();
                    if let Some(w) = window_weak.upgrade() {
                        let sz = w.window().size();
                        w.window().set_position(center_position(sz.width as i32, sz.height as i32));
                        // Apply tool window style BEFORE show to prevent taskbar flash
                        let hwnd = { *our_hwnd.borrow() };
                        if let Some(our) = hwnd { hide_from_taskbar(our); }
                        let _ = w.show();
                        vis.set(true);
                        *shown_at_clone.borrow_mut() = Some(std::time::Instant::now());
                        if let Some(our) = hwnd {
                            unsafe {
                                let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(our);
                            }
                        }
                        w.invoke_focus_search();
                    }
                } else if event.id == settings_i.id() {
                    schedule_index_refresh_if_stale();
                    if let Some(w) = window_weak.upgrade() {
                        let sz = w.window().size();
                        w.window().set_position(center_position(sz.width as i32, sz.height as i32));
                        // Apply tool window style BEFORE show to prevent taskbar flash
                        let hwnd = { *our_hwnd.borrow() };
                        if let Some(our) = hwnd { hide_from_taskbar(our); }
                        let _ = w.show();
                        vis.set(true);
                        w.set_show_settings(true);
                        *shown_at_clone.borrow_mut() = Some(std::time::Instant::now());
                        if let Some(our) = hwnd {
                            unsafe {
                                let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(our);
                            }
                        }
                    }
                } else if event.id == quit_i.id() {
                    slint::quit_event_loop().unwrap();
                }
            }

            // === Focus loss detection ===
            if vis.get() {
                let shown_time = *shown_at_clone.borrow();
                if let Some(t) = shown_time {
                    // Only check after 150ms grace period to let SetForegroundWindow sink in
                    if t.elapsed() > std::time::Duration::from_millis(150) {
                        let hwnd = {
                            let mut cached = our_hwnd.borrow_mut();
                            if cached.is_none() {
                                if let Some(w) = window_weak.upgrade() {
                                    use raw_window_handle::HasWindowHandle;
                                    if let Ok(handle) = w.window().window_handle().window_handle() {
                                        if let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() {
                                            *cached = Some(windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _));
                                        }
                                    }
                                }
                            }
                            *cached
                        };

                        if let Some(our) = hwnd {
                            let fg = unsafe {
                                windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow()
                            };
                            if fg != our && !fg.is_invalid() && fg.0 != std::ptr::null_mut() {
                                if let Some(w) = window_weak.upgrade() {
                                    let _ = w.hide();
                                    vis.set(false);
                                    w.set_search_text(SharedString::from(""));
                                    w.set_selected_index(0);
                                    w.set_show_settings(false);
                                    let model = Rc::new(VecModel::<SearchResult>::default());
                                    w.set_results(ModelRc::from(model));
                                    *shown_at_clone.borrow_mut() = None;
                                    eprintln!("[niventic] Window hidden (focus lost)");
                                }
                            }
                        }
                    }
                }
            }

            // === Hotkey toggle ===
            if let Ok(hotkey::HotkeyEvent::Toggle) = hotkey_rx.try_recv() {
                if let Some(w) = window_weak.upgrade() {
                    if vis.get() {
                        let _ = w.hide();
                        vis.set(false);
                        *shown_at_clone.borrow_mut() = None;
                        eprintln!("[niventic] Window hidden");
                    } else {
                        schedule_index_refresh_if_stale();
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

                        // Resolve HWND (cached after first use)
                        let hwnd = {
                            let mut cached = our_hwnd.borrow_mut();
                            if cached.is_none() {
                                use raw_window_handle::HasWindowHandle;
                                if let Ok(handle) = w.window().window_handle().window_handle() {
                                    if let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() {
                                        *cached = Some(windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _));
                                    }
                                }
                            }
                            *cached
                        };

                        // Apply tool window style BEFORE show to prevent taskbar flash
                        if let Some(our) = hwnd { hide_from_taskbar(our); }
                        let _ = w.show();
                        vis.set(true);
                        *shown_at_clone.borrow_mut() = Some(std::time::Instant::now());
                        
                        if let Some(our) = hwnd {
                            unsafe {
                                let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(our);
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

    // Hide from Windows Taskbar by setting WS_EX_TOOLWINDOW
    use raw_window_handle::HasWindowHandle;
    if let Ok(handle) = main_window.window().window_handle().window_handle() {
        if let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() {
            let hwnd = windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _);
            hide_from_taskbar(hwnd);
        }
    }

    // Set position and focus after a brief delay so window().size() is accurate
    let window_weak = main_window.as_weak();
    let shown_at_init = shown_at.clone();
    slint::Timer::single_shot(std::time::Duration::from_millis(100), move || {
        if let Some(w) = window_weak.upgrade() {
            let sz = w.window().size();
            w.window().set_position(center_position(sz.width as i32, sz.height as i32));
            w.invoke_focus_search();
            *shown_at_init.borrow_mut() = Some(std::time::Instant::now());
        }
    });

    slint::run_event_loop_until_quit()?;

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

    let result = launch_target_hidden(path_to_launch);

    match result {
        Ok(_) => eprintln!("[niventic] Successfully launched: {}", app.name),
        Err(e) => eprintln!("[niventic] Failed to launch {}: {e}", app.name),
    }
}

/// Launch a target path/URI/shell verb via native Windows shell execution.
/// Falls back to hidden `cmd /C start` for edge-case compatibility.
fn launch_target_hidden(target: &str) -> std::io::Result<()> {
    let target = target.trim();
    if target.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "launch target is empty",
        ));
    }

    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;

    let verb_w: Vec<u16> = "open\0".encode_utf16().collect();
    let target_w: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();

    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb_w.as_ptr()),
            PCWSTR(target_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    if (result.0 as usize) <= 32 {
        eprintln!(
            "[niventic] ShellExecuteW failed for '{}' (code {}), falling back to cmd start",
            target,
            result.0 as usize
        );

        // Fallback keeps launch compatibility for unusual shell targets.
        let escaped_target = target.replace('"', "\"\"");
        return Command::new("cmd")
            .raw_arg(format!("/C start \"\" \"{}\"", escaped_target))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ());
    }

    Ok(())
}

/// Bind config string values to Slint properties (for settings UI editing).
fn bind_config_strings(w: &AppWindow, cfg: &config::AppConfig) {
    w.set_cfg_run_at_startup(cfg.run_at_startup);
    w.set_cfg_width(SharedString::from(cfg.appearance.width.to_string()));
    w.set_cfg_height(SharedString::from(cfg.appearance.height.to_string()));
    w.set_cfg_font(SharedString::from(&cfg.appearance.font));
    w.set_cfg_bg(SharedString::from(&cfg.appearance.background));
    w.set_cfg_border_radius(SharedString::from(format!("{}", cfg.appearance.border_radius)));
    w.set_cfg_border_width(SharedString::from(format!("{}", cfg.appearance.border_width)));
    w.set_cfg_border_color(SharedString::from(&cfg.appearance.border_color));
    w.set_cfg_opacity(SharedString::from(format!("{}", cfg.appearance.opacity)));
    w.set_cfg_hotkey_modifier(SharedString::from(&cfg.hotkey.modifier));
    w.set_cfg_hotkey_key(SharedString::from(&cfg.hotkey.key));
}

/// Apply typed appearance values to Slint properties (for live rendering).
fn apply_appearance(w: &AppWindow, appearance: &config::AppearanceConfig) {
    w.set_app_bg(slint::Brush::from(parse_hex_color(&appearance.background)));
    w.set_app_border_radius(appearance.border_radius);
    w.set_app_border_width(appearance.border_width);
    w.set_app_border_color(slint::Brush::from(parse_hex_color(&appearance.border_color)));
    w.set_app_font(SharedString::from(&appearance.font));
    w.set_app_opacity(appearance.opacity);
    w.set_app_width(appearance.width as f32);
    w.set_app_height(appearance.height as f32);
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

/// Opens the native Windows color picker dialog and returns (R, G, B).
fn open_color_dialog() -> Option<(u8, u8, u8)> {
    use windows::Win32::UI::Controls::Dialogs::{
        ChooseColorW, CHOOSECOLORW, CC_FULLOPEN, CC_RGBINIT,
    };
    use windows::Win32::Foundation::COLORREF;

    let mut custom_colors = [COLORREF(0u32); 16];
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        rgbResult: COLORREF(0),
        lpCustColors: custom_colors.as_mut_ptr(),
        Flags: CC_FULLOPEN | CC_RGBINIT,
        ..Default::default()
    };

    let ok = unsafe { ChooseColorW(&mut cc) };
    if ok.as_bool() {
        let rgb = cc.rgbResult.0;
        let r = (rgb & 0xFF) as u8;
        let g = ((rgb >> 8) & 0xFF) as u8;
        let b = ((rgb >> 16) & 0xFF) as u8;
        Some((r, g, b))
    } else {
        None
    }
}

/// Generates the system tray icon from the embedded logo
fn generate_tray_icon() -> tray_icon::Icon {
    let icon_data = include_bytes!("../ui/assets/logo.png");
    let image = image::load_from_memory(icon_data)
        .expect("Failed to load logo.png")
        .into_rgba8();

    let (width, height) = image.dimensions();
    let rgba = image.into_raw();
    tray_icon::Icon::from_rgba(rgba, width, height).unwrap()
}

/// Applies or removes the NiventicLauncher registry key for Run at Startup.
fn apply_run_at_startup(enable: bool) -> std::io::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu.open_subkey_with_flags(
        r#"Software\Microsoft\Windows\CurrentVersion\Run"#,
        KEY_SET_VALUE | KEY_QUERY_VALUE,
    )?;

    let app_name = "NiventicLauncher";

    if enable {
        let exe_path = std::env::current_exe()?;
        let path_str = exe_path.to_string_lossy().to_string();
        // Set the value (will overwrite if exists)
        run_key.set_value(app_name, &path_str)?;
        eprintln!("[niventic] Enabled Run at Startup: {}", path_str);
    } else {
        // Only delete if it exists to avoid error
        let existing: Result<String, _> = run_key.get_value(app_name);
        if existing.is_ok() {
            run_key.delete_value(app_name)?;
            eprintln!("[niventic] Disabled Run at Startup");
        }
    }

    Ok(())
}
