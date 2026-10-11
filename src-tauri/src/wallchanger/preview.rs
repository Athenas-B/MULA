//! Generates a visual preview of how the current wallpaper fits each monitor.

use super::monitors::{get_current_wallpaper, get_monitors};
use super::settings::{ScalingMode, Settings};
use base64::Engine;
use image::{imageops::FilterType, Rgba, RgbaImage};
use serde::Serialize;
use std::io::Cursor;

const PREVIEW_MAX_WIDTH: u32 = 420;
const PREVIEW_MAX_HEIGHT: u32 = 280;

#[derive(Debug, Clone, Serialize)]
pub struct MonitorPreview {
    pub monitor_id: String,
    pub monitor_index: usize,
    pub width: i32,
    pub height: i32,
    pub wallpaper_path: String,
    pub image_width: i32,
    pub image_height: i32,
    pub fit: String,
    pub preview_data_url: String,
}

pub fn build_monitor_previews(settings: &Settings) -> Result<Vec<MonitorPreview>, String> {
    let monitors = get_monitors()?;
    if monitors.is_empty() {
        return Err("No monitors found".to_string());
    }

    let mut previews = Vec::new();
    for (index, monitor) in monitors.iter().enumerate() {
        let wallpaper_path = get_current_wallpaper(&monitor.id).unwrap_or_default();
        let (image_width, image_height, fit, preview) =
            if wallpaper_path.is_empty() || !std::path::Path::new(&wallpaper_path).exists() {
                (
                    0,
                    0,
                    "No wallpaper".to_string(),
                    render_placeholder(
                        monitor.width,
                        monitor.height,
                        settings.background_color_argb,
                    ),
                )
            } else {
                match render_wallpaper_preview(
                    &wallpaper_path,
                    monitor.width,
                    monitor.height,
                    &settings.scaling_mode,
                    settings.background_color_argb,
                ) {
                    Ok((w, h, fit, img)) => (w, h, fit, img),
                    Err(e) => {
                        log::warn!("Failed to render monitor preview for {}: {e}", monitor.id);
                        (
                            0,
                            0,
                            "Error".to_string(),
                            render_placeholder(
                                monitor.width,
                                monitor.height,
                                settings.background_color_argb,
                            ),
                        )
                    }
                }
            };

        previews.push(MonitorPreview {
            monitor_id: monitor.id.clone(),
            monitor_index: index + 1,
            width: monitor.width,
            height: monitor.height,
            wallpaper_path: wallpaper_path.clone(),
            image_width,
            image_height,
            fit,
            preview_data_url: png_data_url(&preview)?,
        });
    }

    Ok(previews)
}

fn render_wallpaper_preview(
    wallpaper_path: &str,
    monitor_width: i32,
    monitor_height: i32,
    scaling_mode: &ScalingMode,
    background_color_argb: i32,
) -> Result<(i32, i32, String, RgbaImage), String> {
    let image = image::open(wallpaper_path)
        .map_err(|e| format!("Failed to open wallpaper for preview: {e}"))?;
    let image = image.to_rgba8();
    let image_width = image.width();
    let image_height = image.height();

    let (preview_width, preview_height) = preview_dimensions(monitor_width, monitor_height);
    let bg = argb_to_rgba(background_color_argb);

    let canvas = fit_to_canvas(&image, preview_width, preview_height, scaling_mode, bg, FilterType::Lanczos3);
    let fit = match scaling_mode {
        ScalingMode::Fill => "Fill (cropped)",
        ScalingMode::FitInsideScreen => "Fit inside screen (letterbox)",
    };

    Ok((image_width as i32, image_height as i32, fit.to_string(), canvas))
}

