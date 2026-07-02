//! Lightweight wrapper for CPU-bound tasks.
//!
//! Currently each job is handled by spawning a detached native thread via
//! Makepad's `cx.spawn_thread`. This keeps the implementation simple while
//! still moving CPU-heavy work off the UI thread.

use makepad_widgets::{Cx, CxOsApi, log};
use std::sync::{atomic::AtomicBool, mpsc::Sender, Arc};
use crate::{
    room::member_search::{self, search_room_members_streaming_with_sort, PrecomputedMemberSort},
    shared::mentionable_text_input::SearchResult,
    sliding_sync::TimelineKind,
};
use matrix_sdk::{room::RoomMember, ruma::OwnedRoomId};

pub enum CpuJob {
    SearchRoomMembers(SearchRoomMembersJob),
    PrecomputeMemberSort(PrecomputeMemberSortJob),
    GenerateQrCode(GenerateQrCodeJob),
    DecodeQrFrame(DecodeQrFrameJob),
}

/// Action posted back to UI thread when precomputed sort is ready.
#[derive(Debug)]
pub struct PrecomputedMemberSortReady {
    pub timeline_kind: TimelineKind,
    pub sort: Arc<PrecomputedMemberSort>,
    /// The Arc<Vec<RoomMember>> this sort was computed for.
    /// Held alive to prevent ABA via address reuse; compared by Arc::ptr_eq.
    pub members_arc: Arc<Vec<RoomMember>>,
}

pub struct PrecomputeMemberSortJob {
    pub timeline_kind: TimelineKind,
    pub members: Arc<Vec<RoomMember>>,
}

pub struct SearchRoomMembersJob {
    pub members: Arc<Vec<RoomMember>>,
    pub search_text: String,
    pub max_results: usize,
    pub sender: Sender<SearchResult>,
    pub search_id: u64,
    pub precomputed_sort: Option<Arc<PrecomputedMemberSort>>,
    pub cancel_token: Option<Arc<AtomicBool>>,
}

fn run_member_search(params: SearchRoomMembersJob) {
    let SearchRoomMembersJob {
        members,
        search_text,
        max_results,
        sender,
        search_id,
        precomputed_sort,
        cancel_token,
    } = params;

    search_room_members_streaming_with_sort(
        members,
        search_text,
        max_results,
        sender,
        search_id,
        precomputed_sort,
        cancel_token,
    );
}

fn run_precompute_sort(params: PrecomputeMemberSortJob) {
    let sort = member_search::precompute_member_sort(&params.members);
    Cx::post_action(PrecomputedMemberSortReady {
        timeline_kind: params.timeline_kind,
        sort: Arc::new(sort),
        members_arc: params.members, // keep alive to prevent ABA
    });
}

pub struct GenerateQrCodeJob {
    pub url: String,
    pub room_id: OwnedRoomId,
}

/// RGBA pixel buffer for a generated QR code image.
#[derive(Debug)]
pub struct QrCodeGeneratedAction {
    pub room_id: OwnedRoomId,
    /// Raw RGBA bytes, `width * height * 4` bytes, row-major.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub struct DecodeQrFrameJob {
    /// RGBA pixels from camera, row-major.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub enum QrFrameDecodedAction {
    Found { content: String },
    NotFound,
}

fn run_generate_qr_code(job: GenerateQrCodeJob) {
    use qrcode::{QrCode, EcLevel};

    const SCALE: u32 = 8;
    const BORDER: u32 = 4; // quiet-zone modules

    let code = match QrCode::with_error_correction_level(job.url.as_bytes(), EcLevel::M) {
        Ok(c) => c,
        Err(e) => {
            log!("QR generation failed: {e}");
            return;
        }
    };

    let modules = code.width() as u32;
    let total = modules + BORDER * 2;
    let px = total * SCALE;
    let mut rgba = vec![255u8; (px * px * 4) as usize];

    for row in 0..modules {
        for col in 0..modules {
            let dark = code[(row as usize, col as usize)] == qrcode::Color::Dark;
            if dark {
                let pr = (row + BORDER) * SCALE;
                let pc = (col + BORDER) * SCALE;
                for dy in 0..SCALE {
                    for dx in 0..SCALE {
                        let idx = ((pr + dy) * px + (pc + dx)) as usize * 4;
                        rgba[idx] = 0;
                        rgba[idx + 1] = 0;
                        rgba[idx + 2] = 0;
                        rgba[idx + 3] = 255;
                    }
                }
            }
        }
    }

    Cx::post_action(QrCodeGeneratedAction {
        room_id: job.room_id,
        rgba,
        width: px,
        height: px,
    });
}

