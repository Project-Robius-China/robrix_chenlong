//! Modal camera scanner that decodes QR codes to join rooms.
//!
//! Uses Makepad's `cx.camera_frame_input` for raw frames (same pattern as
//! `gesture_control/camera_capture.rs`) and `WebcamCapture` for GPU preview.
//! Dispatches `QrScannerModalAction::RoomDetected { content }` when a valid
//! Matrix URI is found in a scanned frame.

use makepad_widgets::*;
use ruma::{MatrixToUri, MatrixUri, matrix_uri::MatrixId};

use crate::{
    cpu_worker::{CpuJob, DecodeQrFrameJob, QrFrameDecodedAction, spawn_cpu_job},
    shared::webcam_capture::{WebcamCaptureAction, WebcamCaptureWidgetRefExt},
};

// On macOS, Makepad's `cx.camera_frame_input` callback never fires in Native
// preview mode (frames go GPU-direct via AVCaptureVideoPreviewLayer). Use a
// parallel AVCaptureSession instead, matching the pattern in robot_screen.rs.
#[cfg(target_os = "macos")]
type QrCapture = crate::gesture_control::avf_capture::AvfCapture;
#[cfg(not(target_os = "macos"))]
type QrCapture = crate::gesture_control::camera_capture::CameraCapture;

#[cfg(target_os = "macos")]
fn start_qr_capture(_cx: &mut Cx) -> Option<QrCapture> {
    match crate::gesture_control::avf_capture::AvfCapture::start() {
        Ok(cap) => Some(cap),
        Err(e) => {
            log!("QrScannerModal: AvfCapture start failed: {e}");
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn start_qr_capture(cx: &mut Cx) -> Option<QrCapture> {
    Some(crate::gesture_control::camera_capture::CameraCapture::start(cx))
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.QrScannerModal = #(QrScannerModal::register_widget(vm)) {
        width: Fit
        height: Fit

        RoundedView {
            flow: Down
            width: 400
            height: Fit
            padding: Inset{top: 16, right: 16, bottom: 16, left: 16}
            spacing: 10

            show_bg: true
            draw_bg +: {
                color: (COLOR_PRIMARY)
                border_radius: 6.0
            }

            title_row := View {
                width: Fill, height: Fit
                flow: Right
                align: Align{y: 0.5}
                spacing: 8

                title_label := Label {
                    width: Fill, height: Fit
                    draw_text +: {
                        text_style: TITLE_TEXT {font_size: 13}
                        color: #000
                    }
                    text: "Scan QR Code to Join Room"
                }

                close_button := RobrixNeutralIconButton {
                    width: 28, height: 28
                    padding: 4
                    draw_icon.svg: (ICON_CLOSE)
                    icon_walk: Walk{width: 14, height: 14}
                    text: ""
                }
            }

            camera_view := View {
                width: 368, height: 276
                webcam := WebcamCapture {}
            }

            status_label := Label {
                width: Fill, height: Fit
                flow: Flow.Right{wrap: true}
                draw_text +: {
                    color: #555
                    text_style: REGULAR_TEXT {font_size: 11}
                }
                text: "Point the camera at a Matrix room QR code."
            }
        }
    }
}

/// Actions for controlling or responding to the QrScannerModal.
#[derive(Debug)]
pub enum QrScannerModalAction {
    /// Request to open the scanner.
    Open,
    /// Modal was closed without a result.
    Close,
    /// A valid Matrix room URI was detected — caller should pre-fill + search.
    RoomDetected {
        /// The raw content string from the QR code (Matrix URI or matrix.to link).
        content: String,
    },
}

#[derive(Script, ScriptHook, Widget)]
pub struct QrScannerModal {
    #[deref] view: View,
    #[rust] camera_capture: Option<QrCapture>,
    #[rust] decoding: bool,
    #[rust] next_frame: NextFrame,
}

impl Widget for QrScannerModal {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        self.widget_match_event(cx, event, scope);

        // Poll camera frames on every frame tick.
        if let Event::NextFrame(_) = event {
            if let Some(ref capture) = self.camera_capture {
                if !self.decoding {
                    if let Some(frame) = capture.try_recv() {
                        self.decoding = true;
                        spawn_cpu_job(cx, CpuJob::DecodeQrFrame(DecodeQrFrameJob {
                            rgba: frame.data,
                            width: frame.width,
                            height: frame.height,
                        }));
                    }
                }
            }
            self.next_frame = cx.new_next_frame();
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl WidgetMatchEvent for QrScannerModal {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, _scope: &mut Scope) {
        // Close button or backdrop dismiss.
        if self.button(cx, ids!(close_button)).clicked(actions)
            || actions.iter().any(|a| matches!(a.downcast_ref(), Some(ModalAction::Dismissed)))
        {
            self.stop_camera(cx);
            cx.action(QrScannerModalAction::Close);
            return;
        }

        // Camera started — register frame callback.
        if let Some(WebcamCaptureAction::CaptureStarted { .. }) = actions
            .iter()
            .find_map(|a| a.as_widget_action()
                .filter(|wa| self.view.widget(cx, ids!(webcam)).widget_uid()
                    == wa.widget_uid)
                .map(|wa| wa.cast_ref::<WebcamCaptureAction>()))
        {
            self.camera_capture = start_qr_capture(cx);
            self.next_frame = cx.new_next_frame();
        }

        // QR decode result.
        for action in actions {
            match action.downcast_ref::<QrFrameDecodedAction>() {
                Some(QrFrameDecodedAction::Found { content }) => {
                    self.decoding = false;
                    if is_matrix_room_uri(content) {
                        self.stop_camera(cx);
                        cx.action(QrScannerModalAction::RoomDetected {
                            content: content.clone(),
                        });
                    } else {
                        // Not a Matrix room link — keep scanning, show hint.
                        self.view.label(cx, ids!(status_label))
                            .set_text(cx, "Not a Matrix room link. Keep scanning...");
                        self.redraw(cx);
                    }
                }
                Some(QrFrameDecodedAction::NotFound) => {
                    self.decoding = false;
                }
                None => {}
            }
        }
    }
}

impl QrScannerModal {
    pub fn open(&mut self, cx: &mut Cx) {
        self.decoding = false;
        self.camera_capture = None;
        self.view.label(cx, ids!(status_label))
            .set_text(cx, "Point the camera at a Matrix room QR code.");
        // Start the webcam preview — frame callback registered on CaptureStarted.
        self.view.widget(cx, ids!(webcam)).as_webcam_capture().start_capture_from_global(cx);
        self.redraw(cx);
    }

    fn stop_camera(&mut self, cx: &mut Cx) {
        self.camera_capture = None;
        self.decoding = false;
        self.view.widget(cx, ids!(webcam)).as_webcam_capture().stop_capture(cx);
    }
}

impl QrScannerModalRef {
    pub fn open(&self, cx: &mut Cx) {
        let Some(mut inner) = self.borrow_mut() else { return };
        inner.open(cx);
    }
}

/// Returns true if `content` is a Matrix room URI (matrix.to or matrix: scheme)
/// that refers to a room (not a user, event, etc.).
fn is_matrix_room_uri(content: &str) -> bool {
    if let Ok(uri) = content.parse::<MatrixToUri>() {
        return matches!(uri.id(), MatrixId::Room(_) | MatrixId::RoomAlias(_));
    }
    if let Ok(uri) = content.parse::<MatrixUri>() {
        return matches!(uri.id(), MatrixId::Room(_) | MatrixId::RoomAlias(_));
    }
    false
}
