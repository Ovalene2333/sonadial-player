// Slint-UI orchestrator (slint-ui branch). Replaces the egui layer: owns the
// audio engine and spectrum analyzer, feeds the .slint window at ~60 fps and
// reuses every backend module unchanged (audio/beat/lyrics/theme/...).
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, VecModel};

use crate::audio::AudioEngine;
use crate::auto_theme::{self, Fade, Mood};
use crate::beat_config::BeatConfig;
use crate::lyrics::{self, LyricLine};
use crate::sound_profile::SoundProfile;
use crate::spectrum::{SpectrumEngine, VisualState, MAX_DB_VALUE};
use crate::storage::{self, PersistedState};
use crate::theme::{self, Look, Rgb};
use crate::theme_library::ThemeLibrary;

slint::include_modules!();

const BEAT_VISUAL_LEAD: f64 = 0.025;
const SAVE_EVERY: Duration = Duration::from_secs(2);
const NUM_BARS: usize = crate::spectrum::NUM_BARS;
/// Greyscale stops for spectrum bars, from viz.rs SPECTRUM_GREYS.
const SPECTRUM_GREYS: [(f32, u8); 4] = [(0.00, 52), (0.22, 104), (0.52, 178), (0.85, 246)];
const AUDIO_EXTS: [&str; 8] = ["mp3", "flac", "m4a", "aac", "ogg", "opus", "wav", "alac"];

pub struct TrackEntry {
    path: PathBuf,
    title: String,
    lrc: Option<PathBuf>,
}

enum PlayMode {
    Sequence,
    RepeatOne,
    Shuffle,
}

impl PlayMode {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::RepeatOne,
            2 => Self::Shuffle,
            _ => Self::Sequence,
        }
    }

    fn label(self) -> &'static str {
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

    fn as_u8(self) -> u8 {
        match self {
            Self::Sequence => 0,
            Self::RepeatOne => 1,
            Self::Shuffle => 2,
        }
    }
}

struct BeatHandle {
    rx: Receiver<BeatResult>,
    cancel: Arc<AtomicBool>,
}

struct BeatResult {
    generation: u64,
    path: PathBuf,
    beats: Vec<(f64, f32)>,
    profile: Option<SoundProfile>,
}

#[derive(Default)]
struct OfflineBeats {
    beats: Vec<(f64, f32)>,
    index: usize,
    seq: u64,
    strength: f32,
    ready: bool,
    last_pos: Option<f64>,
}

pub struct Player {
    engine: AudioEngine,
    analyzer: SpectrumEngine,
    config: BeatConfig,
    bars: Rc<VecModel<BarData>>,
    playlist: Vec<TrackEntry>,
    current: Option<usize>,
    state: PersistedState,
    dirty: bool,
    last_save: Instant,
    device_check_at: Instant,
    offline: OfflineBeats,
    analysis: Option<BeatHandle>,
    generation: u64,
    lyrics: Vec<LyricLine>,
    lyrics_synced: bool,
    lyrics_track: Option<PathBuf>,
    seek_full: bool,
    seek_index: Option<usize>,
    look: Look,
    fade: Option<Fade>,
    ring_rotation: f32,
    samples: Vec<f32>,
    visual: VisualState,
    core_seq: u64,
    beat_age: f32,
    ring_strength: f32,
    library: ThemeLibrary,
}

