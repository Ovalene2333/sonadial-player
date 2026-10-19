use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::viewport::ResizeDirection;
use eframe::egui::{
    self, Align, Align2, Color32, Context, CursorIcon, FontFamily, FontId, Frame, Id, Key, Layout,
    Pos2, Rect, Response, RichText, ScrollArea, Sense, Shape, Stroke, StrokeKind, TextEdit, Ui,
    Vec2, ViewportCommand,
};

use crate::audio::AudioEngine;
use crate::download::{self, DownloadEvent};
use crate::eq::{self, BAND_FREQS, MAX_GAIN_DB, MIN_GAIN_DB, NUM_BANDS};
use crate::spectrum::{SpectrumEngine, VisualState, ANALYSIS_RATE, NUM_BARS};
use crate::storage::{self, PersistedState};
use crate::theme::{self, *};

const TOP_H: f32 = 44.0;
const TOP_LABEL_Y: f32 = 16.0;
const SIDEBAR_W: f32 = 360.0;
const SIDEBAR_W_COMPACT: f32 = 320.0;
const DOCK_H: f32 = 124.0;
const AUDIO_EXTS: &[&str] = &["mp3", "flac", "wav", "ogg", "m4a", "aac", "opus", "alac"];

#[derive(Clone)]
struct TrackEntry {
    path: PathBuf,
    title: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlayMode {
    Sequence,
    RepeatOne,
    Shuffle,
}

impl PlayMode {
    fn label(self) -> &'static str {
        match self {
            Self::Sequence => "SEQUENCE",
            Self::RepeatOne => "REPEAT ONE",
            Self::Shuffle => "SHUFFLE",
        }
    }

    fn short_label(self) -> &'static str {
        match self {
            Self::Sequence => "SEQ",
            Self::RepeatOne => "ONE",
            Self::Shuffle => "SHUF",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Sequence => Self::RepeatOne,
            Self::RepeatOne => Self::Shuffle,
            Self::Shuffle => Self::Sequence,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    System,
    Visuals,
    Theme,
}

enum Action {
    Play(usize),
    AddFiles,
    Toggle,
    Next,
    Prev,
    Stop,
    Seek(f64),
    Volume(f32),
    CycleMode,
    ToggleSettings,
    ToggleEq,
    OpenDownload,
    ToggleSidebar,
    Minimize,
    ToggleMaximize,
    Close,
    ClearError,
}

pub struct SonadialApp {
    engine: AudioEngine,
    analyzer: SpectrumEngine,
    visual: VisualState,
    viz_rotation: f32,
    playlist: Vec<TrackEntry>,
    current: Option<usize>,
    playing: bool,
    paused: bool,
    volume: f32,
    default_volume: f32,
    track_volumes: HashMap<String, f32>,
    search: String,
    samples: Vec<f32>,
    duration: f64,
    format_label: String,
    sample_rate: u32,
    sidebar_open: bool,
    show_settings: bool,
    settings_page: SettingsPage,
    theme_index: usize,
    show_bg_spectrum: bool,
    char_stream: bool,
    show_radial: bool,
    show_core_pulse: bool,
    radial_scale: f32,
    show_eq: bool,
    eq_enabled: bool,
    eq_gains: [f32; NUM_BANDS],
    eq_preset: usize,
    show_download: bool,
    download_url: String,
    download_rx: Option<Receiver<DownloadEvent>>,
    download_cancel: Option<Arc<AtomicBool>>,
    download_progress: f32,
    download_status: String,
    download_running: bool,
    error: Option<String>,
    seek_preview: Option<f32>,
    play_mode: PlayMode,
    last_tick: Instant,
    dirty: bool,
    save_at: Instant,
    device_check_at: Instant,
    output_label: String,
}

impl SonadialApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        let state = storage::load();
        let theme_index = state
            .theme
            .unwrap_or(if state.light { 1 } else { 0 })
            .min(theme::THEME_COUNT - 1);
        theme::set_theme(theme_index);
        theme::install(&cc.egui_ctx);

        let playlist: Vec<TrackEntry> = state
            .playlist
            .iter()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .map(|path| entry_for_path(path))
            .collect();

        let current = state.current.as_ref().and_then(|path| {
            playlist
                .iter()
                .position(|t| t.path.to_string_lossy() == *path)
        });

        let default_volume = state.volume.clamp(0.0, 1.25);
        let track_volumes: HashMap<String, f32> = state
            .track_volumes
            .iter()
            .filter_map(|(path, volume)| {
                volume
                    .is_finite()
                    .then(|| (path.clone(), volume.clamp(0.0, 1.25)))
            })
            .collect();
        let volume = current
            .and_then(|index| playlist.get(index))
            .and_then(|track| track_volumes.get(&track.path.to_string_lossy().to_string()))
            .copied()
            .unwrap_or(default_volume)
            .clamp(0.0, 1.25);
        let mut eq_gains = [0.0_f32; NUM_BANDS];
        for (index, gain) in state.eq_gains.iter().take(NUM_BANDS).enumerate() {
            eq_gains[index] = if gain.is_finite() {
                gain.clamp(MIN_GAIN_DB, MAX_GAIN_DB)
            } else {
                0.0
            };
        }
        let eq_preset = if state.eq_preset <= eq::PRESETS.len() {
            state.eq_preset
        } else {
            1
        };

        let mut app = Self {
            engine: AudioEngine::new(),
            analyzer: SpectrumEngine::new(),
            visual: VisualState::default(),
            viz_rotation: 0.0,
            playlist,
            current,
            playing: false,
            paused: false,
            volume,
            default_volume,
            track_volumes,
            search: String::new(),
            samples: Vec::with_capacity(8192),
            duration: 0.0,
            format_label: "---".to_owned(),
            sample_rate: 44_100,
            sidebar_open: true,
            show_settings: false,
            settings_page: SettingsPage::System,
            theme_index,
            show_bg_spectrum: state.show_bg_spectrum,
            char_stream: state.char_stream,
            show_radial: state.show_radial,
            show_core_pulse: state.show_core_pulse,
            radial_scale: state.radial_scale.clamp(0.7, 1.2),
            show_eq: false,
            eq_enabled: state.eq_enabled,
            eq_gains,
            eq_preset,
            show_download: false,
            download_url: String::new(),
            download_rx: None,
            download_cancel: None,
            download_progress: 0.0,
            download_status: String::new(),
            download_running: false,
            error: None,
            seek_preview: None,
            play_mode: PlayMode::Sequence,
            last_tick: Instant::now(),
            dirty: false,
            save_at: Instant::now(),
            device_check_at: Instant::now(),
            output_label: "NO OUTPUT".to_owned(),
        };

        app.engine.set_volume(app.volume);
        app.engine.set_eq(&app.eq_gains, app.eq_enabled);
        if let Some(index) = app.current {
            let path = app.playlist[index].path.clone();
            if let Ok(info) = crate::audio::probe_track(&path) {
                app.duration = info.duration;
                app.sample_rate = info.sample_rate;
                app.format_label = extension_label(&path);
            }
        }

        if let Some(path) = initial {
            app.add_paths(vec![path.clone()]);
            let current_matches = app
                .current
                .and_then(|index| app.playlist.get(index))
                .map(|track| track.path == path)
                .unwrap_or(false);
            if !(app.playing && current_matches) {
                if let Some(index) = app.playlist.iter().position(|track| track.path == path) {
                    app.play_index(index);
                }
            }
        }

