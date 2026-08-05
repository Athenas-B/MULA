use serde::{Deserialize, Serialize};
use windows::core::HSTRING;
use windows::Win32::Foundation::{RECT, COLORREF};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{DesktopWallpaper, DESKTOP_WALLPAPER_POSITION, IDesktopWallpaper};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Monitor {
    pub id: String,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub width: i32,
    pub height: i32,
}

impl Monitor {
    pub fn aspect_ratio(&self) -> f64 {
        if self.height == 0 {
            return 1.0;
        }
        self.width as f64 / self.height as f64
    }
}

pub fn initialize_com() -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| format!("Failed to initialize COM: {e}"))?;
    }
    Ok(())
}

pub fn create_desktop_wallpaper() -> Result<IDesktopWallpaper, String> {
    unsafe {
        CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL)
            .map_err(|e| format!("Failed to create IDesktopWallpaper: {e}"))
    }
}

pub fn get_monitors() -> Result<Vec<Monitor>, String> {
    initialize_com()?;
    let wallpaper = create_desktop_wallpaper()?;

    unsafe {
        let count = wallpaper.GetMonitorDevicePathCount()
            .map_err(|e| format!("Failed to get monitor count: {e}"))?;

        let mut monitors = Vec::with_capacity(count as usize);
        for i in 0..count {
            let id_pwstr = wallpaper.GetMonitorDevicePathAt(i)
                .map_err(|e| format!("Failed to get monitor device path: {e}"))?;
            let id = id_pwstr.to_string()
                .map_err(|e| format!("Failed to read monitor id: {e}"))?;
            CoTaskMemFree(Some(id_pwstr.as_ptr() as *const _));

            let id_hstring = HSTRING::from(&id);
            let rect: RECT = wallpaper.GetMonitorRECT(&id_hstring)
                .map_err(|e| format!("Failed to get monitor rect: {e}"))?;

            monitors.push(Monitor {
                id,
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
                width: rect.right - rect.left,
                height: rect.bottom - rect.top,
            });
        }

        Ok(monitors)
    }
}

pub fn set_wallpaper_for_monitor(monitor_id: &str, image_path: &str) -> Result<(), String> {
    initialize_com()?;
    let wallpaper = create_desktop_wallpaper()?;

    unsafe {
        let id = HSTRING::from(monitor_id);
        let path = HSTRING::from(image_path);
        wallpaper.SetWallpaper(&id, &path)
            .map_err(|e| format!("Failed to set wallpaper: {e}"))?;
    }
    Ok(())
}

pub fn apply_display_settings(scaling_mode: &super::settings::ScalingMode, background_color_argb: i32) -> Result<(), String> {
    initialize_com()?;
    let wallpaper = create_desktop_wallpaper()?;

    let position = match scaling_mode {
        super::settings::ScalingMode::Fill => DESKTOP_WALLPAPER_POSITION(4), // DWPOS_FILL
        super::settings::ScalingMode::FitInsideScreen => DESKTOP_WALLPAPER_POSITION(3), // DWPOS_FIT
    };

    let color_ref = argb_to_colorref(background_color_argb);

    unsafe {
        wallpaper.SetPosition(position)
            .map_err(|e| format!("Failed to set wallpaper position: {e}"))?;

        if matches!(scaling_mode, super::settings::ScalingMode::FitInsideScreen) {
            wallpaper.SetBackgroundColor(COLORREF(color_ref))
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