impl Player {
    fn new(initial: Option<PathBuf>, bars: Rc<VecModel<BarData>>) -> Self {
        let state = storage::load();
        let config = BeatConfig::load();
        let library = ThemeLibrary::load();
        let mut engine = AudioEngine::new();
        engine.set_volume(state.volume);
        if let Ok(gains) = <[f32; crate::eq::NUM_BANDS]>::try_from(state.eq_gains.clone()) {
            engine.set_eq(&gains, state.eq_enabled);
        }

        let mut playlist = Vec::new();
        for path in &state.playlist {
            let path = PathBuf::from(path);
            let lrc = state
                .lrc_map
                .get(path.to_string_lossy().as_ref())
                .map(PathBuf::from);
            playlist.push(TrackEntry {
                title: track_title(&path),
                lrc,
                path,
            });
        }
        let current = state
            .current
            .as_deref()
            .and_then(|current| {
                playlist.iter().position(|track| track.path == Path::new(current))
            });

        let mut player = Self {
            engine,
            analyzer: SpectrumEngine::new(config.clone()),
            config,
            bars,
            playlist,
            current,
            state,
            dirty: false,
            last_save: Instant::now(),
            device_check_at: Instant::now(),
            offline: OfflineBeats::default(),
            analysis: None,
            generation: 0,
            lyrics: Vec::new(),
            lyrics_synced: false,
            lyrics_track: None,
            seek_full: false,
            seek_index: None,
            look: theme::builtin_look(0),
            fade: None,
            ring_rotation: 0.0,
            samples: Vec::new(),
            visual: VisualState::default(),
            core_seq: 0,
            beat_age: 1.0,
            ring_strength: 0.0,
            library,
        };
        player.look = player.resolve_look();
        theme::set_look(player.look);
        if let Some(index) = player.current {
            // Park the last track: stream loaded and paused so PLAY resumes it.
            let path = player.playlist[index].path.clone();
            if player.engine.play_paused(&path).is_ok() {
                player.refresh_lyrics();
            } else {
                player.current = None;
            }
        }
        if let Some(path) = initial {
            player.add_paths(std::iter::once(path));
        }
        player
    }

    fn resolve_look(&self) -> Look {
        if let Some(dark) = self.state.auto_theme {
            let mood = self.state.auto_mood.unwrap_or_default();
            return auto_theme::look(mood, dark);
        }
        if let Some(name) = &self.state.theme_name {
            if let Some(index) = self.library.find(name) {
                if let Some(entry) = self.library.entry(index) {
                    return entry.look;
                }
            }
        }
        let index = self.state.theme.unwrap_or(0).min(theme::BUILTIN_COUNT - 1);
        theme::builtin_look(index)
    }

    fn theme_label(&self) -> String {
        if self.state.auto_theme.is_some() {
            return "AUTO".to_owned();
        }
        if let Some(name) = &self.state.theme_name {
            return name.to_uppercase();
        }
        let index = self.state.theme.unwrap_or(0).min(theme::BUILTIN_COUNT - 1);
        theme::BUILTIN_NAMES[index].to_owned()
    }

    fn mode(&self) -> PlayMode {
        PlayMode::from_u8(self.state.play_mode)
    }

    fn duration(&self) -> f64 {
        self.engine.duration()
    }

    fn position(&self) -> f64 {
        self.engine.position()
    }

    // ---------- playback ----------

    fn play_index(&mut self, index: usize) {
        let Some(track) = self.playlist.get(index) else {
            return;
        };
        let path = track.path.clone();
        if self.engine.play(&path).is_err() {
            return;
        }
        self.current = Some(index);
        self.state.current = Some(path.to_string_lossy().into_owned());
        self.refresh_lyrics();
        self.beat_age = 1.0;
        self.core_seq = self.visual.beat_seq;
        self.ring_strength = 0.0;
        self.cancel_analysis();
        self.offline = OfflineBeats::default();
        self.ring_rotation = 0.0;
        self.spawn_analysis(path);
        self.dirty = true;
    }

    fn spawn_analysis(&mut self, path: PathBuf) {
        self.generation += 1;
        let generation = self.generation;
        let config = self.config.clone();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("sonadial-beat-analysis".to_owned())
            .spawn(move || {
                let analysis =
                    crate::beat::analyze_file(&path, &thread_cancel, &config).unwrap_or_default();
                let _ = tx.send(BeatResult {
                    generation,
                    path,
                    beats: analysis.beats,
                    profile: analysis.profile,
                });
            });
        if spawned.is_ok() {
            self.analysis = Some(BeatHandle { rx, cancel });
        }
    }

