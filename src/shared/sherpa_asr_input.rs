use makepad_widgets::*;
use makepad_widgets::makepad_platform::audio::{AudioInfo, AudioBuffer};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{self, Receiver};
use sherpa_onnx::{OnlineRecognizer, OnlineStream};

// ─── Global state (set once by App::handle_startup) ──────────────────────────

pub struct SherpaAsrGlobal {
    pub shared:         Arc<SherpaAsrShared>,
    pub model_dir:      String,
    /// Raw microphone RMS level — cloned from `shared.speaking_level` so
    /// VoIP's SpeakingDetector can share the same Arc without registering
    /// a competing `cx.audio_input` callback.
    pub speaking_level: Arc<AtomicU32>,
}

pub fn init_global(cx: &mut Cx, model_dir: String) {
    let shared = SherpaAsrShared::new();
    let speaking_level = shared.speaking_level.clone();
    cx.set_global(SherpaAsrGlobal { shared, model_dir, speaking_level });
}

// ─── Script registration (shaders + widget) ───────────────────────────────────

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    set_type_default() do #(DrawMicButton::script_shader(vm)){
        ..mod.draw.DrawQuad
        is_recording: 0.0
        amplitude: 0.0
        accent_color: #6BA3BE

        pixel: fn() {
            let p = self.pos - vec2(0.5, 0.5)
            let r = length(p)

            let bg_radius = 0.42
            let bg_mask = clamp(1.0 - (r - bg_radius) * 80.0, 0.0, 1.0)

            let bg_off = vec3(0.25, 0.28, 0.30)
            let bg_on = self.accent_color.xyz
            let bg_color = bg_off.mix(bg_on, self.is_recording)

            let glow_radius = 0.48 + self.amplitude * 0.10
            let glow_mask = clamp(1.0 - (r - glow_radius) * 15.0, 0.0, 1.0) * self.is_recording * 0.7

            // Level meter bars shown while recording
            let level_y_start = -0.35
            let level_height = 0.05
            let level_spacing = 0.07
            let level_width = 0.25

            let mut in_level = false
            let mut level_color = vec3(0.2, 0.9, 0.5)

            if abs(p.x) < level_width && p.y > level_y_start && p.y < level_y_start + level_height && self.amplitude > 0.2 && self.is_recording > 0.5 {
                in_level = true
            }
            if abs(p.x) < level_width && p.y > level_y_start + level_spacing && p.y < level_y_start + level_spacing + level_height && self.amplitude > 0.5 && self.is_recording > 0.5 {
                in_level = true
            }
            if abs(p.x) < level_width && p.y > level_y_start + level_spacing * 2.0 && p.y < level_y_start + level_spacing * 2.0 + level_height && self.amplitude > 0.8 && self.is_recording > 0.5 {
                in_level = true
                level_color = vec3(0.9, 0.7, 0.2)
            }

            let mic_width = 0.08
            let mic_height = 0.18
            let mic_top = 0.05
            let mic_body = abs(p.x) < mic_width && p.y > -mic_height && p.y < mic_top
            let mic_head = length(p - vec2(0.0, mic_top)) < mic_width
            let stand = abs(p.x) < 0.015 && p.y > -mic_height - 0.06 && p.y < -mic_height + 0.02
            let arc_dist = abs(length(p - vec2(0.0, -0.02)) - 0.12)
            let arc = arc_dist < 0.02 && p.y < -0.02
            let mic_icon = mic_body || mic_head || stand || arc

            let mut color = bg_color * 0.6
            let mut alpha = glow_mask
            color = color.mix(bg_color, bg_mask)
            alpha = max(alpha, bg_mask)

            if in_level && bg_mask > 0.5 { color = level_color }
            if mic_icon && bg_mask > 0.5 { color = vec3(1.0, 1.0, 1.0) }

            return vec4(color, alpha)
        }
    }

    set_type_default() do #(DrawSpinner::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: #6BA3BE
        time: 0.0

        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let stroke_width = 4.0
            let radius = min(self.rect_size.x * 0.5, self.rect_size.y * 0.5) - stroke_width * 0.5
            let center = self.rect_size * 0.5
            let rotation = self.time * 2.0 * PI * 1.2
            let rotation_cycles = rotation / (2.0 * PI)
            let arc_phase = modf(rotation_cycles * 0.5, 1.0)
            let expand_phase = clamp(arc_phase / 0.55, 0.0, 1.0)
            let contract_phase = clamp((arc_phase - 0.55) / 0.45, 0.0, 1.0)
            let cycle = expand_phase * (1.0 - contract_phase)
            let gap_ratio = mix(0.12, 0.92, cycle)
            let gap_radians = gap_ratio * 2.0 * PI
            let start_angle = rotation
            sdf.arc_round_caps(center.x center.y radius start_angle start_angle + 2.0 * PI - gap_radians stroke_width)
            return sdf.fill(self.color)
        }
    }

    mod.widgets.SherpaAsrInputBase = #(SherpaAsrInput::register_widget(vm))
    mod.widgets.SherpaAsrInput = set_type_default() do mod.widgets.SherpaAsrInputBase {
        width: 32
        height: 32
        mic_button_size: 32.0
        accent_color: #6BA3BE
        draw_spinner +: { color: #6BA3BE }
    }
}