        app
    }

    fn tick(&mut self, ctx: &Context) {
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_tick)
            .as_secs_f32()
            .clamp(0.0, 0.2);
        self.last_tick = now;

        self.engine.drain_analysis(&mut self.samples, 16_384);
        let active = self.playing && !self.paused;
        if active {
            self.analyzer.push(&self.samples);
            self.viz_rotation = (self.viz_rotation - std::f32::consts::PI / 40.0 * dt)
                .rem_euclid(std::f32::consts::TAU);
            self.visual = self.analyzer.tick(true);
        } else {
            self.samples.clear();
            if self.analyzer.has_signal() || self.visual.energy > 0.002 {
                self.analyzer.decay_display(dt);
                self.visual = self.analyzer.tick(false);
                if !self.analyzer.has_signal() && self.visual.energy <= 0.002 {
                    self.analyzer.clear_display();
                    self.visual = VisualState::default();
                    self.viz_rotation = 0.0;
                }
            }
        }

        if self.playing && now.duration_since(self.device_check_at) >= Duration::from_secs(2) {
            self.device_check_at = now;
            match self.engine.follow_default_output() {
                Ok(true) => {
                    self.output_label = self.engine.output_label();
                    ctx.request_repaint();
                }
                Ok(false) => {}
                Err(error) => {
                    self.error = Some(format!("切换默认音频设备失败：{error}"));
                    self.playing = false;
                    self.paused = false;
                }
            }
        }

        if let Some(error) = self.engine.take_error() {
            self.error = Some(error);
            self.playing = false;
        }

        if self.playing && self.engine.is_finished() {
            self.playing = false;
            self.paused = false;
            if !self.playlist.is_empty() {
                self.next_track();
            }
        }

        self.poll_download();

        if self.dirty && now.duration_since(self.save_at) > Duration::from_secs(2) {
            self.save_state();
        }

        if self.playing && !self.paused {
            ctx.request_repaint_after(Duration::from_millis(33));
        } else if self.analyzer.has_signal() || self.visual.energy > 0.002 {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    fn handle_input(&mut self, ctx: &Context) {
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_owned())
                .collect()
        });
        if !dropped.is_empty() {
            self.add_paths(dropped);
        }

        if ctx.egui_wants_keyboard_input() {
            return;
        }

        let (space, ctrl_o, left, right, ctrl_left, ctrl_right, escape) = ctx.input(|input| {
            (
                input.key_pressed(Key::Space),
                input.modifiers.ctrl && input.key_pressed(Key::O),
                input.key_pressed(Key::ArrowLeft),
                input.key_pressed(Key::ArrowRight),
                input.modifiers.ctrl && input.key_pressed(Key::ArrowLeft),
                input.modifiers.ctrl && input.key_pressed(Key::ArrowRight),
                input.key_pressed(Key::Escape),
            )
        });

        if escape {
            self.show_settings = false;
            self.show_eq = false;
            self.show_download = false;
            self.error = None;
            return;
        }
        if ctrl_o {
            self.pick_files();
            return;
        }
        if space {
            self.toggle_play();
            return;
        }
        if ctrl_left {
            self.previous_track();
        } else if ctrl_right {
            self.next_track();
        } else if left {
            self.seek_relative(-5.0);
        } else if right {
            self.seek_relative(5.0);
        }
    }

    fn process_actions(&mut self, ctx: &Context, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Play(index) => self.play_index(index),
                Action::AddFiles => self.pick_files(),
                Action::Toggle => self.toggle_play(),
                Action::Next => self.next_track(),
                Action::Prev => self.previous_track(),
                Action::Stop => self.stop(),
                Action::Seek(fraction) => {
                    let duration = self.duration();
                    self.engine.seek(fraction as f64 * duration);
                    self.engine.flush_analysis();
                    self.analyzer.reset_beat();
                    self.seek_preview = None;
                    self.playing = true;
                    self.paused = false;
                }
                Action::Volume(value) => {
                    self.volume = value.clamp(0.0, 1.25);
                    if let Some(index) = self.current {
                        if let Some(track) = self.playlist.get(index) {
                            let key = track.path.to_string_lossy().to_string();
                            if (self.volume - self.default_volume).abs() < 0.001 {
                                self.track_volumes.remove(&key);
                            } else {
                                self.track_volumes.insert(key, self.volume);
                            }
                        }
                    } else {
                        self.default_volume = self.volume;
                    }
                    self.engine.set_volume(self.volume);
                    self.dirty = true;
                }
                Action::CycleMode => {
                    self.play_mode = self.play_mode.next();
                }
                Action::ToggleSettings => {
                    self.show_settings = !self.show_settings;
                    if self.show_settings {
                        self.show_eq = false;
                        self.show_download = false;
                    }
                }
                Action::ToggleEq => {
                    self.show_eq = !self.show_eq;
                    if self.show_eq {
                        self.show_settings = false;
                        self.show_download = false;
                    }
                }
                Action::OpenDownload => {
                    self.show_download = true;
                    self.show_settings = false;
                    self.show_eq = false;
                }
                Action::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
                Action::Minimize => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
                Action::ToggleMaximize => {
                    let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }
                Action::Close => {
                    self.save_state();
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                Action::ClearError => self.error = None,
            }
        }
    }

    fn select_theme(&mut self, index: usize, ctx: &Context) {
        self.theme_index = index.min(theme::THEME_COUNT - 1);
        theme::set_theme(self.theme_index);
        theme::apply(ctx);
        ctx.request_repaint();
        self.dirty = true;
    }

    fn apply_eq(&mut self) {
        self.engine.set_eq(&self.eq_gains, self.eq_enabled);
        self.dirty = true;
    }

    fn eq_preset_label(&self) -> &'static str {
        if self.eq_preset == 0 || self.eq_preset > eq::PRESETS.len() {
            "CUSTOM"
        } else {
            eq::PRESETS[self.eq_preset - 1].0
        }
    }

    fn cycle_eq_preset(&mut self, delta: i32) {
        let count = eq::PRESETS.len() as i32;
        let current = if self.eq_preset == 0 {
            1
        } else {
            self.eq_preset.min(eq::PRESETS.len()) as i32
        };
        let next = (current - 1 + delta).rem_euclid(count) + 1;
        self.eq_preset = next as usize;
        self.eq_gains = eq::PRESETS[next as usize - 1].1;
        self.apply_eq();
    }

    fn reset_eq(&mut self) {
        self.eq_preset = 1;
        self.eq_gains = eq::PRESETS[0].1;
        self.apply_eq();
    }

    fn start_download(&mut self) {
        if self.download_running {
            return;
        }
        let input = self.download_url.trim().to_owned();
        if input.is_empty() {
            self.download_status = "ENTER A SOURCE URL OR BV ID".to_owned();
            return;
        }

        let output_dir = storage::download_dir();
        let _ = std::fs::create_dir_all(&output_dir);
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        download::spawn(input, output_dir, sender, Arc::clone(&cancel));
        self.download_rx = Some(receiver);
        self.download_cancel = Some(cancel);
        self.download_progress = 0.0;
        self.download_status = "QUEUED".to_owned();
        self.download_running = true;
    }

    fn cancel_download(&mut self) {
        if let Some(cancel) = &self.download_cancel {
            cancel.store(true, Ordering::Relaxed);
            self.download_status = "CANCELLING".to_owned();
        }
    }

    fn poll_download(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.download_rx {
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        for event in events {
            match event {
                DownloadEvent::Status(status) => {
                    self.download_status = status;
                }
                DownloadEvent::Progress(progress) => {
                    self.download_progress = progress;
                }
                DownloadEvent::Done(path) => {
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string());
                    self.download_status = format!("SAVED / {name}");
                    self.download_progress = 1.0;
                    self.download_running = false;
                    self.download_rx = None;
                    self.download_cancel = None;
                    self.add_paths(vec![path]);
                }
                DownloadEvent::Error(error) => {
                    self.download_status = format!("ERROR / {error}");
                    self.error = Some(format!("下载失败：{error}"));
                    self.download_running = false;
                    self.download_rx = None;
                    self.download_cancel = None;
                }
                DownloadEvent::Cancelled => {
                    self.download_status = "CANCELLED".to_owned();
                    self.download_running = false;
                    self.download_rx = None;
                    self.download_cancel = None;
                }
            }
        }

        if disconnected && self.download_running {
            self.download_status = "DOWNLOAD STOPPED".to_owned();
            self.download_running = false;
            self.download_rx = None;
            self.download_cancel = None;
        }
    }

    fn pick_files(&mut self) {
        let files = rfd::FileDialog::new()
            .add_filter("Audio", AUDIO_EXTS)
            .add_filter("All files", &["*"])
            .pick_files();
        if let Some(files) = files {
            self.add_paths(files);
        }
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>) {
        let mut added = Vec::new();
        for path in paths {
            if !path.is_file() || !is_audio_path(&path) {
                continue;
            }
            if self.playlist.iter().any(|track| track.path == path) {
                continue;
            }
            added.push(self.playlist.len());
            self.playlist.push(entry_for_path(path));
        }

        if added.is_empty() {
            return;
        }

        self.dirty = true;
        if self.current.is_none() {
            self.play_index(added[0]);
        }
    }

    fn play_index(&mut self, index: usize) {
        let Some(track) = self.playlist.get(index).cloned() else {
            return;
        };

        self.current = Some(index);
        let path_key = track.path.to_string_lossy().to_string();
        self.volume = self
            .track_volumes
            .get(&path_key)
            .copied()
            .unwrap_or(self.default_volume)
            .clamp(0.0, 1.25);
        self.engine.set_volume(self.volume);
        self.engine.set_eq(&self.eq_gains, self.eq_enabled);
        match self.engine.play(&track.path) {
            Ok(()) => {
                self.playing = true;
                self.paused = false;
                self.error = None;
                self.analyzer.reset();
                self.visual = VisualState::default();
                self.viz_rotation = 0.0;
                if let Some(info) = self.engine.info() {
                    self.duration = info.duration;
                }
                self.format_label = extension_label(&track.path);
                self.output_label = self.engine.output_label();
                self.engine.set_volume(self.volume);
                self.dirty = true;
            }
            Err(error) => {
                self.playing = false;
                self.paused = false;
                self.error = Some(format!("{error:#}"));
            }
        }
    }

    fn toggle_play(&mut self) {
        if self.current.is_none() {
            if !self.playlist.is_empty() {
                self.play_index(0);
            }
            return;
        }

        if self.playing {
            if self.paused {
                self.engine.resume();
                self.paused = false;
            } else {
                self.engine.pause();
                self.paused = true;
            }
        } else {
            self.play_index(self.current.unwrap_or(0));
        }
    }

    fn stop(&mut self) {
        self.engine.stop();
        self.engine.flush_analysis();
        self.playing = false;
        self.paused = false;
        self.seek_preview = None;
    }

    fn next_track(&mut self) {
        if self.playlist.is_empty() {
            return;
        }
        let current = self.current.unwrap_or(0);
        let next = match self.play_mode {
            PlayMode::RepeatOne => current,
            PlayMode::Shuffle => {
                if self.playlist.len() <= 1 {
                    current
                } else {
                    let seed = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.subsec_nanos() as usize)
                        .unwrap_or(1);
                    let mut next = seed % self.playlist.len();
                    if next == current {
                        next = (next + 1) % self.playlist.len();
                    }
                    next
                }
            }
            PlayMode::Sequence => (current + 1) % self.playlist.len(),
        };
        self.play_index(next);
    }

    fn previous_track(&mut self) {
        if self.playlist.is_empty() {
            return;
        }
        let current = self.current.unwrap_or(0);
        let previous = match self.play_mode {
            PlayMode::RepeatOne => current,
            PlayMode::Shuffle => {
                if self.playlist.len() <= 1 {
                    current
                } else {
                    let seed = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.subsec_nanos() as usize)
                        .unwrap_or(1);
                    let mut previous = seed % self.playlist.len();
                    if previous == current {
                        previous = (previous + self.playlist.len() - 1) % self.playlist.len();
                    }
                    previous
                }
            }
            PlayMode::Sequence => (current + self.playlist.len() - 1) % self.playlist.len(),
        };
        self.play_index(previous);
    }

    fn seek_relative(&mut self, seconds: f64) {
        let target = (self.engine.position() + seconds).max(0.0);
        self.engine.seek(target);
        self.engine.flush_analysis();
        self.analyzer.reset_beat();
        self.playing = true;
        self.paused = false;
    }

    fn duration(&self) -> f64 {
        self.engine
            .info()
            .map(|info| info.duration)
            .unwrap_or(self.duration)
    }

    fn position(&self) -> f64 {
        let duration = self.duration();
        let position = self.engine.position();
        if duration > 0.0 {
            position.clamp(0.0, duration)
        } else {
            position.max(0.0)
        }
    }

    fn progress(&self) -> f32 {
        if let Some(preview) = self.seek_preview {
            return preview.clamp(0.0, 1.0);
        }
        let duration = self.duration();
        if duration > 0.0 {
            (self.position() / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        }
    }

    fn current_track(&self) -> Option<&TrackEntry> {
        self.current.and_then(|index| self.playlist.get(index))
    }

    fn save_state(&mut self) {
        let state = PersistedState {
            playlist: self
                .playlist
                .iter()
                .map(|track| track.path.to_string_lossy().to_string())
                .collect(),
            current: self
                .current_track()
                .map(|track| track.path.to_string_lossy().to_string()),
            volume: self.default_volume,
            light: theme::is_light(),
            theme: Some(self.theme_index),
            show_bg_spectrum: self.show_bg_spectrum,
            char_stream: self.char_stream,
            show_radial: self.show_radial,
            show_core_pulse: self.show_core_pulse,
            radial_scale: self.radial_scale,
            track_volumes: self.track_volumes.clone(),
            eq_enabled: self.eq_enabled,
            eq_gains: self.eq_gains.to_vec(),
            eq_preset: self.eq_preset,
        };
        storage::save(&state);
        self.dirty = false;
        self.save_at = Instant::now();
    }

    fn paint_background(&self, ctx: &Context) {
        let painter = ctx.layer_painter(egui::LayerId::background());
        let screen = ctx.content_rect();
        theme::paint_background(&painter, screen);
        painter.rect_stroke(
            screen.shrink(0.5),
            0.0,
            Stroke::new(1.0, theme::line()),
            StrokeKind::Inside,
        );
    }

    fn top_bar(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let ctx = ui.ctx().clone();
        egui::Panel::top("tui-top")
            .exact_size(TOP_H)
            .frame(Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter().clone();
                let cy = rect.center().y;
                painter.line_segment(
                    [
                        Pos2::new(rect.left(), rect.bottom() - 0.5),
                        Pos2::new(rect.right(), rect.bottom() - 0.5),
                    ],
                    Stroke::new(1.0, theme::line()),
                );

                let menu_rect = Rect::from_min_size(
                    Pos2::new(rect.left() + 10.0, cy - 11.0),
                    Vec2::new(30.0, 22.0),
                );
                if text_button(ui, "tui-menu", menu_rect, "[=]", 13.0).clicked() {
                    actions.push(Action::ToggleSidebar);
                }

                let brand_x = rect.left() + 50.0;
                painter.text(
                    Pos2::new(brand_x, cy),
                    Align2::LEFT_CENTER,
                    "sonadial-player",
                    mono(15.0),
                    theme::text_hi(),
                );
                let brand_w = painter
                    .layout_no_wrap("sonadial-player".to_owned(), mono(15.0), theme::text_hi())
                    .size()
                    .x;
                painter.text(
                    Pos2::new(brand_x + brand_w + 9.0, cy),
                    Align2::LEFT_CENTER,
                    "v0.1",
                    mono(12.0),
                    theme::text_dim(),
                );

                let mut x = rect.right() - 10.0;
                for (id, label, action) in [
                    ("tui-close", "[x]", Action::Close),
                    ("tui-max", "[]", Action::ToggleMaximize),
                    ("tui-min", "-", Action::Minimize),
                ] {
                    let button =
                        Rect::from_min_size(Pos2::new(x - 32.0, cy - 11.0), Vec2::new(32.0, 22.0));
                    if text_button(ui, id, button, label, 13.0).clicked() {
                        actions.push(action);
                    }
                    x -= 36.0;
                }

                let sys_rect =
                    Rect::from_min_size(Pos2::new(x - 38.0, cy - 11.0), Vec2::new(38.0, 22.0));
                if text_button(ui, "tui-sys", sys_rect, "SYS", 12.0).clicked() {
                    actions.push(Action::ToggleSettings);
                }
                x -= 42.0;
                let eq_rect =
                    Rect::from_min_size(Pos2::new(x - 34.0, cy - 11.0), Vec2::new(34.0, 22.0));
                if text_button(ui, "tui-eq", eq_rect, "EQ", 12.0).clicked() {
                    actions.push(Action::ToggleEq);
                }
                x -= 38.0;

                let status = format!(
                    "PCM {:.1}K   FFT 1024   {:03} BARS",
                    ANALYSIS_RATE as f32 / 1000.0,
                    NUM_BARS
                );
                let status_w = painter
                    .layout_no_wrap(status.clone(), mono(12.0), theme::text_dim())
                    .size()
                    .x;
                painter.text(
                    Pos2::new(x - 8.0, cy),
                    Align2::RIGHT_CENTER,
                    status,
                    mono(12.0),
                    theme::text_dim(),
                );

                let drag_left = brand_x + brand_w + 56.0;
                let drag_right = (x - status_w - 22.0).max(drag_left + 16.0);
                let drag_rect = Rect::from_min_max(
                    Pos2::new(drag_left, rect.top() + 4.0),
                    Pos2::new(drag_right, rect.bottom() - 4.0),
                );
                let drag = ui.interact(
                    drag_rect,
                    Id::new("tui-window-drag"),
                    Sense::click_and_drag(),
                );
                if drag.drag_started() {
                    ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    actions.push(Action::ToggleMaximize);
                }
                if drag.hovered() {
                    ctx.set_cursor_icon(CursorIcon::Grab);
                }
            });
        actions
    }
    fn sidebar(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let width = if self.sidebar_open { SIDEBAR_W } else { 0.0 };
        egui::Panel::left("tui-sidebar")
            .exact_size(width)
            .resizable(false)
            .frame(Frame::NONE)
            .show(ui, |ui| {
                if !self.sidebar_open {
                    return;
                }
                let rect = ui.max_rect();
                let painter = ui.painter().clone();
                painter.line_segment(
                    [
                        Pos2::new(rect.right() - 0.5, rect.top()),
                        Pos2::new(rect.right() - 0.5, rect.bottom()),
                    ],
                    Stroke::new(1.0, theme::line()),
                );

                let inner_w = (rect.width() - 28.0).max(20.0);
                ui.add_space(TOP_LABEL_Y - 10.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(rect.width(), 20.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new("LIBRARY")
                                .font(mono(14.0))
                                .color(theme::text_hi()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(14.0);
                            ui.label(
                                RichText::new(format!("{:02} TRACKS", self.playlist.len()))
                                    .font(mono(12.0))
                                    .color(theme::text_dim()),
                            );
                        });
                    },
                );

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new("find:")
                            .font(mono(13.0))
                            .color(theme::text_dim()),
                    );
                    ui.add_sized(
                        [(inner_w - 48.0).max(40.0), 20.0],
                        TextEdit::singleline(&mut self.search)
                            .hint_text("title...")
                            .font(title(13.0))
                            .text_color(theme::text_hi())
                            .frame(Frame::NONE),
                    );
                });

                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    if tui_button(ui, "[ add ]", 74.0).clicked() {
                        actions.push(Action::AddFiles);
                    }
                    if tui_button(
                        ui,
                        &format!("[ mode: {} ]", self.play_mode.short_label()),
                        132.0,
                    )
                    .clicked()
                    {
                        actions.push(Action::CycleMode);
                    }
                    if tui_button(ui, "[ download ]", 90.0).clicked() {
                        actions.push(Action::OpenDownload);
                    }
                });

                let line_y = ui.cursor().top() + 5.0;
                painter.line_segment(
                    [
                        Pos2::new(rect.left() + 14.0, line_y),
                        Pos2::new(rect.right() - 14.0, line_y),
                    ],
                    Stroke::new(1.0, theme::line()),
                );
                ui.add_space(5.0);

                let query = self.search.to_lowercase();
                let mut scroll_style = (**ui.style()).clone();
                scroll_style.visuals.widgets.inactive.bg_fill = theme::text_dim();
                scroll_style.visuals.widgets.hovered.bg_fill = theme::text_hi();
                scroll_style.visuals.widgets.active.bg_fill = theme::text_hi();
                scroll_style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, theme::line());
                scroll_style.spacing.scroll = egui::style::ScrollStyle::solid();
                scroll_style.spacing.scroll.bar_width = 6.0;
                scroll_style.spacing.scroll.bar_inner_margin = 2.0;
                scroll_style.spacing.scroll.bar_outer_margin = 4.0;
                ui.scope(|ui| {
                    ui.set_style(scroll_style);
                    ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(inner_w);
                            let row_painter = ui.painter().clone();
                            let mut visible = 0;
                            for (index, track) in self.playlist.iter().enumerate() {
                                if !query.is_empty()
                                    && !track.title.to_lowercase().contains(&query)
                                    && !track.path.to_string_lossy().to_lowercase().contains(&query)
                                {
                                    continue;
                                }
                                visible += 1;
                                let selected = self.current == Some(index);
                                let playing = selected && self.playing;
                                let (row_rect, response) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), 26.0),
                                    Sense::click(),
                                );
                                if response.hovered() {
                                    row_painter.rect_filled(row_rect, 0.0, theme::surface_hi());
                                }
                                if selected {
                                    row_painter.rect_filled(
                                        Rect::from_min_size(
                                            Pos2::new(row_rect.left(), row_rect.top() + 4.0),
                                            Vec2::new(2.0, row_rect.height() - 8.0),
                                        ),
                                        0.0,
                                        theme::text_hi(),
                                    );
                                }
                                let marker = format!(
                                    "{} {:02}{}",
                                    if selected { ">" } else { " " },
                                    index + 1,
                                    if playing { "*" } else { " " }
                                );
                                row_painter.text(
                                    Pos2::new(row_rect.left() + 12.0, row_rect.center().y),
                                    Align2::LEFT_CENTER,
                                    marker,
                                    mono(13.0),
                                    if selected {
                                        theme::text_hi()
                                    } else {
                                        theme::text_dim()
                                    },
                                );

                                let title_rect = Rect::from_min_max(
                                    Pos2::new(row_rect.left() + 70.0, row_rect.top()),
                                    Pos2::new(row_rect.right() - 92.0, row_rect.bottom()),
                                );
                                let clip = row_painter.with_clip_rect(title_rect);
                                let title_font = title(13.0);
                                let fitted = elide_to_width(
                                    &clip,
                                    &track.title,
                                    title_font.clone(),
                                    title_rect.width(),
                                );
                                clip.text(
                                    Pos2::new(title_rect.left(), row_rect.center().y),
                                    Align2::LEFT_CENTER,
                                    fitted,
                                    title_font,
                                    if selected {
                                        theme::text_hi()
                                    } else {
                                        theme::text()
                                    },
                                );

                                let ext = extension_label(&track.path);
                                let time = if selected && self.duration > 0.0 {
                                    short_time(self.duration)
                                } else {
                                    "--:--".to_owned()
                                };
                                row_painter.text(
                                    Pos2::new(row_rect.right() - 12.0, row_rect.center().y),
                                    Align2::RIGHT_CENTER,
                                    format!("{} {}", ext, time),
                                    mono(12.0),
                                    if selected {
                                        theme::text()
                                    } else {
                                        theme::text_dim()
                                    },
                                );

                                if response.clicked() {
                                    actions.push(Action::Play(index));
                                }
                                response.on_hover_cursor(CursorIcon::PointingHand);
                            }
                            if visible == 0 {
                                ui.add_space(12.0);
                                ui.label(
                                    RichText::new("  no tracks")
                                        .font(mono(13.0))
                                        .color(theme::text_dim()),
                                );
                                ui.label(
                                    RichText::new("  drop audio or ctrl+o")
                                        .font(mono(12.0))
                                        .color(theme::text_dim()),
                                );
                            }
                        });
                });
            });
        actions
    }
    fn stage(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        egui::CentralPanel::default()
            .frame(Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter();
                let rotation = self.viz_rotation;

                if self.show_bg_spectrum {
                    if self.char_stream {
                        crate::viz::paint_char_stream(
                            painter,
                            rect,
                            self.analyzer.bars(),
                            self.viz_rotation * 12.0,
                        );
                    } else {
                        crate::viz::paint_bar_spectrum(
                            painter,
                            rect,
                            self.analyzer.bars(),
                            self.playing && !self.paused,
                        );
                    }
                }
                if self.show_radial {
                    crate::viz::paint_spectrum(
                        painter,
                        rect,
                        self.analyzer.bars(),
                        self.playing && !self.paused,
                        rotation,
                        self.radial_scale,
                    );
                }

                painter.text(
                    Pos2::new(rect.left() + 18.0, rect.top() + TOP_LABEL_Y),
                    Align2::LEFT_CENTER,
                    "SPECTRUM / 100",
                    mono(12.0),
                    theme::text_dim(),
                );
                painter.text(
                    Pos2::new(rect.right() - 18.0, rect.top() + TOP_LABEL_Y),
                    Align2::RIGHT_CENTER,
                    "FFT 1024 / 44.1K",
                    mono(12.0),
                    theme::text_dim(),
                );

                let elapsed = short_time(self.position());
                let total = short_time(self.duration());
                let status_rect = crate::viz::paint_core(
                    painter,
                    rect,
                    self.visual,
                    self.playing && !self.paused,
                    self.paused,
                    self.show_core_pulse,
                    &elapsed,
                    &total,
                );
                let core_hit = ui.interact(
                    status_rect.expand(8.0),
                    Id::new("tui-core-toggle"),
                    Sense::click(),
                );
                if core_hit.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if core_hit.clicked() {
                    actions.push(Action::Toggle);
                }
            });
        actions
    }
    fn dock(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        egui::Panel::bottom("tui-dock")
            .exact_size(DOCK_H)
            .frame(Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter().clone();
                painter.line_segment(
                    [
                        Pos2::new(rect.left(), rect.top() + 0.5),
                        Pos2::new(rect.right(), rect.top() + 0.5),
                    ],
                    Stroke::new(1.0, theme::line()),
                );

                let inner = rect.shrink2(Vec2::new(16.0, 8.0));
                let full_title = self
                    .current_track()
                    .map(|track| track.title.clone())
                    .unwrap_or_else(|| "DROP AUDIO HERE / CTRL+O".to_owned());
                let title_font = title(15.0);
                let title_max_w = (rect.width() * 0.55).max(260.0);
                let fitted_title =
                    elide_to_width(&painter, &full_title, title_font.clone(), title_max_w);
                painter.text(
                    Pos2::new(inner.left(), inner.top() + 10.0),
                    Align2::LEFT_CENTER,
                    fitted_title,
                    title_font,
                    if self.current.is_some() {
                        theme::text_hi()
                    } else {
                        theme::text_dim()
                    },
                );

                let live = self.playing && !self.paused;
                painter.text(
                    Pos2::new(inner.right(), inner.top() + 10.0),
                    Align2::RIGHT_CENTER,
                    if live {
                        "[LIVE]"
                    } else if self.paused {
                        "[PAUSED]"
                    } else {
                        "[IDLE]"
                    },
                    mono(12.0),
                    if live {
                        theme::text_hi()
                    } else {
                        theme::text_dim()
                    },
                );

                let output = if self.current.is_some() {
                    elide(&self.output_label, 34)
                } else {
                    "NO OUTPUT".to_owned()
                };
                painter.text(
                    Pos2::new(inner.left(), inner.top() + 30.0),
                    Align2::LEFT_CENTER,
                    format!(
                        "{}   {:.1}K   {}",
                        self.format_label,
                        self.sample_rate as f32 / 1000.0,
                        output
                    ),
                    mono(12.0),
                    theme::text_dim(),
                );
                painter.text(
                    Pos2::new(inner.right(), inner.top() + 30.0),
                    Align2::RIGHT_CENTER,
                    format!("MODE: {}", self.play_mode.label()),
                    mono(12.0),
                    theme::text_dim(),
                );

                let cell_w = painter
                    .layout_no_wrap("M".to_owned(), mono(13.0), theme::text())
                    .size()
                    .x
                    .max(6.0);
                let time_font = mono(13.0);
                let time_text = format!(
                    "{} / {}",
                    short_time(self.position()),
                    short_time(self.duration())
                );
                let time_w = painter
                    .layout_no_wrap(time_text.clone(), time_font.clone(), theme::text_dim())
                    .size()
                    .x;
                let cells = (((title_max_w - 10.0 - time_w) / cell_w).floor() as isize - 2)
                    .clamp(18, 140) as usize;
                let progress = self.progress().clamp(0.0, 1.0);
                let filled = ((progress * cells as f32).round() as usize).min(cells);
                let bar_y = inner.top() + 56.0;
                let bar_w = cell_w * (cells as f32 + 2.0);
                let bar_rect = Rect::from_min_size(
                    Pos2::new(inner.left(), bar_y - 10.0),
                    Vec2::new(bar_w, 20.0),
                );
                painter.text(
                    Pos2::new(bar_rect.left(), bar_y),
                    Align2::LEFT_CENTER,
                    "[",
                    mono(13.0),
                    theme::text_dim(),
                );
                if filled > 0 {
                    painter.text(
                        Pos2::new(bar_rect.left() + cell_w, bar_y),
                        Align2::LEFT_CENTER,
                        "#".repeat(filled),
                        mono(13.0),
                        theme::text_hi(),
                    );
                }
                if filled < cells {
                    painter.text(
                        Pos2::new(bar_rect.left() + cell_w * (filled as f32 + 1.0), bar_y),
                        Align2::LEFT_CENTER,
                        "-".repeat(cells - filled),
                        mono(13.0),
                        theme::alpha(theme::text_dim(), 110),
                    );
                }
                painter.text(
                    Pos2::new(bar_rect.left() + cell_w * (cells as f32 + 1.0), bar_y),
                    Align2::LEFT_CENTER,
                    "]",
                    mono(13.0),
                    theme::text_dim(),
                );
                painter.text(
                    Pos2::new(bar_rect.right() + 10.0, bar_y),
                    Align2::LEFT_CENTER,
                    time_text,
                    time_font,
                    theme::text_dim(),
                );

                let seek = ui.interact(bar_rect, Id::new("tui-seek"), Sense::click_and_drag());
                if seek.dragged() {
                    if let Some(pos) = seek.interact_pointer_pos() {
                        let fraction =
                            ((pos.x - bar_rect.left()) / bar_rect.width()).clamp(0.0, 1.0);
                        self.seek_preview = Some(fraction);
                    }
                }
                if seek.drag_stopped() {
                    if let Some(fraction) = self.seek_preview.take() {
                        actions.push(Action::Seek(fraction as f64));
                    }
                } else if seek.clicked() {
                    if let Some(pos) = seek.interact_pointer_pos() {
                        let fraction =
                            ((pos.x - bar_rect.left()) / bar_rect.width()).clamp(0.0, 1.0);
                        actions.push(Action::Seek(fraction as f64));
                    }
                }

                let vol_y = inner.top() + 82.0;
                let vol_cells = 16usize;
                let vol_norm = (self.volume / 1.25).clamp(0.0, 1.0);
                let vol_filled = ((vol_norm * vol_cells as f32).round() as usize).min(vol_cells);
                let vol_x = inner.left();
                let vol_rect = Rect::from_min_size(
                    Pos2::new(vol_x, vol_y - 10.0),
                    Vec2::new(cell_w * (vol_cells as f32 + 11.0), 20.0),
                );
                painter.text(
                    Pos2::new(vol_x, vol_y),
                    Align2::LEFT_CENTER,
                    if self.current.is_some() {
                        "TRK ["
                    } else {
                        "VOL ["
                    },
                    mono(12.0),
                    theme::text_dim(),
                );
                painter.text(
                    Pos2::new(vol_x + cell_w * 5.0, vol_y),
                    Align2::LEFT_CENTER,
                    "#".repeat(vol_filled),
                    mono(12.0),
                    theme::text(),
                );
                if vol_filled < vol_cells {
                    painter.text(
                        Pos2::new(vol_x + cell_w * (5.0 + vol_filled as f32), vol_y),
                        Align2::LEFT_CENTER,
                        "-".repeat(vol_cells - vol_filled),
                        mono(12.0),
                        theme::alpha(theme::text_dim(), 110),
                    );
                }
                painter.text(
                    Pos2::new(vol_x + cell_w * (5.0 + vol_cells as f32), vol_y),
                    Align2::LEFT_CENTER,
                    format!("] {:>3}%", (vol_norm * 100.0).round() as i32),
                    mono(12.0),
                    theme::text_dim(),
                );
                let volume = ui.interact(vol_rect, Id::new("tui-volume"), Sense::click_and_drag());
                if volume.dragged() || volume.clicked() {
                    if let Some(pos) = volume.interact_pointer_pos() {
                        let fraction =
                            ((pos.x - vol_rect.left()) / vol_rect.width()).clamp(0.0, 1.0);
                        actions.push(Action::Volume(fraction * 1.25));
                    }
                }

                let button_y = inner.bottom() - 12.0;
                let play_label = if self.playing && !self.paused {
                    "PAUSE"
                } else {
                    "PLAY"
                };
                let controls = [
                    ("tui-prev", "< PREV", 64.0, Action::Prev),
                    ("tui-play", play_label, 64.0, Action::Toggle),
                    ("tui-stop", "STOP", 58.0, Action::Stop),
                    ("tui-next", "NEXT >", 64.0, Action::Next),
                ];
                let total_w = controls.iter().map(|item| item.2).sum::<f32>()
                    + 8.0 * (controls.len() - 1) as f32;
                let mut x = inner.center().x - total_w * 0.5;
                for (id, label, width, action) in controls {
                    let button =
                        Rect::from_min_size(Pos2::new(x, button_y - 11.0), Vec2::new(width, 22.0));
                    if text_button(ui, id, button, label, 12.0).clicked() {
                        actions.push(action);
                    }
                    x += width + 8.0;
                }
            });
        actions
    }
    fn eq_window(&mut self, ctx: &Context) {
        let mut open = true;
        egui::Window::new("EQUALIZER")
            .id(Id::new("eq-window"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .frame(
                Frame::NONE
                    .fill(theme::bg())
                    .stroke(Stroke::new(1.0, theme::text_hi()))
                    .inner_margin(20.0),
            )
            .show(ctx, |ui| {
                ui.set_width(660.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("EQUALIZER / 10 BAND")
                            .font(mono(12.0))
                            .color(theme::text_dim()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if close_x_button(ui).clicked() {
                            open = false;
                        }
                    });
                });
                ui.add_space(8.0);
                ui.label(
                    RichText::new("GRAPHIC EQUALIZER")
                        .font(mono(22.0))
                        .color(theme::text_hi()),
                );
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if tui_button(ui, "[ < ]", 42.0).clicked() {
                        self.cycle_eq_preset(-1);
                    }
                    ui.label(
                        RichText::new(format!("PRESET: {:<8}", self.eq_preset_label()))
                            .font(mono(12.0))
                            .color(theme::text_hi()),
                    );
                    if tui_button(ui, "[ > ]", 42.0).clicked() {
                        self.cycle_eq_preset(1);
                    }
                    if tui_button(ui, &toggle_label("ENABLED", self.eq_enabled), 120.0).clicked() {
                        self.eq_enabled = !self.eq_enabled;
                        self.apply_eq();
                    }
                    if tui_button(ui, "[ RESET ]", 84.0).clicked() {
                        self.reset_eq();
                    }
                });

                ui.add_space(10.0);
                let (panel, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 252.0), Sense::hover());
                let painter = ui.painter().with_clip_rect(panel);
                let left = panel.left() + 4.0;
                let right = panel.right() - 4.0;
                let band_width = (right - left) / NUM_BANDS as f32;
                let top = panel.top() + 22.0;
                let bottom = panel.bottom() - 24.0;
                let center_y = (top + bottom) * 0.5;
                let segment = (bottom - top) / 24.0;
                let active = if self.eq_enabled {
                    theme::text_hi()
                } else {
                    theme::text_dim()
                };
                let cut = if self.eq_enabled {
                    theme::text()
                } else {
                    theme::text_dim()
                };

                painter.line_segment(
                    [Pos2::new(left, center_y), Pos2::new(right, center_y)],
                    Stroke::new(1.0, theme::alpha(theme::line(), 150)),
                );
                painter.line_segment(
                    [Pos2::new(left, top), Pos2::new(left, bottom)],
                    Stroke::new(1.0, theme::line()),
                );
                painter.line_segment(
                    [Pos2::new(right, top), Pos2::new(right, bottom)],
                    Stroke::new(1.0, theme::line()),
                );

                for index in 0..NUM_BANDS {
                    let band_left = left + band_width * index as f32;
                    let band_right = band_left + band_width;
                    let center_x = (band_left + band_right) * 0.5;
                    let response = ui.interact(
                        Rect::from_min_max(
                            Pos2::new(band_left + 2.0, top - 4.0),
                            Pos2::new(band_right - 2.0, bottom + 4.0),
                        ),
                        Id::new(("eq-band", index)),
                        Sense::click_and_drag(),
                    );
                    if response.clicked() || response.dragged() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            let t = ((pointer.y - top) / (bottom - top)).clamp(0.0, 1.0);
                            let gain =
                                ((0.5 - t) * (MAX_GAIN_DB - MIN_GAIN_DB) * 2.0).round() / 2.0;
                            let gain = gain.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
                            if (self.eq_gains[index] - gain).abs() > f32::EPSILON {
                                self.eq_gains[index] = gain;
                                self.eq_preset = 0;
                                self.apply_eq();
                            }
                        }
                    }
                    response.on_hover_cursor(CursorIcon::ResizeVertical);

                    let gain = self.eq_gains[index];
                    let bar_w = (band_width * 0.34).clamp(12.0, 22.0);
                    let bar_left = center_x - bar_w * 0.5;
                    for step in 0..24 {
                        let y0 = top + segment * step as f32 + 1.0;
                        let y1 = top + segment * (step + 1) as f32 - 1.0;
                        let rect = Rect::from_min_max(
                            Pos2::new(bar_left, y0),
                            Pos2::new(bar_left + bar_w, y1),
                        );
                        let level = if step < 12 {
                            (11 - step) as f32
                        } else {
                            -((step - 11) as f32)
                        };
                        let lit = if gain > 0.0 {
                            gain >= level - 0.25
                        } else if gain < 0.0 {
                            gain <= level + 0.5
                        } else {
                            false
                        };
                        let color = if lit {
                            if step < 12 {
                                cut
                            } else {
                                active
                            }
                        } else {
                            theme::alpha(theme::line(), 62)
                        };
                        painter.rect_filled(rect, 0.0, color);
                    }

                    painter.text(
                        Pos2::new(center_x, panel.top() + 8.0),
                        Align2::CENTER_CENTER,
                        format!("{gain:+.1}"),
                        mono(10.0),
                        if self.eq_enabled {
                            theme::text()
                        } else {
                            theme::text_dim()
                        },
                    );
                    painter.text(
                        Pos2::new(center_x, panel.bottom() - 9.0),
                        Align2::CENTER_CENTER,
                        format_frequency(BAND_FREQS[index]),
                        mono(10.0),
                        theme::text_dim(),
                    );
                }

                ui.add_space(8.0);
                ui.label(
                    RichText::new(if self.eq_enabled {
                        "DRAG A BAND TO ADJUST / -12..+12 DB"
                    } else {
                        "EQ BYPASS / DRAG TO EDIT"
                    })
                    .font(mono(12.0))
                    .color(theme::text_dim()),
                );
            });
        if !open {
            self.show_eq = false;
        }
    }

    fn download_window(&mut self, ctx: &Context) {
        let mut open = true;
        egui::Window::new("BILIBILI")
            .id(Id::new("download-window"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .frame(
                Frame::NONE
                    .fill(theme::bg())
                    .stroke(Stroke::new(1.0, theme::text_hi()))
                    .inner_margin(20.0),
            )
            .show(ctx, |ui| {
                ui.set_width(520.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if close_x_button(ui).clicked() {
                            open = false;
                        }
                    });
                });
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Download & Decode")
                        .font(mono(24.0))
                        .color(theme::text_hi()),
                );
                ui.add_space(12.0);

                // Near-inverse block draws the eye to the only required input.
                let input_fill = theme::mix(theme::text_hi(), theme::bg(), 0.08);
                let input_ink = theme::bg_deep();
                let input_hint = theme::mix(input_ink, input_fill, 0.45);
                let response = Frame::NONE
                    .fill(input_fill)
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.scope(|ui| {
                            let visuals = ui.visuals_mut();
                            visuals.weak_text_color = Some(input_hint);
                            visuals.text_cursor.stroke.color = input_ink;
                            visuals.selection.bg_fill = theme::alpha(input_ink, 64);
                            ui.add_sized(
                                [ui.available_width(), 20.0],
                                TextEdit::singleline(&mut self.download_url)
                                    .hint_text("https://www.bilibili.com/video/BV...")
                                    .font(title(13.0))
                                    .align(Align2::LEFT_CENTER)
                                    .text_color(input_ink)
                                    .frame(Frame::NONE),
                            )
                        })
                        .inner
                    })
                    .inner;
                if response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                    self.start_download();
                }
                ui.add_space(12.0);

                ui.horizontal(|ui| {
                    if self.download_running {
                        if tui_button(ui, "[ CANCEL ]", 110.0).clicked() {
                            self.cancel_download();
                        }
                    } else if tui_button(ui, "[ DOWNLOAD ]", 120.0).clicked() {
                        self.start_download();
                    }
                    if tui_button(ui, "[ OPEN DIR ]", 112.0).clicked() {
                        let dir = storage::download_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        let _ = std::process::Command::new("explorer").arg(dir).spawn();
                    }
                });

                if self.download_running || self.download_progress > 0.0 {
                    ui.add_space(10.0);
                    let progress = self.download_progress.clamp(0.0, 1.0);
                    let cells = 32usize;
                    let filled = ((progress * cells as f32).round() as usize).min(cells);
                    let bar = format!(
                        "[{}{}] {:>3}%",
                        "#".repeat(filled),
                        "-".repeat(cells - filled),
                        (progress * 100.0).round() as i32
                    );
                    ui.label(RichText::new(bar).font(mono(12.0)).color(theme::text_hi()));
                }
                if !self.download_status.is_empty() {
                    ui.add_space(4.0);
                    let status = elide_to_width(
                        ui.painter(),
                        &self.download_status,
                        ui_font(12.0),
                        ui.available_width(),
                    );
                    ui.label(
                        RichText::new(status)
                            .font(ui_font(12.0))
                            .color(theme::text()),
                    );
                }
                ui.add_space(10.0);
                let out = elide_to_width(
                    ui.painter(),
                    &storage::download_dir().display().to_string(),
                    mono(10.0),
                    ui.available_width(),
                );
                ui.label(
                    RichText::new(format!("OUT  {out}"))
                        .font(mono(10.0))
                        .color(theme::text_dim()),
                );
                ui.label(
                    RichText::new("AAC / M4A  |  FFMPEG")
                        .font(mono(10.0))
                        .color(theme::text_dim()),
                );
            });
        if !open {
            if self.download_running {
                self.cancel_download();
            }
            self.show_download = false;
        }
    }

    fn overlays(&mut self, ctx: &Context) -> Vec<Action> {
        let mut actions = Vec::new();

        if self.show_settings {
            let mut open = true;
            egui::Window::new("SYSTEM")
                .id(Id::new("system-window"))
                .title_bar(false)
                .resizable(false)
                .collapsible(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .frame(
                    Frame::NONE
                        .fill(theme::bg())
                        .stroke(Stroke::new(1.0, theme::text_hi()))
                        .inner_margin(18.0),
                )
                .show(ctx, |ui| {
                    ui.set_width(430.0);
                    ui.horizontal(|ui| {
                        let header = match self.settings_page {
                            SettingsPage::System => "SYSTEM / INFO",
                            SettingsPage::Visuals => "SYSTEM / VISUALS",
                            SettingsPage::Theme => "SYSTEM / THEME",
                        };
                        ui.label(
                            RichText::new(header)
                                .font(mono(12.0))
                                .color(theme::text_dim()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if close_x_button(ui).clicked() {
                                open = false;
                            }
                        });
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        for (page, label) in [
                            (SettingsPage::System, "[ SYSTEM ]"),
                            (SettingsPage::Visuals, "[ VISUALS ]"),
                            (SettingsPage::Theme, "[ THEME ]"),
                        ] {
                            if tab_button(ui, label, 118.0, self.settings_page == page).clicked() {
                                self.settings_page = page;
                            }
                        }
                    });
                    ui.add_space(12.0);
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .min_scrolled_height(360.0)
                        .auto_shrink([false, false])
                        .scroll_bar_visibility(
                            egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden,
                        )
                        .show(ui, |ui| {
                            ui.set_width(430.0);
                            match self.settings_page {
                                SettingsPage::System => {
                                    ui.label(
                                        RichText::new("SONADIAL PLAYER")
                                            .font(mono(24.0))
                                            .color(theme::text_hi()),
                                    );
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(format!(
                                            "v0.1 / {} TRACKS / {}",
                                            self.playlist.len(),
                                            self.play_mode.label()
                                        ))
                                        .font(mono(12.0))
                                        .color(theme::text_dim()),
                                    );
                                    ui.add_space(10.0);
                                    info_row(ui, "OUTPUT", &self.output_label);
                                    ui.add_space(2.0);
                                    ui.horizontal(|ui| {
                                        ui.add_sized(
                                            [120.0, 18.0],
                                            egui::Label::new(
                                                RichText::new("STATE")
                                                    .font(mono(12.0))
                                                    .color(theme::text_dim()),
                                            ),
                                        );
                                        let font = ui_font(12.0);
                                        let path = storage::state_path().display().to_string();
                                        let text = elide_to_width(
                                            ui.painter(),
                                            &path,
                                            font.clone(),
                                            (ui.available_width() - 4.0).max(80.0),
                                        );
                                        let response = ui
                                            .add(
                                                egui::Label::new(
                                                    RichText::new(text)
                                                        .font(font)
                                                        .color(theme::text_hi())
                                                        .underline(),
                                                )
                                                .sense(Sense::click()),
                                            )
                                            .on_hover_text(path);
                                        if response.hovered() {
                                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                        }
                                        if response.clicked() {
                                            let _ = std::process::Command::new("explorer")
                                                .arg(storage::data_dir())
                                                .spawn();
                                        }
                                    });
                                    ui.add_space(10.0);
                                    ui.label(
                                        RichText::new("SHORTCUTS")
                                            .font(mono(12.0))
                                            .color(theme::text_dim()),
                                    );
                                    ui.add_space(4.0);
                                    for (keys, desc) in [
                                        ("SPACE", "播放 / 暂停"),
                                        ("CTRL + O", "添加音频"),
                                        ("LEFT / RIGHT", "后退 / 前进 5 秒"),
                                        ("CTRL + LEFT/RIGHT", "上一首 / 下一首"),
                                        ("ESC", "关闭面板"),
                                    ] {
                                        ui.horizontal(|ui| {
                                            ui.add_sized(
                                                [150.0, 18.0],
                                                egui::Label::new(
                                                    RichText::new(keys)
                                                        .font(mono(12.0))
                                                        .color(theme::text_hi()),
                                                ),
                                            );
                                            ui.label(
                                                RichText::new(desc)
                                                    .font(ui_font(12.0))
                                                    .color(theme::text()),
                                            );
                                        });
                                    }
                                    ui.add_space(12.0);
                                    if tui_button(ui, "[ open data dir ]", 150.0).clicked() {
                                        let _ = std::process::Command::new("explorer")
                                            .arg(storage::data_dir())
                                            .spawn();
                                    }
                                }
                                SettingsPage::Visuals => {
                                    ui.label(
                                        RichText::new("VISUAL EFFECTS")
                                            .font(mono(24.0))
                                            .color(theme::text_hi()),
                                    );
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new("SPECTRUM / CORE DISPLAY")
                                            .font(mono(12.0))
                                            .color(theme::text_dim()),
                                    );
                                    ui.add_space(12.0);
                                    ui.label(
                                        RichText::new("TOGGLES")
                                            .font(mono(12.0))
                                            .color(theme::text_dim()),
                                    );
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        if tui_button(
                                            ui,
                                            &toggle_label("BG SPECTRUM", self.show_bg_spectrum),
                                            205.0,
                                        )
                                        .clicked()
                                        {
                                            self.show_bg_spectrum = !self.show_bg_spectrum;
                                            self.dirty = true;
                                        }
                                        if tui_button(
                                            ui,
                                            &toggle_label("RADIAL", self.show_radial),
                                            120.0,
                                        )
                                        .clicked()
                                        {
                                            self.show_radial = !self.show_radial;
                                            self.dirty = true;
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        if tui_button(
                                            ui,
                                            &toggle_label("CORE PULSE", self.show_core_pulse),
                                            205.0,
                                        )
                                        .clicked()
                                        {
                                            self.show_core_pulse = !self.show_core_pulse;
                                            self.dirty = true;
                                        }
                                        if tui_button(
                                            ui,
                                            &toggle_label("CHAR STREAM", self.char_stream),
                                            155.0,
                                        )
                                        .clicked()
                                        {
                                            self.char_stream = !self.char_stream;
                                            if self.char_stream {
                                                self.show_bg_spectrum = true;
                                            }
                                            self.dirty = true;
                                        }
                                    });
                                    ui.add_space(12.0);
                                    ui.label(
                                        RichText::new("PARAMETERS")
                                            .font(mono(12.0))
                                            .color(theme::text_dim()),
                                    );
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new("RADIAL SIZE")
                                                .font(mono(12.0))
                                                .color(theme::text_dim()),
                                        );
                                        if tui_button(ui, "[-]", 34.0).clicked() {
                                            self.radial_scale = (self.radial_scale - 0.05).max(0.7);
                                            self.dirty = true;
                                        }
                                        ui.label(
                                            RichText::new(format!(
                                                "{:>3}%",
                                                (self.radial_scale * 100.0).round() as i32
                                            ))
                                            .font(mono(12.0))
                                            .color(theme::text()),
                                        );
                                        if tui_button(ui, "[+]", 34.0).clicked() {
                                            self.radial_scale = (self.radial_scale + 0.05).min(1.2);
                                            self.dirty = true;
                                        }
                                    });
                                }
                                SettingsPage::Theme => {
                                    ui.label(
                                        RichText::new("THEME")
                                            .font(mono(24.0))
                                            .color(theme::text_hi()),
                                    );
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(format!(
                                            "COLOR PALETTE / CURRENT: {}",
                                            theme::theme_name()
                                        ))
                                        .font(mono(12.0))
                                        .color(theme::text_dim()),
                                    );
                                    ui.add_space(12.0);
                                    let gap = ui.spacing().item_spacing.x;
                                    let chip_w =
                                        ((ui.available_width() - gap * 2.0) / 3.0).max(96.0);
                                    for row in 0..(theme::THEME_COUNT + 2) / 3 {
                                        ui.horizontal(|ui| {
                                            for col in 0..3 {
                                                let index = row * 3 + col;
                                                if index >= theme::THEME_COUNT {
                                                    break;
                                                }
                                                let name = theme::THEME_NAMES[index];
                                                if theme_chip(
                                                    ui,
                                                    name,
                                                    chip_w,
                                                    index,
                                                    self.theme_index == index,
                                                )
                                                .clicked()
                                                {
                                                    self.select_theme(index, ctx);
                                                }
                                            }
                                        });
                                    }
                                    ui.add_space(16.0);
                                    ui.label(
                                        RichText::new("SELECT A PALETTE / APPLIES IMMEDIATELY")
                                            .font(mono(12.0))
                                            .color(theme::text_dim()),
                                    );
                                }
                            }
                        });
                });
            if !open {
                self.show_settings = false;
            }
        }

        if self.show_eq {
            self.eq_window(ctx);
        }
        if self.show_download {
            self.download_window(ctx);
        }

        if let Some(error) = &self.error {
            let mut dismiss = false;
            egui::Area::new(Id::new("error-toast"))
                .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-22.0, -DOCK_H - 18.0))
                .show(ctx, |ui| {
                    Frame::NONE
                        .fill(theme::bg())
                        .stroke(Stroke::new(1.0, theme::text_hi()))
                        .inner_margin(12.0)
                        .show(ui, |ui| {
                            ui.set_max_width(440.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("ERROR")
                                        .font(mono(12.0))
                                        .color(theme::danger()),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if close_x_button(ui).clicked() {
                                        dismiss = true;
                                    }
                                });
                            });
                            ui.label(
                                RichText::new(elide(error, 160))
                                    .font(ui_font(12.0))
                                    .color(theme::text_hi()),
                            );
                        });
                });
            if dismiss {
                actions.push(Action::ClearError);
            }
        }

        actions
    }

    fn resize_handles(&self, ctx: &Context) {
        let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
        if maximized {
            return;
        }
        let (pointer, pressed) =
            ctx.input(|input| (input.pointer.hover_pos(), input.pointer.primary_pressed()));
        let Some(pointer) = pointer else {
            return;
        };
        let screen = ctx.content_rect();
        let edge = 6.0;

        let left = pointer.x <= screen.left() + edge;
        let right = pointer.x >= screen.right() - edge;
        let top = pointer.y <= screen.top() + edge;
        let bottom = pointer.y >= screen.bottom() - edge;

        let direction = match (left, right, top, bottom) {
            (true, _, true, _) => Some((ResizeDirection::NorthWest, CursorIcon::ResizeNwSe)),
            (_, true, true, _) => Some((ResizeDirection::NorthEast, CursorIcon::ResizeNeSw)),
            (true, _, _, true) => Some((ResizeDirection::SouthWest, CursorIcon::ResizeNeSw)),
            (_, true, _, true) => Some((ResizeDirection::SouthEast, CursorIcon::ResizeNwSe)),
            (true, _, _, _) => Some((ResizeDirection::West, CursorIcon::ResizeHorizontal)),
            (_, true, _, _) => Some((ResizeDirection::East, CursorIcon::ResizeHorizontal)),
            (_, _, true, _) => Some((ResizeDirection::North, CursorIcon::ResizeVertical)),
            (_, _, _, true) => Some((ResizeDirection::South, CursorIcon::ResizeVertical)),
            _ => None,
        };

        if let Some((direction, icon)) = direction {
            ctx.set_cursor_icon(icon);
            if pressed {
                ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
            }
        }
    }
}