fn run_decode_qr_frame(job: DecodeQrFrameJob) {
    let w = job.width as usize;
    let h = job.height as usize;
    log!("DecodeQrFrame: {}x{} RGBA ({} bytes)", w, h, job.rgba.len());

    // Convert RGBA → luma
    let luma: Vec<u8> = job.rgba
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
    log!("DecodeQrFrame: luma min={} max={} mean={}", luma_min, luma_max, luma_mean);

    // Pass 0: zbarimg CLI (most robust — handles anti-aliased / screen QR codes).
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(content) = try_zbarimg(&luma, w, h) {
        Cx::post_action(QrFrameDecodedAction::Found { content });
        return;
    }

    // Pass 1: raw luma (rqrr's own adaptive threshold).
    if let Some(content) = try_rqrr(&luma, w, h, "raw") {
        Cx::post_action(QrFrameDecodedAction::Found { content });
        return;
    }

    // Pass 2: Otsu-binarized luma.
    let threshold = otsu_threshold(&luma);
    log!("DecodeQrFrame: Otsu threshold={} — retrying with binarized image", threshold);
    let binary: Vec<u8> = luma.iter().map(|&v| if v <= threshold { 0 } else { 255 }).collect();
    if let Some(content) = try_rqrr(&binary, w, h, "otsu") {
        Cx::post_action(QrFrameDecodedAction::Found { content });
        return;
    }

    // Pass 3: try rqrr on the full frame and, if grids are detected but ECC
    // fails, crop tightly around the first grid's bounding box and retry.
    // This helps when the QR code is a small portion of a large frame.
    if let Some(content) = try_rqrr_grid_crop(&luma, w, h, "grid-crop") {
        Cx::post_action(QrFrameDecodedAction::Found { content });
        return;
    }
    if let Some(content) = try_rqrr_grid_crop(&binary, w, h, "otsu-grid-crop") {
        Cx::post_action(QrFrameDecodedAction::Found { content });
        return;
    }

    // Pass 4: Inverted luma — some QR codes are rendered white-on-dark
    // (dark-mode apps, some screen renders).
    let inverted: Vec<u8> = luma.iter().map(|&v| 255 - v).collect();
    if let Some(content) = try_rqrr(&inverted, w, h, "invert") {
        Cx::post_action(QrFrameDecodedAction::Found { content });
        return;
    }

    // Pass 5: Inverted + Otsu.
    let ithresh = otsu_threshold(&inverted);
    log!("DecodeQrFrame: invert Otsu threshold={}", ithresh);
    let ibinary: Vec<u8> = inverted.iter().map(|&v| if v <= ithresh { 0 } else { 255 }).collect();
    if let Some(content) = try_rqrr(&ibinary, w, h, "invert-otsu") {
        Cx::post_action(QrFrameDecodedAction::Found { content });
        return;
    }

    // Pass 6: grid-crop on inverted + Otsu.
    if let Some(content) = try_rqrr_grid_crop(&ibinary, w, h, "invert-otsu-grid-crop") {
        Cx::post_action(QrFrameDecodedAction::Found { content });
        return;
    }

    // Pass 7 & 8: smart crop to the QR code modal region (OBS virtual camera).
    //
    // Only active when we detect a bright column (average luma ≥ 200), which
    // signals an OBS virtual-camera or screen-capture frame where the QR code
    // sits inside a white modal on a dark desktop.  When scanning a physical QR
    // code with a real webcam, no such column exists — the fallback chop would
    // mutilate the QR code, so we skip the crop entirely.
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
        log!("DecodeQrFrame: smart-crop x≥{} ({}px wide) — retrying", crop_x, crop_w);
        let cropped: Vec<u8> = (0..h)
            .flat_map(|y| luma[y * w + crop_x..y * w + w].iter().copied())
            .collect();
        if let Some(content) = try_rqrr(&cropped, crop_w, h, "crop") {
            Cx::post_action(QrFrameDecodedAction::Found { content });
            return;
        }
        if let Some(content) = try_rqrr_grid_crop(&cropped, crop_w, h, "crop-grid-crop") {
            Cx::post_action(QrFrameDecodedAction::Found { content });
            return;
        }

        let t2 = otsu_threshold(&cropped);
        let cb: Vec<u8> = cropped.iter().map(|&v| if v <= t2 { 0 } else { 255 }).collect();
        if let Some(content) = try_rqrr(&cb, crop_w, h, "crop-otsu") {
            Cx::post_action(QrFrameDecodedAction::Found { content });
            return;
        }
        if let Some(content) = try_rqrr_grid_crop(&cb, crop_w, h, "crop-otsu-grid-crop") {
            Cx::post_action(QrFrameDecodedAction::Found { content });
            return;
        }
    } else {
        log!("DecodeQrFrame: no bright column found — skipping smart-crop (likely real webcam, not OBS)");
    }

    // Pass 9–12: bounds-crop from detected grid bounding boxes (test-binary-proven).
    if let Some(content) = try_rqrr_bounds_crop(&luma, w, h, "bounds-crop") {
        Cx::post_action(QrFrameDecodedAction::Found { content });
        return;
    }
    if let Some(content) = try_rqrr_bounds_crop(&binary, w, h, "bounds-crop-otsu") {
        Cx::post_action(QrFrameDecodedAction::Found { content });
        return;
    }

    // All passes failed. Save debug PGMs from raw + Otsu so we can inspect.
    save_debug_pgm(&luma, w, h, "raw");
    save_debug_pgm(&binary, w, h, "otsu");
    Cx::post_action(QrFrameDecodedAction::NotFound);
}

