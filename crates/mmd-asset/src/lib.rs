//! CPU-side texture loading.
//!
//! Images are decoded off the render thread. A missing file becomes a 1×1
//! white image and is reported through [`CpuTexture::missing`]. Shared toon
//! bitmaps are loaded from `toon01.bmp` through `toon10.bmp` beside the model.
//! A missing file uses the built-in ramp. That ramp is not MikuMikuDance's
//! toon bitmap, and its use is not a warning.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use mmd_core::{ModelAsset, ToonSource};

/// Failure while decoding an image that was present but unreadable.
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("failed to decode {label}: {reason}")]
    Decode {
        /// Path or texture name.
        label: String,
        reason: String,
    },
}

/// An RGBA8 image ready for a GPU upload.
#[derive(Clone, Debug)]
pub struct CpuTexture {
    pub label: String,
    pub width: u32,
    pub height: u32,
    /// RGBA8 pixels, row-major.
    pub rgba: Vec<u8>,
    /// Mip levels after level 0. Built while the image is decoded.
    pub mips: Vec<Vec<u8>>,
    /// The file was missing or could not be decoded.
    pub missing: bool,
}

/// Load every color and sphere texture referenced by the model.
///
/// Textures decode on all cores. The result keeps the model's texture order.
pub fn load_model_textures(model_path: &Path, model: &ModelAsset) -> Vec<CpuTexture> {
    let directory = model_path.parent().unwrap_or_else(|| Path::new("."));
    let names = &model.textures;
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .min(names.len());
    if workers <= 1 {
        return names
            .iter()
            .map(|name| load_named(directory, name))
            .collect();
    }
    let next = AtomicUsize::new(0);
    let mut slots: Vec<Option<CpuTexture>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(name) = names.get(index) else {
                            break;
                        };
                        done.push((index, load_named(directory, name)));
                    }
                    done
                })
            })
            .collect();
        let mut slots: Vec<Option<CpuTexture>> = (0..names.len()).map(|_| None).collect();
        for handle in handles {
            match handle.join() {
                Ok(done) => {
                    for (index, texture) in done {
                        slots[index] = Some(texture);
                    }
                }
                Err(_) => tracing::error!("a texture decode worker panicked"),
            }
        }
        slots
    });
    names
        .iter()
        .zip(slots.iter_mut())
        .map(|(name, slot)| slot.take().unwrap_or_else(|| white(name)))
        .collect()
}

/// Load the ten shared toon slots. Index 0 is `toon01.bmp`.
///
/// A slot whose bitmap is not beside the model uses the built-in ramp.
pub fn load_shared_toons(model_path: &Path, model: &ModelAsset) -> Vec<CpuTexture> {
    let directory = model_path.parent().unwrap_or_else(|| Path::new("."));
    let used = used_shared_toon_slots(model);
    (0..10)
        .map(|slot| {
            let file_name = format!("toon{:02}.bmp", slot + 1);
            let path = directory.join(&file_name);
            if path.is_file() {
                match decode_file(&path) {
                    Ok(texture) => texture,
                    Err(error) => {
                        if used[slot] {
                            tracing::warn!(error = %error, "shared toon decode failed");
                        }
                        generated_toon(file_name)
                    }
                }
            } else {
                generated_toon(file_name)
            }
        })
        .collect()
}

fn used_shared_toon_slots(model: &ModelAsset) -> [bool; 10] {
    let mut used = [false; 10];
    for material in &model.materials {
        if let ToonSource::Shared(slot) = material.toon
            && let Some(flag) = used.get_mut(usize::from(slot))
        {
            *flag = true;
        }
    }
    used
}

/// A 1×256 opaque ramp, light at the top and dark at the bottom.
///
/// MMD toon bitmaps are sampled at `(0.5, 1 - N·L)`, so a fully lit normal
/// reads the first row.
pub fn generated_toon(label: impl Into<String>) -> CpuTexture {
    let mut rgba = Vec::with_capacity(256 * 4);
    for y in 0..256u32 {
        let value = (255 - y) as u8;
        rgba.extend([value, value, value, 255]);
    }
    finish(CpuTexture {
        label: label.into(),
        width: 1,
        height: 256,
        rgba,
        mips: Vec::new(),
        missing: true,
    })
}

fn load_named(directory: &Path, name: &str) -> CpuTexture {
    let file_name = name.split('*').next().unwrap_or(name).trim();
    if file_name.is_empty() {
        return white(name);
    }
    let path = texture_path(directory, file_name);
    match decode_file(&path) {
        Ok(texture) => texture,
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "texture is unavailable");
            white(file_name)
        }
    }
}

fn texture_path(directory: &Path, name: &str) -> PathBuf {
    let path = PathBuf::from(name);
    if path.is_absolute() {
        path
    } else {
        directory.join(path)
    }
}