impl eframe::App for SonadialApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.tick(&ctx);
        self.handle_input(&ctx);
        self.paint_background(&ctx);

        let mut actions = Vec::new();
        actions.extend(self.top_bar(ui));
        actions.extend(self.dock(ui));
        actions.extend(self.sidebar(ui));
        actions.extend(self.stage(ui));
        actions.extend(self.overlays(&ctx));
        self.resize_handles(&ctx);

        self.process_actions(&ctx, actions);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(cancel) = &self.download_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        self.save_state();
    }
}

fn entry_for_path(path: PathBuf) -> TrackEntry {
    let title = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    TrackEntry { path, title }
}

fn is_audio_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            AUDIO_EXTS
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(ext))
        })
        .unwrap_or(false)
}

fn extension_label(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_uppercase())
        .unwrap_or_else(|| "PCM".to_owned())
}

fn format_frequency(freq: f32) -> String {
    if freq >= 1_000.0 {
        format!("{}k", (freq / 1_000.0).round() as i32)
    } else {
        format!("{}", freq.round() as i32)
    }
}

fn short_time(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn elide(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

fn elide_to_width(painter: &egui::Painter, text: &str, font: FontId, max_width: f32) -> String {
    if max_width <= 0.0 || text.is_empty() {
        return String::new();
    }
    let text = text.to_owned();
    let full = painter.layout_no_wrap(text.clone(), font.clone(), Color32::WHITE);
    if full.size().x <= max_width {
        return text;
    }
    let ellipsis = "...";
    let ellipsis_width = painter
        .layout_no_wrap(ellipsis.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    if max_width <= ellipsis_width {
        return String::new();
    }
    let budget = max_width - ellipsis_width;
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let candidate: String = chars[..mid].iter().collect();
        let width = painter
            .layout_no_wrap(candidate, font.clone(), Color32::WHITE)
            .size()
            .x;
        if width <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    out.push_str("...");
    out
}

fn snap_px(size: f32) -> f32 {
    ((size / 12.0).round().max(1.0) * 12.0).clamp(12.0, 48.0)
}

fn ui_font(size: f32) -> FontId {
    FontId::new(snap_px(size), FontFamily::Name("ui".into()))
}

fn mono(size: f32) -> FontId {
    FontId::new(snap_px(size), FontFamily::Monospace)
}

fn title(size: f32) -> FontId {
    FontId::new(snap_px(size), FontFamily::Name("ui".into()))
}

fn paint_card(painter: &egui::Painter, rect: Rect, _radius: f32, fill: Color32, border: Color32) {
    painter.rect_filled(rect, 0.0, fill);
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, border), StrokeKind::Inside);
}

fn paint_corner_marks(painter: &egui::Painter, rect: Rect, color: Color32) {
    let len = 13.0;
    let inset = 9.0;
    let stroke = Stroke::new(1.0, color);
    let left = rect.left() + inset;
    let right = rect.right() - inset;
    let top = rect.top() + inset;
    let bottom = rect.bottom() - inset;

    for (corner, dx, dy) in [
        (Pos2::new(left, top), 1.0, 1.0),
        (Pos2::new(right, top), -1.0, 1.0),
        (Pos2::new(left, bottom), 1.0, -1.0),
        (Pos2::new(right, bottom), -1.0, -1.0),
    ] {
        painter.line_segment([corner, corner + Vec2::new(len * dx, 0.0)], stroke);
        painter.line_segment([corner, corner + Vec2::new(0.0, len * dy)], stroke);
    }
}

fn paint_stage_marks(painter: &egui::Painter, rect: Rect) {
    let color = theme::alpha(theme::text_dim(), 38);
    let len = 26.0;
    let inset = 12.0;
    let stroke = Stroke::new(1.0, color);
    let left = rect.left() + inset;
    let right = rect.right() - inset;
    let top = rect.top() + inset;
    let bottom = rect.bottom() - inset;
    for (corner, dx, dy) in [
        (Pos2::new(left, top), 1.0, 1.0),
        (Pos2::new(right, top), -1.0, 1.0),
        (Pos2::new(left, bottom), 1.0, -1.0),
        (Pos2::new(right, bottom), -1.0, -1.0),
    ] {
        painter.line_segment([corner, corner + Vec2::new(len * dx, 0.0)], stroke);
        painter.line_segment([corner, corner + Vec2::new(0.0, len * dy)], stroke);
    }
}

fn spectrum_color(value: f32) -> Color32 {
    let value = value.clamp(0.0, 1.0);
    let base = if value < 0.5 {
        mix(
            Color32::from_rgb(48, 48, 48),
            Color32::from_rgb(170, 170, 170),
            value / 0.5,
        )
    } else {
        mix(
            Color32::from_rgb(170, 170, 170),
            Color32::from_rgb(255, 255, 255),
            (value - 0.5) / 0.5,
        )
    };
    let alpha_value = (140.0 + 115.0 * value).min(255.0) as u8;
    theme::alpha(base, alpha_value)
}

fn paint_arc(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    start: f32,
    end: f32,
    stroke: Stroke,
    segments: usize,
) {
    let mut points = Vec::with_capacity(segments + 1);
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let angle = start + (end - start) * t;
        points.push(center + Vec2::new(angle.cos(), angle.sin()) * radius);
    }
    painter.add(Shape::line(points, stroke));
}

