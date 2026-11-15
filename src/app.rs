use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::viewport::ResizeDirection;
use eframe::egui::{
    self, Align, Align2, Color32, Context, CursorIcon, FontFamily, FontId, Frame, Id, Key, Layout,
    Pos2, Rect, Response, RichText, ScrollArea, Sense, Shape, Stroke, StrokeKind, TextEdit, Ui,
    Vec2, ViewportCommand,
};
use eframe::egui::scroll_area::ScrollBarVisibility;

use crate::audio::AudioEngine;
use crate::auto_theme::{self, Fade, Mood};
use crate::beat_config::{BeatConfig, ShortVisual};
use crate::design as tokens;
use crate::download::{self, DownloadEvent};
use crate::eq::{self, BAND_FREQS, MAX_GAIN_DB, MIN_GAIN_DB, NUM_BANDS};
use crate::lyrics::{self, LyricLine};
use crate::notes::NoteAnalysis;
use crate::pitch::NoteFrame;
use crate::sound_profile::SoundProfile;
use crate::spectrum::{SpectrumEngine, VisualState};
use crate::storage::{self, PersistedState};
use crate::theme::{self, *};
use crate::theme_library::{format_hex, parse_hex, Hex, Overrides, ThemeLibrary, ThemeSpec, NAME_MAX};

const TOP_H: f32 = 44.0;
const THEME_KEY_W: f32 = 88.0;
const THEME_TINT_MAX: f32 = 0.6;
const BEAT_VISUAL_LEAD: f64 = 0.025;
const TOP_LABEL_Y: f32 = 16.0;
const SIDEBAR_W: f32 = 360.0;
const SIDEBAR_W_COMPACT: f32 = 320.0;
const DOCK_H: f32 = 124.0;
const VOLUME_STEP: f32 = 0.05;
const SEEK_STEP: f64 = 5.0;
const AUDIO_EXTS: &[&str] = &["mp3", "flac", "wav", "ogg", "m4a", "aac", "opus", "alac"];

#[derive(Clone)]
struct TrackEntry {
    path: PathBuf,
    title: String,
    lrc: Option<PathBuf>,
    search_index: crate::search::SearchIndex,
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

    /// Persisted as a small integer so `storage` does not need to know the
    /// enum. The order is stable: 0 sequence, 1 repeat one, 2 shuffle.
    fn from_saved(value: u8) -> Self {
        match value {
            1 => Self::RepeatOne,
            2 => Self::Shuffle,
            _ => Self::Sequence,
        }
    }

