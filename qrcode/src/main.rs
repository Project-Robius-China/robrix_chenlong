//! Standalone test binary for the QR-code decode pipeline.
//!
//! Usage:
//!   cargo run --bin qr_decode_test -- path/to/qrcode.png
//!
//! Saves debug PNGs in ./qrcode/ as it works.

use std::{env, process};

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: qr_decode_test <image_path>");
        process::exit(1);
    });

    let img = match image::open(&path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to open '{path}': {e}");
            process::exit(1);
        }
    };
    let (w, h) = (img.width() as usize, img.height() as usize);

    // Convert to luma
    let luma: Vec<u8> = img
        .to_rgba8()
        .chunks_exact(4)
        .map(|p| {
            let r = p[0] as u32;
            let g = p[1] as u32;
            let b = p[2] as u32;
            ((r * 299 + g * 587 + b * 114) / 1000) as u8
        })
        .collect();

    let luma_min = luma.iter().copied().min().unwrap_or(0);
    let luma_max = luma.iter().copied().max().unwrap_or(0);
    let luma_mean = luma.iter().map(|&v| v as u64).sum::<u64>() / luma.len().max(1) as u64;
    println!("Image: {w}x{h}  luma min={luma_min} max={luma_max} mean={luma_mean}");

    // Save original luma as debug PNG
    save_debug_png(&luma, w, h, "qrcode/debug_original");

    // --- PASSES ---

    // Pass 1: raw
    if let Some(c) = try_rqrr(&luma, w, h, "raw") {
        println!("✅ Decoded (raw): {c}");
        return;
    }

    // Pass 2: Otsu
    let threshold = otsu_threshold(&luma);
    println!("Otsu threshold={threshold}");
    let binary: Vec<u8> = luma.iter().map(|&v| if v <= threshold { 0 } else { 255 }).collect();
    save_debug_png(&binary, w, h, "qrcode/debug_otsu");
    if let Some(c) = try_rqrr(&binary, w, h, "otsu") {
        println!("✅ Decoded (otsu): {c}");
        return;
    }

    // Pass 3: grid-crop on raw with improved crop
    if let Some(c) = try_rqrr_grid_crop(&luma, w, h, "grid-crop", true) {
        println!("✅ Decoded (grid-crop): {c}");
        return;
    }

    // Pass 4: grid-crop on otsu (skip if crop is nonsense)
    if let Some(c) = try_rqrr_grid_crop(&binary, w, h, "otsu-grid-crop", true) {
        println!("✅ Decoded (otsu-grid-crop): {c}");
        return;
    }

    // Pass 5: contrast stretch
    let stretched = contrast_stretch(&luma);
    save_debug_png(&stretched, w, h, "qrcode/debug_stretched");
    if let Some(c) = try_rqrr(&stretched, w, h, "stretched") {
        println!("✅ Decoded (stretched): {c}");
        return;
    }
    let st = otsu_threshold(&stretched);
    let sb: Vec<u8> = stretched.iter().map(|&v| if v <= st { 0 } else { 255 }).collect();
    if let Some(c) = try_rqrr(&sb, w, h, "stretched-otsu") {
        println!("✅ Decoded (stretched-otsu): {c}");
        return;
    }
    if let Some(c) = try_rqrr_grid_crop(&stretched, w, h, "stretched-crop", true) {
        println!("✅ Decoded (stretched-crop): {c}");
        return;
    }

    // Pass 6: Inverted luma
    let inverted: Vec<u8> = luma.iter().map(|&v| 255 - v).collect();
    if let Some(c) = try_rqrr(&inverted, w, h, "invert") {
        println!("✅ Decoded (invert): {c}");
        return;
    }

    // Pass 7: Inverted + Otsu
    let ithresh = otsu_threshold(&inverted);
    println!("Invert Otsu threshold={ithresh}");
    let ibinary: Vec<u8> = inverted.iter().map(|&v| if v <= ithresh { 0 } else { 255 }).collect();
    if let Some(c) = try_rqrr(&ibinary, w, h, "invert-otsu") {
        println!("✅ Decoded (invert-otsu): {c}");
        return;
    }

    // Pass 8: Multi-scale downscale
    for scale in [2u32, 3, 4] {
        let sw = w / scale as usize;
        let sh = h / scale as usize;
        if sw < 40 || sh < 40 { continue; }
        let small: Vec<u8> = (0..sh).flat_map(|y| {
            (0..sw).map(move |x| {
                let mut sum: u32 = 0;
                for dy in 0..scale as usize {
                    for dx in 0..scale as usize {
                        let sy = (y * scale as usize + dy).min(h - 1);
                        let sx = (x * scale as usize + dx).min(w - 1);
                        sum += luma[sy * w + sx] as u32;
                    }
                }
                (sum / (scale * scale)) as u8
            })
        }).collect();
        let label = format!("downscale-{scale}x");
        println!("Downscale {scale}x: {sw}x{sh}");
        if let Some(c) = try_rqrr(&small, sw, sh, &label) {
            println!("✅ Decoded ({label}): {c}");
            return;
        }
        let dt = otsu_threshold(&small);
        let db: Vec<u8> = small.iter().map(|&v| if v <= dt { 0 } else { 255 }).collect();
        let label2 = format!("downscale-{scale}x-otsu");
        if let Some(c) = try_rqrr(&db, sw, sh, &label2) {
            println!("✅ Decoded ({label2}): {c}");
            return;
        }
    }

    // Pass 9: Smart-crop (OBS screen capture mode)
    let crop_x = {
        let step = 4usize;
        let samples = (h / step).max(1) as u32;
        (0..w).find(|&x| {
            let sum: u32 = (0..h).step_by(step).map(|y| luma[y * w + x] as u32).sum();
            sum / samples >= 200
        })
    };
    if let Some(bright_col) = crop_x {
        let crop_x = bright_col.saturating_sub(80);
        let crop_w = w - crop_x;
        println!("Smart-crop: bright col={bright_col}, crop at x={crop_x} ({crop_w}px wide)");
        let cropped: Vec<u8> = (0..h)
            .flat_map(|y| luma[y * w + crop_x..y * w + crop_x + crop_w].iter().copied())
            .collect();

        if let Some(c) = try_rqrr(&cropped, crop_w, h, "crop") {
            println!("✅ Decoded (crop): {c}");
            return;
        }
        if let Some(c) = try_rqrr_grid_crop(&cropped, crop_w, h, "crop-grid-crop", true) {
            println!("✅ Decoded (crop-grid-crop): {c}");
            return;
        }
        let t2 = otsu_threshold(&cropped);
        let cb: Vec<u8> = cropped.iter().map(|&v| if v <= t2 { 0 } else { 255 }).collect();
        if let Some(c) = try_rqrr(&cb, crop_w, h, "crop-otsu") {
            println!("✅ Decoded (crop-otsu): {c}");
            return;
        }
        if let Some(c) = try_rqrr_grid_crop(&cb, crop_w, h, "crop-otsu-grid-crop", true) {
            println!("✅ Decoded (crop-otsu-grid-crop): {c}");
            return;
        }
    } else {
        println!("No bright column — skipping smart-crop");
    }

    println!("❌ All decode passes failed.");
}