fn paint_level_bars(painter: &egui::Painter, center: Pos2, beat: f32, color: Color32) {
    for i in 0..3 {
        let height = 4.0 + 7.0 * (beat * 0.4 + 0.55 * ((i as f32 + 1.0) / 3.0));
        let x = center.x + (i as f32 - 1.0) * 4.0;
        painter.rect_filled(
            Rect::from_center_size(Pos2::new(x, center.y), Vec2::new(2.0, height)),
            0.0,
            color,
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Icon {
    Play,
    Pause,
    Prev,
    Next,
    Stop,
    Menu,
    Plus,
    Search,
    Settings,
    Equalizer,
    Min,
    Max,
    Restore,
    Close,
    Speaker,
    Repeat,
    RepeatOne,
    Shuffle,
    Folder,
}

fn mode_icon(mode: PlayMode) -> Icon {
    match mode {
        PlayMode::Sequence => Icon::Repeat,
        PlayMode::RepeatOne => Icon::RepeatOne,
        PlayMode::Shuffle => Icon::Shuffle,
    }
}

fn paint_icon_button(ui: &mut Ui, rect: Rect, icon: Icon, active: bool, primary: bool) -> Response {
    let id = Id::new((
        "sonadial-icon",
        icon as u8,
        rect.left() as i32,
        rect.top() as i32,
    ));
    let response = ui.interact(rect, id, Sense::click());
    let painter = ui.painter();
    let hovered = response.hovered();

    if primary {
        painter.rect_filled(rect, 0.0, theme::text_hi());
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, theme::text_hi()),
            StrokeKind::Inside,
        );
        paint_icon(
            painter,
            icon,
            rect.center(),
            rect.width() * 0.38,
            theme::bg(),
        );
    } else {
        if hovered || active {
            painter.rect_filled(rect, 0.0, theme::text_hi());
        }
        paint_icon(
            painter,
            icon,
            rect.center(),
            rect.width() * 0.44,
            if hovered || active {
                theme::bg()
            } else {
                theme::text()
            },
        );
    }

    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}

fn paint_icon(painter: &egui::Painter, icon: Icon, center: Pos2, size: f32, color: Color32) {
    let s = size;
    let stroke = Stroke::new((s * 0.10).clamp(1.2, 2.2), color);
    match icon {
        Icon::Play => {
            let points = vec![
                center + Vec2::new(-s * 0.28, -s * 0.42),
                center + Vec2::new(s * 0.42, 0.0),
                center + Vec2::new(-s * 0.28, s * 0.42),
            ];
            painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Icon::Pause => {
            painter.rect_filled(
                Rect::from_center_size(
                    center + Vec2::new(-s * 0.20, 0.0),
                    Vec2::new(s * 0.18, s * 0.72),
                ),
                0.0,
                color,
            );
            painter.rect_filled(
                Rect::from_center_size(
                    center + Vec2::new(s * 0.20, 0.0),
                    Vec2::new(s * 0.18, s * 0.72),
                ),
                0.0,
                color,
            );
        }
        Icon::Prev => {
            painter.line_segment(
                [
                    center + Vec2::new(-s * 0.36, -s * 0.38),
                    center + Vec2::new(-s * 0.36, s * 0.38),
                ],
                stroke,
            );
            let points = vec![
                center + Vec2::new(s * 0.36, -s * 0.38),
                center + Vec2::new(-s * 0.18, 0.0),
                center + Vec2::new(s * 0.36, s * 0.38),
            ];
            painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Icon::Next => {
            painter.line_segment(
                [
                    center + Vec2::new(s * 0.36, -s * 0.38),
                    center + Vec2::new(s * 0.36, s * 0.38),
                ],
                stroke,
            );
            let points = vec![
                center + Vec2::new(-s * 0.36, -s * 0.38),
                center + Vec2::new(s * 0.18, 0.0),
                center + Vec2::new(-s * 0.36, s * 0.38),
            ];
            painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Icon::Stop => {
            painter.rect_filled(
                Rect::from_center_size(center, Vec2::splat(s * 0.56)),
                0.0,
                color,
            );
        }
        Icon::Menu => {
            for offset in [-0.28, 0.0, 0.28] {
                painter.line_segment(
                    [
                        center + Vec2::new(-s * 0.38, s * offset),
                        center + Vec2::new(s * 0.38, s * offset),
                    ],
                    stroke,
                );
            }
        }
        Icon::Plus => {
            painter.line_segment(
                [
                    center + Vec2::new(-s * 0.32, 0.0),
                    center + Vec2::new(s * 0.32, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + Vec2::new(0.0, -s * 0.32),
                    center + Vec2::new(0.0, s * 0.32),
                ],
                stroke,
            );
        }
        Icon::Search => {
            painter.circle_stroke(center + Vec2::new(-s * 0.08, -s * 0.08), s * 0.26, stroke);
            painter.line_segment(
                [
                    center + Vec2::new(s * 0.12, s * 0.12),
                    center + Vec2::new(s * 0.34, s * 0.34),
                ],
                stroke,
            );
        }
        Icon::Settings => {
            painter.circle_stroke(center, s * 0.20, stroke);
            painter.circle_stroke(center, s * 0.32, Stroke::new(stroke.width * 0.7, color));
            for i in 0..8 {
                let angle = i as f32 / 8.0 * std::f32::consts::TAU;
                let dir = Vec2::new(angle.cos(), angle.sin());
                painter.line_segment(
                    [center + dir * s * 0.32, center + dir * s * 0.43],
                    Stroke::new(stroke.width * 0.8, color),
                );
            }
        }
        Icon::Equalizer => {
            for (i, offset) in [-0.28, 0.0, 0.28].iter().enumerate() {
                let x = center.x + s * offset;
                painter.line_segment(
                    [
                        Pos2::new(x, center.y - s * 0.40),
                        Pos2::new(x, center.y + s * 0.40),
                    ],
                    Stroke::new(stroke.width * 0.8, color),
                );
                let knob_y = center.y + s * [-0.12, 0.18, -0.25][i];
                painter.rect_filled(
                    Rect::from_center_size(Pos2::new(x, knob_y), Vec2::splat(3.0)),
                    0.0,
                    color,
                );
            }
        }
        Icon::Min => {
            painter.line_segment(
                [
                    center + Vec2::new(-s * 0.40, 0.0),
                    center + Vec2::new(s * 0.40, 0.0),
                ],
                stroke,
            );
        }
        Icon::Max => {
            painter.rect_stroke(
                Rect::from_center_size(center, Vec2::splat(s * 0.62)),
                0.0,
                stroke,
                StrokeKind::Inside,
            );
        }
        Icon::Restore => {
            painter.rect_stroke(
                Rect::from_center_size(
                    center + Vec2::new(s * 0.08, -s * 0.08),
                    Vec2::splat(s * 0.52),
                ),
                0.0,
                Stroke::new(stroke.width * 0.8, color),
                StrokeKind::Inside,
            );
            painter.rect_stroke(
                Rect::from_center_size(
                    center + Vec2::new(-s * 0.10, s * 0.10),
                    Vec2::splat(s * 0.52),
                ),
                0.0,
                Stroke::new(stroke.width * 0.8, color),
                StrokeKind::Inside,
            );
        }
        Icon::Close => {
            painter.line_segment(
                [
                    center + Vec2::new(-s * 0.32, -s * 0.32),
                    center + Vec2::new(s * 0.32, s * 0.32),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + Vec2::new(s * 0.32, -s * 0.32),
                    center + Vec2::new(-s * 0.32, s * 0.32),
                ],
                stroke,
            );
        }
        Icon::Speaker => {
            let points = vec![
                center + Vec2::new(-s * 0.42, -s * 0.18),
                center + Vec2::new(-s * 0.18, -s * 0.18),
                center + Vec2::new(s * 0.10, -s * 0.42),
                center + Vec2::new(s * 0.10, s * 0.42),
                center + Vec2::new(-s * 0.18, s * 0.18),
                center + Vec2::new(-s * 0.42, s * 0.18),
            ];
            painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
            paint_arc(
                painter,
                center + Vec2::new(s * 0.14, 0.0),
                s * 0.30,
                -0.8,
                0.8,
                Stroke::new(stroke.width * 0.8, color),
                10,
            );
            paint_arc(
                painter,
                center + Vec2::new(s * 0.14, 0.0),
                s * 0.44,
                -0.8,
                0.8,
                Stroke::new(stroke.width * 0.7, color),
                12,
            );
        }
        Icon::Repeat => {
            paint_arc(painter, center, s * 0.34, -2.5, 2.5, stroke, 20);
            painter.add(Shape::convex_polygon(
                vec![
                    center + Vec2::new(-s * 0.34, -s * 0.10),
                    center + Vec2::new(-s * 0.48, -s * 0.30),
                    center + Vec2::new(-s * 0.20, -s * 0.30),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::RepeatOne => {
            paint_arc(painter, center, s * 0.34, -2.5, 2.5, stroke, 20);
            painter.text(center, Align2::CENTER_CENTER, "1", mono(s * 0.34), color);
        }
        Icon::Shuffle => {
            painter.line_segment(
                [
                    center + Vec2::new(-s * 0.40, -s * 0.28),
                    center + Vec2::new(s * 0.36, s * 0.28),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + Vec2::new(-s * 0.40, s * 0.28),
                    center + Vec2::new(s * 0.36, -s * 0.28),
                ],
                stroke,
            );
            for (pos, dx, dy) in [
                (center + Vec2::new(s * 0.36, s * 0.28), 1.0, 0.0),
                (center + Vec2::new(s * 0.36, -s * 0.28), 1.0, 0.0),
            ] {
                painter.line_segment([pos, pos + Vec2::new(-s * 0.16, -s * 0.02) * dx], stroke);
                painter.line_segment([pos, pos + Vec2::new(-s * 0.16, s * 0.16) * dy], stroke);
            }
        }
        Icon::Folder => {
            painter.rect_stroke(
                Rect::from_center_size(
                    center + Vec2::new(0.0, s * 0.04),
                    Vec2::new(s * 0.78, s * 0.56),
                ),
                2.0,
                stroke,
                StrokeKind::Inside,
            );
            painter.line_segment(
                [
                    center + Vec2::new(-s * 0.34, -s * 0.24),
                    center + Vec2::new(-s * 0.08, -s * 0.24),
                ],
                stroke,
            );
        }
    }
}

fn toggle_label(name: &str, on: bool) -> String {
    format!("[{}] {}", if on { "x" } else { " " }, name)
}

fn info_row(ui: &mut Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [120.0, 18.0],
            egui::Label::new(RichText::new(key).font(mono(12.0)).color(theme::text_dim())),
        );
        ui.label(RichText::new(value).font(mono(12.0)).color(theme::text()));
    });
}

fn text_button(ui: &mut Ui, id: &str, rect: Rect, label: &str, size: f32) -> Response {
    let response = ui.interact(rect, Id::new(id), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 0.0, theme::text_hi());
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        mono(size),
        if hovered {
            theme::bg()
        } else {
            theme::text_hi()
        },
    );
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}

fn close_x_button(ui: &mut Ui) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(34.0, 24.0), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 0.0, theme::text_hi());
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        "[x]",
        mono(16.0),
        if hovered {
            theme::bg()
        } else {
            theme::text_hi()
        },
    );
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}

fn tab_button(ui: &mut Ui, label: &str, width: f32, active: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::click());
    let hovered = response.hovered();
    if active || hovered {
        ui.painter().rect_filled(rect, 0.0, theme::text_hi());
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        mono(12.0),
        if active || hovered {
            theme::bg()
        } else {
            theme::text_dim()
        },
    );
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}

fn theme_chip(ui: &mut Ui, label: &str, width: f32, index: usize, active: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::click());
    let (_, fg, dim) = theme::theme_colors(index);
    let hovered = response.hovered();
    let painter = ui.painter();
    theme::paint_theme_preview(painter, rect, index);
    let border = if active {
        Stroke::new(2.0, fg)
    } else if hovered {
        Stroke::new(1.0, fg)
    } else {
        Stroke::new(1.0, dim)
    };
    painter.rect_stroke(rect, 0.0, border, StrokeKind::Inside);
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(rect.left() + 1.0, rect.bottom() - 3.0),
            Pos2::new(rect.right() - 1.0, rect.bottom() - 1.0),
        ),
        0.0,
        fg,
    );
    painter.text(
        Pos2::new(rect.center().x, rect.center().y - 1.0),
        Align2::CENTER_CENTER,
        label,
        mono(12.0),
        fg,
    );
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}

fn tui_button(ui: &mut Ui, label: &str, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 0.0, theme::text_hi());
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        mono(12.0),
        if hovered { theme::bg() } else { theme::text() },
    );
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}
