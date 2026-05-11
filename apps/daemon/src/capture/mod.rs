use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use image::{ColorType, ImageFormat};

pub fn write_xrgb8888_png(
    output_dir: &Path,
    frame_index: usize,
    width: usize,
    height: usize,
    stride: usize,
    framebuffer: &[u8],
) -> Result<PathBuf, io::Error> {
    fs::create_dir_all(output_dir)?;

    let mut rgba = vec![0u8; width * height * 4];
    for y in 0..height {
        let row_start = y * stride;
        let rgba_row_start = y * width * 4;

        for x in 0..width {
            let src = row_start + x * 4;
            let dst = rgba_row_start + x * 4;

            rgba[dst] = framebuffer[src + 2];
            rgba[dst + 1] = framebuffer[src + 1];
            rgba[dst + 2] = framebuffer[src];
            rgba[dst + 3] = 0xff;
        }
    }

    let path = output_dir.join(format!("frame-{frame_index:04}.png"));
    image::save_buffer_with_format(
        &path,
        &rgba,
        width as u32,
        height as u32,
        ColorType::Rgba8,
        ImageFormat::Png,
    )
    .map_err(io::Error::other)?;

    Ok(path)
}