    fn saved(self) -> u8 {
        match self {
            Self::Sequence => 0,
            Self::RepeatOne => 1,
            Self::Shuffle => 2,
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

/// Text buffers for the user theme being edited; committed values live in
/// the theme library.
struct ThemeEditor {
    spec: usize,
    name: String,
    background: [String; 4],
    accent: String,
    delete_armed: bool,
}

impl ThemeEditor {
    fn new(spec: usize, source: &ThemeSpec) -> Self {
        let mut editor = Self {
            spec,
            name: String::new(),
            background: Default::default(),
            accent: String::new(),
            delete_armed: false,
        };
        editor.sync(source);
        editor
    }

    fn sync(&mut self, source: &ThemeSpec) {
        self.name = source.name.trim().to_owned();
        for (slot, text) in self.background.iter_mut().enumerate() {
            *text = source
                .background
                .get(slot)
                .map(|hex| format_hex(hex.0))
                .unwrap_or_default();
        }
        self.accent = format_hex(source.accent.0);
    }
}

enum ThemeEdit {
    Rename(String),
    Background(usize, Rgb),
    BackgroundCount(usize),
    Accent(Rgb),
    Tint(f32),
    Dark(Option<bool>),
    ClearOverrides,
}

enum ThemeAction {
    Select(usize),
    /// Follow the track; `true` for dark.
    Auto(bool),
    /// Copy an entry into a new user theme and open it in the editor.
    Copy(usize),
    /// Same, for the AUTO look on screen.
    CopyAuto,
    Edit(usize),
    Reload,
    Back,
    Delete(usize),
}

enum Action {
    Play(usize),
    Select(usize),
    AddFiles,
    Toggle,
    Next,
    Prev,
    Stop,
    Seek(f64),
    Volume(f32),
    CycleMode,
    ToggleSettings,
    ToggleVisualMode,
    ToggleEq,
    OpenDownload,
    ToggleSidebar,
    Minimize,
    ToggleMaximize,
    Close,
    ClearError,
    QueueNext(usize),
    ImportLrc(usize),
    ClearLrc(usize),
    OpenTrackLocation(usize),
    RemoveTrack(usize),
    DeleteTrack(usize),
    ConfirmDeleteTrack,
    CancelDeleteTrack,
}

struct BeatAnalysisHandle {
    rx: Receiver<BeatAnalysisResult>,
    cancel: Arc<AtomicBool>,
}

struct BeatAnalysisResult {
    generation: u64,
    path: PathBuf,
    beats: Vec<(f64, f32)>,
    profile: Option<SoundProfile>,
}

struct NoteAnalysisHandle {
    rx: Receiver<NoteAnalysisResult>,
    cancel: Arc<AtomicBool>,
}

struct NoteAnalysisResult {
    generation: u64,
    path: PathBuf,
    analysis: std::result::Result<NoteAnalysis, String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NotesState {
    Idle,
    Running,
    Ready,
    Failed,
}

pub struct SonadialApp {
    engine: AudioEngine,
    beat_config: BeatConfig,
    analyzer: SpectrumEngine,
    visual: VisualState,
    viz_rotation: f32,
    core_beat_age: f32,
    core_beat_seq: u64,
    core_ring_strength: f32,
    beat_analysis: Option<BeatAnalysisHandle>,
    beat_generation: u64,
    note_analysis: Option<NoteAnalysisHandle>,
    note_generation: u64,
    offline_notes: Option<NoteAnalysis>,
    notes_state: NotesState,
    /// Track the cached `offline_notes` belong to.
    notes_path: Option<PathBuf>,
    offline_beats: Vec<(f64, f32)>,
    offline_index: usize,
    offline_seq: u64,
    offline_strength: f32,
    offline_ready: bool,
    offline_last_pos: Option<f64>,
    playlist: Vec<TrackEntry>,
    current: Option<usize>,
    /// Row marked by a single click; double-click plays it.
    selected_track: Option<usize>,
    lrc_map: HashMap<String, PathBuf>,
    next_queue: VecDeque<PathBuf>,
    history: Vec<PathBuf>,
    history_pos: usize,
    shuffle_order: Vec<usize>,
    rng_state: u64,
    lyrics: Vec<LyricLine>,
    lyrics_synced: bool,
    lyrics_track: Option<PathBuf>,
    lyric_seek_full: bool,
    lyric_seek_index: Option<usize>,
    pending_delete: Option<PathBuf>,
    playing: bool,
    paused: bool,
    volume: f32,
    search: String,
    /// Custom track-list scrollbar: pending offset for the next frame and the
    /// pointer's grab offset inside the thumb.
    track_scroll_target: Option<f32>,
    track_scroll_grab: Option<f32>,
    samples: Vec<f32>,
    duration: f64,
    format_label: String,
    sample_rate: u32,
    sidebar_open: bool,
    show_settings: bool,
    settings_page: SettingsPage,
    themes: ThemeLibrary,
    theme_name: String,
    theme_editor: Option<ThemeEditor>,
    themes_dirty: bool,
    /// AUTO theme: `Some(dark)` while on; `theme_name` keeps the manual pick.
    auto_dark: Option<bool>,
    /// Mood the AUTO look is generated from; the last analysed track's while
    /// the current one is still being analysed.
    auto_mood: Option<Mood>,
    /// Mood measured for the current track, once its analysis is in.
    track_mood: Option<Mood>,
    theme_fade: Option<Fade>,
    show_bg_spectrum: bool,
    braille_stream: bool,
    led_bars: bool,
    peak_hold: bool,
    show_lyrics: bool,
    piano_mode: bool,
    piano_history: VecDeque<NoteFrame>,
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
    /// Proxy for the downloader, empty for a direct connection.
    proxy: String,
    /// Proxy found in the environment at startup, offered as a one-click fill.
    env_proxy: Option<String>,
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
        let beat_config = BeatConfig::load();
        let themes = ThemeLibrary::load();
        let theme_error = themes
            .load_error()
            .map(|error| format!("THEMES.JSON / {error}"));
        // Name first; the index is what builds before named themes stored.
        let theme_entry = state
            .theme_name
            .as_deref()
            .and_then(|name| themes.find(name))
            .or_else(|| state.theme.and_then(|index| themes.find_builtin(index)))
            .or_else(|| themes.find_builtin(if state.light { 1 } else { 0 }))
            .unwrap_or(0);
        let theme_name = themes.entries()[theme_entry].name.clone();
        let auto_dark = state.auto_theme;
        let auto_mood = state.auto_mood.map(Mood::sanitized);
        theme::set_look(match auto_dark {
            Some(dark) => auto_theme::look(auto_mood.unwrap_or_default(), dark),
            None => themes.entries()[theme_entry].look,
        });
        theme::install(&cc.egui_ctx);

        let lrc_map: HashMap<String, PathBuf> = state
            .lrc_map
            .iter()
            .map(|(track, lrc)| (track.clone(), PathBuf::from(lrc)))
            .filter(|(_, lrc)| lrc.is_file())
            .collect();
        let playlist: Vec<TrackEntry> = state
            .playlist
            .iter()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .map(|path| {
                let lrc = resolve_lrc(&path, &lrc_map);
                entry_for_path(path, lrc)
            })
            .collect();

        let current = state.current.as_ref().and_then(|path| {
            playlist
                .iter()
                .position(|t| t.path.to_string_lossy() == *path)
        });

        // Volume is one global level. Older builds stored a separate level per
        // track; that made the slider look like it had forgotten the change as
        // soon as another track started, so it is no longer read.
        let volume = if state.volume.is_finite() {
            state.volume.clamp(0.0, 1.25)
        } else {
            0.78
        };
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
        // The two render styles are exclusive; legacy saves that mixed the
        // old toggles fall back to the default style.
        let dot_style = state.braille_stream && state.led_bars;

        let mut app = Self {
            engine: AudioEngine::new(),
            analyzer: SpectrumEngine::new(beat_config.clone()),
            beat_config,
            visual: VisualState::default(),
            viz_rotation: 0.0,
            core_beat_age: 1.0,
            core_beat_seq: 0,
            core_ring_strength: 0.0,
            beat_analysis: None,
            beat_generation: 0,
            note_analysis: None,
            note_generation: 0,
            offline_notes: None,
            notes_state: NotesState::Idle,
            notes_path: None,
            offline_beats: Vec::new(),
            offline_index: 0,
            offline_seq: 0,
            offline_strength: 0.0,
            offline_ready: false,
            offline_last_pos: None,
            playlist,
            current,
            selected_track: current,
            lrc_map,
            next_queue: VecDeque::new(),
            history: Vec::new(),
            history_pos: 0,
            shuffle_order: Vec::new(),
            rng_state: SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos() as u64)
                .unwrap_or(0x9E37_79B9_7F4A_7C15),
            lyrics: Vec::new(),
            lyrics_synced: false,
            lyrics_track: None,
            lyric_seek_full: false,
            lyric_seek_index: None,
            pending_delete: None,
            playing: false,
            paused: false,
            volume,
            play_mode: PlayMode::from_saved(state.play_mode),
            search: String::new(),
            track_scroll_target: None,
            track_scroll_grab: None,
            samples: Vec::with_capacity(8192),
            duration: 0.0,
            format_label: "---".to_owned(),
            sample_rate: 44_100,
            sidebar_open: true,
            show_settings: false,
            settings_page: SettingsPage::System,
            themes,
            theme_name,
            theme_editor: None,
            themes_dirty: false,
            auto_dark,
            auto_mood,
            track_mood: None,
            theme_fade: None,
            show_bg_spectrum: state.show_bg_spectrum,
            braille_stream: dot_style,
            led_bars: dot_style,
            peak_hold: state.peak_hold,
            show_lyrics: state.show_lyrics,
            piano_mode: state.piano_mode,
            piano_history: VecDeque::new(),
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
            proxy: state.proxy.clone(),
            env_proxy: env_proxy(),
            error: theme_error,
            seek_preview: None,
            last_tick: Instant::now(),
            dirty: false,
            save_at: Instant::now(),
            device_check_at: Instant::now(),
            output_label: "NO OUTPUT".to_owned(),
        };

        // The shuffle bag is drawn once per launch. Next and Previous both
        // walk this fixed random order, so stepping back and forth never
        // falls back to the sequential playlist order.
        app.rebuild_shuffle_order();

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

        app.refresh_lyrics();
        app
    }

    fn tick(&mut self, ctx: &Context) {
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_tick)
            .as_secs_f32()
            .clamp(0.0, 0.2);
        self.last_tick = now;

        self.poll_beat_analysis();
        self.poll_note_analysis();
        self.step_theme_fade(ctx);
        self.engine.drain_analysis(&mut self.samples, 16_384);
        let active = self.playing && !self.paused;
        let offline_beat = if active {
            self.offline_beat_state(self.engine.position())
        } else {
            None
        };
        if active {
            self.analyzer.push(&self.samples);
            self.viz_rotation = (self.viz_rotation - std::f32::consts::PI / 40.0 * dt)
                .rem_euclid(std::f32::consts::TAU);
            self.visual = self.analyzer.tick(true, offline_beat);
        } else {
            self.samples.clear();
            if self.analyzer.has_signal() || self.visual.energy > 0.002 {
                self.analyzer.decay_display(dt);
                self.visual = self.analyzer.tick(false, None);
                if !self.analyzer.has_signal() && self.visual.energy <= 0.002 {
                    self.analyzer.clear_display();
                    self.visual = VisualState::default();
                    self.viz_rotation = 0.0;
                }
            }
        }

        self.sample_piano_history(active);

        if active {
            if self.visual.beat_seq > self.core_beat_seq {
                self.core_beat_seq = self.visual.beat_seq;
                let strength = self.visual.beat_strength;
                let heavy = strength >= self.beat_config.heavy_ring_threshold;
                let short = !heavy && strength >= self.beat_config.quick_strength_min;
                let launch_ring =
                    heavy || (short && self.beat_config.short_visual == ShortVisual::Ring);
                if launch_ring {
                    // Hard beat, or a short-pair hit configured to ring.
                    self.core_beat_age = 0.0;
                    self.core_ring_strength = if heavy {
                        strength
                    } else {
                        self.beat_config.short_ring_strength
                    };
                } else {
                    // Light pulse: let an existing ring finish; the circle
                    // brightness alone reacts through `kick`.
                    self.core_beat_age = (self.core_beat_age + dt).min(1.0);
                }
            } else {
                if self.visual.beat_seq < self.core_beat_seq {
                    self.core_beat_seq = self.visual.beat_seq;
                }
                self.core_beat_age = (self.core_beat_age + dt).min(1.0);
            }
        } else {
            self.core_beat_age = 1.0;
            self.core_ring_strength = 0.0;
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

        if let Some(notice) = self.engine.take_notice() {
            // The seek was rejected and the clock is back at the old position,
            // so stop showing the line we tried to land on as complete.
            self.lyric_seek_full = false;
            self.lyric_seek_index = None;
            self.error = Some(notice);
        }

        if self.playing && self.engine.is_finished() {
            self.playing = false;
            self.paused = false;
            if !self.playlist.is_empty() {
                self.advance_track(true);
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

        // A confirmation modal or an open context menu owns the input.
        if self.pending_delete.is_some() || egui::Popup::is_any_open(ctx) {
            return;
        }

        if ctx.egui_wants_keyboard_input() {
            return;
        }

        let (
            space,
            ctrl_o,
            up,
            down,
            left,
            right,
            ctrl_left,
            ctrl_right,
            page_up,
            page_down,
            escape,
        ) = ctx.input(|input| {
            (
                input.key_pressed(Key::Space),
                input.modifiers.ctrl && input.key_pressed(Key::O),
                input.key_pressed(Key::ArrowUp),
                input.key_pressed(Key::ArrowDown),
                input.key_pressed(Key::ArrowLeft) && !input.modifiers.ctrl,
                input.key_pressed(Key::ArrowRight) && !input.modifiers.ctrl,
                input.modifiers.ctrl && input.key_pressed(Key::ArrowLeft),
                input.modifiers.ctrl && input.key_pressed(Key::ArrowRight),
                input.key_pressed(Key::PageUp),
                input.key_pressed(Key::PageDown),
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
        if up {
            self.adjust_volume(VOLUME_STEP);
        } else if down {
            self.adjust_volume(-VOLUME_STEP);
        } else if page_up || ctrl_left {
            self.previous_track();
        } else if page_down || ctrl_right {
            self.next_track();
        } else if left {
            self.seek_relative(-SEEK_STEP);
        } else if right {
            self.seek_relative(SEEK_STEP);
        }
    }

    fn process_actions(&mut self, ctx: &Context, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Play(index) => self.play_index(index),
                Action::Select(index) => self.selected_track = Some(index),
                Action::QueueNext(index) => self.queue_next(index),
                Action::ImportLrc(index) => self.import_lrc(index),
                Action::ClearLrc(index) => self.clear_lrc(index),
                Action::OpenTrackLocation(index) => self.open_track_location(index),
                Action::RemoveTrack(index) => self.remove_track_at(index, false),
                Action::DeleteTrack(index) => {
                    self.pending_delete = self.playlist.get(index).map(|track| track.path.clone());
                }
                Action::ConfirmDeleteTrack => {
                    if let Some(path) = self.pending_delete.take() {
                        if let Some(index) =
                            self.playlist.iter().position(|track| track.path == path)
                        {
                            self.remove_track_at(index, true);
                        }
                    }
                }
                Action::CancelDeleteTrack => self.pending_delete = None,
                Action::AddFiles => self.pick_files(),
                Action::Toggle => self.toggle_play(),
                Action::Next => self.next_track(),
                Action::Prev => self.previous_track(),
                Action::Stop => self.stop(),
                Action::Seek(fraction) => {
                    let target = fraction as f64 * self.duration();
                    self.seek_to(target);
                }
                Action::Volume(value) => self.set_volume(value),
                Action::CycleMode => {
                    self.play_mode = self.play_mode.next();
                    self.dirty = true;
                }
                Action::ToggleSettings => {
                    self.show_settings = !self.show_settings;
                    if self.show_settings {
                        self.show_eq = false;
                        self.show_download = false;
                    }
                }
                Action::ToggleVisualMode => {
                    if self.piano_mode {
                        self.set_piano_mode(false);
                        self.set_dot_style(false);
                    } else if self.dot_style() {
                        self.set_piano_mode(true);
                    } else {
                        self.set_dot_style(true);
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

    /// A manual pick, which also leaves AUTO.
    fn select_theme(&mut self, index: usize, ctx: &Context) {
        let Some(entry) = self.themes.entry(index) else {
            return;
        };
        self.theme_name = entry.name.clone();
        self.auto_dark = None;
        self.theme_fade = None;
        theme::set_look(entry.look);
        theme::apply(ctx);
        ctx.request_repaint();
        self.dirty = true;
    }

    /// Re-applies the current theme after the library changed; falls back to
    /// MONO or PAPER, matching the current brightness, when it is gone.
    fn refresh_theme(&mut self, ctx: &Context) {
        if self.auto_dark.is_some() {
            // The AUTO look does not come from the library.
            return;
        }
        let index = self
            .themes
            .find(&self.theme_name)
            .or_else(|| {
                self.themes
                    .find_builtin(if theme::is_light() { 1 } else { 0 })
            })
            .unwrap_or(0);
        self.select_theme(index, ctx);
    }

    /// What the AUTO look would be generated from right now. Turning AUTO on
    /// switches straight to it; only track changes fade.
    fn auto_preview_mood(&self) -> Mood {
        match self.auto_dark {
            Some(_) => self.auto_mood,
            None => self.track_mood.or(self.auto_mood),
        }
        .unwrap_or_default()
    }

    fn select_auto(&mut self, dark: bool, ctx: &Context) {
        let mood = self.auto_preview_mood();
        self.auto_mood = Some(mood);
        self.auto_dark = Some(dark);
        self.theme_fade = None;
        theme::set_look(auto_theme::look(mood, dark));
        theme::apply(ctx);
        ctx.request_repaint();
        self.dirty = true;
    }

    /// Fades the AUTO look to a newly analysed track.
    fn follow_track_mood(&mut self, mood: Mood) {
        self.track_mood = Some(mood);
        let Some(dark) = self.auto_dark else {
            return;
        };
        self.auto_mood = Some(mood);
        self.theme_fade = Some(Fade::new(
            theme::current_look(),
            auto_theme::look(mood, dark),
        ));
        self.dirty = true;
    }

    fn step_theme_fade(&mut self, ctx: &Context) {
        let Some(fade) = self.theme_fade else {
            return;
        };
        let (look, done) = fade.sample(Instant::now());
        theme::set_look(look);
        theme::apply(ctx);
        if done {
            self.theme_fade = None;
        } else {
            ctx.request_repaint();
        }
    }

    fn open_theme_editor(&mut self, index: usize) {
        let Some(spec) = self.themes.entry(index).and_then(|entry| entry.spec) else {
            return;
        };
        if let Some(source) = self.themes.spec(spec) {
            self.theme_editor = Some(ThemeEditor::new(spec, source));
        }
    }

    fn sync_theme_editor(&mut self) {
        if let Some(editor) = self.theme_editor.as_mut() {
            match self.themes.spec(editor.spec) {
                Some(source) => editor.sync(source),
                None => self.theme_editor = None,
            }
        }
    }

    fn edit_theme(&mut self, edit: ThemeEdit, ctx: &Context) {
        let Some(spec) = self.theme_editor.as_ref().map(|editor| editor.spec) else {
            return;
        };
        if !self.themes.editable() {
            return;
        }
        let index = self.themes.edit(spec, |target| match edit {
            ThemeEdit::Rename(name) => target.name = name,
            ThemeEdit::Background(slot, rgb) => {
                if let Some(hex) = target.background.get_mut(slot) {
                    *hex = Hex(rgb);
                }
            }
            ThemeEdit::BackgroundCount(count) => target.set_background_count(count),
            ThemeEdit::Accent(rgb) => target.accent = Hex(rgb),
            ThemeEdit::Tint(tint) => target.tint = tint,
            ThemeEdit::Dark(dark) => target.dark = dark,
            ThemeEdit::ClearOverrides => target.colors = Overrides::default(),
        });
        self.themes_dirty = true;
        match index {
            Some(index) => self.select_theme(index, ctx),
            None => self.refresh_theme(ctx),
        }
    }

    fn theme_action(&mut self, action: ThemeAction, ctx: &Context) {
        match action {
            ThemeAction::Select(index) => self.select_theme(index, ctx),
            ThemeAction::Auto(dark) => self.select_auto(dark, ctx),
            ThemeAction::CopyAuto => {
                let Some(dark) = self.auto_dark else {
                    return;
                };
                if !self.themes.editable() {
                    return;
                }
                // The target look, not a frame of a fade still running.
                let mood = self.auto_preview_mood();
                let look = auto_theme::look(mood, dark);
                if let Some(copy) = self.themes.adopt(&mood.name(), &look) {
                    self.themes_dirty = true;
                    self.select_theme(copy, ctx);
                    self.open_theme_editor(copy);
                }
            }
            ThemeAction::Copy(index) => {
                if !self.themes.editable() {
                    return;
                }
                if let Some(copy) = self.themes.duplicate(index) {
                    self.themes_dirty = true;
                    self.select_theme(copy, ctx);
                    self.open_theme_editor(copy);
                }
            }
            ThemeAction::Edit(index) => self.open_theme_editor(index),
            ThemeAction::Reload => {
                // Pending UI edits are written first; if the file was changed
                // by hand meanwhile, the save is refused and the disk wins.
                if self.themes_dirty {
                    let _ = self.themes.save();
                    self.themes_dirty = false;
                }
                self.themes.reload();
                if let Some(error) = self.themes.load_error() {
                    self.error = Some(format!("THEMES.JSON / {error}"));
                }
                self.theme_editor = None;
                self.refresh_theme(ctx);
            }
            ThemeAction::Back => self.theme_editor = None,
            ThemeAction::Delete(spec) => {
                self.themes.remove(spec);
                self.themes_dirty = true;
                self.theme_editor = None;
                self.refresh_theme(ctx);
            }
        }
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
        let proxy = {
            let proxy = self.proxy.trim().to_owned();
            (!proxy.is_empty()).then_some(proxy)
        };
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        download::spawn(input, output_dir, proxy, sender, Arc::clone(&cancel));
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
            let key = path.to_string_lossy().to_string();
            let lrc = self
                .lrc_map
                .get(&key)
                .filter(|lrc| lrc.is_file())
                .cloned()
                .or_else(|| find_sidecar_lrc(&path));
            if let Some(lrc) = &lrc {
                self.lrc_map.insert(key, lrc.clone());
            }
            added.push(self.playlist.len());
            self.playlist.push(entry_for_path(path, lrc));
        }

        if added.is_empty() {
            return;
        }

        self.rebuild_shuffle_order();
        self.dirty = true;
        if self.current.is_none() {
            self.play_index(added[0]);
        }
    }

    fn start_beat_analysis(&mut self, path: &Path) {
        self.cancel_beat_analysis();
        self.clear_offline_beats();
        self.track_mood = None;
        self.beat_generation = self.beat_generation.wrapping_add(1);
        let generation = self.beat_generation;
        let path = path.to_owned();
        let thread_path = path.clone();
        let thread_config = self.beat_config.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = cancel.clone();
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("sonadial-beat-analysis".to_owned())
            .spawn(move || {
                let analysis =
                    crate::beat::analyze_file(&thread_path, &thread_cancel, &thread_config)
                        .unwrap_or_default();
                let _ = tx.send(BeatAnalysisResult {
                    generation,
                    path: thread_path,
                    beats: analysis.beats,
                    profile: analysis.profile,
                });
            });
        if spawned.is_ok() {
            self.beat_analysis = Some(BeatAnalysisHandle { rx, cancel });
        }
    }

    fn cancel_beat_analysis(&mut self) {
        if let Some(handle) = self.beat_analysis.take() {
            handle.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn clear_offline_beats(&mut self) {
        self.offline_beats.clear();
        self.offline_index = 0;
        self.offline_seq = 0;
        self.offline_strength = 0.0;
        self.offline_ready = false;
        self.offline_last_pos = None;
    }

    fn poll_beat_analysis(&mut self) {
        let mut received = None;
        let mut disconnected = false;
        if let Some(handle) = &self.beat_analysis {
            match handle.rx.try_recv() {
                Ok(result) => received = Some(result),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => disconnected = true,
            }
        }
        if let Some(result) = received {
            self.beat_analysis = None;
            let matches_current = self
                .current
                .and_then(|index| self.playlist.get(index))
                .map(|track| track.path == result.path)
                .unwrap_or(false);
            if result.generation == self.beat_generation && matches_current {
                self.offline_beats = result.beats;
                self.offline_ready = true;
                self.offline_index = 0;
                self.offline_seq = 0;
                self.offline_strength = 0.0;
                self.offline_last_pos = None;
                let position = self.engine.position();
                self.offline_index = self
                    .offline_beats
                    .partition_point(|(time, _)| *time < position - 0.025);
                if let Some(profile) = result.profile {
                    self.follow_track_mood(Mood::from_profile(&profile));
                }
            }
        } else if disconnected {
            self.beat_analysis = None;
        }
    }

    fn start_note_analysis(&mut self, path: &Path) {
        self.cancel_note_analysis();
        self.offline_notes = None;
        self.notes_path = None;
        self.notes_state = NotesState::Running;
        self.note_generation = self.note_generation.wrapping_add(1);
        let generation = self.note_generation;
        let thread_path = path.to_owned();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = cancel.clone();
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("sonadial-note-analysis".to_owned())
            .stack_size(1024 * 1024)
            .spawn(move || {
                let analysis = crate::notes::analyze_file(&thread_path, &thread_cancel)
                    .map_err(|error| error.to_string());
                let _ = tx.send(NoteAnalysisResult {
                    generation,
                    path: thread_path,
                    analysis,
                });
            });
        if spawned.is_ok() {
            self.note_analysis = Some(NoteAnalysisHandle { rx, cancel });
        } else {
            self.notes_state = NotesState::Failed;
        }
    }

    fn cancel_note_analysis(&mut self) {
        if let Some(handle) = self.note_analysis.take() {
            handle.cancel.store(true, Ordering::Relaxed);
            if self.notes_state == NotesState::Running {
                self.notes_state = NotesState::Idle;
            }
        }
    }

    fn poll_note_analysis(&mut self) {
        let mut received = None;
        let mut disconnected = false;
        if let Some(handle) = &self.note_analysis {
            match handle.rx.try_recv() {
                Ok(result) => received = Some(result),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => disconnected = true,
            }
        }
        if let Some(result) = received {
            self.note_analysis = None;
            let matches_current = self
                .current
                .and_then(|index| self.playlist.get(index))
                .map(|track| track.path == result.path)
                .unwrap_or(false);
            if result.generation == self.note_generation && matches_current {
                match result.analysis {
                    Ok(analysis) => {
                        self.offline_notes = Some(analysis);
                        self.notes_path = Some(result.path.clone());
                        self.notes_state = NotesState::Ready;
                    }
                    Err(_) => {
                        self.offline_notes = None;
                        self.notes_path = None;
                        self.notes_state = NotesState::Failed;
                    }
                }
            }
        } else if disconnected {
            self.note_analysis = None;
            self.notes_state = NotesState::Failed;
        }
    }

    /// Piano mode is the only consumer of the offline note transcription, so the
    /// whole-song decode only runs while it is on. Turning it off frees the
    /// result again; turning it back on hits the on-disk cache.
    fn set_piano_mode(&mut self, on: bool) {
        if self.piano_mode == on {
            return;
        }
        self.piano_mode = on;
        self.piano_history.clear();
        self.dirty = true;
        if on {
            let Some(path) = self.current_track().map(|track| track.path.clone()) else {
                return;
            };
            let ready = self.notes_state == NotesState::Ready
                && self.notes_path.as_deref() == Some(path.as_path());
            if !ready && self.note_analysis.is_none() {
                self.start_note_analysis(&path);
            }
        } else {
            self.cancel_note_analysis();
            self.offline_notes = None;
            self.notes_path = None;
            self.notes_state = NotesState::Idle;
        }
    }

    fn offline_roll_notes(&self) -> Option<&[crate::notes::NoteEvent]> {
        self.offline_notes
            .as_ref()
            .filter(|analysis| analysis.notes.len() >= 8)
            .map(|analysis| analysis.notes.as_slice())
    }

    /// Sample keyboard activity on the audio timeline for the piano style.
    fn sample_piano_history(&mut self, active: bool) {
        if !self.piano_mode || !active || self.offline_roll_notes().is_some() {
            return;
        }
        let position = self.engine.position();
        if let Some(last) = self.piano_history.back() {
            if position + 0.30 < last.time {
                self.piano_history.clear();
            } else if position - last.time < 0.04 {
                return;
            }
        }
        let levels = self.analyzer.pitch().levels;
        self.piano_history.push_back(NoteFrame {
            time: position - 0.06,
            levels,
        });
        while let Some(front) = self.piano_history.front() {
            if position - front.time > 9.0 {
                self.piano_history.pop_front();
            } else {
                break;
            }
        }
    }

    fn offline_beat_state(&mut self, position: f64) -> Option<(u64, f32)> {
        if !self.offline_ready || self.offline_beats.is_empty() {
            return None;
        }
        let position = position.max(0.0);
        if self.offline_last_pos.map_or(true, |previous| {
            position < previous - 0.08 || position > previous + 0.30
        }) {
            self.offline_index = self
                .offline_beats
                .partition_point(|(time, _)| *time < position - 0.025);
        }

        let upper = position + BEAT_VISUAL_LEAD;
        let lower = self.offline_last_pos.map_or(position - 0.050, |previous| {
            (previous - 0.010).min(position)
        });
        let mut strength: f32 = 0.0;
        while self.offline_index < self.offline_beats.len()
            && self.offline_beats[self.offline_index].0 <= upper
        {
            let (time, value) = self.offline_beats[self.offline_index];
            if time >= lower {
                strength = strength.max(value);
            }
            self.offline_index += 1;
        }
        if strength > 0.0 {
            self.offline_seq = self.offline_seq.wrapping_add(1);
            self.offline_strength = strength;
        }
        self.offline_last_pos = Some(position);
        Some((self.offline_seq, self.offline_strength))
    }

    fn play_index(&mut self, index: usize) {
        self.start_playback(index, true);
    }

    fn start_playback(&mut self, index: usize, record_history: bool) {
        self.start_playback_at(index, record_history, None);
    }

    /// `park_at` opens the track silently at that position: the stream exists
    /// and the buffer is filled, but playback stays paused until PLAY.
    fn start_playback_at(&mut self, index: usize, record_history: bool, park_at: Option<f64>) {
        let Some(track) = self.playlist.get(index).cloned() else {
            return;
        };

        if record_history {
            self.record_history(&track.path);
        } else if self.history.get(self.history_pos) != Some(&track.path) {
            if let Some(position) = self.history.iter().position(|path| path == &track.path) {
                self.history_pos = position;
            }
        }

        self.current = Some(index);
        self.refresh_lyrics();
        self.core_beat_age = 1.0;
        self.core_beat_seq = self.visual.beat_seq;
        self.core_ring_strength = 0.0;
        self.cancel_beat_analysis();
        self.clear_offline_beats();
        self.cancel_note_analysis();
        self.offline_notes = None;
        self.notes_path = None;
        self.notes_state = NotesState::Idle;
        self.engine.set_volume(self.volume);
        self.engine.set_eq(&self.eq_gains, self.eq_enabled);
        let opened = match park_at {
            Some(_) => self.engine.play_paused(&track.path),
            None => self.engine.play(&track.path),
        };
        match opened {
            Ok(()) => {
                self.playing = true;
                self.paused = park_at.is_some();
                self.error = None;
                self.analyzer.reset();
                self.piano_history.clear();
                self.visual = VisualState::default();
                self.viz_rotation = 0.0;
                if let Some(info) = self.engine.info() {
                    self.duration = info.duration;
                }
                self.format_label = extension_label(&track.path);
                self.output_label = self.engine.output_label();
                self.engine.set_volume(self.volume);
                self.start_beat_analysis(&track.path);
                if self.piano_mode {
                    self.start_note_analysis(&track.path);
                }
                if let Some(target) = park_at {
                    // The stream is already parked, so this seek only moves the
                    // buffer and the clock; the user still has to press PLAY.
                    if self.engine.seek(target) {
                        self.engine.flush_analysis();
                        self.mark_lyric_seek(target);
                    }
                }
                self.dirty = true;
            }
            Err(error) => {
                self.playing = false;
                self.paused = false;
                self.error = Some(format!("{error:#}"));
            }
        }
    }

    fn record_history(&mut self, path: &Path) {
        if self.history.is_empty() {
            self.history.push(path.to_owned());
            self.history_pos = 0;
        } else if self
            .history
            .get(self.history_pos)
            .map(|entry| entry == path)
            .unwrap_or(false)
        {
            // The track is already the current history entry.
        } else if self
            .history
            .get(self.history_pos + 1)
            .map(|entry| entry == path)
            .unwrap_or(false)
        {
            // Stepping forward through history should not fork it.
            self.history_pos += 1;
        } else {
            self.history.truncate(self.history_pos + 1);
            self.history.push(path.to_owned());
            self.history_pos = self.history.len() - 1;
        }
        // A long session would otherwise keep every visited path forever.
        const HISTORY_LIMIT: usize = 200;
        if self.history.len() > HISTORY_LIMIT {
            let excess = self.history.len() - HISTORY_LIMIT;
            self.history.drain(..excess);
            self.history_pos = self.history_pos.saturating_sub(excess);
        }
    }

    fn refresh_lyrics(&mut self) {
        self.lyric_seek_full = false;
        self.lyric_seek_index = None;
        let Some(track) = self.current_track().cloned() else {
            self.lyrics.clear();
            self.lyrics_synced = false;
            self.lyrics_track = None;
            return;
        };
        self.lyrics_track = Some(track.path.clone());
        let parsed = track.lrc.as_deref().map(lyrics::load).unwrap_or_default();
        self.lyrics = parsed.lines;
        self.lyrics_synced = parsed.synced;
    }

    fn lyric_index_at(&self, position: f64) -> Option<(usize, bool)> {
        if self.lyrics.is_empty() || self.current.is_none() {
            return None;
        }
        if self.lyrics_synced {
            return Some(match lyrics::active_index(&self.lyrics, position) {
                Some(index) => (index, true),
                None => (0, false),
            });
        }

        // Plain-text LRC files have no timestamps. Spread the lines over the
        // track length so the two-line display still advances.
        let count = self.lyrics.len().max(1);
        let duration = self.duration();
        let interval = if duration > 0.0 {
            (duration / count as f64).clamp(3.0, 8.0)
        } else {
            6.0
        };
        Some((
            ((position / interval).floor() as usize).min(count.saturating_sub(1)),
            true,
        ))
    }

    fn lyric_line_interval(&self, index: usize) -> (f64, f64) {
        let count = self.lyrics.len().max(1);
        if self.lyrics_synced {
            let start = self.lyrics.get(index).map(|line| line.time).unwrap_or(0.0);
            let next = self
                .lyrics
                .get(index + 1)
                .map(|line| line.time)
                .unwrap_or(start + 4.0);
            (start, (next - start).max(0.2))
        } else {
            let duration = self.duration();
            let interval = if duration > 0.0 {
                (duration / count as f64).clamp(3.0, 8.0)
            } else {
                6.0
            };
            (index as f64 * interval, interval)
        }
    }

    /// Number of characters visible while the active lyric is being typed.
    /// `None` means "inactive, show the whole line without a cursor".
    fn lyric_typed_chars(&self, index: usize, active: bool, position: f64) -> Option<usize> {
        if !active {
            return None;
        }
        let line = self.lyrics.get(index)?;
        let chars = line.text.chars().count();
        if chars == 0 {
            return Some(0);
        }
        let (start, interval) = self.lyric_line_interval(index);
        // Chinese: 5 characters per second. English stays syllable-based but
        // is 20% slower than the previous 5 syllables/sec: 4 syllables/sec
        // (weight 1.25 at the 5-units/sec base). The next line is the final
        // hard limit, with a 100 ms guard.
        let units = lyric_units(&line.text);
        let natural = (units / 5.0).clamp(0.35, 10.0);
        let next_line_limit = (interval - 0.10).max(0.18);
        let duration = natural.min(next_line_limit);
        let elapsed = (position - start).max(0.0);
        if elapsed >= duration {
            return Some(chars);
        }
        let visible = ((elapsed / duration) * chars as f64).floor() as usize;
        Some(visible.min(chars))
    }

    fn paint_lyrics(&mut self, painter: &egui::Painter, rect: Rect) {
        // Select the line and its typed prefix from one audio-clock snapshot.
        let position = self.position();
        let Some((index, active)) = self.lyric_index_at(position) else {
            return;
        };

        let mut typed = self.lyric_typed_chars(index, active, position);
        if self.lyric_seek_full {
            if self.lyric_seek_index == Some(index) {
                typed = None;
            } else {
                self.lyric_seek_full = false;
                self.lyric_seek_index = None;
            }
        }

        let prompt_x = rect.left() + 18.0;
        let active_y = rect.top() + 50.0;
        let next_y = rect.top() + 78.0;
        let active_font = lyric_font(16.0);
        let next_font = lyric_font(12.0);
        let prompt_font = active_font.clone();
        let prompt_w = painter
            .layout_no_wrap(">".to_owned(), prompt_font.clone(), theme::text())
            .size()
            .x;
        // Both rows share one text column so typing never shifts the line.
        let text_x = prompt_x + prompt_w + 8.0;
        let text_width = (rect.right() - text_x - 18.0).max(80.0);
        let active_color = if active {
            theme::text_hi()
        } else {
            theme::text_dim()
        };
        let prompt_color = if active {
            theme::text()
        } else {
            theme::text_dim()
        };
        painter.text(
            Pos2::new(prompt_x, active_y),
            Align2::LEFT_CENTER,
            ">",
            prompt_font,
            prompt_color,
        );
        paint_lyric_text(
            painter,
            self.lyrics.get(index),
            text_x,
            active_y,
            text_width,
            active_font,
            active_color,
            typed,
            true,
        );
        paint_lyric_text(
            painter,
            self.lyrics.get(index + 1),
            text_x,
            next_y,
            text_width,
            next_font,
            theme::text_dim(),
            None,
            false,
        );
    }

    fn import_lrc(&mut self, index: usize) {
        let Some(track) = self.playlist.get(index) else {
            return;
        };
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Lyrics", &["lrc"])
            .add_filter("All files", &["*"]);
        if let Some(parent) = track.path.parent() {
            dialog = dialog.set_directory(parent);
        }
        let Some(path) = dialog.pick_file() else {
            return;
        };
        let key = track.path.to_string_lossy().to_string();
        self.lrc_map.insert(key, path.clone());
        if let Some(track) = self.playlist.get_mut(index) {
            track.lrc = Some(path);
        }
        if self.current == Some(index) {
            self.refresh_lyrics();
        }
        self.dirty = true;
    }

    fn clear_lrc(&mut self, index: usize) {
        let Some(track) = self.playlist.get(index) else {
            return;
        };
        let key = track.path.to_string_lossy().to_string();
        self.lrc_map.remove(&key);
        if let Some(track) = self.playlist.get_mut(index) {
            track.lrc = None;
        }
        if self.current == Some(index) {
            self.refresh_lyrics();
        }
        self.dirty = true;
    }

    fn dot_style(&self) -> bool {
        self.braille_stream && self.led_bars
    }

    fn set_dot_style(&mut self, dot: bool) {
        self.braille_stream = dot;
        self.led_bars = dot;
        self.dirty = true;
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
        self.cancel_beat_analysis();
        self.cancel_note_analysis();
        self.clear_offline_beats();
        self.engine.stop();
        self.engine.flush_analysis();
        self.playing = false;
        self.paused = false;
        self.core_beat_age = 1.0;
        self.core_ring_strength = 0.0;
        self.seek_preview = None;
        self.lyric_seek_full = false;
        self.lyric_seek_index = None;
    }

    fn queue_next(&mut self, index: usize) {
        if self.current == Some(index) {
            return;
        }
        let Some(track) = self.playlist.get(index) else {
            return;
        };
        if !self.next_queue.iter().any(|queued| queued == &track.path) {
            self.next_queue.push_back(track.path.clone());
        }
    }

    fn open_track_location(&mut self, index: usize) {
        let Some(track) = self.playlist.get(index) else {
            return;
        };
        let path = track.path.clone();
        if !path.exists() {
            self.error = Some(format!("FILE NOT FOUND / {}", path.display()));
            return;
        }
        if let Err(error) = std::process::Command::new("explorer")
            .arg("/select,")
            .arg(&path)
            .spawn()
        {
            self.error = Some(format!("OPEN LOCATION FAILED / {error}"));
        }
    }

    fn remove_track_at(&mut self, index: usize, delete_file: bool) {
        let Some(track) = self.playlist.get(index).cloned() else {
            return;
        };
        let path = track.path;
        let is_current = self.current == Some(index);

        if is_current {
            self.stop();
        }
        if delete_file {
            if let Err(error) = std::fs::remove_file(&path) {
                self.error = Some(format!("DELETE FAILED / {error}"));
                return;
            }
        }

        self.playlist.remove(index);
        if self.selected_track == Some(index) {
            self.selected_track = None;
        } else if let Some(selected) = self.selected_track {
            if selected > index {
                self.selected_track = Some(selected - 1);
            }
        }
        self.next_queue.retain(|queued| queued != &path);
        self.rebuild_shuffle_order();
        if is_current {
            self.current = None;
            self.duration = 0.0;
            self.format_label = "---".to_owned();
            self.seek_preview = None;
            self.history.clear();
            self.history_pos = 0;
            self.lyrics.clear();
            self.lyrics_synced = false;
            self.lyrics_track = None;
            self.lyric_seek_full = false;
            self.lyric_seek_index = None;
        } else {
            if let Some(current) = self.current {
                if current > index {
                    self.current = Some(current - 1);
                }
            }
            self.history.retain(|entry| entry != &path);
            let current_path = self.current_track().map(|track| track.path.clone());
            if let Some(position) = current_path
                .as_ref()
                .and_then(|path| self.history.iter().position(|entry| entry == path))
            {
                self.history_pos = position;
            } else {
                self.history.clear();
                self.history_pos = 0;
            }
        }
        self.dirty = true;
    }

    fn next_track(&mut self) {
        self.advance_track(false);
    }

    fn advance_track(&mut self, automatic: bool) {
        if self.playlist.is_empty() {
            return;
        }
        // An explicit "play next" request takes priority over the play mode.
        while let Some(path) = self.next_queue.pop_front() {
            if let Some(index) = self.playlist.iter().position(|track| track.path == path) {
                self.play_index(index);
                return;
            }
        }
        // If the user stepped backwards, Next walks forward through the same
        // history before choosing a new track. Shuffle skips this and follows
        // its own fixed random order in both directions.
        if self.play_mode == PlayMode::Sequence {
            for position in self.history_pos + 1..self.history.len() {
                let path = self.history[position].clone();
                if let Some(index) = self.playlist.iter().position(|track| track.path == path) {
                    self.history_pos = position;
                    self.start_playback(index, false);
                    return;
                }
            }
        }
        let Some(current) = self.current else {
            self.play_index(0);
            return;
        };
        let next = match self.play_mode {
            PlayMode::RepeatOne if automatic => current,
            PlayMode::Shuffle => self.shuffle_step(current, true),
            PlayMode::Sequence | PlayMode::RepeatOne => (current + 1) % self.playlist.len(),
        };
        self.play_index(next);
    }

    fn previous_track(&mut self) {
        if self.playlist.is_empty() {
            return;
        }
        let Some(current) = self.current else {
            self.play_index(0);
            return;
        };
        // Shuffle walks the same pre-drawn random order in both directions,
        // so Prev then Next returns along that order instead of picking new
        // randomness or falling back to the sequential playlist.
        if self.play_mode == PlayMode::Shuffle {
            let previous = self.shuffle_step(current, false);
            self.play_index(previous);
            return;
        }
        // In the ordered modes Previous follows real playback history.
        for position in (0..self.history_pos).rev() {
            let path = self.history[position].clone();
            if let Some(index) = self.playlist.iter().position(|track| track.path == path) {
                self.history_pos = position;
                self.start_playback(index, false);
                return;
            }
        }
        let previous = (current + self.playlist.len() - 1) % self.playlist.len();
        self.play_index(previous);
    }

    /// Draw a fresh shuffle bag for the whole playlist. Called once at
    /// startup, and again only when the playlist changes.
    fn rebuild_shuffle_order(&mut self) {
        let len = self.playlist.len();
        self.shuffle_order = (0..len).collect();
        for index in (1..len).rev() {
            let swap = (self.next_random() % (index as u64 + 1)) as usize;
            self.shuffle_order.swap(index, swap);
        }
    }

    /// Move one step through the fixed shuffle order. The position is derived
    /// from the current track, so clicking a track directly re-anchors the
    /// walk without losing the rest of the bag.
    fn shuffle_step(&mut self, current: usize, forward: bool) -> usize {
        let len = self.playlist.len();
        if len <= 1 {
            return current;
        }
        if self.shuffle_order.len() != len {
            self.rebuild_shuffle_order();
        }
        let Some(position) = self
            .shuffle_order
            .iter()
            .position(|index| *index == current)
        else {
            self.rebuild_shuffle_order();
            return self.shuffle_order.first().copied().unwrap_or(current);
        };
        if forward {
            self.shuffle_order[(position + 1) % len]
        } else {
            self.shuffle_order[(position + len - 1) % len]
        }
    }

    fn next_random(&mut self) -> u64 {
        let mut value = self.rng_state;
        if value == 0 {
            value = 0x9E37_79B9_7F4A_7C15;
        }
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.rng_state = value;
        value
    }

    /// Single volume path for the slider and keyboard. The level is global:
    /// switching tracks does not reset it, and `save_state` persists it.
    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.25);
        self.engine.set_volume(self.volume);
        self.dirty = true;
    }

    fn adjust_volume(&mut self, delta: f32) {
        self.set_volume(self.volume + delta);
    }

    fn seek_relative(&mut self, seconds: f64) {
        self.seek_to(self.position() + seconds);
    }

    /// Single seek path. Both the progress bar and keyboard seeks go through
    /// here so position, lyrics, beat state and preview can never diverge.
    ///
    /// Seeking during playback keeps playing. Seeking while paused, stopped or
    /// finished only moves the position and stays parked; PLAY resumes there.
    fn seek_to(&mut self, target: f64) {
        let target = target.clamp(0.0, self.duration().max(0.0));
        let was_playing = self.playing && !self.paused && !self.engine.is_paused();

        if self.engine.info().is_none() {
            // Nothing is loaded. Open the selected track silently at the target
            // so the jump still lands instead of faking a playing state.
            let Some(index) = self.current else {
                return;
            };
            self.start_playback_at(index, false, Some(target));
            return;
        }

        if !self.engine.seek(target) {
            return;
        }
        self.engine.flush_analysis();
        self.mark_lyric_seek(target);
        self.analyzer.reset_beat();
        self.offline_index = 0;
        self.offline_seq = 0;
        self.offline_strength = 0.0;
        self.offline_last_pos = None;
        self.seek_preview = None;
        self.playing = true;
        if was_playing {
            self.paused = false;
        } else {
            // Keep the parked state so the button stays on PLAY: the jump must
            // not start audio the user did not ask for.
            self.engine.pause();
            self.paused = true;
        }
    }

    /// A manual seek should not replay the typewriter for the line we landed
    /// on; show that line complete and resume typing from the next one. The
    /// line is selected from the seek target time while the engine flushes.
    fn mark_lyric_seek(&mut self, target: f64) {
        let landed = self.lyric_index_at(target);
        self.lyric_seek_full = landed.map_or(false, |(_, active)| active);
        self.lyric_seek_index = landed.map(|(index, _)| index);
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
            volume: self.volume,
            play_mode: self.play_mode.saved(),
            light: theme::is_light(),
            theme: self
                .themes
                .find(&self.theme_name)
                .and_then(|index| self.themes.entry(index))
                .and_then(|entry| entry.builtin),
            theme_name: Some(self.theme_name.clone()),
            auto_theme: self.auto_dark,
            auto_mood: self.auto_mood,
            show_bg_spectrum: self.show_bg_spectrum,
            braille_stream: self.braille_stream,
            led_bars: self.led_bars,
            show_radial: self.show_radial,
            show_core_pulse: self.show_core_pulse,
            radial_scale: self.radial_scale,
            eq_enabled: self.eq_enabled,
            eq_gains: self.eq_gains.to_vec(),
            eq_preset: self.eq_preset,
            lrc_map: self
                .lrc_map
                .iter()
                .filter(|(_, lrc)| lrc.is_file())
                .map(|(track, lrc)| (track.clone(), lrc.to_string_lossy().to_string()))
                .collect(),
            peak_hold: self.peak_hold,
            show_lyrics: self.show_lyrics,
            piano_mode: self.piano_mode,
            proxy: self.proxy.clone(),
        };
        storage::save(&state);
        if self.themes_dirty {
            self.themes_dirty = false;
            if let Err(error) = self.themes.save() {
                self.error = Some(error);
            }
        }
        self.dirty = false;
        self.save_at = Instant::now();
    }

    fn paint_background(&self, ctx: &Context) {
        let painter = ctx.layer_painter(egui::LayerId::background());
        let screen = ctx.content_rect();
        theme::paint_background(&painter, screen);
        painter.rect_stroke(
            screen.shrink(0.5),
            tokens::RADIUS,
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
                    Pos2::new(rect.left() + 10.0, cy - tokens::CONTROL_H * 0.5),
                    Vec2::new(30.0, tokens::CONTROL_H),
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

                let mut x = rect.right() - 10.0;
                for (id, label, action) in [
                    ("tui-close", "[x]", Action::Close),
                    ("tui-max", "[]", Action::ToggleMaximize),
                    ("tui-min", "-", Action::Minimize),
                ] {
                    let button = Rect::from_min_size(
                        Pos2::new(x - 32.0, cy - tokens::CONTROL_H * 0.5),
                        Vec2::new(32.0, tokens::CONTROL_H),
                    );
                    if text_button(ui, id, button, label, 13.0).clicked() {
                        actions.push(action);
                    }
                    x -= 36.0;
                }

                let sys_rect = Rect::from_min_size(
                    Pos2::new(x - 38.0, cy - tokens::CONTROL_H * 0.5),
                    Vec2::new(38.0, tokens::CONTROL_H),
                );
                if text_button(ui, "tui-sys", sys_rect, "SYS", 12.0).clicked() {
                    actions.push(Action::ToggleSettings);
                }
                x -= 42.0;
                let eq_rect = Rect::from_min_size(
                    Pos2::new(x - 34.0, cy - tokens::CONTROL_H * 0.5),
                    Vec2::new(34.0, tokens::CONTROL_H),
                );
                if text_button(ui, "tui-eq", eq_rect, "EQ", 12.0).clicked() {
                    actions.push(Action::ToggleEq);
                }
                x -= 38.0;
                let mode_rect = Rect::from_min_size(
                    Pos2::new(x - 58.0, cy - tokens::CONTROL_H * 0.5),
                    Vec2::new(58.0, tokens::CONTROL_H),
                );
                if text_button(ui, "tui-mode", mode_rect, "[MODE]", 12.0).clicked() {
                    actions.push(Action::ToggleVisualMode);
                }
                x -= 62.0;

                let drag_left = brand_x + brand_w + tokens::GUTTER;
                let drag_right = (x - tokens::SPACE_5).max(drag_left + 16.0);
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
                        ui.add_space(tokens::GUTTER);
                        ui.label(
                            RichText::new("LIBRARY")
                                .font(mono(14.0))
                                .color(theme::text_hi()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(tokens::GUTTER);
                            ui.label(
                                RichText::new(format!("{:02} TRACKS", self.playlist.len()))
                                    .font(mono(12.0))
                                    .color(theme::text_dim()),
                            );
                        });
                    },
                );

                ui.add_space(tokens::SPACE_2);
                ui.horizontal(|ui| {
                    ui.add_space(tokens::GUTTER);
                    let label_font = mono(13.0);
                    let label_w = ui
                        .painter()
                        .layout_no_wrap("find:".to_owned(), label_font.clone(), theme::text_dim())
                        .size()
                        .x;
                    let (label_rect, _) = ui.allocate_exact_size(
                        Vec2::new(label_w, tokens::FIELD_H),
                        Sense::hover(),
                    );
                    // A singleline TextEdit draws its text two pixels below a
                    // plain label centred on the same row; drop the prompt to
                    // the same baseline instead of adding another centring pass.
                    ui.painter().text(
                        Pos2::new(label_rect.left(), label_rect.center().y + 2.0),
                        Align2::LEFT_CENTER,
                        "find:",
                        label_font,
                        theme::text_dim(),
                    );
                    ui.add_sized(
                        [(inner_w - 48.0).max(40.0), tokens::FIELD_H],
                        TextEdit::singleline(&mut self.search)
                            .hint_text("title...")
                            .font(title(13.0))
                            .align(Align2::LEFT_CENTER)
                            .text_color(theme::text_hi())
                            .frame(Frame::NONE),
                    );
                });

                ui.add_space(tokens::SPACE_TIGHT);
                ui.horizontal(|ui| {
                    ui.add_space(tokens::GUTTER);
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
                ui.add_space(tokens::SPACE_TIGHT);

                let query = self.search.trim().to_lowercase();
                let filtered: Vec<usize> = self
                    .playlist
                    .iter()
                    .enumerate()
                    .filter(|(_, track)| {
                        query.is_empty()
                            || track.search_index.matches(&query)
                    })
                    .map(|(index, _)| index)
                    .collect();
                let scroll_offset = self.track_scroll_target.take();
                let mut scroll = ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden);
                if let Some(offset) = scroll_offset {
                    scroll = scroll.vertical_scroll_offset(offset);
                }
                // `show_rows` adds the item spacing itself; pass the bare row
                // height so scrolling stays aligned with the layout.
                let output = scroll.show_rows(ui, tokens::ROW_H, filtered.len(), |ui, rows| {
                    ui.set_width((inner_w - 14.0).max(40.0));
                            let row_painter = ui.painter().clone();
                            for visible in rows {
                                let index = filtered[visible];
                                let track = &self.playlist[index];
                                let selected = self.current == Some(index);
                                let focused = self.selected_track == Some(index);
                                let playing = selected && self.playing;
                                let (row_rect, response) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), tokens::ROW_H),
                                    Sense::click(),
                                );
                                if response.hovered() && !selected && !focused {
                                    row_painter.rect_stroke(
                                        row_rect.shrink(0.5),
                                        0.0,
                                        Stroke::new(
                                            1.0,
                                            theme::alpha(theme::text_hi(), 120),
                                        ),
                                        StrokeKind::Inside,
                                    );
                                }
                                if selected {
                                    // Inverse video: the current row becomes one
                                    // solid terminal block instead of a faint tint.
                                    row_painter.rect_filled(row_rect, 0.0, theme::text_hi());
                                }
                                if focused {
                                    // Single click selects; a wireframe keeps the
                                    // selection visible on plain and inverse rows.
                                    let color = if selected {
                                        theme::bg()
                                    } else {
                                        theme::text_hi()
                                    };
                                    row_painter.rect_stroke(
                                        row_rect.shrink(1.0),
                                        0.0,
                                        Stroke::new(1.0, color),
                                        StrokeKind::Inside,
                                    );
                                }
                                let marker = format!(
                                    "{} {:02}{}{}",
                                    if selected { ">" } else { " " },
                                    index + 1,
                                    if playing { "*" } else { " " },
                                    if track.lrc.is_some() { "L" } else { " " }
                                );
                                row_painter.text(
                                    Pos2::new(row_rect.left() + 12.0, row_rect.center().y),
                                    Align2::LEFT_CENTER,
                                    marker,
                                    mono(13.0),
                                    if selected {
                                        theme::bg()
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
                                        theme::bg()
                                    } else {
                                        theme::text()
                                    },
                                );

                                let meta = if selected && self.duration > 0.0 {
                                    format!(
                                        "{} {}",
                                        extension_label(&track.path),
                                        short_time(self.duration)
                                    )
                                } else {
                                    extension_label(&track.path)
                                };
                                row_painter.text(
                                    Pos2::new(row_rect.right() - 12.0, row_rect.center().y),
                                    Align2::RIGHT_CENTER,
                                    meta,
                                    mono(12.0),
                                    if selected {
                                        theme::mix(theme::bg(), theme::text_hi(), 0.22)
                                    } else {
                                        theme::text_dim()
                                    },
                                );

                                if response.double_clicked() {
                                    actions.push(Action::Select(index));
                                    actions.push(Action::Play(index));
                                } else if response.clicked() {
                                    actions.push(Action::Select(index));
                                }
                                let response = response.on_hover_cursor(CursorIcon::PointingHand);

                                egui::Popup::context_menu(&response)
                                    .frame(
                                        Frame::NONE
                                            .fill(theme::bg())
                                            .stroke(Stroke::new(1.0, theme::text_hi()))
                                            .inner_margin(0.0),
                                    )
                                    .width(tokens::MENU_W)
                                    .show(|ui| {
                                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                                        ui.set_width(tokens::MENU_W);
                                        tui_menu_header(ui, &track.title);
                                        ui.add_space(tokens::SPACE_1);
                                        if tui_menu_item(ui, "PLAY", false).clicked() {
                                            actions.push(Action::Play(index));
                                        }
                                        if tui_menu_item(ui, "PLAY NEXT", false).clicked() {
                                            actions.push(Action::QueueNext(index));
                                        }
                                        if tui_menu_item(
                                            ui,
                                            if track.lrc.is_some() {
                                                "REPLACE LRC..."
                                            } else {
                                                "IMPORT LRC..."
                                            },
                                            false,
                                        )
                                        .clicked()
                                        {
                                            actions.push(Action::ImportLrc(index));
                                        }
                                        if track.lrc.is_some()
                                            && tui_menu_item(ui, "CLEAR LRC", false).clicked()
                                        {
                                            actions.push(Action::ClearLrc(index));
                                        }
                                        if tui_menu_item(ui, "OPEN LOCATION", false).clicked() {
                                            actions.push(Action::OpenTrackLocation(index));
                                        }
                                        tui_menu_separator(ui);
                                        if tui_menu_item(ui, "REMOVE FROM LIST", false).clicked() {
                                            actions.push(Action::RemoveTrack(index));
                                        }
                                        if tui_menu_item(ui, "DELETE FILE...", true).clicked() {
                                            actions.push(Action::DeleteTrack(index));
                                        }
                                        ui.add_space(tokens::SPACE_1);
                                    });
                            }
                            if filtered.is_empty() {
                                ui.add_space(tokens::SPACE_3);
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

                // TUI scrollbar: a vertical mirror of the seek bar — a `#` thumb
                // riding a `-` track. It stays faint until the pointer reaches
                // the right edge, then becomes prominent.
                let viewport = output.inner_rect;
                let content_h = output.content_size.y;
                let max_offset = (content_h - viewport.height()).max(0.0);
                if max_offset > 0.5 {
                    let bar_w = 14.0;
                    let bar_x = viewport.right() - bar_w * 0.5;
                    let cell_h = 13.0;
                    let track_top = viewport.top();
                    let track_h = viewport.height().max(cell_h);
                    let cells = (track_h / cell_h).floor().max(1.0) as usize;
                    let pitch = track_h / cells as f32;
                    let offset = output.state.clamped_offset().y.clamp(0.0, max_offset);
                    let frac = offset / max_offset;
                    let thumb_cells = ((viewport.height() / content_h) * cells as f32)
                        .round()
                        .clamp(1.0, cells as f32) as usize;
                    let thumb_start = ((cells - thumb_cells) as f32 * frac).round() as usize;
                    let thumb_h = thumb_cells as f32 * pitch;
                    let thumb_rect = Rect::from_min_size(
                        Pos2::new(bar_x - 2.0, track_top + thumb_start as f32 * pitch),
                        Vec2::new(4.0, thumb_h),
                    );
                    let thumb_hit = Rect::from_min_size(
                        Pos2::new(bar_x - bar_w * 0.5, thumb_rect.top()),
                        Vec2::new(bar_w, thumb_h),
                    );
                    let track_hit = Rect::from_min_size(
                        Pos2::new(bar_x - bar_w * 0.5, viewport.top()),
                        Vec2::new(bar_w, viewport.height()),
                    );
                    let track_response = ui.interact(
                        track_hit,
                        Id::new("tui-track-scrollbar"),
                        Sense::click(),
                    );
                    let thumb_response = ui.interact(
                        thumb_hit,
                        Id::new("tui-track-scrollbar-thumb"),
                        Sense::click_and_drag(),
                    );

                    if thumb_response.hovered() || thumb_response.dragged() {
                        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
                    }
                    let active = thumb_response.dragged() || thumb_response.hovered();
                    let near = active
                        || self.track_scroll_grab.is_some()
                        || ui
                            .input(|input| input.pointer.hover_pos())
                            .is_some_and(|pos| {
                                viewport.expand2(Vec2::new(28.0, 0.0)).contains(pos)
                            });
                    let thumb_color = if active {
                        theme::text_hi()
                    } else if near {
                        theme::text_dim()
                    } else {
                        theme::alpha(theme::text_dim(), 60)
                    };
                    let empty_color =
                        theme::alpha(theme::text_dim(), if near { 110 } else { 35 });
                    let painter = ui.painter().with_clip_rect(viewport);
                    let font = mono(12.0);
                    let current_cell = self.current.and_then(|current| {
                        filtered.iter().position(|&index| index == current).map(|row| {
                            (((row as f32 + 0.5) / filtered.len() as f32) * cells as f32)
                                .floor().min((cells - 1) as f32) as usize
                        })
                    });
                    for i in 0..cells {
                        let is_current = current_cell == Some(i);
                        let in_thumb = i >= thumb_start && i < thumb_start + thumb_cells;
                        painter.text(
                            Pos2::new(bar_x, track_top + (i as f32 + 0.5) * pitch),
                            Align2::CENTER_CENTER,
                            if is_current { "@" } else if in_thumb { "#" } else { "-" },
                            font.clone(),
                            if is_current { theme::text_hi() } else if in_thumb { thumb_color } else { empty_color },
                        );
                    }

                    if thumb_response.drag_started() {
                        let pointer_y = ui
                            .input(|input| input.pointer.interact_pos())
                            .map_or(thumb_rect.top(), |pos| pos.y);
                        self.track_scroll_grab =
                            Some((pointer_y - thumb_rect.top()).clamp(0.0, thumb_h));
                    }
                    if thumb_response.dragged() {
                        if let Some(pointer) = ui.input(|input| input.pointer.interact_pos()) {
                            let grab = self.track_scroll_grab.unwrap_or(thumb_h * 0.5);
                            let travel = (track_h - thumb_h).max(1.0);
                            let frac = ((pointer.y - grab - track_top) / travel)
                                .clamp(0.0, 1.0);
                            self.track_scroll_target = Some(frac * max_offset);
                        }
                    }
                    if thumb_response.drag_stopped() {
                        self.track_scroll_grab = None;
                    }
                    if track_response.clicked() && !thumb_response.hovered() {
                        if let Some(pointer) = ui.input(|input| input.pointer.interact_pos()) {
                            let travel = (track_h - thumb_h).max(1.0);
                            let frac = ((pointer.y - track_top - thumb_h * 0.5)
                                / travel)
                                .clamp(0.0, 1.0);
                            self.track_scroll_target = Some(frac * max_offset);
                        }
                    }
                }
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
                let elapsed = short_time(self.position());
                let total = short_time(self.duration());

                if self.piano_mode {
                    crate::viz::paint_piano_style(
                        painter,
                        rect,
                        &self.piano_history,
                        self.offline_roll_notes(),
                        &self.analyzer.pitch().levels,
                        self.position(),
                        self.playing && !self.paused,
                    );
                    let state = if self.playing && !self.paused {
                        "[PLAYING]"
                    } else if self.paused {
                        "[PAUSED]"
                    } else {
                        "[STANDBY]"
                    };
                    let analysis_state = match self.notes_state {
                        NotesState::Running => "  [ANALYZING]",
                        NotesState::Ready => {
                            let low = self.offline_notes.as_ref().map_or(true, |analysis| {
                                analysis.notes.len() < 8 || analysis.avg_confidence < 0.24
                            });
                            if low {
                                "  [LOW CONF]"
                            } else {
                                "  [READY]"
                            }
                        }
                        NotesState::Failed => "  [NO NOTES]",
                        NotesState::Idle => "",
                    };
                    let key = self
                        .offline_notes
                        .as_ref()
                        .and_then(|analysis| analysis.key.as_ref())
                        .map(|key| format!("  {}", key.label()))
                        .unwrap_or_default();
                    painter.text(
                        Pos2::new(rect.left() + 18.0, rect.top() + TOP_LABEL_Y),
                        Align2::LEFT_CENTER,
                        format!("PIANO (DEV)  {state}{analysis_state}{key}"),
                        mono(10.0),
                        theme::text_dim(),
                    );
                    let status = format!("{elapsed} / {total}");
                    painter.text(
                        Pos2::new(rect.right() - 18.0, rect.top() + TOP_LABEL_Y),
                        Align2::RIGHT_CENTER,
                        status,
                        mono(10.0),
                        theme::text_dim(),
                    );
                } else {
                    if self.show_bg_spectrum {
                        if self.braille_stream {
                            crate::viz::paint_dot_stream(painter, rect, self.analyzer.bars());
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
                            self.analyzer.peaks(),
                            self.peak_hold,
                            self.playing && !self.paused,
                            rotation,
                            self.radial_scale,
                            self.led_bars,
                            self.dot_style(),
                        );
                    }

                    painter.text(
                        Pos2::new(rect.left() + 18.0, rect.top() + TOP_LABEL_Y),
                        Align2::LEFT_CENTER,
                        "SPECTRUM",
                        mono(12.0),
                        theme::text_dim(),
                    );
                    painter.text(
                        Pos2::new(rect.right() - 18.0, rect.top() + TOP_LABEL_Y),
                        Align2::RIGHT_CENTER,
                        "FFT 1024",
                        mono(12.0),
                        theme::text_dim(),
                    );

                    if self.show_lyrics {
                        self.paint_lyrics(painter, rect);
                    }

                    let status_rect = crate::viz::paint_core(
                        painter,
                        rect,
                        self.visual,
                        self.playing && !self.paused,
                        self.paused,
                        self.dot_style(),
                        self.show_core_pulse,
                        self.core_beat_age,
                        self.core_ring_strength,
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

                let mode_text = if self.next_queue.is_empty() {
                    format!("MODE: {}", self.play_mode.label())
                } else {
                    format!(
                        "MODE: {}   NEXT {:02}",
                        self.play_mode.label(),
                        self.next_queue.len()
                    )
                };
                let meta_font = mono(12.0);
                let meta_prefix = format!(
                    "{}   {:.1}K   ",
                    self.format_label,
                    self.sample_rate as f32 / 1000.0
                );
                let prefix_w = painter
                    .layout_no_wrap(meta_prefix.clone(), meta_font.clone(), theme::text())
                    .size()
                    .x;
                // Keep the whole metadata line inside the title width so long
                // device names are not cut down to the old 34-character cap.
                let output_room = (title_max_w - prefix_w - 12.0).max(80.0);
                let output = if self.current.is_some() {
                    elide_to_width(&painter, &self.output_label, meta_font.clone(), output_room)
                } else {
                    "NO OUTPUT".to_owned()
                };
                painter.text(
                    Pos2::new(inner.left(), inner.top() + 30.0),
                    Align2::LEFT_CENTER,
                    format!("{meta_prefix}{output}"),
                    meta_font,
                    theme::text_dim(),
                );
                painter.text(
                    Pos2::new(inner.right(), inner.top() + 30.0),
                    Align2::RIGHT_CENTER,
                    mode_text,
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
                // The bar includes one cell for "[" and one for "]"; map clicks
                // to the actual "#/-" track, not the whole bracket rectangle.
                let track_left = bar_rect.left() + cell_w;
                let track_width = (cell_w * cells as f32).max(1.0);
                if seek.dragged() {
                    if let Some(pos) = seek.interact_pointer_pos() {
                        let fraction = ((pos.x - track_left) / track_width).clamp(0.0, 1.0);
                        self.seek_preview = Some(fraction);
                    }
                }
                if seek.drag_stopped() {
                    if let Some(fraction) = self.seek_preview.take() {
                        actions.push(Action::Seek(fraction as f64));
                    }
                } else if seek.clicked() {
                    if let Some(pos) = seek.interact_pointer_pos() {
                        let fraction = ((pos.x - track_left) / track_width).clamp(0.0, 1.0);
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
                    "VOL [",
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
                    let button = Rect::from_min_size(
                        Pos2::new(x, button_y - tokens::CONTROL_H * 0.5),
                        Vec2::new(width, tokens::CONTROL_H),
                    );
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
                    .inner_margin(tokens::WINDOW_PAD),
            )
            .show(ctx, |ui| {
                ui.set_width(tokens::EQ_W);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("GRAPHIC EQUALIZER")
                            .font(mono(22.0))
                            .color(theme::text_hi()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if close_x_button(ui).clicked() {
                            open = false;
                        }
                    });
                });
                ui.add_space(tokens::SPACE_2);

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

                ui.add_space(tokens::SPACE_3);
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
                        painter.rect_filled(rect, tokens::RADIUS, color);
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

                ui.add_space(tokens::SPACE_2);
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
                    .inner_margin(tokens::WINDOW_PAD),
            )
            .show(ctx, |ui| {
                ui.set_width(tokens::DOWNLOAD_W);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if close_x_button(ui).clicked() {
                            open = false;
                        }
                    });
                });
                ui.add_space(tokens::HAIRLINE);
                ui.label(
                    RichText::new("Download & Decode")
                        .font(mono(24.0))
                        .color(theme::text_hi()),
                );
                ui.add_space(tokens::SPACE_3);

                // Near-inverse block draws the eye to the only required input.
                let input_fill = theme::mix(theme::text_hi(), theme::bg(), 0.08);
                let input_ink = theme::bg_deep();
                let input_hint = theme::mix(input_ink, input_fill, 0.45);
                let response = Frame::NONE
                    .fill(input_fill)
                    .inner_margin(egui::Margin::symmetric(
                        tokens::SPACE_2 as i8,
                        tokens::SPACE_2 as i8,
                    ))
                    .show(ui, |ui| {
                        ui.scope(|ui| {
                            let visuals = ui.visuals_mut();
                            visuals.weak_text_color = Some(input_hint);
                            visuals.text_cursor.stroke.color = input_ink;
                            visuals.selection.bg_fill = theme::alpha(input_ink, 64);
                            ui.add_sized(
                                [ui.available_width(), tokens::FIELD_H],
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
                ui.add_space(tokens::SPACE_2);

                // Optional proxy. A proxy resolves the host itself, so this is
                // also the escape hatch when the system DNS keeps failing.
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("PROXY")
                            .font(mono(10.0))
                            .color(theme::text_dim()),
                    );
                    let env = self.env_proxy.clone();
                    let reserved = if env.is_some() && self.proxy.trim().is_empty() {
                        56.0
                    } else {
                        0.0
                    };
                    let width = (ui.available_width() - reserved).max(80.0);
                    let changed = ui
                        .add_sized(
                            [width, tokens::FIELD_H],
                            TextEdit::singleline(&mut self.proxy)
                                .hint_text("DIRECT / 127.0.0.1:7890")
                                .font(mono(11.0))
                                .text_color(theme::text())
                                .frame(Frame::NONE),
                        )
                        .changed();
                    if changed {
                        self.dirty = true;
                    }
                    if let Some(env) = env {
                        if self.proxy.trim().is_empty() && tui_button(ui, "[ ENV ]", 52.0).clicked()
                        {
                            self.proxy = env;
                            self.dirty = true;
                        }
                    }
                });
                ui.add_space(tokens::SPACE_3);

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
                    ui.add_space(tokens::SPACE_2);
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
                    ui.add_space(tokens::SPACE_1);
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
                ui.add_space(tokens::SPACE_2);
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
                    RichText::new("AAC / M4A  |  FFMPEG OPTIONAL")
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

    fn auto_status(&self) -> String {
        let Some(track) = self.track_mood else {
            let state = match self.beat_analysis {
                Some(_) => "ANALYSING TRACK",
                None => "NO TRACK ANALYSED",
            };
            return format!("{state} / KEEPING THE LAST LOOK");
        };
        let percent = |value: f32| (value * 100.0).round() as i32;
        format!(
            "TRACK: {} / TONE {:+}% / DRIVE {:+}% / PULSE {:+}% / CLARITY {:+}%",
            track.kind.label(),
            percent(track.tone),
            percent(track.drive),
            percent(track.pulse),
            percent(track.clarity)
        )
    }

    fn theme_page(&mut self, ui: &mut Ui, ctx: &Context) {
        if self.theme_editor.is_some() {
            self.theme_editor_page(ui, ctx);
            return;
        }
        let auto = self.auto_dark;
        let auto_mood = self.auto_preview_mood();
        let current = match auto {
            Some(_) => None,
            None => self.themes.find(&self.theme_name),
        };
        let current_entry = current.and_then(|index| self.themes.entry(index));
        let category = current_entry.map_or("-", |entry| entry.category.label());
        let current_custom = current_entry.is_some_and(|entry| entry.spec.is_some());
        let editable = self.themes.editable();
        let mut action = None;

        ui.label(
            RichText::new("THEME")
                .font(mono(24.0))
                .color(theme::text_hi()),
        );
        ui.add_space(tokens::SPACE_1);
        let current_line = match auto {
            Some(dark) => format!(
                "CURRENT: AUTO {} / {}",
                if dark { "DARK" } else { "LIGHT" },
                auto_mood.kind.label()
            ),
            None => format!("CURRENT: {} / {category}", self.theme_name),
        };
        ui.label(
            RichText::new(current_line)
                .font(mono(12.0))
                .color(theme::text_dim()),
        );
        if auto.is_some() {
            ui.label(
                RichText::new(self.auto_status())
                    .font(mono(12.0))
                    .color(theme::text_dim()),
            );
        }
        ui.add_space(tokens::SPACE_2);
        ui.horizontal(|ui| {
            if editable {
                match current {
                    None if auto.is_some() => {
                        if tui_button(ui, "[ CUSTOMIZE ]", 120.0).clicked() {
                            action = Some(ThemeAction::CopyAuto);
                        }
                    }
                    Some(index) if current_custom => {
                        if tui_button(ui, "[ EDIT ]", 96.0).clicked() {
                            action = Some(ThemeAction::Edit(index));
                        }
                    }
                    Some(index) => {
                        if tui_button(ui, "[ CUSTOMIZE ]", 120.0).clicked() {
                            action = Some(ThemeAction::Copy(index));
                        }
                    }
                    None => {}
                }
            }
            if tui_button(ui, "[ RELOAD ]", 96.0).clicked() {
                action = Some(ThemeAction::Reload);
            }
        });
        if let Some(error) = self.themes.load_error() {
            ui.label(
                RichText::new(format!("THEMES.JSON ERROR / {error}"))
                    .font(mono(12.0))
                    .color(theme::danger()),
            );
            ui.label(
                RichText::new("FIX THE FILE, THEN RELOAD")
                    .font(mono(12.0))
                    .color(theme::text_dim()),
            );
        }
        let warnings = self.themes.warnings();
        if let Some(first) = warnings.first() {
            let more = match warnings.len() {
                1 => String::new(),
                count => format!(" (+{} MORE)", count - 1),
            };
            ui.label(
                RichText::new(format!("SKIPPED {first}{more}"))
                    .font(mono(12.0))
                    .color(theme::danger()),
            );
        }
        ui.add_space(tokens::SPACE_3);

        let gap = ui.spacing().item_spacing.x;
        let chip_w = ((ui.available_width() - gap * 3.0) / 4.0).max(80.0);

        ui.label(
            RichText::new("AUTO")
                .font(mono(12.0))
                .color(theme::text_dim()),
        );
        ui.add_space(tokens::SPACE_1);
        ui.horizontal(|ui| {
            for (label, dark) in [("AUTO DARK", true), ("AUTO LIGHT", false)] {
                let look = auto_theme::look(auto_mood, dark);
                if theme_chip(ui, label, chip_w, &look, auto == Some(dark)).clicked() {
                    action = Some(ThemeAction::Auto(dark));
                }
            }
        });
        if auto.is_none() {
            ui.label(
                RichText::new("GRADIENT BY SONG TYPE, READ FROM THE TRACK'S SOUND")
                    .font(mono(12.0))
                    .color(theme::text_dim()),
            );
        }
        ui.add_space(tokens::SPACE_3);

        let entries = self.themes.entries();
        for category in Category::ALL {
            let members: Vec<usize> = (0..entries.len())
                .filter(|&index| entries[index].category == category)
                .collect();
            if members.is_empty() && category != Category::Custom {
                continue;
            }
            ui.label(
                RichText::new(category.label())
                    .font(mono(12.0))
                    .color(theme::text_dim()),
            );
            ui.add_space(tokens::SPACE_1);
            // Dark and light variants get their own rows unless one row holds all.
            let (dark, light): (Vec<usize>, Vec<usize>) = members
                .iter()
                .partition(|&&index| entries[index].look.palette.dark);
            let rows: Vec<&[usize]> = if members.len() <= 4 {
                vec![&members[..]]
            } else {
                dark.chunks(4).chain(light.chunks(4)).collect()
            };
            for row in rows.into_iter().filter(|row| !row.is_empty()) {
                ui.horizontal(|ui| {
                    for &index in row {
                        let entry = &entries[index];
                        if theme_chip(ui, &entry.name, chip_w, &entry.look, current == Some(index))
                            .clicked()
                        {
                            action = Some(ThemeAction::Select(index));
                        }
                    }
                });
            }
            if category == Category::Custom {
                if members.is_empty() {
                    ui.label(
                        RichText::new("NONE YET / CUSTOMIZE COPIES THE CURRENT THEME")
                            .font(mono(12.0))
                            .color(theme::text_dim()),
                    );
                }
                ui.add_space(tokens::SPACE_1);
                path_row(ui, "FILE", &ThemeLibrary::path());
            } else {
                ui.add_space(tokens::SPACE_3);
            }
        }

        if let Some(action) = action {
            self.theme_action(action, ctx);
        }
    }

    fn theme_editor_page(&mut self, ui: &mut Ui, ctx: &Context) {
        let Some(mut editor) = self.theme_editor.take() else {
            return;
        };
        let Some(spec) = self.themes.spec(editor.spec).cloned() else {
            return;
        };
        let entry = self
            .themes
            .entries()
            .iter()
            .position(|entry| entry.spec == Some(editor.spec));
        let resolved_dark = entry
            .and_then(|index| self.themes.entry(index))
            .map(|entry| entry.look.palette.dark);
        let mut edits = Vec::new();
        let mut action = None;

        ui.horizontal(|ui| {
            if tui_button(ui, "[ < BACK ]", 96.0).clicked() {
                action = Some(ThemeAction::Back);
            }
            ui.add_space(tokens::SPACE_2);
            ui.label(
                RichText::new("CUSTOM THEME / CHANGES APPLY LIVE")
                    .font(mono(12.0))
                    .color(theme::text_dim()),
            );
        });
        ui.add_space(tokens::SPACE_3);

        ui.horizontal(|ui| {
            theme_key(ui, "NAME");
            let response = text_field(ui, &mut editor.name, 150.0, NAME_MAX);
            let name = editor.name.trim().to_owned();
            let changed = name != spec.name.trim();
            let free = self.themes.name_free(&name, Some(editor.spec));
            if response.lost_focus() {
                if changed && free {
                    edits.push(ThemeEdit::Rename(name.clone()));
                    editor.name = name;
                } else {
                    editor.name = spec.name.trim().to_owned();
                }
            } else if changed && !free {
                ui.add_space(tokens::SPACE_2);
                ui.label(
                    RichText::new(if name.is_empty() { "EMPTY" } else { "IN USE" })
                        .font(mono(12.0))
                        .color(theme::danger()),
                );
            }
        });

        let count = spec.background.len();
        ui.horizontal(|ui| {
            theme_key(ui, "BACKGROUND");
            for (option, label) in [(1, "SOLID"), (2, "2-COLOR"), (4, "4-CORNER")] {
                if tui_button(ui, &toggle_label(label, count == option), 96.0).clicked()
                    && count != option
                {
                    edits.push(ThemeEdit::BackgroundCount(option));
                }
            }
        });
        // Fields sit where their colour lands on screen; BLEND runs from the
        // top-left to the bottom-right corner.
        let rows: &[(&str, &[usize])] = match count {
            1 => &[("COLOR", &[0])],
            2 => &[("BLEND", &[0, 1])],
            _ => &[("TOP", &[0, 1]), ("BOTTOM", &[3, 2])],
        };
        for &(key, slots) in rows {
            ui.horizontal(|ui| {
                theme_key(ui, key);
                for (position, &slot) in slots.iter().enumerate() {
                    let Some(&Hex(current)) = spec.background.get(slot) else {
                        continue;
                    };
                    if position > 0 {
                        ui.add_space(tokens::SPACE_3);
                    }
                    if let Some(rgb) = hex_field(ui, &mut editor.background[slot], current) {
                        edits.push(ThemeEdit::Background(slot, rgb));
                    }
                }
            });
        }

        ui.horizontal(|ui| {
            theme_key(ui, "ACCENT");
            if let Some(rgb) = hex_field(ui, &mut editor.accent, spec.accent.0) {
                edits.push(ThemeEdit::Accent(rgb));
            }
            ui.add_space(tokens::SPACE_3);
            ui.label(
                RichText::new("TINTS SPECTRUM + TEXT")
                    .font(mono(12.0))
                    .color(theme::text_dim()),
            );
        });

        let tint = spec.tint();
        ui.horizontal(|ui| {
            theme_key(ui, "TINT");
            if tui_button(ui, "[-]", 34.0).clicked() {
                edits.push(ThemeEdit::Tint(step_tint(tint, -1.0)));
            }
            ui.label(
                RichText::new(format!("{:>3}%", (tint * 100.0).round() as i32))
                    .font(mono(12.0))
                    .color(theme::text()),
            );
            if tui_button(ui, "[+]", 34.0).clicked() {
                edits.push(ThemeEdit::Tint(step_tint(tint, 1.0)));
            }
            ui.add_space(tokens::SPACE_3);
            ui.label(
                RichText::new("ACCENT SHARE IN SPECTRUM")
                    .font(mono(12.0))
                    .color(theme::text_dim()),
            );
        });

        ui.horizontal(|ui| {
            theme_key(ui, "MODE");
            for (option, label) in [(None, "AUTO"), (Some(true), "DARK"), (Some(false), "LIGHT")] {
                if tui_button(ui, &toggle_label(label, spec.dark == option), 80.0).clicked()
                    && spec.dark != option
                {
                    edits.push(ThemeEdit::Dark(option));
                }
            }
            if let (None, Some(dark)) = (spec.dark, resolved_dark) {
                ui.label(
                    RichText::new(if dark { "= DARK" } else { "= LIGHT" })
                        .font(mono(12.0))
                        .color(theme::text_dim()),
                );
            }
        });

        let overrides = spec.colors.count();
        if overrides > 0 {
            ui.horizontal(|ui| {
                theme_key(ui, "OVERRIDES");
                ui.label(
                    RichText::new(format!("{overrides} SET IN JSON"))
                        .font(mono(12.0))
                        .color(theme::text()),
                );
                ui.add_space(tokens::SPACE_2);
                if tui_button(ui, "[ CLEAR ]", 80.0).clicked() {
                    edits.push(ThemeEdit::ClearOverrides);
                }
            });
        }

        ui.add_space(tokens::SPACE_3);
        ui.horizontal(|ui| {
            if let Some(index) = entry {
                if tui_button(ui, "[ DUPLICATE ]", 112.0).clicked() {
                    action = Some(ThemeAction::Copy(index));
                }
            }
            if editor.delete_armed {
                if tui_button_ink(ui, "[ CONFIRM DELETE ]", 150.0, theme::danger()).clicked() {
                    action = Some(ThemeAction::Delete(editor.spec));
                }
                if tui_button(ui, "[ KEEP ]", 80.0).clicked() {
                    editor.delete_armed = false;
                }
            } else if tui_button_ink(ui, "[ DELETE ]", 96.0, theme::danger()).clicked() {
                editor.delete_armed = true;
            }
        });

        self.theme_editor = Some(editor);
        let resync = edits
            .iter()
            .any(|edit| matches!(edit, ThemeEdit::BackgroundCount(_) | ThemeEdit::ClearOverrides));
        for edit in edits {
            self.edit_theme(edit, ctx);
        }
        if resync {
            self.sync_theme_editor();
        }
        if let Some(action) = action {
            self.theme_action(action, ctx);
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
                        .inner_margin(tokens::WINDOW_PAD),
                )
                .show(ctx, |ui| {
                    ui.set_width(tokens::SETTINGS_W);
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
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if close_x_button(ui).clicked() {
                                open = false;
                            }
                        });
                    });
                    ui.add_space(tokens::SPACE_3);
                    egui::ScrollArea::vertical()
                        .max_height(tokens::SETTINGS_BODY_H)
                        .min_scrolled_height(tokens::SETTINGS_BODY_H)
                        .auto_shrink([false, false])
                        .scroll_bar_visibility(
                            egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden,
                        )
                        .show(ui, |ui| {
                            ui.set_width(tokens::SETTINGS_W);
                            match self.settings_page {
                                SettingsPage::System => {
                                    ui.label(
                                        RichText::new("SONADIAL PLAYER")
                                            .font(mono(24.0))
                                            .color(theme::text_hi()),
                                    );
                                    ui.add_space(tokens::SPACE_1);
                                    ui.label(
                                        RichText::new(format!(
                                            "v0.1 / {} TRACKS / {}",
                                            self.playlist.len(),
                                            self.play_mode.label()
                                        ))
                                        .font(mono(12.0))
                                        .color(theme::text_dim()),
                                    );
                                    ui.add_space(tokens::SPACE_3);
                                    info_row(ui, "OUTPUT", &self.output_label);
                                    ui.add_space(tokens::HAIRLINE);
                                    path_row(ui, "STATE", &storage::state_path());
                                    ui.add_space(tokens::SPACE_3);
                                    ui.label(
                                        RichText::new("SHORTCUTS")
                                            .font(mono(12.0))
                                            .color(theme::text_dim()),
                                    );
                                    ui.add_space(tokens::SPACE_1);
                                    for (keys, desc) in [
                                        ("SPACE", "播放 / 暂停"),
                                        ("UP / DOWN", "音量 + / -"),
                                        ("LEFT / RIGHT", "后退 / 前进 5 秒"),
                                        ("PGUP / PGDN", "上一首 / 下一首"),
                                        ("CTRL + LEFT/RIGHT", "上一首 / 下一首"),
                                        ("CTRL + O", "添加音频"),
                                        ("RIGHT-CLICK", "队列 / 导入 LRC / 删除"),
                                        ("ESC", "关闭面板"),
                                    ] {
                                        ui.horizontal(|ui| {
                                            ui.add_sized(
                                                [150.0, tokens::LINE_H],
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
                                    ui.add_space(tokens::SPACE_3);
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
                                    ui.add_space(tokens::SPACE_3);
                                    let gap = ui.spacing().item_spacing.x;
                                    let toggle_w = ((ui.available_width() - gap) * 0.5).max(120.0);
                                    let style_w =
                                        ((ui.available_width() - gap * 2.0) / 3.0).max(88.0);
                                    ui.label(
                                        RichText::new("MODE")
                                            .font(mono(12.0))
                                            .color(theme::text_dim()),
                                    );
                                    ui.add_space(tokens::SPACE_1);
                                    let dot_style = self.dot_style();
                                    ui.horizontal(|ui| {
                                        if tui_button(
                                            ui,
                                            &toggle_label(
                                                "DEFAULT",
                                                !self.piano_mode && !dot_style,
                                            ),
                                            style_w,
                                        )
                                        .clicked()
                                        {
                                            self.set_piano_mode(false);
                                            self.set_dot_style(false);
                                        }
                                        if tui_button(
                                            ui,
                                            &toggle_label(
                                                "DOT MATRIX",
                                                !self.piano_mode && dot_style,
                                            ),
                                            style_w,
                                        )
                                        .clicked()
                                        {
                                            self.set_piano_mode(false);
                                            self.set_dot_style(true);
                                        }
                                        if tui_button(
                                            ui,
                                            &toggle_label("PIANO (DEV)", self.piano_mode),
                                            style_w,
                                        )
                                        .clicked()
                                        {
                                            self.set_piano_mode(true);
                                        }
                                    });
                                    ui.add_space(tokens::SPACE_3);
                                    ui.label(
                                        RichText::new("LAYERS")
                                            .font(mono(12.0))
                                            .color(theme::text_dim()),
                                    );
                                    ui.add_space(tokens::SPACE_1);
                                    ui.horizontal(|ui| {
                                        if tui_button(
                                            ui,
                                            &toggle_label("BG SPECTRUM", self.show_bg_spectrum),
                                            toggle_w,
                                        )
                                        .clicked()
                                        {
                                            self.show_bg_spectrum = !self.show_bg_spectrum;
                                            self.dirty = true;
                                        }
                                        if tui_button(
                                            ui,
                                            &toggle_label("RADIAL", self.show_radial),
                                            toggle_w,
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
                                            toggle_w,
                                        )
                                        .clicked()
                                        {
                                            self.show_core_pulse = !self.show_core_pulse;
                                            self.dirty = true;
                                        }
                                        if tui_button(
                                            ui,
                                            &toggle_label("LYRICS", self.show_lyrics),
                                            toggle_w,
                                        )
                                        .clicked()
                                        {
                                            self.show_lyrics = !self.show_lyrics;
                                            self.dirty = true;
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        if tui_button(
                                            ui,
                                            &toggle_label("PEAK HOLD", self.peak_hold),
                                            toggle_w,
                                        )
                                        .clicked()
                                        {
                                            self.peak_hold = !self.peak_hold;
                                            self.dirty = true;
                                        }
                                    });
                                    ui.add_space(tokens::SPACE_3);
                                    ui.label(
                                        RichText::new("PARAMETERS")
                                            .font(mono(12.0))
                                            .color(theme::text_dim()),
                                    );
                                    ui.add_space(tokens::SPACE_1);
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
                                SettingsPage::Theme => self.theme_page(ui, ctx),
                            }
                        });
                });
            if !open {
                self.show_settings = false;
                // Drop half-typed text so reopening shows the saved values.
                self.sync_theme_editor();
            }
        }

        if self.show_eq {
            self.eq_window(ctx);
        }
        if self.show_download {
            self.download_window(ctx);
        }

        if let Some(path) = self.pending_delete.clone() {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            let path_text = path.display().to_string();
            let mut confirm = false;
            let mut cancel = false;
            let modal = egui::Modal::new(Id::new("delete-track-modal"))
                .frame(
                    Frame::NONE
                        .fill(theme::bg())
                        .stroke(Stroke::new(1.0, theme::danger()))
                        .inner_margin(tokens::WINDOW_PAD),
                )
                .backdrop_color(theme::alpha(theme::bg_deep(), 145))
                .show(ctx, |ui| {
                    ui.set_width(tokens::DIALOG_W);
                    ui.label(
                        RichText::new("REMOVE TRACK FROM DISK?")
                            .font(mono(22.0))
                            .color(theme::text_hi()),
                    );
                    ui.add_space(tokens::SPACE_2);
                    let fitted =
                        elide_to_width(ui.painter(), &name, mono(13.0), ui.available_width());
                    ui.label(
                        RichText::new(fitted)
                            .font(mono(13.0))
                            .color(theme::text_hi()),
                    );
                    let fitted_path =
                        elide_to_width(ui.painter(), &path_text, mono(10.0), ui.available_width());
                    ui.label(
                        RichText::new(fitted_path)
                            .font(mono(10.0))
                            .color(theme::text_dim()),
                    );
                    ui.add_space(tokens::SPACE_3);
                    ui.label(
                        RichText::new("THIS CANNOT BE UNDONE.")
                            .font(mono(12.0))
                            .color(theme::danger()),
                    );
                    ui.add_space(tokens::SPACE_4);
                    ui.horizontal(|ui| {
                        if tui_button(ui, "[ CANCEL ]", 100.0).clicked() {
                            cancel = true;
                        }
                        if tui_button(ui, "[ DELETE ]", 100.0).clicked() {
                            confirm = true;
                        }
                    });
                });
            if modal.should_close() {
                cancel = true;
            }
            if cancel {
                actions.push(Action::CancelDeleteTrack);
            } else if confirm {
                actions.push(Action::ConfirmDeleteTrack);
            }
        }

        if let Some(error) = &self.error {
            let mut dismiss = false;
            egui::Area::new(Id::new("error-toast"))
                .anchor(
                    Align2::RIGHT_BOTTOM,
                    Vec2::new(-tokens::SPACE_5, -DOCK_H - tokens::LINE_H),
                )
                .show(ctx, |ui| {
                    Frame::NONE
                        .fill(theme::bg())
                        .stroke(Stroke::new(1.0, theme::text_hi()))
                        .inner_margin(tokens::SPACE_3)
                        .show(ui, |ui| {
                            ui.set_max_width(tokens::TOAST_W);
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
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // eframe keeps logic ticking while minimized/occluded, without drawing UI.
        self.tick(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
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

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // The glyph atlas width follows the GPU's maximum texture side. On a
        // 16k-capable driver that makes every glyph row 64 KiB, and the 80%
        // auto-clear threshold is never reached. This app uploads no other
        // textures, so a smaller cap keeps the atlas small and clearable.
        raw_input.max_texture_side = Some(2048);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(cancel) = &self.download_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        self.cancel_beat_analysis();
        self.cancel_note_analysis();
        self.save_state();
    }
}

fn entry_for_path(path: PathBuf, lrc: Option<PathBuf>) -> TrackEntry {
    let title = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let search_index = crate::search::SearchIndex::new(&format!("{}\n{}", title, path.to_string_lossy()));
    TrackEntry { path, title, lrc, search_index }
}

fn resolve_lrc(path: &Path, lrc_map: &HashMap<String, PathBuf>) -> Option<PathBuf> {
    let key = path.to_string_lossy();
    lrc_map
        .get(key.as_ref())
        .filter(|lrc| lrc.is_file())
        .cloned()
        .or_else(|| find_sidecar_lrc(path))
}

fn find_sidecar_lrc(path: &Path) -> Option<PathBuf> {
    let candidate = path.with_extension("lrc");
    if candidate.is_file() {
        return Some(candidate);
    }
    let upper = path.with_extension("LRC");
    upper.is_file().then_some(upper)
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

fn is_lyric_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3040..=0x30FF // kana
            | 0x3400..=0x4DBF // CJK extension A
            | 0x4E00..=0x9FFF // CJK unified
            | 0xAC00..=0xD7AF // hangul
            | 0xF900..=0xFAFF // compatibility ideographs
    )
}

fn is_lyric_vowel(c: char) -> bool {
    matches!(
        c,
        'a' | 'e'
            | 'i'
            | 'o'
            | 'u'
            | 'y'
            | 'á'
            | 'à'
            | 'â'
            | 'ä'
            | 'ã'
            | 'å'
            | 'é'
            | 'è'
            | 'ê'
            | 'ë'
            | 'í'
            | 'ì'
            | 'î'
            | 'ï'
            | 'ó'
            | 'ò'
            | 'ô'
            | 'ö'
            | 'õ'
            | 'ú'
            | 'ù'
            | 'û'
            | 'ü'
            | 'ý'
            | 'ÿ'
    )
}

fn latin_syllables(word: &str) -> f64 {
    let mut groups = 0;
    let mut previous_vowel = false;
    for c in word.chars() {
        let vowel = is_lyric_vowel(c);
        if vowel && !previous_vowel {
            groups += 1;
        }
        previous_vowel = vowel;
    }
    if word.ends_with('e') && groups > 1 {
        groups -= 1;
    }
    groups.max(1) as f64
}

/// Approximate sung syllables. Han/kana/hangul count one each; Latin runs are
/// grouped into words and estimated by vowel groups.
fn lyric_units(text: &str) -> f64 {
    let mut units = 0.0;
    let mut word = String::new();
    // Previous English default was 5 syllables/sec. A 20% slower rate is
    // 4 syllables/sec, i.e. 1.25x the Chinese unit weight at 5 units/sec.
    const LATIN_WEIGHT: f64 = 1.25;
    let mut flush_word = |word: &mut String, units: &mut f64| {
        if !word.is_empty() {
            *units += latin_syllables(word) * LATIN_WEIGHT;
            word.clear();
        }
    };

    for c in text.chars() {
        if is_lyric_cjk(c) {
            flush_word(&mut word, &mut units);
            units += 1.0;
        } else if c.is_alphabetic() {
            word.push(c.to_ascii_lowercase());
        } else if c == '\'' || c == '\u{2019}' {
            // Keep contractions together: "don't" -> "dont".
        } else {
            flush_word(&mut word, &mut units);
        }
    }
    flush_word(&mut word, &mut units);
    units.max(1.0)
}

#[allow(clippy::too_many_arguments)]
fn paint_lyric_text(
    painter: &egui::Painter,
    line: Option<&LyricLine>,
    text_x: f32,
    center_y: f32,
    max_width: f32,
    font: FontId,
    color: Color32,
    typed: Option<usize>,
    caret: bool,
) {
    let Some(line) = line else {
        return;
    };
    const CURSOR_W: f32 = 6.0;
    let available = (max_width - if caret { CURSOR_W } else { 0.0 }).max(1.0);
    let full = elide_to_width(painter, &line.text, font.clone(), available);
    if full.is_empty() {
        return;
    }
    let full_len = full.chars().count();
    let visible_len = typed.map(|count| count.min(full_len)).unwrap_or(full_len);
    let visible: String = full.chars().take(visible_len).collect();
    if !visible.is_empty() {
        painter.text(
            Pos2::new(text_x, center_y),
            Align2::LEFT_CENTER,
            visible.clone(),
            font.clone(),
            color,
        );
    }
    if caret && typed.is_some() {
        let typing = visible_len < full_len;
        let blink_on = painter.ctx().input(|input| input.time).fract() < 0.55;
        if typing || blink_on {
            let width = painter
                .layout_no_wrap(visible, font.clone(), color)
                .size()
                .x;
            let cursor = Rect::from_min_size(
                Pos2::new(text_x + width, center_y - 7.0),
                Vec2::new(CURSOR_W, 14.0),
            );
            painter.rect_filled(cursor, 0.0, theme::alpha(color, 210));
        }
    }
}

fn snap_px(size: f32) -> f32 {
    ((size / 12.0).round().max(1.0) * 12.0).clamp(12.0, 48.0)
}

fn ui_font(size: f32) -> FontId {
    FontId::new(snap_px(size), FontFamily::Name("ui".into()))
}

fn lyric_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("lyric".into()))
}

fn mono(size: f32) -> FontId {
    FontId::new(snap_px(size), FontFamily::Monospace)
}

fn title(size: f32) -> FontId {
    FontId::new(snap_px(size), FontFamily::Name("ui".into()))
}

/// Proxy the machine already advertises. The downloader ignores these by
/// default, but offering them keeps the app from having to be configured twice.
fn env_proxy() -> Option<String> {
    ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
        .iter()
        .find_map(|key| std::env::var(key).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn paint_card(painter: &egui::Painter, rect: Rect, _radius: f32, fill: Color32, border: Color32) {
    painter.rect_filled(rect, tokens::RADIUS, fill);
    painter.rect_stroke(
        rect,
        tokens::RADIUS,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
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
            tokens::RADIUS,
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
        painter.rect_filled(rect, tokens::RADIUS, theme::text_hi());
        painter.rect_stroke(
            rect,
            tokens::RADIUS,
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
            painter.rect_filled(rect, tokens::RADIUS, theme::text_hi());
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
                tokens::RADIUS,
                color,
            );
            painter.rect_filled(
                Rect::from_center_size(
                    center + Vec2::new(s * 0.20, 0.0),
                    Vec2::new(s * 0.18, s * 0.72),
                ),
                tokens::RADIUS,
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
                tokens::RADIUS,
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
                    tokens::RADIUS,
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
                tokens::RADIUS,
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
                tokens::RADIUS,
                Stroke::new(stroke.width * 0.8, color),
                StrokeKind::Inside,
            );
            painter.rect_stroke(
                Rect::from_center_size(
                    center + Vec2::new(-s * 0.10, s * 0.10),
                    Vec2::splat(s * 0.52),
                ),
                tokens::RADIUS,
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
                tokens::RADIUS_SM,
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
            [120.0, tokens::LINE_H],
            egui::Label::new(RichText::new(key).font(mono(12.0)).color(theme::text_dim())),
        );
        ui.label(RichText::new(value).font(mono(12.0)).color(theme::text()));
    });
}

fn text_button(ui: &mut Ui, id: &str, rect: Rect, label: &str, size: f32) -> Response {
    let response = ui.interact(rect, Id::new(id), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, tokens::RADIUS, theme::text_hi());
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
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(34.0, tokens::MENU_ITEM_H), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, tokens::RADIUS, theme::text_hi());
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
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, tokens::CONTROL_H), Sense::click());
    let hovered = response.hovered();
    if active || hovered {
        ui.painter()
            .rect_filled(rect, tokens::RADIUS, theme::text_hi());
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

fn theme_chip(ui: &mut Ui, label: &str, width: f32, look: &Look, active: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, tokens::CHIP_H), Sense::click());
    let (fg, dim) = (look.text_hi(), look.text_dim());
    let hovered = response.hovered();
    let painter = ui.painter();
    theme::paint_look(painter, rect, look);
    let label = elide_to_width(painter, label, mono(12.0), width - tokens::SPACE_2 * 2.0);
    let border = if active {
        Stroke::new(2.0, fg)
    } else if hovered {
        Stroke::new(1.0, fg)
    } else {
        Stroke::new(1.0, dim)
    };
    painter.rect_stroke(rect, tokens::RADIUS, border, StrokeKind::Inside);
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(rect.left() + 1.0, rect.bottom() - 3.0),
            Pos2::new(rect.right() - 1.0, rect.bottom() - 1.0),
        ),
        tokens::RADIUS,
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
    tui_button_ink(ui, label, width, theme::text())
}

fn tui_button_ink(ui: &mut Ui, label: &str, width: f32, ink: Color32) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, tokens::CONTROL_H), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, tokens::RADIUS, theme::text_hi());
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        mono(12.0),
        if hovered { theme::bg() } else { ink },
    );
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}

fn theme_key(ui: &mut Ui, key: &str) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(THEME_KEY_W, tokens::CONTROL_H), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        key,
        mono(12.0),
        theme::text_dim(),
    );
}

fn text_field(ui: &mut Ui, text: &mut String, width: f32, limit: usize) -> Response {
    Frame::NONE
        .stroke(Stroke::new(1.0, theme::line()))
        .inner_margin(egui::Margin::symmetric(tokens::SPACE_1 as i8, 0))
        .show(ui, |ui| {
            ui.visuals_mut().selection.bg_fill = theme::alpha(theme::text_hi(), 64);
            ui.add_sized(
                [width, tokens::FIELD_H],
                TextEdit::singleline(text)
                    .font(mono(12.0))
                    .text_color(theme::text_hi())
                    .char_limit(limit)
                    .align(Align2::LEFT_CENTER)
                    .frame(Frame::NONE),
            )
        })
        .inner
}

/// Swatch plus `#RRGGBB` input. Returns the colour once the text turns into a
/// new valid value; on blur the text snaps back to the stored colour.
fn hex_field(ui: &mut Ui, text: &mut String, current: Rgb) -> Option<Rgb> {
    let (swatch, _) = ui.allocate_exact_size(Vec2::splat(tokens::CONTROL_H), Sense::hover());
    ui.painter().rect_filled(
        swatch,
        tokens::RADIUS,
        Color32::from_rgb(current[0], current[1], current[2]),
    );
    ui.painter().rect_stroke(
        swatch,
        tokens::RADIUS,
        Stroke::new(1.0, theme::line()),
        StrokeKind::Inside,
    );
    let response = text_field(ui, text, 64.0, 7);
    let parsed = parse_hex(text);
    if response.lost_focus() {
        *text = format_hex(parsed.unwrap_or(current));
    }
    parsed.filter(|rgb| response.changed() && *rgb != current)
}

fn step_tint(tint: f32, direction: f32) -> f32 {
    // 2 % steps; a hand-set value above the UI range is never pushed down by [+].
    let next = ((tint + 0.02 * direction) * 50.0).round() / 50.0;
    next.clamp(0.0, THEME_TINT_MAX.max(tint))
}

/// Clickable path that opens its folder in Explorer.
fn path_row(ui: &mut Ui, key: &str, path: &Path) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [120.0, tokens::LINE_H],
            egui::Label::new(RichText::new(key).font(mono(12.0)).color(theme::text_dim())),
        );
        let font = ui_font(12.0);
        let shown = path.display().to_string();
        let text = elide_to_width(
            ui.painter(),
            &shown,
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
            .on_hover_text(shown);
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if response.clicked() {
            if let Some(dir) = path.parent() {
                let _ = std::process::Command::new("explorer").arg(dir).spawn();
            }
        }
    });
}

/// Left inset shared by the menu header and item labels so the song title
/// lines up with the commands below it.
const MENU_TEXT_INSET: f32 = 9.0;

#[derive(Clone, Default)]
struct MenuHeaderMarquee {
    title: String,
    start: f64,
    last: f64,
}

fn tui_menu_header(ui: &mut Ui, title: &str) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(width, tokens::MENU_ITEM_H + tokens::SPACE_1),
        Sense::hover(),
    );
    let text_rect = Rect::from_min_max(
        Pos2::new(rect.left() + MENU_TEXT_INSET, rect.top()),
        Pos2::new(rect.right() - MENU_TEXT_INSET, rect.bottom()),
    );
    let painter = ui.painter().with_clip_rect(text_rect);
    // The two leading spaces match the offset of the item labels.
    let label = format!("  {title}");
    let font = mono(12.0);
    let text_w = painter
        .layout_no_wrap(label.clone(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let baseline = Pos2::new(text_rect.left(), rect.center().y);

    if text_w <= text_rect.width() {
        painter.text(baseline, Align2::LEFT_CENTER, label, font, theme::text_hi());
    } else {
        // Long titles scroll like a marquee instead of being cut off.
        const PAUSE: f64 = 1.0;
        const SPEED: f32 = 26.0;
        const GAP: f32 = 40.0;
        let now = ui.ctx().input(|input| input.time);
        let start = ui.ctx().data_mut(|data| {
            let state = data.get_temp_mut_or_default::<MenuHeaderMarquee>(Id::new(
                "tui-menu-header-marquee",
            ));
            // Opening the menu again (or switching songs) restarts the scroll.
            if state.title != title || now - state.last > 0.25 {
                state.title = title.to_owned();
                state.start = now;
            }
            state.last = now;
            state.start
        });
        let travel = text_w + GAP;
        let elapsed = (now - start).max(0.0);
        let offset = if elapsed < PAUSE {
            0.0
        } else {
            ((elapsed - PAUSE) as f32 * SPEED) % travel
        };
        // Draw a second copy one period ahead so the wrap is seamless.
        for shift in [0.0, travel] {
            painter.text(
                Pos2::new(baseline.x - offset + shift, baseline.y),
                Align2::LEFT_CENTER,
                label.clone(),
                font.clone(),
                theme::text_hi(),
            );
        }
        ui.ctx().request_repaint();
    }
    let painter = ui.painter();
    painter.line_segment(
        [
            Pos2::new(rect.left() + 1.0, rect.bottom() - 0.5),
            Pos2::new(rect.right() - 1.0, rect.bottom() - 0.5),
        ],
        Stroke::new(1.0, theme::line()),
    );
}

fn tui_menu_item(ui: &mut Ui, label: &str, danger: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), tokens::MENU_ITEM_H),
        Sense::click(),
    );
    let hovered = response.hovered();
    let painter = ui.painter();
    if hovered {
        painter.rect_filled(rect, tokens::RADIUS, theme::text_hi());
    }
    painter.text(
        Pos2::new(rect.left() + MENU_TEXT_INSET, rect.center().y),
        Align2::LEFT_CENTER,
        if hovered {
            format!("> {label}")
        } else {
            format!("  {label}")
        },
        mono(12.0),
        if hovered {
            theme::bg()
        } else if danger {
            theme::danger()
        } else {
            theme::text()
        },
    );
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}

fn tui_menu_separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 7.0), Sense::hover());
    let painter = ui.painter();
    painter.line_segment(
        [
            Pos2::new(rect.left() + 8.0, rect.center().y),
            Pos2::new(rect.right() - 8.0, rect.center().y),
        ],
        Stroke::new(1.0, theme::line()),
    );
}
