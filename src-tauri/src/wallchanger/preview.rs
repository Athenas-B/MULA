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

    let mut canvas = RgbaImage::from_pixel(preview_width, preview_height, bg);

    match scaling_mode {
        ScalingMode::Fill => {
            let scale_x = preview_width as f64 / image_width as f64;
            let scale_y = preview_height as f64 / image_height as f64;
            let scale = scale_x.max(scale_y).max(f64::MIN_POSITIVE);
            let scaled_w = (image_width as f64 * scale).round() as u32;
            let scaled_h = (image_height as f64 * scale).round() as u32;

            let resized = image::imageops::resize(&image, scaled_w, scaled_h, FilterType::Lanczos3);
            let x = ((scaled_w as i64 - preview_width as i64) / 2).max(0);
            let y = ((scaled_h as i64 - preview_height as i64) / 2).max(0);
            let cropped = image::imageops::crop_imm(
                &resized,
                x as u32,
                y as u32,
                preview_width,
                preview_height,
            );
            image::imageops::overlay(&mut canvas, &cropped.to_image(), 0, 0);

            Ok((
                image_width as i32,
                image_height as i32,
                "Fill (cropped)".to_string(),
                canvas,
            ))
        }
        ScalingMode::FitInsideScreen => {
            let scale_x = preview_width as f64 / image_width as f64;
            let scale_y = preview_height as f64 / image_height as f64;
            let scale = scale_x.min(scale_y).max(f64::MIN_POSITIVE);
            let scaled_w = (image_width as f64 * scale).round() as u32;
            let scaled_h = (image_height as f64 * scale).round() as u32;

            let resized = image::imageops::resize(&image, scaled_w, scaled_h, FilterType::Lanczos3);
            let x = ((preview_width as i64 - scaled_w as i64) / 2).max(0);
            let y = ((preview_height as i64 - scaled_h as i64) / 2).max(0);
            image::imageops::overlay(&mut canvas, &resized, x, y);

            Ok((
                image_width as i32,
                image_height as i32,
                "Fit inside screen (letterbox)".to_string(),
                canvas,
            ))
        }
    }
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

fn argb_to_rgba(argb: i32) -> Rgba<u8> {
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
