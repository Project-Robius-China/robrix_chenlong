//! RC car control screen.
//!
//! Presented when the user taps "RC Control" from the Robot tab.  Layout:
//!   left column   — throttle vertical slider (油门, ±100%)
//!   centre column — live MJPEG camera feed + camera-on/off toggle
//!   right column  — direction vertical slider (方向, ±100%)
//!   bottom row    — left/right motor readout, brake, grab/release, back
//!
//! Differential-drive mixing:
//!   left_motor  = clamp(throttle + direction, -1, 1)
//!   right_motor = clamp(throttle - direction, -1, 1)
//!
//! HTTP control: GET http://{ip}/api/control?left={l:.2}&right={r:.2}
//! HTTP stream:  GET http://{ip}/api/camera/stream?fps=30  (MJPEG)

use std::time::Instant;

use makepad_widgets::*;

use crate::{
    gesture_control::{
        GestureAction,
        mjpeg_reader::MjpegReader,
        robot_http::{HttpOutcome, HttpResult, RobotHttpSender},
    },
    settings::app_preferences::AppPreferencesGlobal,
    shared::webrtc_video::WebRtcVideoWidgetExt,
};

/// Actions emitted by RcScreen for its parent widget to handle.
#[derive(Clone, Debug)]
pub enum RcScreenAction {
    /// User tapped "RC Control" in the Robot screen — show the RC overlay.
    Open,
    /// User tapped 返回 inside the RC screen — dismiss the overlay.
    Back,
}

