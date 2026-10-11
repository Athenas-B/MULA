use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::COLORREF;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{DesktopWallpaper, DESKTOP_WALLPAPER_POSITION, DSS_ENABLED, IDesktopWallpaper};

pub use super::Monitor;
use super::settings::ScalingMode;

pub fn initialize_com() -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| format!("Failed to initialize COM: {e}"))?;
    }
    Ok(())
}

/// Initializes COM on the calling thread (idempotent) and creates the wallpaper COM object.
fn desktop_wallpaper() -> Result<IDesktopWallpaper, String> {
    initialize_com()?;
    unsafe {
        CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL)
            .map_err(|e| format!("Failed to create IDesktopWallpaper: {e}"))
    }
}

/// Copies a string allocated by COM and frees it.
unsafe fn take_pwstr(value: PWSTR) -> Result<String, String> {
    let text = value.to_string().map_err(|e| format!("Failed to read string from Windows: {e}"));
    CoTaskMemFree(Some(value.as_ptr() as *const _));
    text
}

fn monitor_ids(wallpaper: &IDesktopWallpaper) -> Result<Vec<String>, String> {
    unsafe {
        let count = wallpaper
            .GetMonitorDevicePathCount()
            .map_err(|e| format!("Failed to get monitor count: {e}"))?;
        (0..count)
            .map(|i| {
                let id = wallpaper
                    .GetMonitorDevicePathAt(i)
                    .map_err(|e| format!("Failed to get monitor device path: {e}"))?;
                take_pwstr(id)
            })
            .collect()
    }
}

fn wallpaper_for_monitor(wallpaper: &IDesktopWallpaper, monitor_id: &HSTRING) -> Result<String, String> {
    unsafe {
        let path = wallpaper
            .GetWallpaper(monitor_id)
            .map_err(|e| format!("Failed to get current wallpaper: {e}"))?;
        take_pwstr(path)
    }
}

pub fn get_monitors() -> Result<Vec<Monitor>, String> {
    let wallpaper = desktop_wallpaper()?;

    let mut monitors = Vec::new();
    for id in monitor_ids(&wallpaper)? {
        match unsafe { wallpaper.GetMonitorRECT(&HSTRING::from(&id)) } {
            Ok(rect) => monitors.push(Monitor { id, width: rect.right - rect.left, height: rect.bottom - rect.top }),
            Err(e) => log::warn!("Skipping monitor {id}: could not get monitor rect: {e}"),
        }
    }

    if monitors.is_empty() {
        return Err("No active monitors were found".to_string());
    }
    Ok(monitors)
}

pub fn set_wallpaper_for_monitor(monitor_id: &str, image_path: &str) -> Result<(), String> {
    let wallpaper = desktop_wallpaper()?;
    unsafe {
        wallpaper
            .SetWallpaper(&HSTRING::from(monitor_id), &HSTRING::from(image_path))
            .map_err(|e| format!("Failed to set wallpaper: {e}"))
    }
}

pub fn apply_display_settings(scaling_mode: &ScalingMode, background_color_argb: i32) -> Result<(), String> {
    let wallpaper = desktop_wallpaper()?;

    let position = match scaling_mode {
        ScalingMode::Fill => DESKTOP_WALLPAPER_POSITION(4),            // DWPOS_FILL
        ScalingMode::FitInsideScreen => DESKTOP_WALLPAPER_POSITION(3), // DWPOS_FIT
    };

    unsafe {
        wallpaper
            .SetPosition(position)
            .map_err(|e| format!("Failed to set wallpaper position: {e}"))?;

        if matches!(scaling_mode, ScalingMode::FitInsideScreen) {
            wallpaper
                .SetBackgroundColor(COLORREF(argb_to_colorref(background_color_argb)))
                .map_err(|e| format!("Failed to set background color: {e}"))?;
        }
    }
    Ok(())
}

fn argb_to_colorref(argb: i32) -> u32 {
    let r = (argb >> 16) & 0xFF;
    let g = (argb >> 8) & 0xFF;
    let b = argb & 0xFF;
    (b as u32) << 16 | (g as u32) << 8 | (r as u32)
}

pub fn is_windows_slideshow_enabled() -> Result<bool, String> {
    let wallpaper = desktop_wallpaper()?;
    unsafe {
        let state = wallpaper
            .GetStatus()
            .map_err(|e| format!("Failed to get wallpaper status: {e}"))?;
        Ok(state.contains(DSS_ENABLED))
    }
}

/// Returns the wallpaper file currently set for a monitor, according to Windows itself
/// (used as the "before" frame when building a fade transition).
pub fn get_current_wallpaper(monitor_id: &str) -> Result<String, String> {
    let wallpaper = desktop_wallpaper()?;
    wallpaper_for_monitor(&wallpaper, &HSTRING::from(monitor_id))
}

/// Re-applies each monitor's current wallpaper, which stops a running Windows slideshow.
pub fn try_disable_windows_slideshow() -> Result<(), String> {
    let wallpaper = desktop_wallpaper()?;

    for id in monitor_ids(&wallpaper)? {
        let id = HSTRING::from(&id);
        let current = match wallpaper_for_monitor(&wallpaper, &id) {
            Ok(p) if !p.is_empty() => p,
            _ => continue,
        };
        unsafe {
            wallpaper
                .SetWallpaper(&id, &HSTRING::from(&current))
                .map_err(|e| format!("Failed to reapply wallpaper to stop slideshow: {e}"))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argb_is_converted_to_a_bgr_colorref_without_alpha() {
        assert_eq!(argb_to_colorref(0xFF11_2233u32 as i32), 0x0033_2211);
        assert_eq!(argb_to_colorref(0x00FF_0000), 0x0000_00FF);
        assert_eq!(argb_to_colorref(0xFF00_0000u32 as i32), 0);
    }

    #[test]
    fn aspect_ratio_handles_zero_height() {
        let m = Monitor { width: 2560, height: 1440, ..Default::default() };
        assert!((m.aspect_ratio() - 16.0 / 9.0).abs() < 1e-9);
        assert_eq!(Monitor::default().aspect_ratio(), 1.0);
    }
}
