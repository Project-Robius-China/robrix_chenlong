//! Modal that displays a Matrix room's QR code.
//!
//! Opened by dispatching `QrCodeModalAction::Open { room_name, url, room_id }`.

use makepad_widgets::{image_cache::ImageBuffer, *};
use matrix_sdk::ruma::OwnedRoomId;

use crate::{
    cpu_worker::{CpuJob, GenerateQrCodeJob, QrCodeGeneratedAction, spawn_cpu_job},
    shared::popup_list::{PopupKind, enqueue_popup_notification},
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.QrCodeModal = #(QrCodeModal::register_widget(vm)) {
        width: Fit
        height: Fit

        RoundedView {
            flow: Down
            width: Fit
            height: Fit
            padding: Inset{top: 24, right: 24, bottom: 20, left: 24}
            spacing: 12

            show_bg: true
            draw_bg +: {
                color: (COLOR_PRIMARY)
                border_radius: 6.0
            }

            title_row := View {
                width: Fill, height: Fit
                flow: Right
                align: Align{y: 0.5}

                title_label := Label {
                    width: Fill, height: Fit
                    draw_text +: {
                        text_style: TITLE_TEXT {font_size: 13}
                        color: #000
                    }
                    text: "Room QR Code"
                }

                close_button := RobrixNeutralIconButton {
                    width: 28, height: 28
                    padding: 4
                    draw_icon.svg: (ICON_CLOSE)
                    icon_walk: Walk{width: 14, height: 14}
                    text: ""
                }
            }

            qr_image := Image {
                width: Fit, height: Fit
                visible: false
            }

            loading_label := Label {
                width: Fill, height: Fit
                text: "Generating QR code..."
                draw_text +: {
                    color: #888
                    text_style: REGULAR_TEXT {font_size: 11}
                }
                visible: true
            }

            url_label := Label {
                width: Fill, height: Fit
                flow: Flow.Right{wrap: true}
                draw_text +: {
                    color: #555
                    text_style: REGULAR_TEXT {font_size: 9}
                }
            }
        }
    }
}

/// Actions for controlling the QrCodeModal.
#[derive(Debug)]
pub enum QrCodeModalAction {
    /// Request to open the modal. Triggers async QR generation.
    Open {
        room_name: String,
        url: String,
        room_id: OwnedRoomId,
    },
    /// Modal was closed.
    Close,
}

#[derive(Script, ScriptHook, Widget)]
pub struct QrCodeModal {
    #[deref] view: View,
    #[rust] pending_room_id: Option<OwnedRoomId>,
}

impl Widget for QrCodeModal {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        self.widget_match_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl WidgetMatchEvent for QrCodeModal {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, _scope: &mut Scope) {
        if self.button(cx, ids!(close_button)).clicked(actions)
            || actions.iter().any(|a| matches!(a.downcast_ref(), Some(ModalAction::Dismissed)))
        {
            cx.action(QrCodeModalAction::Close);
            return;
        }

        // Handle async QR generation result.
        for action in actions {
            if let Some(QrCodeGeneratedAction { room_id, rgba, width, height }) = action.downcast_ref() {
                if self.pending_room_id.as_ref() != Some(room_id) {
                    continue;
                }
                self.pending_room_id = None;
                match ImageBuffer::new(rgba, *width as usize, *height as usize) {
                    Ok(buf) => {
                        let texture = Some(buf.into_new_texture(cx));
                        self.view.image(cx, ids!(qr_image)).set_texture(cx, texture);
                        self.view.image(cx, ids!(qr_image)).set_visible(cx, true);
                        self.view.label(cx, ids!(loading_label)).set_visible(cx, false);
                    }
                    Err(e) => {
                        enqueue_popup_notification(
                            format!("Failed to render QR code: {e}"),
                            PopupKind::Error,
                            Some(5.0),
                        );
                        cx.action(QrCodeModalAction::Close);
                    }
                }
                self.redraw(cx);
            }
        }
    }
}

impl QrCodeModal {
    pub fn open(&mut self, cx: &mut Cx, room_name: String, url: String, room_id: OwnedRoomId) {
        self.view.label(cx, ids!(title_label)).set_text(cx, &room_name);
        self.view.label(cx, ids!(url_label)).set_text(cx, &url);
        self.view.image(cx, ids!(qr_image)).set_visible(cx, false);
        self.view.label(cx, ids!(loading_label)).set_visible(cx, true);
        self.pending_room_id = Some(room_id.clone());
        spawn_cpu_job(cx, CpuJob::GenerateQrCode(GenerateQrCodeJob { url, room_id }));
        self.redraw(cx);
    }
}

impl QrCodeModalRef {
    pub fn open(&self, cx: &mut Cx, room_name: String, url: String, room_id: OwnedRoomId) {
        let Some(mut inner) = self.borrow_mut() else { return };
        inner.open(cx, room_name, url, room_id);
    }
}