fn white(label: &str) -> CpuTexture {
    finish(CpuTexture {
        label: label.to_string(),
        width: 1,
        height: 1,
        rgba: vec![255, 255, 255, 255],
        mips: Vec::new(),
        missing: true,
    })
}

fn decode_file(path: &Path) -> Result<CpuTexture, AssetError> {
    let bytes = std::fs::read(path).map_err(|error| AssetError::Decode {
        label: path.display().to_string(),
        reason: error.to_string(),
    })?;
    decode_bytes(&bytes, path.display().to_string())
}

fn decode_bytes(bytes: &[u8], label: String) -> Result<CpuTexture, AssetError> {
    let image = image::load_from_memory(bytes).map_err(|error| AssetError::Decode {
        label: label.clone(),
        reason: error.to_string(),
    })?;
    let rgba = image.to_rgba8();
    Ok(finish(CpuTexture {
        label,
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        mips: Vec::new(),
        missing: false,
    }))
}

fn finish(mut texture: CpuTexture) -> CpuTexture {
    texture.mips = mip_chain(&texture.rgba, texture.width, texture.height);
    texture
}

fn mip_chain(rgba: &[u8], width: u32, height: u32) -> Vec<Vec<u8>> {
    let mut level_width = width.max(1);
    let mut level_height = height.max(1);
    let levels = mip_level_count(level_width, level_height);
    let mut chain: Vec<Vec<u8>> = Vec::with_capacity(levels.saturating_sub(1) as usize);
    let base = tight_rgba(rgba, level_width, level_height);
    for _ in 1..levels {
        let source = chain.last().map_or(base.as_ref(), Vec::as_slice);
        let next = downsample_box(source, level_width, level_height);
        level_width = (level_width / 2).max(1);
        level_height = (level_height / 2).max(1);
        chain.push(next);
    }
    chain
}

fn mip_level_count(width: u32, height: u32) -> u32 {
    let largest = width.max(height).max(1);
    u32::BITS - largest.leading_zeros()
}

/// The pixels as exactly `width * height` texels, borrowed when they already are.
fn tight_rgba(rgba: &[u8], width: u32, height: u32) -> std::borrow::Cow<'_, [u8]> {
    let needed = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if rgba.len() == needed {
        return std::borrow::Cow::Borrowed(rgba);
    }
    let mut tight = vec![0u8; needed];
    let copy = needed.min(rgba.len());
    tight[..copy].copy_from_slice(&rgba[..copy]);
    std::borrow::Cow::Owned(tight)
}

fn downsample_box(src: &[u8], width: u32, height: u32) -> Vec<u8> {
    let dst_width = (width / 2).max(1);
    let dst_height = (height / 2).max(1);
    let mut dst = vec![0u8; (dst_width * dst_height * 4) as usize];
    for y in 0..dst_height {
        for x in 0..dst_width {
            let mut color = [0u32; 3];
            let mut coverage = 0u32;
            let mut count = 0u32;
            for offset_y in 0..2 {
                for offset_x in 0..2 {
                    let sample_x = (x * 2 + offset_x).min(width - 1);
                    let sample_y = (y * 2 + offset_y).min(height - 1);
                    let index = ((sample_y * width + sample_x) * 4) as usize;
                    if index + 3 < src.len() {
                        let texel_alpha = u32::from(src[index + 3]);
                        coverage += texel_alpha;
                        for channel in 0..3 {
                            color[channel] += u32::from(src[index + channel]) * texel_alpha;
                        }
                        count += 1;
                    }
                }
            }
            if count == 0 {
                continue;
            }
            let index = ((y * dst_width + x) * 4) as usize;
            if coverage == 0 {
                dst[index + 3] = 0;
                continue;
            }
            for channel in 0..3 {
                dst[index + channel] = (color[channel] / coverage) as u8;
            }
            dst[index + 3] = (coverage / count) as u8;
        }
    }
    dst
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_texels_do_not_darken_the_mip() {
        let mut rgba = vec![0u8; 16];
        rgba[0..4].copy_from_slice(&[255, 255, 255, 255]);
        let mip = downsample_box(&rgba, 2, 2);
        assert_eq!(&mip[..3], &[255, 255, 255]);
        assert!(mip[3] < 128, "alpha {}", mip[3]);
    }

    #[test]
    fn generated_ramp_is_light_at_the_top() {
        let toon = generated_toon("ramp");
        assert_eq!(toon.width, 1);
        assert_eq!(toon.height, 256);
        assert_eq!(&toon.rgba[..4], &[255, 255, 255, 255]);
        assert_eq!(&toon.rgba[255 * 4..], &[0, 0, 0, 255]);
    }

    #[test]
    fn png_bytes_decode() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([10, 20, 30, 255]));
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .expect("png");
        let texture = decode_bytes(cursor.get_ref(), "pixel.png".to_string()).expect("decode");
        assert_eq!(texture.rgba, vec![10, 20, 30, 255]);
        assert!(!texture.missing);
    }
}
