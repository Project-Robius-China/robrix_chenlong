//! MJPEG stream reader for the RC car camera.
//!
//! Opens a persistent TCP connection to the robot's HTTP streaming endpoint
//! (`GET /api/camera/stream?fps=30`), parses the multipart MJPEG body,
//! decodes each JPEG frame to RGBA, and delivers frames via a bounded
//! crossbeam channel.  The reader runs entirely on the shared tokio runtime;
//! the main thread drains frames on each `Event::NextFrame`.

use crossbeam_channel::{Sender, Receiver, bounded};
use makepad_widgets::log;

use crate::shared::webrtc_video::WebRtcVideoFrame;

/// Handle to a running MJPEG reader task. Drop to stop the stream —
/// the channel close propagates to the background task on its next I/O.
pub struct MjpegReader {
    frame_rx: Receiver<WebRtcVideoFrame>,
}

impl MjpegReader {
    /// Start reading MJPEG frames from `http://{ip}/api/camera/stream?fps=30`.
    /// Returns immediately; frames are delivered asynchronously.
    pub fn start(ip: &str) -> Self {
        let (tx, rx) = bounded::<WebRtcVideoFrame>(2);
        let ip = ip.to_string();
        crate::sliding_sync::spawn_async_task(async move {
            read_mjpeg_stream(ip, tx).await;
        });
        MjpegReader { frame_rx: rx }
    }

    /// Non-blocking: returns the newest available frame, discarding any
    /// stale ones queued behind it.
    pub fn try_recv(&self) -> Option<WebRtcVideoFrame> {
        let mut latest = None;
        while let Ok(f) = self.frame_rx.try_recv() {
            latest = Some(f);
        }
        latest
    }
}

async fn read_mjpeg_stream(ip: String, tx: Sender<WebRtcVideoFrame>) {
    let host = ip.split(':').next().unwrap_or(&ip).to_string();
    let addr = if ip.contains(':') {
        ip.clone()
    } else {
        format!("{}:80", ip)
    };

    log!("MjpegReader: connecting to {addr}");
    let stream = match tokio::net::TcpStream::connect(&addr).await {
        Ok(s) => s,
        Err(e) => {
            log!("MjpegReader: connect failed: {e}");
            return;
        }
    };

    use tokio::io::AsyncWriteExt;
    let (read_half, mut write_half) = tokio::io::split(stream);

    let request = format!(
        "GET /api/camera/stream?fps=30 HTTP/1.1\r\n\
         Host: {host}\r\n\
         Accept: image/webp,image/avif,image/*;q=0.8,*/*;q=0.5\r\n\
         Connection: keep-alive\r\n\
         Cache-Control: no-cache\r\n\
         \r\n"
    );
    if let Err(e) = write_half.write_all(request.as_bytes()).await {
        log!("MjpegReader: request write failed: {e}");
        return;
    }

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let mut reader = BufReader::with_capacity(128 * 1024, read_half);

    // --- Parse HTTP response headers ---
    let mut boundary = String::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => {
                log!("MjpegReader: connection closed while reading HTTP headers");
                return;
            }
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("content-type:") {
            if let Some(b) = extract_boundary(&trimmed[13..]) {
                boundary = b;
            }
        }
    }

    if boundary.is_empty() {
        log!("MjpegReader: no boundary in response headers; using fallback 'frame'");
        boundary = "frame".to_string();
    }
    log!("MjpegReader: boundary = {:?}", boundary);

    // The boundary marker in the body is "--<boundary>".
    let marker = format!("--{}", boundary);

    // --- Main frame loop ---
    loop {
        // Skip lines until we hit the boundary marker (or "--<boundary>--" end).
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let t = line.trim();
            if t == marker || t.starts_with(&marker) {
                // Check for terminal boundary "-- marker --"
                if t.ends_with("--") && t.len() > marker.len() {
                    log!("MjpegReader: terminal boundary received, stream ended");
                    return;
                }
                break;
            }
        }

        // Read part headers until blank line.
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let t = line.trim();
            if t.is_empty() {
                break;
            }
            let lower = t.to_ascii_lowercase();
            if lower.starts_with("content-length:") {
                let v = t[15..].trim();
                if let Ok(n) = v.parse::<usize>() {
                    content_length = Some(n);
                }
            }
        }

        let Some(len) = content_length else {
            log!("MjpegReader: part has no Content-Length; skipping");
            continue;
        };
        if len == 0 {
            continue;
        }

        // Read exactly `len` bytes of JPEG data.
        let mut jpeg_buf = vec![0u8; len];
        if let Err(e) = reader.read_exact(&mut jpeg_buf).await {
            log!("MjpegReader: read_exact({len}) failed: {e}");
            return;
        }

        // Decode JPEG → RGBA and enqueue.
        match image::load_from_memory(&jpeg_buf) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let frame = WebRtcVideoFrame {
                    data: rgba.into_raw(),
                    width: w,
                    height: h,
                    participant_id: None,
                };
                // Discard if the consumer can't keep up — bounded(2) naturally
                // keeps only fresh frames.
                let _ = tx.try_send(frame);
            }
            Err(e) => {
                log!("MjpegReader: JPEG decode failed ({len} bytes): {e}");
            }
        }

        // Consume the trailing CRLF after the JPEG payload.
        let mut crlf = String::new();
        let _ = reader.read_line(&mut crlf).await;
    }
}

fn extract_boundary(content_type_value: &str) -> Option<String> {
    for segment in content_type_value.split(';') {
        let s = segment.trim();
        if s.to_ascii_lowercase().starts_with("boundary=") {
            let b = s[9..].trim_matches('"').trim().to_string();
            if !b.is_empty() {
                return Some(b);
            }
        }
    }
    None
}