const ASR_SAMPLE_RATE: f64 = 16000.0;
const MAX_RECENT_SAMPLES: usize = 1600; // 100ms at 16kHz

// ─── Shared state (audio thread ↔ UI timer) ──────────────────────────────────

pub struct SherpaAsrShared {
    pub pending_samples: Mutex<Vec<f32>>,
    pub recent_samples:  Mutex<Vec<f32>>,
    pub is_recording:    AtomicBool,
    /// Raw microphone RMS stored as f32 bits.  Updated by `process_audio_input`
    /// every audio callback; readable by VoIP SpeakingDetector via a cloned Arc.
    pub speaking_level:  Arc<AtomicU32>,
}

impl SherpaAsrShared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pending_samples: Mutex::new(Vec::new()),
            recent_samples:  Mutex::new(Vec::new()),
            is_recording:    AtomicBool::new(false),
            speaking_level:  Arc::new(AtomicU32::new(0)),
        })
    }

    pub fn drain_pending(&self) -> Vec<f32> {
        std::mem::take(&mut *self.pending_samples.lock().unwrap())
    }

    pub fn calculate_amplitude(&self) -> f32 {
        let recent = self.recent_samples.lock().unwrap();
        if recent.is_empty() { return 0.0; }
        let rms = (recent.iter().map(|s| s * s).sum::<f32>() / recent.len() as f32).sqrt();
        (rms * 30.0).min(1.0)
    }
}

// ─── Audio resampler ──────────────────────────────────────────────────────────

fn resample_to_16k_mono(input: &AudioBuffer, from_rate: f64) -> Vec<f32> {
    if input.frame_count() == 0 { return Vec::new(); }
    let ratio = ASR_SAMPLE_RATE / from_rate;
    let new_len = ((input.frame_count() as f64 * ratio).round() as usize).max(1);
    let mut output = vec![0.0f32; new_len];
    let channel_count = input.channel_count().max(1) as f32;
    for i in 0..new_len {
        let src_pos = i as f64 / ratio;
        let src_idx = src_pos as usize;
        let frac = (src_pos - src_idx as f64) as f32;
        let mut s0 = 0.0f32;
        let mut s1 = 0.0f32;
        for ch in 0..input.channel_count() {
            s0 += input.channel(ch).get(src_idx).copied().unwrap_or(0.0);
            s1 += input.channel(ch).get(src_idx + 1).copied().unwrap_or(0.0);
        }
        s0 /= channel_count;
        s1 /= channel_count;
        if s1 == 0.0 { s1 = s0; }
        output[i] = s0 + (s1 - s0) * frac;
    }
    output
}

/// Call this from `cx.audio_input()` callback. Safe to call from any thread.
pub fn process_audio_input(
    shared: &Arc<SherpaAsrShared>,
    info: AudioInfo,
    input_buffer: &AudioBuffer,
) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        log!("[ASR] process_audio_input first call: sample_rate={} frames={} channels={}", info.sample_rate, input_buffer.frame_count(), input_buffer.channel_count());
    }
    let resampled = resample_to_16k_mono(input_buffer, info.sample_rate);
    {
        let mut recent = shared.recent_samples.lock().unwrap();
        recent.extend_from_slice(&resampled);
        let len = recent.len();
        if len > MAX_RECENT_SAMPLES {
            recent.drain(0..len - MAX_RECENT_SAMPLES);
        }
    }
    // Update raw RMS so VoIP SpeakingDetector can read it without its own callback.
    if !resampled.is_empty() {
        let rms = (resampled.iter().map(|s| s * s).sum::<f32>() / resampled.len() as f32).sqrt();
        shared.speaking_level.store(rms.to_bits(), Ordering::Relaxed);
    }
    if shared.is_recording.load(Ordering::SeqCst) {
        shared.pending_samples.lock().unwrap().extend_from_slice(&resampled);
    }
}

