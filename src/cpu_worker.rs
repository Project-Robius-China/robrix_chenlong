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

    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| luma[y * w + x]);
    let grids = img.detect_grids();
    for grid in grids {
        if let Ok((_, content)) = grid.decode() {
            Cx::post_action(QrFrameDecodedAction::Found { content });
            return;
        }
    }
    Cx::post_action(QrFrameDecodedAction::NotFound);
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