#[cfg(not(target_arch = "wasm32"))]
fn try_zbarimg(luma: &[u8], w: usize, h: usize) -> Option<String> {
    let path = "/tmp/robrix_qr_zbar.pgm";
    // Write a PGM file for zbarimg.
    let mut f = std::fs::File::create(path).ok()?;
    use std::io::Write;
    f.write_all(format!("P5\n{w} {h}\n255\n").as_bytes()).ok()?;
    f.write_all(luma).ok()?;
    drop(f);
    let output = std::process::Command::new("zbarimg")
        .args(["-q", "--raw", path])
        .output()
        .ok()?;
    if output.status.success() {
        let content = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !content.is_empty() {
            log!("DecodeQrFrame[zbarimg]: OK — content={:?}", content);
            return Some(content);
        }
    }
    None
}

fn try_rqrr(luma: &[u8], w: usize, h: usize, label: &str) -> Option<String> {
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| luma[y * w + x]);
    let grids = img.detect_grids();
    log!("DecodeQrFrame[{}]: detected {} grid(s)", label, grids.len());
    for (i, grid) in grids.into_iter().enumerate() {
        match grid.decode() {
            Ok((meta, content)) => {
                log!("DecodeQrFrame[{}]: grid[{}] OK — version={} ecc={:?} content={:?}",
                    label, i, meta.version.0, meta.ecc_level, content);
                return Some(content);
            }
            Err(e) => {
                log!("DecodeQrFrame[{}]: grid[{}] error: {}", label, i, e);
            }
        }
    }
    None
}

/// When rqrr detects grids on a large frame but ECC fails (common when the QR
/// code is a small portion of a 1920×1080 webcam/screenshot frame), scan the
/// image for a dense high-variance rectangular region (QR codes have many sharp
/// black-white transitions per row) and crop tightly around it.
fn try_rqrr_grid_crop(luma: &[u8], w: usize, h: usize, label: &str) -> Option<String> {
    // Only bother if we actually found grids on the full image.
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| luma[y * w + x]);
    if img.detect_grids().is_empty() {
        return None;
    }

    // Scan for the region with highest density of sharp vertical edges (QR codes
    // have lots of module boundaries).  Compute row-wise variance per 8-pixel
    // block, then find the tightest bounding box that captures them.
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

    // Use a threshold: mean + 1 stddev
    let row_mean = row_var.iter().sum::<u32>() / row_var.len().max(1) as u32;
    let col_mean = col_var.iter().sum::<u32>() / col_var.len().max(1) as u32;
    let row_std = ((row_var.iter().map(|&v| (v as f64 - row_mean as f64).powi(2)).sum::<f64>()
        / row_var.len().max(1) as f64).sqrt()) as u32;
    let col_std = ((col_var.iter().map(|&v| (v as f64 - col_mean as f64).powi(2)).sum::<f64>()
        / col_var.len().max(1) as f64).sqrt()) as u32;

    let row_lo = row_var.iter().position(|&v| v > row_mean + row_std).unwrap_or(0);
    let row_hi = row_var.iter().rposition(|&v| v > row_mean + row_std).map(|p| p + 1).unwrap_or(h);
    let col_lo = col_var.iter().position(|&v| v > col_mean + col_std).unwrap_or(0);
    let col_hi = col_var.iter().rposition(|&v| v > col_mean + col_std).map(|p| p + 1).unwrap_or(w);

    // Expand by 10% to include quiet zone.
    let pad_rows = ((row_hi - row_lo) / 10).max(4);
    let pad_cols = ((col_hi - col_lo) / 10).max(4);
    let gy = row_lo.saturating_sub(pad_rows);
    let gh = (row_hi + pad_rows).min(h) - gy;
    let gx = col_lo.saturating_sub(pad_cols);
    let gw = (col_hi + pad_cols).min(w) - gx;

    // Skip if the crop doesn't reduce the region meaningfully.
    if gw >= w.saturating_sub(16) || gh >= h.saturating_sub(16) || gw < 20 || gh < 20 {
        return None;
    }

    log!("DecodeQrFrame[{}]: variance-crop {gx},{gy} {gw}x{gh} (original {w}x{h})", label);
    let cropped: Vec<u8> = (gy..gy + gh)
        .flat_map(|y| luma[y * w + gx..y * w + gx + gw].iter().copied())
        .collect();
    try_rqrr(&cropped, gw, gh, label)
}