// ─── Draw structs (shader registration in host app's script_mod!) ─────────────

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawMicButton {
    #[deref] pub draw_super:   DrawQuad,
    #[live]  pub is_recording: f32,
    #[live]  pub amplitude:    f32,
    #[live]  pub accent_color: Vec4,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawSpinner {
    #[deref] pub draw_super: DrawQuad,
    #[live]  pub color:      Vec4,
    #[live]  pub time:       f32,
}

// ─── Actions ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub enum SherpaAsrInputAction {
    #[default]
    None,
    RecordingStarted,
    RecordingStopped,
    InterimResult(String),
    FinalResult(String),
    ModelLoadError(String),
}

// ─── Model loading ────────────────────────────────────────────────────────────

fn load_recognizer(model_dir: &str) -> Result<OnlineRecognizer, String> {
    use sherpa_onnx::{
        OnlineRecognizerConfig, OnlineModelConfig,
        OnlineTransducerModelConfig, OnlineZipformer2CtcModelConfig,
    };
    use std::fs;

    let entries = fs::read_dir(model_dir)
        .map_err(|_| format!("model not found: {}", model_dir))?;

    let mut encoder = None;
    let mut decoder = None;
    let mut joiner  = None;
    let mut ctc     = None;
    let mut tokens  = None;

    // Collect candidates; prefer int8 (smaller, ARM64-safe) over full-precision.
    let mut encoder_fp = None;
    let mut decoder_fp = None;
    let mut joiner_fp  = None;
    let mut ctc_fp     = None;

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path().to_string_lossy().to_string();
        let is_int8 = name.contains(".int8.");
        if name.starts_with("encoder") && name.ends_with(".onnx") {
            if is_int8 { encoder = Some(path); } else { encoder_fp = Some(path); }
        } else if name.starts_with("decoder") && name.ends_with(".onnx") {
            if is_int8 { decoder = Some(path); } else { decoder_fp = Some(path); }
        } else if name.starts_with("joiner") && name.ends_with(".onnx") {
            if is_int8 { joiner = Some(path); } else { joiner_fp = Some(path); }
        } else if name.starts_with("ctc") && name.ends_with(".onnx") {
            if is_int8 { ctc = Some(path); } else { ctc_fp = Some(path); }
        } else if name == "tokens.txt" {
            tokens = Some(path);
        }
    }

    // Fall back to full-precision if no int8 variant exists.
    if encoder.is_none() { encoder = encoder_fp; }
    if decoder.is_none() { decoder = decoder_fp; }
    if joiner.is_none()  { joiner  = joiner_fp; }
    if ctc.is_none()     { ctc     = ctc_fp; }

    let tokens = tokens.ok_or_else(||
        format!("unsupported model layout in {}", model_dir)
    )?;

    let model_config = if let (Some(enc), Some(dec), Some(joi)) = (encoder, decoder, joiner) {
        OnlineModelConfig {
            transducer: OnlineTransducerModelConfig {
                encoder: Some(enc),
                decoder: Some(dec),
                joiner:  Some(joi),
            },
            tokens: Some(tokens),
            num_threads: 1,
            ..Default::default()
        }
    } else if let Some(ctc_path) = ctc {
        OnlineModelConfig {
            zipformer2_ctc: OnlineZipformer2CtcModelConfig {
                model: Some(ctc_path),
            },
            tokens: Some(tokens),
            num_threads: 1,
            ..Default::default()
        }
    } else {
        return Err(format!("unsupported model layout in {}", model_dir));
    };

    let config = OnlineRecognizerConfig {
        model_config,
        enable_endpoint: true,
        ..Default::default()
    };

    OnlineRecognizer::create(&config)
        .ok_or_else(|| format!("sherpa-onnx failed to create recognizer from {}", model_dir))
}

// ─── SIGBUS guard for ONNX model loading ─────────────────────────────────────
//
// On macOS ARM64 the static ONNX Runtime can raise SIGBUS while parsing large
// model files.  A SIGBUS in any thread kills the whole process, so we catch it
// here with sigsetjmp/siglongjmp and turn it into an Err instead.
//
// Safety contract:
//   • Only one loading thread runs at a time (enforced by loading_rx).
//   • The jump buffer lives on the loading thread's stack; the pointer is stored
//     in SIGBUS_JMP_BUF only for the duration of the FFI call.
//   • After longjmp the ONNX Runtime's internal state may be corrupted, but we
//     immediately discard everything and report an error.

