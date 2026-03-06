use std::sync::mpsc;
use std::thread;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

const HOTKEY_ID: i32 = 1;

/// Events sent from the hotkey listener thread to the main thread.
#[derive(Debug)]
pub enum HotkeyEvent {
    Toggle,
}

/// Spawn a background thread that listens for the global hotkey using RegisterHotKey.
/// Returns a Receiver that emits `HotkeyEvent::Toggle` each time the hotkey is pressed.
pub fn start_listener(
    modifiers: HOT_KEY_MODIFIERS,
    vk_code: u32,
) -> mpsc::Receiver<HotkeyEvent> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        unsafe {
            // Add MOD_NOREPEAT to prevent repeated events when key is held
            let final_modifiers = modifiers | MOD_NOREPEAT;

            // Register the global hotkey (None = thread-level, not bound to a window)
            if let Err(e) = RegisterHotKey(None, HOTKEY_ID, final_modifiers, vk_code) {
                eprintln!("[niventic] Failed to register hotkey: {e}");
                return;
            }
            eprintln!("[niventic] Global hotkey registered successfully.");

            // Message loop — blocks until a message arrives
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_HOTKEY && msg.wParam.0 == HOTKEY_ID as usize {
                    let _ = tx.send(HotkeyEvent::Toggle);
                }
            }

            // Cleanup when loop exits
            let _ = UnregisterHotKey(None, HOTKEY_ID);
        }
    });

    rx
}