/// Crop tightly around the bounding box of the first detected grid (from
/// `grid.bounds`), then retry with both luma and Otsu. This is the approach
/// proven in `qr_decode_test` — it handles QR codes where the grid finder
/// detects the pattern but ECC fails on the full frame.
fn try_rqrr_bounds_crop(luma: &[u8], w: usize, h: usize, label: &str) -> Option<String> {
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| luma[y * w + x]);
    for grid in img.detect_grids() {
        let xs: Vec<i32> = grid.bounds.iter().map(|p| p.x).collect();
        let ys: Vec<i32> = grid.bounds.iter().map(|p| p.y).collect();
        let x0 = (*xs.iter().min().unwrap_or(&0)).max(0) as usize;
        let x1 = ((*xs.iter().max().unwrap_or(&(w as i32 - 1))) as usize).min(w - 1);
        let y0 = (*ys.iter().min().unwrap_or(&0)).max(0) as usize;
        let y1 = ((*ys.iter().max().unwrap_or(&(h as i32 - 1))) as usize).min(h - 1);

        let pad = 24;
        let gx = x0.saturating_sub(pad);
        let gy = y0.saturating_sub(pad);
        let gw = (x1 + pad).min(w) - gx;
        let gh = (y1 + pad).min(h) - gy;
        if gw < 20 || gh < 20 || gw >= w {
            continue;
        }
        log!("DecodeQrFrame[{}]: bounds-crop {gx},{gy} {gw}x{gh} (original {w}x{h})", label);

        let crop: Vec<u8> = (gy..gy + gh)
            .flat_map(|y| luma[y * w + gx..y * w + gx + gw].iter().copied())
            .collect();

        // Try luma crop directly.
        if let Some(c) = try_rqrr(&crop, gw, gh, &format!("{label}-luma")) {
            return Some(c);
        }

        // Otsu-binarize the crop and retry.
        let t = otsu_threshold(&crop);
        let bin: Vec<u8> = crop.iter().map(|&v| if v <= t { 0 } else { 255 }).collect();
        if let Some(c) = try_rqrr(&bin, gw, gh, &format!("{label}-otsu")) {
            return Some(c);
        }
    }
    None
}

/// Otsu's method: find the threshold that maximises inter-class variance.
fn otsu_threshold(luma: &[u8]) -> u8 {
    let mut hist = [0u32; 256];
    for &v in luma { hist[v as usize] += 1; }
    let total = luma.len() as f64;
    let sum: f64 = hist.iter().enumerate().map(|(i, &h)| i as f64 * h as f64).sum();
    let (mut sum_b, mut w_b, mut best_var, mut threshold) = (0.0f64, 0.0f64, 0.0f64, 0u8);
    for i in 0..256usize {
        w_b += hist[i] as f64;
        if w_b == 0.0 { continue; }
        let w_f = total - w_b;
        if w_f == 0.0 { break; }
        sum_b += i as f64 * hist[i] as f64;
        let mu_b = sum_b / w_b;
        let mu_f = (sum - sum_b) / w_f;
        let var = w_b * w_f * (mu_b - mu_f) * (mu_b - mu_f);
        if var > best_var { best_var = var; threshold = i as u8; }
    }
    threshold
}

/// Write one PGM debug file per decode failure so we can visually inspect
/// exactly what rqrr receives. Only the first call per process writes a file
/// (subsequent failures reuse the same path, overwriting it).
fn save_debug_pgm(luma: &[u8], w: usize, h: usize, label: &str) {
    use std::io::Write;
    let path = format!("/tmp/robrix_qr_debug_{label}.pgm");
    match std::fs::File::create(&path) {
        Ok(mut f) => {
            let _ = f.write_all(format!("P5\n{w} {h}\n255\n").as_bytes());
            let _ = f.write_all(luma);
            log!("DecodeQrFrame: saved debug {label} to {path} — open with any PGM viewer");
        }
        Err(e) => log!("DecodeQrFrame: could not write debug PGM: {e}"),
    }
}

/// Spawns a CPU-bound job on a detached native thread.
pub fn spawn_cpu_job(cx: &mut Cx, job: CpuJob) {
    cx.spawn_thread(move || match job {
        CpuJob::SearchRoomMembers(params) => run_member_search(params),
        CpuJob::PrecomputeMemberSort(params) => run_precompute_sort(params),
        CpuJob::GenerateQrCode(params) => run_generate_qr_code(params),
        CpuJob::DecodeQrFrame(params) => run_decode_qr_frame(params),
    });
}
