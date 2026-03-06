use std::collections::HashMap;

use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON};

/// Cache for extracted app icons (maps path -> RGBA pixel data + size)
pub struct IconCache {
    cache: HashMap<String, Option<(Vec<u8>, u32, u32)>>,
}

impl IconCache {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Get icon for a path. Returns cached result or extracts fresh.
    pub fn get_icon(&mut self, path: &str) -> Option<(Vec<u8>, u32, u32)> {
        if let Some(cached) = self.cache.get(path) {
            return cached.clone();
        }
        let result = extract_icon(path);
        self.cache.insert(path.to_string(), result.clone());
        result
    }

    /// Convert icon data to a slint::Image
    pub fn get_slint_image(&mut self, path: &str) -> slint::Image {
        if let Some((rgba, w, h)) = self.get_icon(path) {
            let buffer =
                slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&rgba, w, h);
            slint::Image::from_rgba8(buffer)
        } else {
            slint::Image::default()
        }
    }

    /// Try exe_path first, then lnk_path as fallback
    pub fn get_slint_image_with_fallback(
        &mut self,
        exe_path: &str,
        lnk_path: Option<&str>,
    ) -> slint::Image {
        // Try exe path first
        if !exe_path.is_empty() {
            let img = self.get_slint_image(exe_path);
            if img.size().width > 0 {
                return img;
            }
        }
        // Fallback: try the .lnk file itself
        if let Some(lnk) = lnk_path {
            let img = self.get_slint_image(lnk);
            if img.size().width > 0 {
                return img;
            }
        }
        slint::Image::default()
    }
}

/// Extract icon from a file path using ExtractIconExW.
/// Works with .exe, .dll, .lnk, .ico files.
fn extract_icon(file_path: &str) -> Option<(Vec<u8>, u32, u32)> {
    if file_path.is_empty() {
        return None;
    }

    unsafe {
        let wide_path: Vec<u16> = file_path.encode_utf16().chain(std::iter::once(0)).collect();

        // Extract the first large icon from the file
        let mut large_icon: [HICON; 1] = [HICON::default(); 1];
        let count = ExtractIconExW(
            windows::core::PCWSTR(wide_path.as_ptr()),
            0,
            Some(large_icon.as_mut_ptr()),
            None,
            1,
        );

        if count == 0 || large_icon[0].is_invalid() {
            eprintln!("[niventic] No icon found for: {}", file_path);
            return None;
        }

        let hicon = large_icon[0];
        eprintln!("[niventic] Icon extracted for: {}", file_path);

        let result = hicon_to_rgba(hicon);

        let _ = DestroyIcon(hicon);

        result
    }
}

/// Convert an HICON to RGBA pixel data
unsafe fn hicon_to_rgba(hicon: HICON) -> Option<(Vec<u8>, u32, u32)> {
    unsafe {
        // Get icon info to access the bitmap
        let mut icon_info = std::mem::zeroed();
        if !GetIconInfo(hicon, &mut icon_info).is_ok() {
            return None;
        }

        let hbm_color = icon_info.hbmColor;
        let hbm_mask = icon_info.hbmMask;

        // Must have color bitmap
        if hbm_color.is_invalid() {
            if !hbm_mask.is_invalid() {
                let _ = DeleteObject(hbm_mask.into());
            }
            return None;
        }

        let hdc = CreateCompatibleDC(None);
        if hdc.is_invalid() {
            let _ = DeleteObject(hbm_color.into());
            if !hbm_mask.is_invalid() {
                let _ = DeleteObject(hbm_mask.into());
            }
            return None;
        }

        // First call: get bitmap dimensions
        let mut bmp_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };

        let old = SelectObject(hdc, hbm_color.into());
        GetDIBits(hdc, hbm_color, 0, 0, None, &mut bmp_info, DIB_RGB_COLORS);
        SelectObject(hdc, old);

        let width = bmp_info.bmiHeader.biWidth as u32;
        let height = bmp_info.bmiHeader.biHeight.unsigned_abs();

        if width == 0 || height == 0 {
            let _ = DeleteDC(hdc);
            let _ = DeleteObject(hbm_color.into());
            if !hbm_mask.is_invalid() {
                let _ = DeleteObject(hbm_mask.into());
            }
            return None;
        }

        // Second call: extract pixels (top-down)
        bmp_info.bmiHeader.biWidth = width as i32;
        bmp_info.bmiHeader.biHeight = -(height as i32); // negative = top-down
        bmp_info.bmiHeader.biBitCount = 32;
        bmp_info.bmiHeader.biCompression = BI_RGB.0 as u32;
        bmp_info.bmiHeader.biSizeImage = 0;
        bmp_info.bmiHeader.biPlanes = 1;

        let mut pixels = vec![0u8; (width * height * 4) as usize];

        let old = SelectObject(hdc, hbm_color.into());
        let lines = GetDIBits(
            hdc,
            hbm_color,
            0,
            height,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmp_info,
            DIB_RGB_COLORS,
        );
        SelectObject(hdc, old);

        let _ = DeleteDC(hdc);
        let _ = DeleteObject(hbm_color.into());
        if !hbm_mask.is_invalid() {
            let _ = DeleteObject(hbm_mask.into());
        }

        if lines == 0 {
            return None;
        }

        // Convert BGRA to RGBA
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.swap(0, 2); // swap B and R
        }

        eprintln!("[niventic] Icon size: {}x{}", width, height);
        Some((pixels, width, height))
    }
}