use std::sync::atomic::AtomicPtr;

// macOS ARM64: sizeof(sigjmp_buf) = 196 bytes = 49 × i32  (verified with cc)
#[cfg(target_os = "macos")]
#[repr(C)]
struct SigJmpBuf([i32; 49]);

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn sigsetjmp(env: *mut SigJmpBuf, savemask: libc::c_int) -> libc::c_int;
    fn siglongjmp(env: *mut SigJmpBuf, val: libc::c_int) -> !;
}

static SIGBUS_JMP_BUF: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());

#[cfg(target_os = "macos")]
unsafe extern "C" fn sigbus_handler(_sig: libc::c_int) {
    let ptr = SIGBUS_JMP_BUF.load(Ordering::SeqCst) as *mut SigJmpBuf;
    if !ptr.is_null() {
        unsafe { siglongjmp(ptr, 1); }
    }
}

fn load_recognizer_guarded(model_dir: &str) -> Result<OnlineRecognizer, String> {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut buf = SigJmpBuf([0i32; 49]);
        SIGBUS_JMP_BUF.store(&mut buf as *mut SigJmpBuf as *mut u8, Ordering::SeqCst);

        let old = libc::signal(libc::SIGBUS, sigbus_handler as *const () as libc::sighandler_t);

        let result = if sigsetjmp(&mut buf as *mut SigJmpBuf, 1) == 0 {
            Some(load_recognizer(model_dir))
        } else {
            None
        };

        libc::signal(libc::SIGBUS, old);
        SIGBUS_JMP_BUF.store(std::ptr::null_mut(), Ordering::SeqCst);

        match result {
            Some(r) => r,
            None => Err(format!(
                "ONNX Runtime crashed (SIGBUS) loading model at '{}'. \
                 The int8 model files may be more compatible — ensure the directory \
                 contains only *.int8.onnx files.",
                model_dir
            )),
        }
    }

    #[cfg(not(target_os = "macos"))]
    load_recognizer(model_dir)
}

// ─── Widget ───────────────────────────────────────────────────────────────────

/// A standalone microphone button widget that performs on-device streaming ASR
/// via sherpa-onnx and appends recognized text to a linked `CommandTextInput`.
///
/// Wire it up by calling [`SherpaAsrInput::set_command_input`] with a reference
/// to the `CommandTextInput` (or `MentionableTextInput`) you want to target.
#[derive(Script, ScriptHook, Widget)]
pub struct SherpaAsrInput {
    // Required Makepad widget fields
    #[uid]    uid:    WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk]   walk:   Walk,
    #[layout] layout: Layout,

    // Live fields (DSL-configurable)
    #[live] pub model_dir:        String,
    #[live] pub accent_color:     Vec4,
    #[live(40.0)] pub mic_button_size: f64,

    // Draw state
    #[redraw] #[live] draw_mic:     DrawMicButton,
    #[redraw] #[live] draw_spinner: DrawSpinner,

    #[live(true)] #[visible] visible: bool,

    // Rust-only runtime state
    #[rust] recognizer:        Option<OnlineRecognizer>,
    #[rust] stream:            Option<OnlineStream>,
    #[rust] shared:            Option<Arc<SherpaAsrShared>>,
    #[rust] model_dir_loaded:  String,
    #[rust] current_amplitude: f32,
    #[rust] update_timer:      Timer,
    #[rust] mic_area:          Area,
    #[rust] loading_rx:        Option<Receiver<Result<OnlineRecognizer, String>>>,

    /// The `CommandTextInput` (or `MentionableTextInput`) that receives
    /// recognized text. Set via [`set_command_input`].
    #[rust] command_input: WidgetRef,
}

impl SherpaAsrInput {
    /// Called by the host app in `handle_startup`. Stores the shared audio
    /// state and starts the 30 fps update timer.
    pub fn init(&mut self, cx: &mut Cx, shared: Arc<SherpaAsrShared>) {
        self.shared = Some(shared);
        self.update_timer = cx.start_interval(0.033);
    }

    /// Set the model directory at runtime (e.g., from an env var).
    /// The recognizer will be loaded on the next timer tick.
    pub fn set_model_dir(&mut self, cx: &mut Cx, path: &str) {
        self.model_dir = path.to_string();
        self.redraw(cx);
    }

