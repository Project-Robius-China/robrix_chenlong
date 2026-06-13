//! Background inference thread that owns a `HandModel` and processes frames
//! pushed via a bounded(1) channel.
//!
//! Single-shot mode (one frame per button click) and continuous mode (camera
//! capture pushing every Nth frame) both work — the worker just consumes
//! whatever shows up on `frame_rx`.

use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use makepad_widgets::{Cx, log};

use crate::gesture_control::{
    GestureAction,
    gesture_classifier,
    hand_model::{HandLandmarks, HandModel},
};
use crate::shared::webrtc_video::WebRtcVideoFrame;

/// Lifecycle signal posted by the background `spawn_blocking` task that
/// owns the `HandModel`. The Robot tab listens for this on its Actions
/// loop and only promotes a pending `InferenceWorker` into the active
/// `self.inference` slot once `Ready` has fired — that way callers can
/// assume the worker is actually loaded and looping before they start
/// counting on frame results.
#[derive(Clone, Debug)]
pub enum InferenceWorkerAction {
    /// Model loaded successfully and `worker_loop` is now running.
    Ready,
    /// `HandModel::load` returned an error; the worker exited without
    /// entering its processing loop.
    LoadFailed(String),
}

/// Skip every Nth frame received from the capture callback. With a 30 fps
/// camera and N=3 this gives ~10 inferences per second.
pub const INFERENCE_FRAME_SKIP: u32 = 3;

/// Confidence threshold below which a detection is dropped (no emit, no HTTP).
pub const CONFIDENCE_THRESHOLD: f32 = 0.6;

/// One inference output delivered to the UI thread.
#[derive(Clone, Debug)]
pub struct InferenceResult {
    pub landmarks: Option<HandLandmarks>,
    pub detected: GestureAction,
}

/// Handle to a running inference worker. Drop it to shut down the worker —
/// `frame_tx` closes when the struct goes out of scope, which makes the
/// worker's blocking `recv()` return `Err` and the loop exits naturally.
pub struct InferenceWorker {
    frame_tx: Sender<WebRtcVideoFrame>,
    result_rx: Receiver<InferenceResult>,
}

impl InferenceWorker {
    /// Create the channels and schedule the background worker. Both
    /// `HandModel::load` (CPU-bound 4 MB ONNX parse) and the per-frame
    /// inference loop run inside a single `tokio::task::spawn_blocking`
    /// task on the shared matrix runtime — that way the UI thread never
    /// stalls waiting on the model parse, and there's a single managed
    /// blocking pool for all background CPU work instead of an ad-hoc
    /// thread-per-feature sprawl.
    ///
    /// `mirror_x` is forwarded to `HandModel::run` so the letterbox sampling
    /// loop can un-mirror selfie-camera frames without a separate full-frame
    /// copy. Pass `true` on platforms where the front camera delivers a
    /// mirrored image that hasn't been corrected upstream.
    ///
    /// The returned `Self` is "pending": its `frame_tx` channel exists,
    /// but `worker_loop` hasn't necessarily started consuming frames yet.
    /// Callers should hold the value off to the side until they observe
    /// an [`InferenceWorkerAction::Ready`] on the Actions bus, then
    /// promote it into the active slot. If the load fails, an
    /// [`InferenceWorkerAction::LoadFailed`] is posted instead and the
    /// pending value can be dropped.
    pub fn spawn(mirror_x: bool) -> Self {
        let (frame_tx, frame_rx) = bounded::<WebRtcVideoFrame>(1);
        let (result_tx, result_rx) = unbounded::<InferenceResult>();
        log!("InferenceWorker::spawning (mirror_x={})", mirror_x);
        crate::sliding_sync::spawn_async_task(async move {
            let _ = tokio::task::spawn_blocking(move || {
                match HandModel::load() {
                    Ok(model) => {
                        log!("InferenceWorker: hand-model loaded; posting Ready");
                        Cx::post_action(InferenceWorkerAction::Ready);
                        worker_loop(Arc::new(model), frame_rx, result_tx, mirror_x);
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        log!("InferenceWorker: hand-model load failed: {msg}");
                        Cx::post_action(InferenceWorkerAction::LoadFailed(msg));
                        // Drop frame_rx / result_tx implicitly; pending host
                        // can release the `Self` once it sees LoadFailed.
                    }
                }
            })
            .await;
        });

        Self {
            frame_tx,
            result_rx,
        }
    }

    /// Submit a frame for inference. Returns `false` if the channel is full
    /// (worker is still processing the previous frame) — caller can ignore.
    pub fn submit(&self, frame: WebRtcVideoFrame) -> bool {
        match self.frame_tx.try_send(frame) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Pull the next inference result if available without blocking.
    pub fn try_recv(&self) -> Option<InferenceResult> {
        self.result_rx.try_recv().ok()
    }
}

fn worker_loop(
    model: Arc<HandModel>,
    frame_rx: Receiver<WebRtcVideoFrame>,
    result_tx: Sender<InferenceResult>,
    mirror_x: bool,
) {
    log!("inference worker started");
    while let Ok(frame) = frame_rx.recv() {
        match model.run(&frame.data, frame.width, frame.height, mirror_x) {
            Ok(Some(hand)) => {
                let detected = if hand.confidence >= CONFIDENCE_THRESHOLD {
                    gesture_classifier::classify(&hand.landmarks, hand.handedness)
                        .unwrap_or(GestureAction::None)
                } else {
                    GestureAction::None
                };
                let result = InferenceResult { landmarks: Some(hand), detected };
                if result_tx.send(result).is_err() {
                    break;
                }
            }
            Ok(None) => {
                if result_tx
                    .send(InferenceResult {
                        landmarks: None,
                        detected: GestureAction::None,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Err(e) => {
                log!("inference error: {e:#}");
            }
        }
    }
    log!("inference worker exiting");
}