// ---------------------------------------------------------------------------
// Contrast stretch
// ---------------------------------------------------------------------------
fn contrast_stretch(luma: &[u8]) -> Vec<u8> {
    let min = luma.iter().copied().min().unwrap_or(0);
    let max = luma.iter().copied().max().unwrap_or(255);
    if max <= min + 10 { return luma.to_vec(); }
    luma.iter().map(|&v| {
        let stretched = ((v as f64 - min as f64) / (max - min) as f64 * 255.0) as u8;
        stretched
    }).collect()
}

// ---------------------------------------------------------------------------
// Otsu thresholding
// ---------------------------------------------------------------------------
fn otsu_threshold(luma: &[u8]) -> u8 {
    let mut hist = [0u32; 256];
    for &v in luma { hist[v as usize] += 1; }
    let total = luma.len() as f64;
    let mut sum_b = 0f64;
    let mut w_b = 0f64;
    let sum_total: f64 = hist.iter().enumerate().map(|(i, &c)| i as f64 * c as f64).sum();
    let mut max_between = 0f64;
    let mut best_thr = 128u8;
    for t in 0..=255 {
        let c = hist[t] as f64;
        w_b += c;
        if w_b == 0.0 { continue; }
        let w_f = total - w_b;
        if w_f == 0.0 { break; }
        sum_b += t as f64 * c;
        let m_b = sum_b / w_b;
        let m_f = (sum_total - sum_b) / w_f;
        let between = w_b * w_f * (m_b - m_f) * (m_b - m_f);
        if between > max_between { max_between = between; best_thr = t as u8; }
    }
    best_thr
}