    /// Spawn a background thread (64 MB stack) to load the recognizer so the
    /// main thread is never blocked by protobuf/ONNX parsing.
    fn start_loading(&mut self) {
        let dir = self.model_dir.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || { let _ = tx.send(load_recognizer_guarded(&dir)); })
            .ok();
        self.loading_rx = Some(rx);
        self.model_dir_loaded = self.model_dir.clone();
    }

    /// Wire this mic button to a `CommandTextInput` or `MentionableTextInput`.
    /// On each ASR final result, the recognized text is appended to that
    /// widget's inner `TextInput` and a `TextInputAction::Changed` is posted
    /// so downstream listeners (e.g., mention-search) react normally.
    pub fn set_command_input(&mut self, widget: WidgetRef) {
        self.command_input = widget;
    }

    /// Extract this widget's action from an `Actions` list.
    pub fn handle_action(&self, actions: &Actions) -> Option<SherpaAsrInputAction> {
        actions
            .find_widget_action(self.widget_uid())
            .map(|a| a.cast::<SherpaAsrInputAction>())
    }

    fn push_text_to_command_input(&mut self, cx: &mut Cx, text: String) {
        if self.command_input.is_empty() { return; }

        let text_input = self.command_input.text_input(cx, ids!(text_input));
        let uid = text_input.widget_uid();
        let existing = text_input.text();
        let new_text = if existing.is_empty() {
            text
        } else {
            format!("{} {}", existing, text)
        };
        text_input.set_text(cx, &new_text);
        cx.widget_action(uid, TextInputAction::Changed(new_text));
    }

    fn timer_tick(&mut self, cx: &mut Cx) {
        // ── 0. Poll background loading thread ──────────────────────────────────
        if let Some(rx) = &self.loading_rx {
            if let Ok(result) = rx.try_recv() {
                self.loading_rx = None;
                match result {
                    Ok(rec) => { self.recognizer = Some(rec); }
                    Err(e) => {
                        self.recognizer = None;
                        cx.widget_action(self.widget_uid(), SherpaAsrInputAction::ModelLoadError(e));
                    }
                }
            }
        }

        // ── 1. Detect model_dir change → kick off background load ──────────────
        if self.model_dir != self.model_dir_loaded && self.loading_rx.is_none() {
            if let Some(shared) = &self.shared {
                if shared.is_recording.load(Ordering::SeqCst) {
                    shared.is_recording.store(false, Ordering::SeqCst);
                    self.stream = None;
                    cx.widget_action(self.widget_uid(), SherpaAsrInputAction::RecordingStopped);
                }
            }
            if self.model_dir.is_empty() {
                self.recognizer = None;
                self.model_dir_loaded = String::new();
            } else {
                self.recognizer = None;
                self.start_loading();
            }
        }

        // ── 2. Recognition loop ─────────────────────────────────────────────────
        let is_recording = self.shared.as_ref()
            .map(|s| s.is_recording.load(Ordering::SeqCst))
            .unwrap_or(false);

        if is_recording {
            if self.recognizer.is_some() && self.stream.is_some() && self.shared.is_some() {
                let samples = self.shared.as_ref().unwrap().drain_pending();
                if !samples.is_empty() {
                    self.stream.as_ref().unwrap().accept_waveform(16000, &samples);
                }
                while self.recognizer.as_ref().unwrap().is_ready(self.stream.as_ref().unwrap()) {
                    self.recognizer.as_ref().unwrap().decode(self.stream.as_ref().unwrap());
                }
                let result_opt = self.recognizer.as_ref().unwrap().get_result(self.stream.as_ref().unwrap());
                let is_endpoint = self.recognizer.as_ref().unwrap().is_endpoint(self.stream.as_ref().unwrap());
                if let Some(result) = result_opt {
                    let result_text = result.text.clone();
                    if is_endpoint {
                        // Append final result to CommandTextInput and fire TextInputAction::Changed.
                        self.push_text_to_command_input(cx, result_text.clone());
                        cx.widget_action(
                            self.widget_uid(),
                            SherpaAsrInputAction::FinalResult(result_text),
                        );
                        self.recognizer.as_ref().unwrap().reset(self.stream.as_ref().unwrap());
                    } else if !result_text.is_empty() {
                        cx.widget_action(
                            self.widget_uid(),
                            SherpaAsrInputAction::InterimResult(result_text),
                        );
                    }
                }
            }
        }

        // ── 3. Update amplitude visualization ──────────────────────────────────
        let amplitude = self.shared.as_ref()
            .map(|s| s.calculate_amplitude())
            .unwrap_or(0.0);
        self.current_amplitude = self.current_amplitude * 0.7 + amplitude * 0.3;

        if is_recording {
            log!("[ASR] amplitude={:.4} current={:.4}", amplitude, self.current_amplitude);
        }

        self.redraw(cx);
    }

    fn toggle_recording(&mut self, cx: &mut Cx) {
        let shared = match &self.shared { Some(s) => s.clone(), None => return };
        if self.recognizer.is_none() {
            let msg = if self.model_dir.is_empty() {
                "Speech recognition is not configured. Set MAKEPAD_ASR_MODEL_DIR to a sherpa-onnx model path.".into()
            } else if self.model_dir_loaded == self.model_dir {
                // Tried loading but failed — show a useful error.
                format!("Speech recognition failed to load model at: {}", self.model_dir)
            } else {
                "Speech recognition model is still loading — please wait.".into()
            };
            cx.widget_action(self.widget_uid(), SherpaAsrInputAction::ModelLoadError(msg));
            return;
        }

        if shared.is_recording.load(Ordering::SeqCst) {
            shared.is_recording.store(false, Ordering::SeqCst);
            self.stream = None;
            cx.widget_action(self.widget_uid(), SherpaAsrInputAction::RecordingStopped);
        } else {
            let stream = self.recognizer.as_ref().unwrap().create_stream();
            self.stream = Some(stream);
            shared.pending_samples.lock().unwrap().clear();
            shared.is_recording.store(true, Ordering::SeqCst);
            cx.widget_action(self.widget_uid(), SherpaAsrInputAction::RecordingStarted);
        }
        self.redraw(cx);
    }
}

