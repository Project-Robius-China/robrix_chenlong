//! QR decode test — rqrr (bounds-crop + Otsu) + zbarimg fallback.
use std::{env, process};

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: qr_decode_test <image_path>");
        process::exit(1);
    });

    let img = match image::open(&path) {
        Ok(i) => i,
        Err(e) => { eprintln!("Failed to open '{path}': {e}"); process::exit(1); }
    };
    let (w, h) = (img.width() as usize, img.height() as usize);
    let raw: Vec<u8> = img.to_luma8().into_raw();
    println!("Image: {w}x{h}");

    // --- Primary: zbarimg CLI (most robust) ---
    println!("--- zbarimg ---");
    match process::Command::new("zbarimg")
        .args(["-q", "--raw", &path])
        .output()
    {
        Ok(o) if o.status.success() => {
            let out = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            if !out.is_empty() { println!("✅ zbarimg: {out}"); return; }
        }
        _ => println!("  zbarimg not available or no result"),
    }

    // --- Fallback: rqrr ---
    let t = otsu_threshold(&raw);
    println!("Otsu threshold={t}");

    for (label, luma) in [("raw", &raw[..]), ("otsu", &raw.iter().map(|&v| if v <= t { 0u8 } else { 255u8 }).collect::<Vec<_>>())] {
        if let Some(c) = decode_luma(luma, w, h, label) { println!("✅ {c}"); return; }
        if let Some(c) = decode_with_bounds_crop(luma, w, h, &format!("{label}-crop")) { println!("✅ {c}"); return; }
    }

    println!("❌ All passes failed.");
}

fn decode_luma(luma: &[u8], w: usize, h: usize, label: &str) -> Option<String> {
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| luma[y * w + x]);
    let grids = img.detect_grids();
    println!("[{label}] {g} grid(s)", g = grids.len());
    for grid in grids {
        match grid.decode() {
            Ok((meta, content)) => { println!("[{label}] version={} ecc={:?}", meta.version.0, meta.ecc_level); return Some(content); }
            Err(e) => println!("[{label}] ECC error: {e}"),
        }
    }
    None
}

fn decode_with_bounds_crop(luma: &[u8], w: usize, h: usize, label: &str) -> Option<String> {
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| luma[y * w + x]);
    for grid in img.detect_grids() {
        let xs: Vec<i32> = grid.bounds.iter().map(|p| p.x).collect();
        let ys: Vec<i32> = grid.bounds.iter().map(|p| p.y).collect();
        let x0 = (*xs.iter().min().unwrap_or(&0)).max(0) as usize;
        let x1 = ((*xs.iter().max().unwrap_or(&(w as i32 - 1))) as usize).min(w - 1);
        let y0 = (*ys.iter().min().unwrap_or(&0)).max(0) as usize;
        let y1 = ((*ys.iter().max().unwrap_or(&(h as i32 - 1))) as usize).min(h - 1);

        let pad = 24;
        let gx = x0.saturating_sub(pad); let gy = y0.saturating_sub(pad);
        let gw = (x1 + pad).min(w) - gx; let gh = (y1 + pad).min(h) - gy;
        if gw < 20 || gh < 20 || gw >= w { continue; }
        println!("[{label}] crop {gx},{gy} {gw}x{gh}");

        let crop: Vec<u8> = (gy..gy + gh).flat_map(|y| luma[y * w + gx..y * w + gx + gw].iter().copied()).collect();
        let t = otsu_threshold(&crop);
        let bin: Vec<u8> = crop.iter().map(|&v| if v <= t { 0 } else { 255 }).collect();

        for (slabel, data) in [("luma", &crop), ("otsu", &bin)] {
            let mut ci = rqrr::PreparedImage::prepare_from_greyscale(gw, gh, |x, y| data[y * gw + x]);
            for g in ci.detect_grids() {
                match g.decode() {
                    Ok((_, content)) => { println!("[{label}-{slabel}] OK"); return Some(content); }
                    Err(e) => println!("[{label}-{slabel}] ECC error: {e}"),
                }
            }
        }
    }
    None
}

fn otsu_threshold(luma: &[u8]) -> u8 {
    let mut hist = [0u32; 256];
    for &v in luma { hist[v as usize] += 1; }
    let total = luma.len() as f64;
    let (mut sum_b, mut w_b) = (0f64, 0f64);
    let sum_total: f64 = hist.iter().enumerate().map(|(i, &h)| i as f64 * h as f64).sum();
    let (mut max_var, mut best) = (0f64, 0u8);
    for t in 0..=255 {
        let c = hist[t] as f64;
        w_b += c;
        if w_b == 0.0 { continue; }
        let w_f = total - w_b;
        if w_f == 0.0 { break; }
        sum_b += t as f64 * c;
        let m_b = sum_b / w_b;
        let m_f = (sum_total - sum_b) / w_f;
        let var = w_b * w_f * (m_b - m_f) * (m_b - m_f);
        if var > max_var { max_var = var; best = t as u8; }
    }
    best
}