// ---------------------------------------------------------------------------
// Core rqrr decode
// ---------------------------------------------------------------------------
fn try_rqrr(luma: &[u8], w: usize, h: usize, label: &str) -> Option<String> {
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| luma[y * w + x]);
    let grids = img.detect_grids();
    println!("  [{label}] {w}x{h}: {g} grid(s)", g = grids.len());
    for (i, grid) in grids.into_iter().enumerate() {
        match grid.decode() {
            Ok((meta, content)) => {
                println!("  [{label}] grid[{i}]: version={v} ecc={e:?}",
                    v = meta.version.0, e = meta.ecc_level);
                return Some(content);
            }
            Err(e) => {
                println!("  [{label}] grid[{i}]: decode error: {e}");
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Variance-based grid-crop
// ---------------------------------------------------------------------------
fn try_rqrr_grid_crop(luma: &[u8], w: usize, h: usize, label: &str, save_debug: bool) -> Option<String> {
    // Only bother if grids are found
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| luma[y * w + x]);
    if img.detect_grids().is_empty() { return None; }

    let row_var: Vec<u32> = (0..h).map(|y| {
        let row = &luma[y * w..y * w + w];
        let mut var: u32 = 0;
        for x in 1..w { var += (row[x] as i32 - row[x - 1] as i32).unsigned_abs() as u32; }
        var
    }).collect();

    let col_var: Vec<u32> = (0..w).map(|x| {
        let mut var: u32 = 0;
        for y in 1..h { var += (luma[y * w + x] as i32 - luma[(y - 1) * w + x] as i32).unsigned_abs() as u32; }
        var
    }).collect();

    let row_mean = row_var.iter().sum::<u32>() / row_var.len().max(1) as u32;
    let col_mean = col_var.iter().sum::<u32>() / col_var.len().max(1) as u32;
    let row_std = ((row_var.iter().map(|&v| (v as f64 - row_mean as f64).powi(2)).sum::<f64>()
        / row_var.len().max(1) as f64).sqrt()) as u32;
    let col_std = ((col_var.iter().map(|&v| (v as f64 - col_mean as f64).powi(2)).sum::<f64>()
        / col_var.len().max(1) as f64).sqrt()) as u32;

    // Try multiple thresholds: mean + 0.5σ, mean + 1.0σ, mean + 1.5σ
    for mult in [0.5f64, 1.0, 1.5] {
        let rthr = (row_mean as f64 + row_std as f64 * mult) as u32;
        let cthr = (col_mean as f64 + col_std as f64 * mult) as u32;

        let row_lo = row_var.iter().position(|&v| v > rthr).unwrap_or(0);
        let row_hi = row_var.iter().rposition(|&v| v > rthr).map(|p| p + 1).unwrap_or(h);
        let col_lo = col_var.iter().position(|&v| v > cthr).unwrap_or(0);
        let col_hi = col_var.iter().rposition(|&v| v > cthr).map(|p| p + 1).unwrap_or(w);

        let pad_rows = ((row_hi - row_lo) / 10).max(8);
        let pad_cols = ((col_hi - col_lo) / 10).max(8);
        let gy = row_lo.saturating_sub(pad_rows);
        let gh = (row_hi + pad_rows).min(h) - gy;
        let gx = col_lo.saturating_sub(pad_cols);
        let gw = (col_hi + pad_cols).min(w) - gx;

        // Minimum QR code size is ~20x20 modules; at this resolution ~100px minimum
        if gw >= w.saturating_sub(16) || gh >= h.saturating_sub(16) || gw < 60 || gh < 60 {
            continue;
        }

        let crop_label = format!("{label}-{mult}");
        println!("  [{crop_label}] variance-crop {gx},{gy} {gw}x{gh} (original {w}x{h})");

        let cropped: Vec<u8> = (gy..gy + gh)
            .flat_map(|y| luma[y * w + gx..y * w + gx + gw].iter().copied())
            .collect();

        if save_debug {
            save_debug_png(&cropped, gw, gh, &format!("qrcode/debug_{crop_label}"));
        }

        if let Some(c) = try_rqrr(&cropped, gw, gh, &crop_label) {
            return Some(c);
        }

        // Also try Otsu on the crop
        let ct = otsu_threshold(&cropped);
        let cb: Vec<u8> = cropped.iter().map(|&v| if v <= ct { 0 } else { 255 }).collect();
        let cotsu = format!("{crop_label}-otsu");
        if let Some(c) = try_rqrr(&cb, gw, gh, &cotsu) {
            return Some(c);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Save luma as PNG for debugging
// ---------------------------------------------------------------------------
fn save_debug_png(luma: &[u8], w: usize, h: usize, name: &str) {
    let mut rgba = vec![255u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let v = luma[y * w + x];
            let idx = (y * w + x) * 4;
            rgba[idx] = v;
            rgba[idx + 1] = v;
            rgba[idx + 2] = v;
            rgba[idx + 3] = 255;
        }
    }
    let path = format!("{name}.png");
    if let Err(e) = image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8) {
        eprintln!("  Failed to save {path}: {e}");
    } else {
        println!("  Saved {path}");
    }
}