impl WidgetMatchEvent for SherpaAsrInput {
    fn handle_actions(&mut self, _cx: &mut Cx, _actions: &Actions, _scope: &mut Scope) {}
}

impl Widget for SherpaAsrInput {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, _walk: Walk) -> DrawStep {
        if !self.visible { return DrawStep::done(); }

        self.draw_mic.accent_color = self.accent_color;
        let button_walk = Walk::fixed(self.mic_button_size, self.mic_button_size);

        let is_loading = !self.model_dir.is_empty()
            && self.model_dir != self.model_dir_loaded
            && self.recognizer.is_none();

        if is_loading {
            self.draw_spinner.time = cx.time() as f32;
            let _ = self.draw_spinner.draw_walk(cx, button_walk);
        } else {
            let is_recording = self.shared.as_ref()
                .map(|s| s.is_recording.load(Ordering::SeqCst))
                .unwrap_or(false);
            self.draw_mic.is_recording = if is_recording { 1.0 } else { 0.0 };
            self.draw_mic.amplitude = self.current_amplitude;
            let _ = self.draw_mic.draw_walk(cx, button_walk);
            self.mic_area = self.draw_mic.area();
        }

        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if !self.visible { return; }

        // Lazily pick up the audio shared state set by App::handle_startup.
        if self.shared.is_none() && cx.has_global::<SherpaAsrGlobal>() {
            let (shared, model_dir) = {
                let g = cx.get_global::<SherpaAsrGlobal>();
                (g.shared.clone(), g.model_dir.clone())
            };
            self.shared = Some(shared);
            self.update_timer = cx.start_interval(0.033);
            if self.model_dir.is_empty() && !model_dir.is_empty() {
                self.model_dir = model_dir;
            }
            // Kick off background loading so we never block the UI thread.
            if !self.model_dir.is_empty() && self.recognizer.is_none() && self.loading_rx.is_none() {
                self.start_loading();
            }
        }

        if let Event::Timer(te) = event {
            if self.update_timer.is_timer(te).is_some() {
                self.timer_tick(cx);
            }
        }

        if let Hit::FingerDown(_) = event.hits(cx, self.mic_area) {
            self.toggle_recording(cx);
        }
    }
}

impl SherpaAsrInputRef {
    /// See [`SherpaAsrInput::init`].
    pub fn init(&self, cx: &mut Cx, shared: Arc<SherpaAsrShared>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.init(cx, shared);
        }
    }

    /// See [`SherpaAsrInput::set_command_input`].
    pub fn set_command_input(&self, widget: WidgetRef) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_command_input(widget);
        }
    }

    /// See [`SherpaAsrInput::set_model_dir`].
    pub fn set_model_dir(&self, cx: &mut Cx, path: &str) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_model_dir(cx, path);
        }
    }
}
