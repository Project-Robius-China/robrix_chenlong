# QR Code Features Design

**Date:** 2026-07-01  
**Branch:** china2-webrtc-recognition  
**Status:** Approved

## Overview

Two features: (A) display a QR code for the current room so others can scan and join, and (B) scan a QR code with the device camera to join a room.

---

## Feature A — Room QR Code Display

### Placement
- New "Show QR Code" item in `src/home/room_context_menu.rs` (right-click/long-press on a room in the rooms list).

### Flow
1. User right-clicks a room → context menu → "Show QR Code"
2. `RoomContextMenu` emits a `ShowQrCode { room_id }` action.
3. Parent widget (`rooms_list.rs`) calls `submit_async_request(MatrixRequest::GenerateMatrixLink { room_id, event_id: None, use_matrix_scheme: false, join_on_click: true })`.
4. When `MatrixLinkAction::MatrixToUri(link)` arrives, generate a QR PNG in a background CPU task.
5. QR PNG is encoded with `qrcode` crate + existing `image` crate → raw `Vec<u8>` RGBA bytes.
6. Open `QrCodeModal` (new file: `src/home/qr_code_modal.rs`) with the image bytes rendered in a Makepad `Image` widget via `set_texture`.

### New crate
```toml
qrcode = "0.14"
```

### New files
- `src/home/qr_code_modal.rs` — modal widget with close button and `Image` widget

---

## Feature B — QR Code Camera Scanner

### Placement
- New "Scan QR Code" button in `src/home/add_room.rs`, alongside the existing room-ID text input at the top of the "Join Room" tab.

### Flow
1. User clicks "Scan QR Code" in Add Room screen.
2. Opens `QrScannerModal` (new file: `src/home/qr_scanner_modal.rs`).
3. Modal starts `nokhwa` camera capture in a background thread (reuses existing `nokhwa` dependency).
4. Each captured frame (downscaled to ≤320×240 for speed) is sent to `cpu_worker` via `CpuWorkerRequest::DecodeQrFrame { pixels, width, height }`.
5. CPU worker decodes with `rqrr` crate. On success, posts `QrScanResultAction::Detected { content }`.
6. UI receives action, validates content as a Matrix URI (`MatrixToUri` / `MatrixUri` via existing `ruma` types already imported in `add_room.rs`).
7. On valid Matrix room URI: close scanner, populate the room-ID input, trigger the existing join flow.
8. On invalid URI: show a brief "Not a Matrix room link" overlay, keep scanning.

### New crate
```toml
rqrr = "0.8"
```

### New files
- `src/home/qr_scanner_modal.rs` — modal with webcam preview + decode loop

---

## Architecture Notes

- QR generation runs in `cpu_worker` (`CpuWorkerRequest` enum, existing pattern) to avoid blocking the UI thread.
- Frame decoding also runs in `cpu_worker` for the same reason.
- `nokhwa` is already gated to desktop (`[target.'cfg(not(target_os = "ios"))'.dependencies]`) — scanner is desktop-only for now.
- `qrcode` crate generates a `QrCode` → render to `image::ImageBuffer<Luma<u8>, _>` → convert to RGBA → load as Makepad texture.

---

## Out of Scope
- Android camera (already excluded by existing `nokhwa` gating)
- Scanning non-room Matrix URIs (user profile links, etc.)
- QR code display inside the room screen header (future enhancement)
