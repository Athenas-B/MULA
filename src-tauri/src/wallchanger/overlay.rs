//! Renders the picture name overlay (filename/path caption) onto a copy of the
//! wallpaper image before it is handed to Windows, mirroring the original
//! Wall Changer's "Show picture name overlay" feature.

use super::settings::{PictureNameOverlayTextMode, Settings};
use ab_glyph::{FontRef, PxScale};
use image::{Rgba, RgbaImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_text_mut, text_size};
use imageproc::rect::Rect;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Common Windows font files, tried in order, since MULA currently only runs on Windows.
const CANDIDATE_FONTS: &[&str] = &["segoeui.ttf", "arial.ttf", "tahoma.ttf"];

fn windows_fonts_dir() -> PathBuf {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
    Path::new(&windir).join("Fonts")
}

fn load_font_bytes() -> Option<Vec<u8>> {
    let fonts_dir = windows_fonts_dir();
    for name in CANDIDATE_FONTS {
        let path = fonts_dir.join(name);
        if let Ok(bytes) = std::fs::read(&path) {
            return Some(bytes);
        }
    }
    None
}

fn overlay_text(original_path: &str, mode: PictureNameOverlayTextMode) -> String {
    let path = Path::new(original_path);
    match mode {
        PictureNameOverlayTextMode::FileNameWithoutExtension => path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        PictureNameOverlayTextMode::FileName => path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        PictureNameOverlayTextMode::FolderAndFileName => {
            let file_name = path.file_name().map(|s| s.to_string_lossy().to_string());
            let folder = path
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().to_string());
            match (folder, file_name) {
                (Some(folder), Some(file_name)) => format!("{folder}\\{file_name}"),
                (None, Some(file_name)) => file_name,
                _ => original_path.to_string(),
            }
        }
        PictureNameOverlayTextMode::FullPath => original_path.to_string(),
    }
}

fn argb_to_rgba(argb: i32) -> Rgba<u8> {
    let u32v = argb as u32;
    let a = ((u32v >> 24) & 0xFF) as u8;
    let r = ((u32v >> 16) & 0xFF) as u8;
    let g = ((u32v >> 8) & 0xFF) as u8;
    let b = (u32v & 0xFF) as u8;
    Rgba([r, g, b, a])
}

fn rendered_cache_dir() -> Result<PathBuf, String> {
    let dir = super::settings::config_dir()?.join("rendered");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create rendered wallpaper cache directory: {e}"))?;
    Ok(dir)
}

fn cache_key(original_path: &str, settings: &Settings) -> String {
    let mtime = std::fs::metadata(original_path)
        .and_then(|m| m.modified())
        .ok();

    let mut hasher = DefaultHasher::new();
    original_path.hash(&mut hasher);
    format!("{mtime:?}").hash(&mut hasher);
    (settings.picture_name_overlay_text_mode as u8).hash(&mut hasher);
    settings.picture_name_overlay_font_size.hash(&mut hasher);
    settings.picture_name_overlay_text_color_argb.hash(&mut hasher);
    settings.picture_name_overlay_offset_x.hash(&mut hasher);
    settings.picture_name_overlay_offset_y.hash(&mut hasher);
    settings.picture_name_overlay_use_backdrop.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Returns the path that should be handed to Windows for this wallpaper: either the
/// original image, or a rendered copy with the picture name overlay baked in.
pub fn resolve_wallpaper_path(original_path: &str, settings: &Settings) -> String {
    if !settings.show_picture_name_overlay {
        return original_path.to_string();
    }

    match render_with_overlay(original_path, settings) {
        Ok(rendered_path) => rendered_path,
        Err(e) => {
            log::warn!("Failed to render picture name overlay for {original_path}: {e}");
            original_path.to_string()
        }
    }
}

fn render_with_overlay(original_path: &str, settings: &Settings) -> Result<String, String> {
    let cache_dir = rendered_cache_dir()?;
    let key = cache_key(original_path, settings);
    let out_path = cache_dir.join(format!("{key}.jpg"));

    if out_path.exists() {
        return Ok(out_path.to_string_lossy().to_string());
    }

    let font_bytes = load_font_bytes()
        .ok_or_else(|| "No system font found to render the overlay".to_string())?;
    let font = FontRef::try_from_slice(&font_bytes)
        .map_err(|e| format!("Failed to parse system font: {e}"))?;

    let mut image: RgbaImage = image::open(original_path)
        .map_err(|e| format!("Failed to open image for overlay rendering: {e}"))?
        .to_rgba8();

    let text = overlay_text(original_path, settings.picture_name_overlay_text_mode);
    if !text.is_empty() {
        draw_overlay_text(&mut image, &text, &font, settings);
    }

    // JPEG doesn't support an alpha channel; flatten to RGB before saving.
    image::DynamicImage::ImageRgba8(image)
        .to_rgb8()
        .save(&out_path)
        .map_err(|e| format!("Failed to save rendered wallpaper: {e}"))?;

    Ok(out_path.to_string_lossy().to_string())
}

fn draw_overlay_text(image: &mut RgbaImage, text: &str, font: &FontRef<'_>, settings: &Settings) {
    let scale = PxScale::from(settings.picture_name_overlay_font_size.max(1) as f32);
    let (text_w, text_h) = text_size(scale, font, text);

    let margin = 16i32;
    let x = margin + settings.picture_name_overlay_offset_x;
    let y = (image.height() as i32) - text_h as i32 - margin - settings.picture_name_overlay_offset_y;
    let x = x.clamp(0, image.width() as i32);
    let y = y.clamp(0, image.height() as i32);

    if settings.picture_name_overlay_use_backdrop {
        let pad = 6i32;
        let rect_x = (x - pad).max(0);
        let rect_y = (y - pad).max(0);
        let rect_w = (text_w as i32 + pad * 2).min(image.width() as i32 - rect_x).max(1);
        let rect_h = (text_h as i32 + pad * 2).min(image.height() as i32 - rect_y).max(1);

        draw_filled_rect_mut(
            image,
            Rect::at(rect_x, rect_y).of_size(rect_w as u32, rect_h as u32),
            Rgba([0, 0, 0, 140]),
        );
    }

    let color = argb_to_rgba(settings.picture_name_overlay_text_color_argb);
    draw_text_mut(image, color, x, y, scale, font, text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallchanger::settings::Settings;

    #[test]
    fn renders_overlay_onto_a_copy_of_the_image() {
        let tmp_dir = std::env::temp_dir().join("mula_overlay_test");
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let source_path = tmp_dir.join("sample.png");

        let img = RgbaImage::from_pixel(320, 200, Rgba([10, 10, 10, 255]));
        img.save(&source_path).unwrap();

        let mut settings: Settings = serde_json::from_str("{}").unwrap();
        settings.show_picture_name_overlay = true;
        settings.picture_name_overlay_text_mode = PictureNameOverlayTextMode::FileName;
        settings.picture_name_overlay_use_backdrop = true;

        let source_path_str = source_path.to_string_lossy().to_string();
        let resolved = resolve_wallpaper_path(&source_path_str, &settings);

        assert_ne!(resolved, source_path_str, "overlay should render to a new cached file");
        assert!(Path::new(&resolved).exists());

        let rendered = image::open(&resolved).unwrap().to_rgba8();
        let has_light_pixel = rendered.pixels().any(|p| p[0] > 200 && p[1] > 200 && p[2] > 200);
        assert!(has_light_pixel, "expected white overlay text to be drawn somewhere on the image");
    }
}