/// Renders `image` onto a `width` x `height` canvas the way Windows applies `scaling_mode`:
/// Fill scales to cover and crops the overflow; FitInsideScreen scales to fit and letterboxes
/// with `background`.
pub(super) fn fit_to_canvas(
    image: &RgbaImage,
    width: u32,
    height: u32,
    scaling_mode: &ScalingMode,
    background: Rgba<u8>,
    filter: FilterType,
) -> RgbaImage {
    let (width, height) = (width.max(1), height.max(1));
    let mut canvas = RgbaImage::from_pixel(width, height, background);
    let (image_width, image_height) = (image.width().max(1), image.height().max(1));

    let scale_x = width as f64 / image_width as f64;
    let scale_y = height as f64 / image_height as f64;
    let scale = match scaling_mode {
        ScalingMode::Fill => scale_x.max(scale_y),
        ScalingMode::FitInsideScreen => scale_x.min(scale_y),
    }
    .max(f64::MIN_POSITIVE);
    let scaled_w = ((image_width as f64 * scale).round() as u32).max(1);
    let scaled_h = ((image_height as f64 * scale).round() as u32).max(1);
    let resized = image::imageops::resize(image, scaled_w, scaled_h, filter);

    match scaling_mode {
        ScalingMode::Fill => {
            let x = scaled_w.saturating_sub(width) / 2;
            let y = scaled_h.saturating_sub(height) / 2;
            let cropped = image::imageops::crop_imm(&resized, x, y, width.min(scaled_w), height.min(scaled_h));
            image::imageops::overlay(&mut canvas, &cropped.to_image(), 0, 0);
        }
        ScalingMode::FitInsideScreen => {
            let x = (width.saturating_sub(scaled_w) / 2) as i64;
            let y = (height.saturating_sub(scaled_h) / 2) as i64;
            image::imageops::overlay(&mut canvas, &resized, x, y);
        }
    }
    canvas
}

fn preview_dimensions(monitor_width: i32, monitor_height: i32) -> (u32, u32) {
    let mon_w = monitor_width.max(1) as f64;
    let mon_h = monitor_height.max(1) as f64;
    let mon_aspect = mon_w / mon_h;

    let max_w = PREVIEW_MAX_WIDTH as f64;
    let max_h = PREVIEW_MAX_HEIGHT as f64;
    let preview_aspect = max_w / max_h;

    if mon_aspect > preview_aspect {
        let w = max_w.min(mon_w);
        let h = (w / mon_aspect).round();
        (w as u32, h.max(1.0) as u32)
    } else {
        let h = max_h.min(mon_h);
        let w = (h * mon_aspect).round();
        (w.max(1.0) as u32, h as u32)
    }
}

fn render_placeholder(
    monitor_width: i32,
    monitor_height: i32,
    background_color_argb: i32,
) -> RgbaImage {
    let (preview_width, preview_height) = preview_dimensions(monitor_width, monitor_height);
    RgbaImage::from_pixel(
        preview_width,
        preview_height,
        argb_to_rgba(background_color_argb),
    )
}

pub(super) fn argb_to_rgba(argb: i32) -> Rgba<u8> {
    let u = argb as u32;
    let a = ((u >> 24) & 0xFF) as u8;
    let r = ((u >> 16) & 0xFF) as u8;
    let g = ((u >> 8) & 0xFF) as u8;
    let b = (u & 0xFF) as u8;
    Rgba([r, g, b, a])
}

fn png_data_url(image: &RgbaImage) -> Result<String, String> {
    let mut buffer = Vec::new();
    image::DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut Cursor::new(&mut buffer), image::ImageFormat::Png)
        .map_err(|e| format!("Failed to encode preview PNG: {e}"))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&buffer);
    Ok(format!("data:image/png;base64,{encoded}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgba<u8> = Rgba([255, 0, 0, 255]);
    const BG: Rgba<u8> = Rgba([0, 0, 255, 255]);

    #[test]
    fn fill_covers_the_whole_canvas() {
        let wide = RgbaImage::from_pixel(400, 100, RED);
        let out = fit_to_canvas(&wide, 100, 100, &ScalingMode::Fill, BG, FilterType::Triangle);
        assert_eq!(out.dimensions(), (100, 100));
        assert!(out.pixels().all(|p| *p == RED), "Fill must not leave background visible");
    }

    #[test]
    fn fit_letterboxes_with_the_background_color() {
        let wide = RgbaImage::from_pixel(400, 100, RED);
        let out = fit_to_canvas(&wide, 100, 100, &ScalingMode::FitInsideScreen, BG, FilterType::Triangle);
        assert_eq!(out.dimensions(), (100, 100));
        assert_eq!(*out.get_pixel(50, 5), BG, "top band is letterbox");
        assert_eq!(*out.get_pixel(50, 50), RED, "image is centered");
        assert_eq!(*out.get_pixel(50, 95), BG, "bottom band is letterbox");
    }
}