// ─── LiveId for the per-IP RC control request ───────────────────────────────
const RC_CONTROL_REQUEST_ID: LiveId = live_id!(rc_control);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.RcScreen = #(RcScreen::register_widget(vm)) {
        width: Fill, height: Fill
        flow: Down
        show_bg: true
        draw_bg.color: #x0d1117
        padding: Inset{top: 8, bottom: 8, left: 8, right: 8}
        spacing: 6

        // ── Top area: throttle | camera | direction ──────────────────────
        control_row := View {
            width: Fill, height: Fill
            flow: Right
            spacing: 8

            // Left: throttle slider
            throttle_col := View {
                width: 70, height: Fill
                flow: Down
                align: Align{x: 0.5}
                spacing: 4

                throttle_label := Label {
                    text: "油门"
                    draw_text +: {
                        color: #xCCCCCC
                        text_style: theme.font_regular { font_size: 13.0 }
                    }
                }
                throttle_slider := SliderMinimal {
                    width: 40, height: Fill
                    axis: Vertical
                    min: -1.0
                    max: 1.0
                    default: 0.0
                    step: 0.0
                    precision: 0
                    draw_bg +: {
                        color: #x1e2433
                        color_2: #x3a7bd5
                        val_color: #x2ecc71
                        handle_color: #x2ecc71
                        handle_size: uniform(18.0)
                        offset_y: uniform(4.0)
                    }
                    draw_text +: { color: #x00000000 }
                }
                throttle_pct := Label {
                    text: "·0%"
                    draw_text +: {
                        color: #x888888
                        text_style: theme.font_regular { font_size: 11.0 }
                    }
                }
            }

            // Centre: camera feed
            camera_col := View {
                width: Fill, height: Fill
                flow: Down
                spacing: 4

                // Top bar: label + HD button + toggle
                cam_topbar := View {
                    width: Fill, height: Fit
                    flow: Right
                    align: Align{y: 0.5}
                    spacing: 6
                    padding: Inset{bottom: 2}

                    cam_title := Label {
                        width: Fill
                        text: "Camera"
                        draw_text +: {
                            color: #xCCCCCC
                            text_style: theme.font_regular { font_size: 12.0 }
                        }
                    }
                    cam_toggle := Button {
                        text: "摄像头"
                        width: Fit, height: 26
                        draw_text +: {
                            color: #x000000
                            color_hover: #x000000
                            color_down: #x000000
                            text_style: theme.font_regular { font_size: 11.0 }
                        }
                    }
                }

                // Video display
                cam_view := RoundedView {
                    width: Fill, height: Fill
                    flow: Overlay
                    show_bg: true
                    draw_bg +: { color: #x000000, border_radius: 4.0 }

                    cam_video := WebRtcVideo {
                        width: Fill, height: Fill
                        visible: false
                    }

                    cam_placeholder := View {
                        width: Fill, height: Fill
                        align: Align{x: 0.5, y: 0.5}
                        cam_placeholder_label := Label {
                            text: "tap 摄像头 to start"
                            draw_text +: {
                                color: #x444444
                                text_style: theme.font_regular { font_size: 12.0 }
                            }
                        }
                    }
                }
            }

            // Right: direction slider
            direction_col := View {
                width: 70, height: Fill
                flow: Down
                align: Align{x: 0.5}
                spacing: 4

                direction_label := Label {
                    text: "方向"
                    draw_text +: {
                        color: #xCCCCCC
                        text_style: theme.font_regular { font_size: 13.0 }
                    }
                }
                direction_slider := SliderMinimal {
                    width: 40, height: Fill
                    axis: Vertical
                    min: -1.0
                    max: 1.0
                    default: 0.0
                    step: 0.0
                    precision: 0
                    draw_bg +: {
                        color: #x1e2433
                        color_2: #x3a7bd5
                        val_color: #x3a7bd5
                        handle_color: #x5a9de0
                        handle_size: uniform(18.0)
                        offset_y: uniform(4.0)
                    }
                    draw_text +: { color: #x00000000 }
                }
                direction_pct := Label {
                    text: "·0%"
                    draw_text +: {
                        color: #x888888
                        text_style: theme.font_regular { font_size: 11.0 }
                    }
                }
            }
        }

        // ── Motor readout ────────────────────────────────────────────────
        motor_row := View {
            width: Fill, height: Fit
            flow: Right
            align: Align{y: 0.5}
            spacing: 6

            motor_left_label := Label {
                text: "左"
                draw_text +: {
                    color: #xCCCCCC
                    text_style: theme.font_regular { font_size: 13.0 }
                }
            }
            motor_left_value := Label {
                text: "0.00"
                draw_text +: {
                    color: #x2ecc71
                    text_style: theme.font_bold { font_size: 13.0 }
                }
            }
            motor_spacer := View { width: 10, height: Fit }
            motor_right_label := Label {
                text: "右"
                draw_text +: {
                    color: #xCCCCCC
                    text_style: theme.font_regular { font_size: 13.0 }
                }
            }
            motor_right_value := Label {
                text: "0.00"
                draw_text +: {
                    color: #x2ecc71
                    text_style: theme.font_bold { font_size: 13.0 }
                }
            }
            motor_dot := RoundedView {
                width: 8, height: 8
                show_bg: true
                draw_bg +: { color: #x888888, border_radius: 4.0 }
            }
        }

        // ── Brake (full width, red) ──────────────────────────────────────
        btn_brake := Button {
            text: "■ 刹车"
            width: Fill, height: 44
            draw_bg +: { color: #x8b1a1a }
            draw_text +: {
                color: #xFFFFFF
                color_hover: #xFFFFFF
                color_down: #xFFFFFF
                text_style: theme.font_bold { font_size: 15.0 }
            }
        }

        // ── Grab / Release ───────────────────────────────────────────────
        action_row := View {
            width: Fill, height: Fit
            flow: Right
            spacing: 6

            btn_grab := Button {
                text: "✋ 抓取"
                width: Fill, height: 40
                draw_bg +: { color: #x1a6b2a }
                draw_text +: {
                    color: #xFFFFFF
                    color_hover: #xFFFFFF
                    color_down: #xFFFFFF
                    text_style: theme.font_bold { font_size: 14.0 }
                }
            }
            btn_release := Button {
                text: "✋ 释放"
                width: Fill, height: 40
                draw_bg +: { color: #x1a3a8b }
                draw_text +: {
                    color: #xFFFFFF
                    color_hover: #xFFFFFF
                    color_down: #xFFFFFF
                    text_style: theme.font_bold { font_size: 14.0 }
                }
            }
        }

        // ── Back ─────────────────────────────────────────────────────────
        back_row := View {
            width: Fill, height: Fit
            align: Align{x: 0.5, y: 0.5}

            btn_back := Button {
                text: "返回"
                width: 90, height: 34
                draw_text +: { color: #x000000, color_hover: #x000000, color_down: #x000000 }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RcScreen {
    #[deref] view: View,

    /// Current throttle value in [-1.0, 1.0]. Positive = forward.
    #[rust] throttle: f64,
    /// Current direction value in [-1.0, 1.0]. Positive = turn right.
    #[rust] direction: f64,

    /// Computed left motor value (throttle + direction, clamped).
    #[rust] left_motor: f64,
    /// Computed right motor value (throttle - direction, clamped).
    #[rust] right_motor: f64,

    /// Robot IP (host[:port]) sourced from AppPreferences.
    #[rust] ip: Option<String>,

    /// HTTP sender for RC control commands.
    #[rust] http: Option<RobotHttpSender>,

    /// Active MJPEG reader. `None` when camera is off.
    #[rust] mjpeg: Option<MjpegReader>,

    /// Whether the MJPEG camera stream is running.
    #[rust] camera_active: bool,

    /// Wall-clock time of the last control command send.
    #[rust] last_send_at: Option<Instant>,

    /// True once we've pulled the IP and subscribed to NextFrame.
    #[rust] initialized: bool,

    /// True once we've received at least one MJPEG frame (used to hide
    /// the placeholder label).
    #[rust] first_frame_seen: bool,
}

const SEND_INTERVAL_MS: u64 = 100;

impl Widget for RcScreen {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.initialized {
            self.initialize(cx);
        }
        self.view.draw_walk(cx, scope, walk)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.initialized {
            self.initialize(cx);
        }

        if matches!(event, Event::NextFrame(_)) {
            self.pump_mjpeg_frames(cx);
            self.maybe_send_control(cx);
            cx.new_next_frame();
        }

        if let Event::NetworkResponses(responses) = event {
            for resp in responses {
                // Drain RC control responses (we only care about errors for now).
                if let NetworkResponse::HttpError { request_id, error } = resp {
                    if *request_id == RC_CONTROL_REQUEST_ID {
                        log!("RcScreen: control error: {}", error.message);
                    }
                }
                // Also forward to the gesture-action HTTP sender if active.
                if let Some(sender) = self.http.as_mut() {
                    if let Some(result) = sender.handle_network_response(resp) {
                        self.apply_http_result(cx, result);
                    }
                }
            }
        }

        self.view.handle_event(cx, event, scope);

        if let Event::Actions(actions) = event {
            self.handle_actions(cx, actions);
        }
    }
}

impl RcScreen {
    fn initialize(&mut self, cx: &mut Cx) {
        if cx.has_global::<AppPreferencesGlobal>() {
            self.ip = cx.global::<AppPreferencesGlobal>().0.robot_control_ip.clone();
        }
        cx.new_next_frame();
        self.initialized = true;
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        // Throttle slider
        let throttle_ref = self.view.slider(cx, ids!(throttle_col.throttle_slider));
        if let Some(a) = actions.find_widget_action(throttle_ref.widget_uid()) {
            match a.cast() {
                SliderAction::Slide(v) | SliderAction::EndSlide(v) => {
                    self.throttle = v;
                    self.update_motors(cx);
                }
                _ => {}
            }
        }

        // Direction slider
        let dir_ref = self.view.slider(cx, ids!(direction_col.direction_slider));
        if let Some(a) = actions.find_widget_action(dir_ref.widget_uid()) {
            match a.cast() {
                SliderAction::Slide(v) | SliderAction::EndSlide(v) => {
                    self.direction = v;
                    self.update_motors(cx);
                }
                _ => {}
            }
        }

        // Camera toggle
        if self.view.button(cx, ids!(camera_col.cam_topbar.cam_toggle)).clicked(actions) {
            if self.camera_active {
                self.stop_camera(cx);
            } else {
                self.start_camera(cx);
            }
        }

        // Brake
        if self.view.button(cx, ids!(btn_brake)).clicked(actions) {
            self.throttle = 0.0;
            self.direction = 0.0;
            self.update_motors(cx);
            // Reset sliders to 0.
            self.view.slider(cx, ids!(throttle_col.throttle_slider)).set_value(cx, 0.0);
            self.view.slider(cx, ids!(direction_col.direction_slider)).set_value(cx, 0.0);
            self.send_control_now(cx);
        }

        // Grab
        if self.view.button(cx, ids!(action_row.btn_grab)).clicked(actions) {
            self.fire_gesture(cx, GestureAction::Catch);
        }

        // Release
        if self.view.button(cx, ids!(action_row.btn_release)).clicked(actions) {
            self.fire_gesture(cx, GestureAction::Drop);
        }

        // Back — tell the parent RobotScreen to dismiss the RC overlay.
        if self.view.button(cx, ids!(back_row.btn_back)).clicked(actions) {
            self.stop_camera(cx);
            cx.widget_action(self.widget_uid(), RcScreenAction::Back);
        }
    }

    /// Recompute left/right motor values from throttle + direction and update UI.
    fn update_motors(&mut self, cx: &mut Cx) {
        self.left_motor = (self.throttle + self.direction).clamp(-1.0, 1.0);
        self.right_motor = (self.throttle - self.direction).clamp(-1.0, 1.0);

        let pct_t = (self.throttle * 100.0).round() as i32;
        let pct_d = (self.direction * 100.0).round() as i32;

        self.view
            .label(cx, ids!(throttle_col.throttle_pct))
            .set_text(cx, &format!("·{}%", pct_t));
        self.view
            .label(cx, ids!(direction_col.direction_pct))
            .set_text(cx, &format!("·{}%", pct_d));
        self.view
            .label(cx, ids!(motor_row.motor_left_value))
            .set_text(cx, &format!("{:.2}", self.left_motor));
        self.view
            .label(cx, ids!(motor_row.motor_right_value))
            .set_text(cx, &format!("{:.2}", self.right_motor));
        self.view.redraw(cx);
    }

    /// Send a one-shot motor command immediately (used by Brake).
    fn send_control_now(&mut self, cx: &mut Cx) {
        let Some(ref ip) = self.ip.clone() else { return };
        let url = format!(
            "http://{}/api/control?left={:.2}&right={:.2}",
            ip, self.left_motor, self.right_motor
        );
        let req = HttpRequest::new(url, HttpMethod::GET);
        cx.http_request(RC_CONTROL_REQUEST_ID, req);
        self.last_send_at = Some(Instant::now());
    }

    /// Throttled periodic control send (called on every NextFrame).
    fn maybe_send_control(&mut self, cx: &mut Cx) {
        let should_send = match self.last_send_at {
            None => true,
            Some(t) => t.elapsed().as_millis() >= SEND_INTERVAL_MS as u128,
        };
        if should_send {
            self.send_control_now(cx);
        }
    }

    /// Fire a discrete gesture command (Catch / Drop) via the gesture HTTP API.
    fn fire_gesture(&mut self, cx: &mut Cx, action: GestureAction) {
        let Some(ref ip) = self.ip.clone() else { return };
        if self.http.is_none() {
            self.http = Some(RobotHttpSender::new());
        }
        if let Some(sender) = self.http.as_mut() {
            sender.send(cx, action, ip);
        }
    }

    fn apply_http_result(&self, _cx: &mut Cx, result: HttpResult) {
        match result.outcome {
            HttpOutcome::Error(ref e) => log!("RcScreen: gesture HTTP error: {e}"),
            _ => {}
        }
    }

    fn start_camera(&mut self, cx: &mut Cx) {
        let Some(ref ip) = self.ip.clone() else {
            log!("RcScreen: no IP set, cannot start camera");
            return;
        };
        log!("RcScreen: starting MJPEG reader for {ip}");
        self.mjpeg = Some(MjpegReader::start(ip));
        self.camera_active = true;
        self.first_frame_seen = false;
        self.view
            .button(cx, ids!(camera_col.cam_topbar.cam_toggle))
            .set_text(cx, "关闭摄像头");
        self.view.redraw(cx);
    }

    fn stop_camera(&mut self, cx: &mut Cx) {
        log!("RcScreen: stopping MJPEG reader");
        self.mjpeg = None;
        self.camera_active = false;
        self.first_frame_seen = false;
        self.view
            .web_rtc_video(cx, ids!(camera_col.cam_view.cam_video))
            .set_visible(cx, false);
        self.view
            .view(cx, ids!(camera_col.cam_view.cam_placeholder))
            .set_visible(cx, true);
        self.view
            .button(cx, ids!(camera_col.cam_topbar.cam_toggle))
            .set_text(cx, "摄像头");
        self.view.redraw(cx);
    }

    fn pump_mjpeg_frames(&mut self, cx: &mut Cx) {
        let Some(ref reader) = self.mjpeg else { return };
        let Some(frame) = reader.try_recv() else { return };

        if !self.first_frame_seen {
            self.first_frame_seen = true;
            self.view
                .view(cx, ids!(camera_col.cam_view.cam_placeholder))
                .set_visible(cx, false);
            self.view
                .web_rtc_video(cx, ids!(camera_col.cam_view.cam_video))
                .set_visible(cx, true);
        }

        self.view
            .web_rtc_video(cx, ids!(camera_col.cam_view.cam_video))
            .set_frame(cx, frame);
    }
}