    fn cancel_analysis(&mut self) {
        if let Some(handle) = self.analysis.take() {
            handle.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn poll_analysis(&mut self) {
        let mut received = None;
        let mut disconnected = false;
        if let Some(handle) = &self.analysis {
            match handle.rx.try_recv() {
                Ok(result) => received = Some(result),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => disconnected = true,
            }
        }
        if let Some(result) = received {
            self.analysis = None;
            let matches_current = self
                .current
                .and_then(|index| self.playlist.get(index))
                .map(|track| track.path == result.path)
                .unwrap_or(false);
            if result.generation == self.generation && matches_current {
                let position = self.engine.position();
                self.offline.beats = result.beats;
                self.offline.ready = true;
                self.offline.index = self
                    .offline
                    .beats
                    .partition_point(|(time, _)| *time < position - 0.025);
                if let Some(profile) = result.profile {
                    self.follow_track_mood(Mood::from_profile(&profile));
                }
            }
        } else if disconnected {
            self.analysis = None;
        }
    }

    fn follow_track_mood(&mut self, mood: Mood) {
        let Some(dark) = self.state.auto_theme else {
            return;
        };
        self.state.auto_mood = Some(mood);
        self.dirty = true;
        let target = auto_theme::look(mood, dark);
        if target.palette.bg == self.look.palette.bg
            && target.gradient == self.look.gradient
        {
            return;
        }
        self.fade = Some(Fade::new(self.look, target));
    }

    /// Offline beat feed for the analyzer, mirroring app.rs offline_beat_state.
    fn offline_beat_state(&mut self, position: f64) -> Option<(u64, f32)> {
        if !self.offline.ready || self.offline.beats.is_empty() {
            return None;
        }
        let position = position.max(0.0);
        if self.offline.last_pos.map_or(true, |previous| {
            position < previous - 0.08 || position > previous + 0.30
        }) {
            self.offline.index = self
                .offline
                .beats
                .partition_point(|(time, _)| *time < position - 0.025);
        }

        let upper = position + BEAT_VISUAL_LEAD;
        let lower = self.offline.last_pos.map_or(position - 0.050, |previous| {
            (previous - 0.010).min(position)
        });
        let mut strength: f32 = 0.0;
        while self.offline.index < self.offline.beats.len()
            && self.offline.beats[self.offline.index].0 <= upper
        {
            let (time, value) = self.offline.beats[self.offline.index];
            if time >= lower {
                strength = strength.max(value);
            }
            self.offline.index += 1;
        }
        if strength > 0.0 {
            self.offline.seq = self.offline.seq.wrapping_add(1);
            self.offline.strength = strength;
        }
        self.offline.last_pos = Some(position);
        Some((self.offline.seq, self.offline.strength))
    }

    // ---------- lyrics ----------

    fn refresh_lyrics(&mut self) {
        self.seek_full = false;
        self.seek_index = None;
        let Some(index) = self.current else {
            self.lyrics.clear();
            self.lyrics_synced = false;
            self.lyrics_track = None;
            return;
        };
        let Some(track) = self.playlist.get(index) else {
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
        // Plain-text LRC files spread lines over the track length.
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

    fn mark_lyric_seek(&mut self, target: f64) {
        self.seek_full = true;
        self.seek_index = self.lyric_index_at(target).map(|(index, _)| index);
    }

    // ---------- playlist ----------

    fn add_paths<I: IntoIterator<Item = PathBuf>>(&mut self, paths: I) {
        let mut first_new = None;
        for path in paths {
            let scanned = if path.is_dir() { scan_folder(&path) } else { vec![path] };
            for path in scanned {
                let exists = self.playlist.iter().any(|track| track.path == path);
                if exists {
                    continue;
                }
                if first_new.is_none() {
                    first_new = Some(self.playlist.len());
                }
                let lrc = self
                    .state
                    .lrc_map
                    .get(path.to_string_lossy().as_ref())
                    .map(PathBuf::from);
                self.playlist.push(TrackEntry {
                    title: track_title(&path),
                    lrc,
                    path,
                });
            }
        }
        if first_new.is_some() {
            self.state.playlist = self
                .playlist
                .iter()
                .map(|track| track.path.to_string_lossy().into_owned())
                .collect();
            self.dirty = true;
        }
        if let Some(index) = first_new {
            self.play_index(index);
        }
    }

    fn add_files_dialog(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .set_title("Add music")
            .add_filter("Audio", &AUDIO_EXTS)
            .pick_files()
        {
            self.add_paths(paths);
        }
    }

    fn pick_next(&self, forward: bool) -> Option<usize> {
        if self.playlist.is_empty() {
            return None;
        }
        match self.mode() {
            PlayMode::RepeatOne => self.current.or(Some(0)),
            PlayMode::Shuffle => {
                if self.playlist.len() == 1 {
                    return Some(0);
                }
                let mut seed = SystemTimeSeed::new();
                let mut index = (seed.next() as usize) % self.playlist.len();
                if Some(index) == self.current {
                    index = (index + 1) % self.playlist.len();
                }
                Some(index)
            }
            PlayMode::Sequence => {
                let current = self.current.unwrap_or(0);
                let count = self.playlist.len();
                let next = if forward {
                    (current + 1) % count
                } else {
                    (current + count - 1) % count
                };
                Some(next)
            }
        }
    }

    fn advance(&mut self, forward: bool) {
        if let Some(index) = self.pick_next(forward) {
            self.play_index(index);
        }
    }

    // ---------- persistence ----------

    fn persist(&mut self) {
        self.state.volume = self.engine.volume();
        self.state.play_mode = self.mode().as_u8();
        storage::save(&self.state);
        self.dirty = false;
        self.last_save = Instant::now();
    }

    fn save_if_due(&mut self) {
        if self.dirty && self.last_save.elapsed() >= SAVE_EVERY {
            self.persist();
        }
    }

    // ---------- per-frame tick ----------

    fn tick(&mut self, ui: &Main, dt: f32) {
        if let Some(error) = self.engine.take_error() {
            // Decoder died mid-track: skip forward rather than stall.
            eprintln!("playback error: {error}");
            if self.engine.info().is_none() {
                self.advance(true);
            }
        }

        self.engine.drain_analysis(&mut self.samples, 16_384);
        let paused = self.engine.is_paused();
        let active = self.engine.info().is_some() && !paused;

        if active {
            self.analyzer.push(&self.samples);
            let position = self.engine.position();
            let external_beat = self.offline_beat_state(position);
            self.visual = self.analyzer.tick(true, external_beat);
            self.ring_rotation = (self.ring_rotation - std::f32::consts::PI / 40.0 * dt)
                .rem_euclid(std::f32::consts::TAU);
            if self.device_check_at.elapsed() >= Duration::from_secs(2) {
                self.device_check_at = Instant::now();
                let _ = self.engine.follow_default_output();
            }
            if self.visual.beat_seq > self.core_seq {
                self.core_seq = self.visual.beat_seq;
                let strength = self.visual.beat_strength;
                let heavy = strength >= self.config.heavy_ring_threshold;
                let short = !heavy && strength >= self.config.quick_strength_min;
                if heavy || short {
                    // Hard beat, or a short-pair hit: launch the expanding ring.
                    self.beat_age = 0.0;
                    self.ring_strength = if heavy {
                        strength
                    } else {
                        self.config.short_ring_strength
                    };
                } else {
                    self.beat_age = (self.beat_age + dt).min(1.0);
                }
            } else {
                if self.visual.beat_seq < self.core_seq {
                    self.core_seq = self.visual.beat_seq;
                }
                self.beat_age = (self.beat_age + dt).min(1.0);
            }
        } else {
            self.analyzer.decay_display(dt);
            if !self.analyzer.has_signal() && self.visual.energy <= 0.002 {
                self.analyzer.clear_display();
                self.visual = VisualState::default();
                self.ring_rotation = 0.0;
            }
            self.beat_age = 1.0;
            self.ring_strength = 0.0;
        }

        if self.engine.is_finished() && self.engine.info().is_some() {
            let at_end = self
                .current
                .map_or(false, |index| index + 1 >= self.playlist.len());
            match self.mode() {
                PlayMode::Sequence if at_end => {
                    // End of the queue: stop and stand by.
                    self.engine.stop();
                }
                _ => self.advance(true),
            }
        }

        self.poll_analysis();

        if let Some(fade) = &self.fade {
            let (look, done) = fade.sample(Instant::now());
            self.look = look;
            theme::set_look(look);
            if done {
                self.fade = None;
            }
        }

        self.push_state(ui);
        self.save_if_due();
    }

    fn push_state(&mut self, ui: &Main) {
        let theme = ui.global::<Theme>();
        let look = theme::current_look();
        let corners = look.gradient.unwrap_or([look.palette.bg; 4]);
        let [tl, tr, br, bl] = corners;
        let bg_top = theme::mix_rgb(tl, tr, 0.5);
        let bg_bottom = theme::mix_rgb(bl, br, 0.5);
        theme.set_bg_top(rgb(bg_top));
        theme.set_bg_bottom(rgb(bg_bottom));
        theme.set_corner_tl(rgba(tl, 96));
        theme.set_corner_br(rgba(br, 96));
        theme.set_c_bg_deep(rgb(look.palette.bg_deep));
        theme.set_c_surface(rgb(look.palette.surface));
        theme.set_c_line(rgb(look.palette.line));
        theme.set_c_text_hi(rgb(look.palette.text_hi));
        theme.set_c_text(rgb(look.palette.text));
        theme.set_c_text_dim(rgb(look.palette.text_dim));
        theme.set_c_ring(rgba(theme::ink(210), 20));
        theme.set_c_ring_hi(rgba(theme::ink(225), 110));

        let bars = self.analyzer.bars();
        let peaks = self.analyzer.peaks();
        let show_peaks = self.state.peak_hold;
        for index in 0..NUM_BARS {
            let raw = bars.get(index).copied().unwrap_or(0.0);
            let len = (raw / MAX_DB_VALUE).clamp(0.0, 1.0);
            let peak = (peaks.get(index).copied().unwrap_or(0.0) / MAX_DB_VALUE)
                .clamp(0.0, 1.0);
            let col = rgb(theme::ink(spectrum_grey(len)));
            // Bar angle: start at the bottom like viz.rs and spin slowly.
            let angle = std::f32::consts::FRAC_PI_2
                + index as f32 / NUM_BARS as f32 * std::f32::consts::TAU
                + self.ring_rotation;
            self.bars.set_row_data(
                index,
                BarData {
                    len,
                    peak: if show_peaks { peak } else { 0.0 },
                    dx: angle.cos(),
                    dy: angle.sin(),
                    col,
                },
            );
        }
        ui.set_pulse(self.ring_strength * (1.0 - self.beat_age));
        ui.set_beat_age(self.beat_age);
        ui.set_kick(self.visual.kick);
        ui.set_radial_scale(self.state.radial_scale);
        ui.set_show_bg_spectrum(self.state.show_bg_spectrum);
        ui.set_show_radial(self.state.show_radial);
        ui.set_show_peaks(show_peaks);

        let position = self.position();
        let duration = self.duration();
        let paused = self.engine.is_paused();
        ui.set_time_text(format!(
            "{}/{}",
            short_time(position),
            short_time(duration)
        ).into());
        let status = if self.engine.info().is_none() {
            "[STANDBY]"
        } else if paused {
            "[PAUSED]"
        } else {
            "[PLAYING]"
        };
        ui.set_status_text(status.to_owned().into());
        ui.set_paused(paused);
        ui.set_has_track(self.engine.info().is_some());
        ui.set_track_title(
            self.current
                .and_then(|index| self.playlist.get(index))
                .map(|track| track.title.clone())
                .unwrap_or_default()
                .into(),
        );
        ui.set_theme_label(self.theme_label().into());
        ui.set_mode_label(self.mode().label().to_owned().into());
        ui.set_volume(self.engine.volume());
        if duration > 0.0 {
            ui.set_progress((position / duration).clamp(0.0, 1.0) as f32);
        } else {
            ui.set_progress(0.0);
        }

        self.push_lyrics(ui, position);
    }

    fn push_lyrics(&mut self, ui: &Main, position: f64) {
        let Some((index, active)) = self.lyric_index_at(position) else {
            ui.set_lyric_current(String::new().into());
            ui.set_lyric_next(String::new().into());
            ui.set_lyric_active(false);
            return;
        };
        let (start, interval) = self.lyric_line_interval(index);
        let line = self
            .lyrics
            .get(index)
            .map(|line| line.text.as_str())
            .unwrap_or("");
        if self.seek_full && self.seek_index != Some(index) {
            self.seek_full = false;
            self.seek_index = None;
        }
        // After a manual seek the landed line shows fully once; otherwise it
        // types out across its interval.
        let count = if self.seek_full && self.seek_index == Some(index) {
            line.chars().count()
        } else {
            let total = line.chars().count();
            let progress = ((position - start) / interval.max(0.4)).clamp(0.0, 1.0);
            (((progress * total as f64).floor() as usize).min(total)).min(total)
        };
        ui.set_lyric_current(line.chars().take(count).collect::<String>().into());
        ui.set_lyric_next(
            self.lyrics
                .get(index + 1)
                .map(|line| line.text.clone())
                .unwrap_or_default()
                .into(),
        );
        ui.set_lyric_active(active);
    }
}

// ---------- helpers ----------

fn rgb(rgb: Rgb) -> slint::Color {
    slint::Color::from_rgb_u8(rgb[0], rgb[1], rgb[2])
}

fn rgba(rgb: Rgb, a: u8) -> slint::Color {
    slint::Color::from_argb_u8(a, rgb[0], rgb[1], rgb[2])
}

fn spectrum_grey(norm: f32) -> u8 {
    let value = norm.clamp(0.0, 1.0);
    let mut lower = SPECTRUM_GREYS[0];
    let mut upper = SPECTRUM_GREYS[SPECTRUM_GREYS.len() - 1];
    for pair in SPECTRUM_GREYS.windows(2) {
        if value >= pair[0].0 && value <= pair[1].0 {
            lower = pair[0];
            upper = pair[1];
            break;
        }
    }
    let span = (upper.0 - lower.0).max(1e-6);
    let t = ((value - lower.0) / span).clamp(0.0, 1.0);
    (lower.1 as f32 + (upper.1 as f32 - lower.1 as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn short_time(seconds: f64) -> String {
    let seconds = seconds.max(0.0).floor() as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn track_title(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn scan_folder(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(scan_folder(&path));
        } else if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
            if AUDIO_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Tiny xorshift so shuffle needs no dependency.
struct SystemTimeSeed(u64);

impl SystemTimeSeed {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x2545_F491_4F6C_DD1D);
        Self(nanos | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

// ---------- entry point ----------

thread_local! {
    static PLAYER: RefCell<Option<Player>> = const { RefCell::new(None) };
}

fn with_player<R>(body: impl FnOnce(&mut Player) -> R) -> Option<R> {
    PLAYER.with(|cell| {
        let mut guard = cell.borrow_mut();
        guard.as_mut().map(body)
    })
}

pub fn run(initial: Option<PathBuf>) {
    register_fonts();

    let ui = Main::new().expect("slint window");
    let bars = Rc::new(VecModel::from(
        std::iter::repeat(BarData {
            len: 0.0,
            peak: 0.0,
            dx: 0.0,
            dy: 0.0,
            col: slint::Color::from_rgb_u8(0, 0, 0),
        })
        .take(NUM_BARS)
        .collect::<Vec<_>>(),
    ));
    ui.set_bars(slint::ModelRc::from(bars.clone()));

    let player = Player::new(initial, bars);

    ui.on_toggle_play(|| {
        with_player(|player| {
            if player.engine.info().is_none() {
                let index = player.current.or_else(|| player.pick_next(true));
                if let Some(index) = index {
                    player.play_index(index);
                } else {
                    player.add_files_dialog();
                }
            } else {
                player.engine.toggle_pause();
                player.dirty = true;
            }
            player.save_if_due();
        });
    });
    ui.on_next_track(|| {
        with_player(|player| {
            player.advance(true);
            player.save_if_due();
        });
    });
    ui.on_prev_track(|| {
        with_player(|player| {
            player.advance(false);
            player.save_if_due();
        });
    });
    ui.on_seek_to(|fraction| {
        with_player(|player| {
            let duration = player.duration();
            if duration > 0.0 {
                let target = (fraction.clamp(0.0, 1.0) as f64) * duration;
                if player.engine.seek(target) {
                    player.mark_lyric_seek(target);
                }
            }
        });
    });
    ui.on_set_volume(|value| {
        with_player(|player| {
            let value = value.clamp(0.0, 1.0);
            player.engine.set_volume(value);
            player.state.volume = value;
            player.dirty = true;
        });
    });
    ui.on_cycle_mode(|| {
        with_player(|player| {
            player.state.play_mode = player.mode().next().as_u8();
            player.dirty = true;
        });
    });
    ui.on_add_files(|| {
        with_player(|player| player.add_files_dialog());
    });
    ui.on_next_theme(|| {
        with_player(|player| {
            let index = (player.state.theme.unwrap_or(0) + 1) % theme::BUILTIN_COUNT;
            player.state.theme = Some(index);
            player.state.theme_name = None;
            player.state.auto_theme = None;
            player.fade = None;
            player.look = theme::builtin_look(index);
            theme::set_look(player.look);
            player.dirty = true;
        });
    });
    ui.on_toggle_auto_theme(|| {
        with_player(|player| {
            if player.state.auto_theme.is_some() {
                player.state.auto_theme = None;
                player.fade = None;
                player.look = player.resolve_look();
                theme::set_look(player.look);
            } else {
                let dark = !theme::is_light();
                player.state.auto_theme = Some(dark);
                let mood = player.state.auto_mood.unwrap_or_default();
                player.look = auto_theme::look(mood, dark);
                theme::set_look(player.look);
            }
            player.dirty = true;
        });
    });

    PLAYER.with(|cell| *cell.borrow_mut() = Some(player));

    let weak = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(16),
        move || {
            let Some(ui) = weak.upgrade() else { return };
            with_player(|player| player.tick(&ui, 0.016));
        },
    );

    ui.run().expect("slint event loop");
    with_player(|player| player.persist());
}

/// Register the bundled TUI/pixel fonts with Slint's font collection so the
/// .slint font-family names resolve. Uses the unstable fontique hook, same as
/// the wasm gallery demo does for its downloaded CJK font.
fn register_fonts() {
    const FONTS: &[&[u8]] = &[
        include_bytes!("../assets/fonts/FusionPixel12-Mono-CJK.ttf"),
        include_bytes!("../assets/fonts/FusionPixel12-Mono-JA.ttf"),
        include_bytes!("../assets/fonts/FusionPixel12-Mono-KO.ttf"),
        include_bytes!("../assets/fonts/FusionPixel12-Mono-TUI.ttf"),
        include_bytes!("../assets/fonts/SarasaMonoSC-TUI.ttf"),
    ];
    use slint::fontique_011::fontique;
    let mut collection = slint::fontique_011::shared_collection();
    for data in FONTS {
        let blob = fontique::Blob::new(Arc::new((*data).to_vec()));
        for (family_id, _infos) in collection.register_fonts(blob, None) {
            // CJK pixel faces must join the fallback chain for Han/Kana/Hangul
            // so mixed-language lyrics render without a system font.
            for script in ["Hani", "Kana", "Hang"] {
                collection.append_fallbacks(
                    fontique::FallbackKey::new(
                        fontique::Script::from_str_unchecked(script),
                        None,
                    ),
                    std::iter::once(family_id),
                );
            }
        }
    }
}
