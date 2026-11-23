// Slint-UI orchestrator (slint-ui branch). Replaces the egui layer: owns the
// audio engine and spectrum analyzer, feeds the .slint window at ~60 fps and
// reuses every backend module unchanged (audio/beat/lyrics/theme/...).
use std::cell::RefCell;
use std::rc::Rc;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use crate::audio::AudioEngine;
use crate::auto_theme::{self, Fade, Mood};
use crate::beat_config::BeatConfig;
use crate::download::{self, DownloadEvent};
use crate::eq::{self, BAND_FREQS, MAX_GAIN_DB, MIN_GAIN_DB, NUM_BANDS, PRESETS};
use crate::lyrics::{self, LyricLine};
use crate::search::SearchIndex;
use crate::sound_profile::SoundProfile;
use crate::spectrum::{SpectrumEngine, VisualState};
use crate::stage::{Stage, StageInput};
use crate::storage::{self, PersistedState};
use crate::theme::{self, Look, Rgb};
use crate::theme::Category;
use crate::theme_library::{ThemeEntry, ThemeLibrary};

slint::include_modules!();

mod preview;

const BEAT_VISUAL_LEAD: f64 = 0.025;
const SAVE_EVERY: Duration = Duration::from_secs(2);
const AUDIO_EXTS: [&str; 8] = ["mp3", "flac", "m4a", "aac", "ogg", "opus", "wav", "alac"];
const VOLUME_STEP: f32 = 0.05;
const SEEK_STEP: f64 = 5.0;

#[derive(Clone)]
pub struct TrackEntry {
    path: PathBuf,
    title: String,
    lrc: Option<PathBuf>,
    search: SearchIndex,
}

impl TrackEntry {
    fn new(path: PathBuf, lrc: Option<PathBuf>) -> Self {
        let title = track_title(&path);
        let text = format!("{} {}", title, path.display());
        Self { path, title, lrc, search: SearchIndex::new(&text) }
    }
}

#[derive(Clone, Copy)]
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
    stage: Stage,
    stage_dirty: bool,
    last_stage_size: (u32, u32, f32, bool),
    playlist: Vec<TrackEntry>,
    selected_track: Option<usize>,
    search: String,
    sidebar_open: bool,
    next_queue: Vec<PathBuf>,
    /// Shuffle's next pick, rolled ahead of time so the dock's NEXT preview and
    /// the track that actually plays agree. Stored by path to survive list edits.
    shuffle_next: Option<PathBuf>,
    /// Seconds of playback since the current track started; drives the title
    /// shimmer, so it freezes on pause and restarts from the left on a new track.
    shimmer_clock: f32,
    scroll_y: f32,
    auto_scroll_target: Option<usize>,
    show_settings: bool,
    show_eq: bool,
    show_download: bool,
    track_ui_dirty: bool,
    theme_ui_dirty: bool,
    eq_ui_dirty: bool,
    download_url: String,
    env_proxy: Option<String>,
    download_running: bool,
    download_progress: f32,
    download_status: String,
    download_rx: Option<Receiver<DownloadEvent>>,
    download_cancel: Option<Arc<AtomicBool>>,
    error: Option<String>,
    pending_delete: Option<PathBuf>,
    ui_weak: slint::Weak<Main>,
    seek_preview: Option<f32>,
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
    fn new(initial: Option<PathBuf>) -> Self {
        let mut state = storage::load();
        // The Slint stage currently implements DEFAULT and DOT MATRIX. Old
        // PIANO selections used to leave the previous drawing style active.
        state.piano_mode = false;
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
            playlist.push(TrackEntry::new(path, lrc));
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
            stage: Stage::new(),
            stage_dirty: true,
            last_stage_size: (0, 0, 0.0, true),
            playlist,
            selected_track: None,
            search: String::new(),
            auto_scroll_target: None,
            sidebar_open: true,
            next_queue: Vec::new(),
            shuffle_next: None,
            shimmer_clock: 0.0,
            scroll_y: 0.0,
            show_settings: false,
            show_eq: false,
            show_download: false,
            track_ui_dirty: true,
            theme_ui_dirty: true,
            eq_ui_dirty: true,
            download_url: String::new(),
            env_proxy: std::env::var("HTTP_PROXY")
                .or_else(|_| std::env::var("http_proxy"))
                .or_else(|_| std::env::var("HTTPS_PROXY"))
                .or_else(|_| std::env::var("https_proxy"))
                .ok()
                .filter(|value| !value.trim().is_empty()),
            download_running: false,
            download_progress: 0.0,
            download_status: String::new(),
            download_rx: None,
            download_cancel: None,
            error: None,
            pending_delete: None,
            ui_weak: Default::default(),
            seek_preview: None,
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
            // The command-line track plays immediately, even if it was
            // already in the persisted playlist.
            let mut index = player.playlist.iter().position(|track| track.path == path);
            if index.is_none() {
                player.add_paths(std::iter::once(path.clone()));
                index = player.playlist.iter().position(|track| track.path == path);
            }
            if let Some(index) = index {
                player.play_index(index);
            }
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
        self.shuffle_next = None;
        self.shimmer_clock = 0.0;
        self.state.current = Some(path.to_string_lossy().into_owned());
        self.refresh_lyrics();
        self.beat_age = 1.0;
        self.core_seq = self.visual.beat_seq;
        self.ring_strength = 0.0;
        self.cancel_analysis();
        self.offline = OfflineBeats::default();
        self.ring_rotation = 0.0;
        self.stage_dirty = true;
        self.track_ui_dirty = true;
        self.auto_scroll_target = Some(index);
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
                self.playlist.push(TrackEntry::new(path, lrc));
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

    fn pick_next(&mut self, forward: bool) -> Option<usize> {
        if self.playlist.is_empty() {
            return None;
        }
        match self.mode() {
            PlayMode::RepeatOne => self.current.or(Some(0)),
            PlayMode::Shuffle if forward => self.shuffle_upcoming(),
            PlayMode::Shuffle => Some(self.shuffle_roll()),
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

    /// A random track other than the current one (any track when the list has one).
    fn shuffle_roll(&self) -> usize {
        let count = self.playlist.len();
        if count <= 1 {
            return 0;
        }
        let mut seed = SystemTimeSeed::new();
        let mut index = (seed.next() as usize) % count;
        if Some(index) == self.current {
            index = (index + 1) % count;
        }
        index
    }

    /// Shuffle's pre-rolled next track; rolls (and remembers) one if the stored
    /// pick is missing, was removed from the list, or became the current track.
    fn shuffle_upcoming(&mut self) -> Option<usize> {
        if self.playlist.is_empty() {
            return None;
        }
        let stored = self.shuffle_next.as_ref().and_then(|path| {
            self.playlist.iter().position(|track| &track.path == path)
        });
        if let Some(index) = stored {
            if self.playlist.len() == 1 || Some(index) != self.current {
                return Some(index);
            }
        }
        let index = self.shuffle_roll();
        self.shuffle_next = Some(self.playlist[index].path.clone());
        Some(index)
    }

    /// Text for the dock's NEXT preview, following the same rules as `tick`'s
    /// end-of-track handling and `advance(true)`. Empty when nothing is loaded.
    fn upcoming_label(&mut self) -> String {
        let Some(current) = self.current else {
            return String::new();
        };
        if let Some(path) = self.next_queue.first() {
            return self
                .playlist
                .iter()
                .find(|track| &track.path == path)
                .map(|track| track.title.clone())
                .unwrap_or_else(|| track_title(path));
        }
        match self.mode() {
            PlayMode::RepeatOne => "REPEAT ONE".to_owned(),
            PlayMode::Sequence => self
                .playlist
                .get(current + 1)
                .map(|track| track.title.clone())
                .unwrap_or_else(|| "END OF LIST".to_owned()),
            PlayMode::Shuffle => self
                .shuffle_upcoming()
                .and_then(|index| self.playlist.get(index))
                .map(|track| track.title.clone())
                .unwrap_or_default(),
        }
    }

    fn advance(&mut self, forward: bool) {
        if forward {
            if !self.next_queue.is_empty() {
                let path = self.next_queue.remove(0);
                if let Some(index) = self
                    .playlist
                    .iter()
                    .position(|track| track.path == path)
                {
                    self.track_ui_dirty = true;
                    self.play_index(index);
                    return;
                }
            }
        }
        if let Some(index) = self.pick_next(forward) {
            self.play_index(index);
        }
    }

    fn close_context(&mut self) {
        CONTEXT_SLOT.with(|cell| {
            if let Some((index, x, y)) = cell.borrow_mut().take() {
                if let Some(ui) = self.ui_weak.upgrade() {
                    ui.set_context_index(-1);
                    let _ = (x, y);
                }
                self.track_ui_dirty = true;
            }
        });
    }

    fn open_context_menu(&mut self, ui: &Main) {
        CONTEXT_SLOT.with(|cell| {
            // Drain the slot: the menu stays open through context-index, and
            // clicking outside (or another row) re-arms the slot as needed.
            if let Some((index, x, y)) = cell.borrow_mut().take() {
                let Some(track) = self.playlist.get(index) else {
                    ui.set_context_index(-1);
                    return;
                };
                ui.set_context_index(index as i32);
                ui.set_context_x(x);
                ui.set_context_y(y);
                ui.set_context_title(track.title.clone().into());
                ui.set_context_has_lrc(track.lrc.is_some());
            }
        });
    }

    fn toggle_play(&mut self) {
        if self.engine.info().is_none() {
            let index = match self.current {
                Some(index) => Some(index),
                None => self.pick_next(true),
            };
            if let Some(index) = index {
                self.play_index(index);
            } else {
                self.add_files_dialog();
            }
        } else {
            self.engine.toggle_pause();
            self.track_ui_dirty = true;
            self.dirty = true;
        }
    }

    fn stop(&mut self) {
        self.cancel_analysis();
        self.engine.stop();
        self.offline = OfflineBeats::default();
        self.visual = VisualState::default();
        self.core_seq = 0;
        self.beat_age = 1.0;
        self.ring_strength = 0.0;
        self.track_ui_dirty = true;
        self.dirty = true;
    }

    fn queue_next(&mut self, index: usize) {
        if self.current == Some(index) {
            return;
        }
        let Some(track) = self.playlist.get(index) else {
            return;
        };
        if !self.next_queue.iter().any(|queued| queued == &track.path) {
            self.next_queue.push(track.path.clone());
            self.track_ui_dirty = true;
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
        let path = track.path.clone();
        let is_current = self.current == Some(index);

        if is_current {
            self.stop();
        }
        if delete_file {
            if let Err(error) = std::fs::remove_file(&path) {
                self.error = Some(format!("DELETE FAILED / {error}"));
                return;
            }
            if let Some(lrc) = track.lrc.clone() {
                let _ = std::fs::remove_file(&lrc);
            }
        }

        self.playlist.remove(index);
        self.lrc_map_remove(&path);
        if self.selected_track == Some(index) {
            self.selected_track = None;
        } else if let Some(selected) = self.selected_track {
            if selected > index {
                self.selected_track = Some(selected - 1);
            }
        }
        self.next_queue.retain(|queued| queued != &path);
        if is_current {
            self.current = None;
            self.lyrics.clear();
            self.lyrics_synced = false;
            self.lyrics_track = None;
        } else if let Some(current) = self.current {
            if current > index {
                self.current = Some(current - 1);
            }
        }
        self.track_ui_dirty = true;
        self.dirty = true;
    }

    fn lrc_map_remove(&mut self, path: &Path) {
        self.state
            .lrc_map
            .remove(path.to_string_lossy().as_ref());
    }

    fn import_lrc(&mut self, index: usize) {
        let Some(track) = self.playlist.get(index) else {
            return;
        };
        let Some(lrc) = rfd::FileDialog::new()
            .set_title("Import lyrics")
            .add_filter("Lyrics", &["lrc"])
            .pick_file()
        else {
            return;
        };
        self.state
            .lrc_map
            .insert(track.path.to_string_lossy().into_owned(), lrc.display().to_string());
        self.refresh_lyrics_for(index);
        self.track_ui_dirty = true;
        self.dirty = true;
    }

    fn clear_lrc(&mut self, index: usize) {
        let path = match self.playlist.get(index) {
            Some(track) => track.path.clone(),
            None => return,
        };
        self.lrc_map_remove(&path);
        self.refresh_lyrics_for(index);
        self.track_ui_dirty = true;
        self.dirty = true;
    }

    fn refresh_lyrics_for(&mut self, index: usize) {
        if self.current == Some(index) {
            self.refresh_lyrics();
        }
    }

    fn seek_relative(&mut self, seconds: f64) {
        if self.engine.info().is_none() {
            return;
        }
        let position = self.engine.position();
        let duration = self.duration();
        let target = (position + seconds).clamp(0.0, duration.max(0.0));
        if self.engine.seek(target) {
            self.mark_lyric_seek(target);
        }
    }

    fn adjust_volume(&mut self, delta: f32) {
        let volume = (self.engine.volume() + delta).clamp(0.0, 1.25);
        self.engine.set_volume(volume);
        self.state.volume = volume;
        self.dirty = true;
    }

    fn set_volume(&mut self, volume: f32) {
        let volume = volume.clamp(0.0, 1.25);
        self.engine.set_volume(volume);
        self.state.volume = volume;
        self.dirty = true;
    }

    fn dot_style(&self) -> bool {
        self.state.led_bars && self.state.braille_stream
    }

    fn peak_hold_on(&self) -> bool {
        if self.dot_style() { self.state.peak_hold_dots } else { self.state.peak_hold }
    }

    /// Peak hold for the active visual style (pixel mode has its own flag).
    fn peak_hold_flag(&mut self) -> &mut bool {
        if self.dot_style() {
            &mut self.state.peak_hold_dots
        } else {
            &mut self.state.peak_hold
        }
    }

    fn set_style(&mut self, style: u8) {
        let dots = match style {
            0 => false,
            1 => true,
            _ => return,
        };
        self.state.piano_mode = false;
        self.state.led_bars = dots;
        self.state.braille_stream = dots;
        self.stage_dirty = true;
        self.dirty = true;
    }

    fn cycle_visual_mode(&mut self) {
        if self.state.led_bars && self.state.braille_stream {
            self.set_style(0);
        } else {
            self.set_style(1);
        }
    }

    fn toggle_overlay(&mut self, which: u8) {
        match which {
            0 => {
                self.show_settings = !self.show_settings;
                if self.show_settings {
                    self.show_eq = false;
                    self.show_download = false;
                    self.theme_ui_dirty = true;
                }
            }
            1 => {
                self.show_eq = !self.show_eq;
                if self.show_eq {
                    self.show_settings = false;
                    self.show_download = false;
                }
            }
            _ => {
                self.show_download = !self.show_download;
                if self.show_download {
                    self.show_settings = false;
                    self.show_eq = false;
                }
            }
        }
    }

    // ---------- EQ ----------

    fn apply_eq(&mut self) {
        let Ok(gains) = <[f32; NUM_BANDS]>::try_from(self.state.eq_gains.clone()) else {
            return;
        };
        self.engine.set_eq(&gains, self.state.eq_enabled);
        self.dirty = true;
    }

    fn eq_preset_label(&self) -> String {
        if self.state.eq_preset == 0 {
            "CUSTOM  ".to_owned()
        } else {
            format!("{:<8}", PRESETS[self.state.eq_preset - 1].0)
        }
    }

    fn cycle_eq_preset(&mut self, delta: i32) {
        let count = PRESETS.len() as i32 + 1;
        let current = self.state.eq_preset as i32;
        let next = (current + delta).rem_euclid(count) as usize;
        self.state.eq_preset = next;
        if next > 0 {
            self.state.eq_gains = PRESETS[next - 1].1.to_vec();
            self.apply_eq();
        }
        self.eq_ui_dirty = true;
    }

    fn reset_eq(&mut self) {
        self.state.eq_preset = 1;
        self.state.eq_gains = PRESETS[0].1.to_vec();
        self.apply_eq();
        self.eq_ui_dirty = true;
    }

    fn set_eq_band(&mut self, index: usize, gain: f32) {
        if index >= NUM_BANDS {
            return;
        }
        let gain = gain.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
        if (self.state.eq_gains[index] - gain).abs() > f32::EPSILON {
            self.state.eq_gains[index] = gain;
            self.state.eq_preset = 0;
            self.apply_eq();
            self.eq_ui_dirty = true;
        }
    }

    // ---------- download ----------

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
            let proxy = self.state.proxy.trim().to_owned();
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
        let Some(mut receiver) = self.download_rx.take() else {
            return;
        };
        let mut keep = true;
        loop {
            match receiver.try_recv() {
                Ok(event) => match event {
                    DownloadEvent::Status(text) => self.download_status = text,
                    DownloadEvent::Progress(fraction) => {
                        self.download_progress = fraction.clamp(0.0, 1.0);
                    }
                    DownloadEvent::Done(path) => {
                        self.download_running = false;
                        self.download_cancel = None;
                        keep = false;
                        self.add_paths([path]);
                    }
                    DownloadEvent::Error(text) => {
                        self.download_running = false;
                        self.download_cancel = None;
                        keep = false;
                        self.download_status = text;
                    }
                    DownloadEvent::Cancelled => {
                        self.download_running = false;
                        self.download_cancel = None;
                        keep = false;
                        self.download_status = String::new();
                        self.download_progress = 0.0;
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.download_running = false;
                    self.download_cancel = None;
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.download_rx = Some(receiver);
        }
    }

    // ---------- selection / search ----------

    fn filtered_indices(&self) -> Vec<usize> {
        let query = self.search.trim().to_lowercase();
        self.playlist
            .iter()
            .enumerate()
            .filter(|(_, track)| query.is_empty() || track.search.matches(&query))
            .map(|(index, _)| index)
            .collect()
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

        // Title shimmer: advance while playing; once paused, let a band that is
        // already crossing the title finish its sweep instead of vanishing.
        let style = ui.global::<Style>();
        let shimmer_period = style.get_dock_shimmer_period() as f32 / 1000.0;
        if shimmer_period > 0.0 {
            let phase = (self.shimmer_clock % shimmer_period) / shimmer_period;
            let in_flight = phase > 0.0 && phase < style.get_dock_shimmer_sweep();
            if active || (self.current.is_some() && in_flight) {
                self.shimmer_clock += dt;
            }
        }

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
                PlayMode::Sequence if at_end && self.next_queue.is_empty() => {
                    // End of the queue: stop and stand by.
                    self.engine.stop();
                }
                _ => self.advance(true),
            }
        }

        self.poll_analysis();
        self.poll_download();

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
        theme.set_c_bg(rgba(look.palette.bg, 255));
        theme.set_c_bg_deep(rgba(look.palette.bg_deep, 255));
        theme.set_c_surface(rgba(look.palette.surface, 255));
        theme.set_c_line(rgba(look.palette.line, 255));
        theme.set_c_text_hi(rgba(look.palette.text_hi, 255));
        theme.set_c_text(rgba(look.palette.text, 255));
        theme.set_c_text_dim(rgba(look.palette.text_dim, 255));
        theme.set_c_danger(rgba(theme::danger(), 255));

        // Geometry: sidebar + top bar + dock carve the stage out of the window.
        let window = ui.window();
        ui.set_window_maximized(window.is_maximized());
        let scale = window.scale_factor() as f32;
        let size = window.size();
        ui.set_sidebar_open(self.sidebar_open);
        // Slint owns the layout. Read its resolved geometry so the CPU stage,
        // resize handling and hit areas all follow ui/style.slint.
        let stage_rect = (
            ui.get_stage_x(),
            ui.get_stage_y(),
            ui.get_stage_w(),
            ui.get_stage_h(),
        );
        let style = ui.global::<Style>();

        // The stage texture only needs a repaint while something actually
        // moves; standby with a silent analyzer is a fully static frame.
        let active = self.engine.info().is_some() && !self.engine.is_paused();
        let size_changed = self.last_stage_size
            != (size.width, size.height, scale, self.sidebar_open);
        let must_paint = active
            || self.analyzer.has_signal()
            || self.visual.energy > 0.002
            || self.fade.is_some()
            || self.stage_dirty
            || size_changed;
        if must_paint {
            self.last_stage_size = (size.width, size.height, scale, self.sidebar_open);
            self.stage_dirty = false;
            let output = self.stage.paint(
                size.width.max(1),
                size.height.max(1),
                scale,
                &look,
                stage_rect,
                StageInput {
                    bars: self.analyzer.bars(),
                    peaks: self.analyzer.peaks(),
                    visual: self.visual,
                    playing: active,
                    pulse_enabled: self.state.show_core_pulse,
                    show_bg_spectrum: self.state.show_bg_spectrum,
                    show_radial: self.state.show_radial,
                    peak_hold: self.peak_hold_on(),
                    rotation: self.ring_rotation,
                    radial_scale: self.state.radial_scale,
                    led_bars: self.state.led_bars,
                    dot_style: self.state.led_bars && self.state.braille_stream,
                    braille_stream: self.state.braille_stream,
                    beat_age: self.beat_age,
                    ring_strength: self.ring_strength,
                },
            );
            ui.set_stage(slint::Image::from_rgba8(output.buffer));
            ui.set_disc_r(output.disc_r);
        }

        // Dock: title, ASCII seek + volume, transport labels.
        // Before `info` borrows the engine: shuffle may roll and store its pick.
        let next_text = self.upcoming_label();
        ui.set_dock_next(next_text.into());
        let position = self.position();
        let duration = self.duration();
        let paused = self.engine.is_paused();
        let info = self.engine.info();
        // Pad the position to the duration's width so the label, and the meter
        // length derived from it, stay fixed for the whole track.
        let (position_text, duration_text) = (short_time(position), short_time(duration));
        let time_width = position_text.chars().count().max(duration_text.chars().count());
        let time_text = format!("{position_text:>time_width$} / {duration_text}");
        let progress = if duration > 0.0 {
            (position / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };

        let (title_text, title_live) = self
            .current
            .and_then(|index| self.playlist.get(index))
            .map(|track| (track.title.clone(), true))
            .unwrap_or_else(|| ("DROP AUDIO HERE / CTRL+O".to_owned(), false));
        ui.set_dock_title(title_text.into());
        ui.set_dock_title_live(title_live);
        let shimmer_period = style.get_dock_shimmer_period() as f32 / 1000.0;
        // Paused or resting phases put the band off the title, so it stays hidden.
        ui.set_dock_shimmer(if title_live && shimmer_period > 0.0 {
            (self.shimmer_clock % shimmer_period) / shimmer_period
        } else {
            -1.0
        });

        let mode = self.mode();
        ui.set_next_queue_count(self.next_queue.len() as i32);
        ui.set_mode_long(mode.label().to_owned().into());
        ui.set_mode_short(mode.short_label().to_owned().into());

        set_seek_meter(ui, &time_text, self.seek_preview.unwrap_or(progress));
        ui.set_progress(progress);

        let vol_norm = (self.engine.volume() / style.get_volume_max()).clamp(0.0, 1.0);
        let vol_cells = style.get_dock_volume_cells() as usize;
        let vol_filled = ((vol_norm * vol_cells as f32).round() as usize).min(vol_cells);
        ui.set_vol_filled(vol_filled as i32);
        ui.set_vol_fill_str("#".repeat(vol_filled).into());
        ui.set_vol_rest_str("-".repeat(vol_cells - vol_filled).into());
        ui.set_vol_pct(format!("{:>3}%", (vol_norm * 100.0).round() as i32).into());

        ui.set_play_label(if active { "PAUSE" } else { "PLAY" }.into());
        ui.set_core_status(if info.is_none() {
            "[STANDBY]"
        } else if paused {
            "[PAUSED]"
        } else {
            "[PLAYING]"
        }.into());

        // Overlays.
        ui.set_show_settings(self.show_settings);
        ui.set_show_eq(self.show_eq);
        ui.set_show_download(self.show_download);
        ui.set_show_lyrics(self.state.show_lyrics);
        ui.set_peak_hold(self.peak_hold_on());
        ui.set_show_bg_spectrum(self.state.show_bg_spectrum);
        ui.set_show_radial(self.state.show_radial);
        ui.set_show_core_pulse(self.state.show_core_pulse);
        ui.set_radial_scale(self.state.radial_scale);
        let visual_style = if self.state.led_bars && self.state.braille_stream {
            1
        } else {
            0
        };
        ui.set_visual_style(visual_style);
        ui.set_out_label(if info.is_some() {
            self.engine.output_label()
        } else {
            String::new()
        }.into());

        // EQ panel.
        ui.set_eq_enabled(self.state.eq_enabled);
        ui.set_eq_preset_name(self.eq_preset_label().into());
        ui.set_eq_hint(
            if self.state.eq_enabled {
                "DRAG A BAND TO ADJUST / -12..+12 DB"
            } else {
                "EQ BYPASS / DRAG TO EDIT"
            }
            .into(),
        );
        if self.eq_ui_dirty {
            self.eq_ui_dirty = false;
            let mut model = slint::VecModel::<EqBandData>::default();
            for (index, gain) in self.state.eq_gains.iter().enumerate() {
                model.push(EqBandData {
                    gain: *gain,
                    label: format_gain(*gain).into(),
                    freq: format_frequency(BAND_FREQS[index]).into(),
                });
            }
            ui.set_eq_bands(slint::ModelRc::from(Rc::new(model)));
        }

        // Download panel.
        ui.set_download_running(self.download_running);
        ui.set_env_proxy_visible(self.env_proxy.is_some());
        if self.download_running || self.download_progress > 0.0 {
            let cells = 32usize;
            let filled = ((self.download_progress * cells as f32).round() as usize).min(cells);
            ui.set_download_progress_text(
                format!(
                    "[{}{}] {:>3}%",
                    "#".repeat(filled),
                    "-".repeat(cells - filled),
                    (self.download_progress * 100.0).round() as i32
                )
                .into(),
            );
        } else {
            ui.set_download_progress_text(String::new().into());
        }
        ui.set_download_status(self.download_status.clone().into());
        ui.set_download_out_dir(storage::download_dir().display().to_string().into());

        // Modal + toast.
        ui.set_confirm_visible(self.pending_delete.is_some());
        if let Some(path) = &self.pending_delete {
            ui.set_confirm_name(track_title(path).into());
            ui.set_confirm_path(path.display().to_string().into());
        }
        ui.set_error_visible(self.error.is_some());
        if let Some(error) = &self.error {
            ui.set_error_text(elide_chars(error, 160).into());
        }

        // Sidebar UI: rebuild the track model on change; auto-scroll to the
        // track that just started playing.
        ui.set_track_count(self.playlist.len() as i32);
        let filtered = self.filtered_indices();
        let list_h = ui.get_library_viewport_h();
        let row_h = style.get_row_h();
        let content_h = filtered.len() as f32 * row_h;
        let scroll_max = (content_h - list_h).max(0.0);
        ui.set_scroll_max(scroll_max);
        if let Some(index) = self.auto_scroll_target.take() {
            if let Some(row) = filtered
                .iter()
                .position(|&filtered| filtered == index)
            {
                self.scroll_y = ((row as f32 * row_h - list_h * 0.5).clamp(0.0, scroll_max)) as f32;
            }
        }
        self.scroll_y = self.scroll_y.clamp(0.0, scroll_max);
        ui.set_scroll_y(self.scroll_y);
        let current_cell = if filtered.is_empty() {
            -1
        } else {
            self.current
                .and_then(|current| filtered.iter().position(|&index| index == current))
                .map(|row| {
                    let cells = ((list_h / style.get_scroll_cell_h()).floor().max(1.0)) as i32;
                    (((row as f32 + 0.5) / filtered.len() as f32) * cells as f32)
                        .floor()
                        .min((cells - 1) as f32) as i32
                })
                .unwrap_or(-1)
        };
        ui.set_current_cell(current_cell);
        ui.set_selected_index(self.selected_track.map_or(-1, |index| index as i32));

        if self.track_ui_dirty {
            self.track_ui_dirty = false;
            let mut model = slint::VecModel::<TrackRow>::default();
            for &index in &filtered {
                let track = &self.playlist[index];
                // Only the current track is inverted. Mouse hover and the
                // clicked row use outlines without rebuilding the model.
                let selected = self.current == Some(index);
                let playing = self.current == Some(index) && active;
                model.push(TrackRow {
                    index: index as i32,
                    title: track.title.clone().into(),
                    meta: extension_label(&track.path).into(),
                    has_lrc: track.lrc.is_some(),
                    selected,
                    playing,
                    focused: self.selected_track == Some(index),
                });
            }
            ui.set_tracks(slint::ModelRc::from(Rc::new(model)));
        }

        self.open_context_menu(ui);

        // Theme panel data.
        if self.theme_ui_dirty {
            self.theme_ui_dirty = false;
            self.push_theme_models(ui);
        }

        self.push_lyrics(ui, position);
    }

    fn push_theme_models(&mut self, ui: &Main) {
        let auto = self.state.auto_theme;
        let auto_mood = self.state.auto_mood.unwrap_or_default();
        let current = auto.map(|_| None).unwrap_or_else(|| self.state.theme_name.as_deref().and_then(|name| self.library.find(name)));
        let current_entry = current.and_then(|index| self.library.entry(index));

        let current_line = match auto {
            Some(dark) => format!(
                "CURRENT: AUTO {} / {}",
                if dark { "DARK" } else { "LIGHT" },
                auto_mood.kind.label()
            ),
            None => {
                let category = current_entry.map_or("-", |entry| entry.category.label());
                format!(
                    "CURRENT: {} / {category}",
                    self.state.theme_name.as_deref().unwrap_or("-")
                )
            }
        };
        ui.set_theme_current_line(current_line.into());
        ui.set_theme_auto_status(if auto.is_some() {
            auto_status(auto, self.state.auto_mood)
        } else {
            String::new()
        }.into());
        ui.set_theme_file_path(crate::theme_library::ThemeLibrary::path().display().to_string().into());
        ui.set_theme_load_error(
            self.library
                .load_error()
                .map(|error| format!("THEMES.JSON ERROR / {error}"))
                .unwrap_or_default()
                .into(),
        );
        ui.set_theme_warnings(
            self.library
                .warnings()
                .first()
                .map(|first| {
                    let count = self.library.warnings().len();
                    let more = if count == 1 { String::new() } else { format!(" (+{} MORE)", count - 1) };
                    format!("SKIPPED {first}{more}")
                })
                .unwrap_or_default()
                .into(),
        );
        let customize = match (auto, current_entry) {
            (Some(_), _) => false,
            (None, Some(entry)) if entry.spec.is_some() => true,
            (None, Some(_)) => true,
            (None, None) => false,
        };
        ui.set_theme_can_customize(customize && self.library.editable());
        ui.set_theme_customize_label(
            if current_entry.is_some_and(|entry| entry.spec.is_some()) {
                "[ EDIT ]"
            } else {
                "[ CUSTOMIZE ]"
            }
            .into(),
        );

        let chip_colors = |look: &Look| (rgba(look.gradient.map_or(look.palette.bg, |g| g[0]), 255), rgba(look.gradient.map_or(look.palette.bg, |g| g[3]), 255), rgba(look.palette.text_hi, 255), rgba(look.palette.text_dim, 255));
        let mut autos = slint::VecModel::<ChipData>::default();
        for (label, dark) in [("AUTO DARK", true), ("AUTO LIGHT", false)] {
            let look = auto_theme::look(auto_mood, dark);
            let (ga, gb, fg, dim) = chip_colors(&look);
            autos.push(ChipData {
                name: label.into(),
                grad_a: ga,
                grad_b: gb,
                fg,
                dim,
                active: auto == Some(dark),
                dark,
                index: 0,
                col: 0,
                row: 0,
            });
        }
        ui.set_auto_chips(slint::ModelRc::from(Rc::new(autos)));

        let mut groups = slint::VecModel::<ChipGroup>::default();
        let entries = self.library.entries();
        // Match the theme grid's row stride, columns and spacing in Style.
        let style = ui.global::<Style>();
        let columns = style.get_theme_columns() as usize;
        let mut content_h = style.get_theme_auto_label_h()
            + style.get_theme_gap() + style.get_chip_h();
        for category in Category::ALL {
            let members: Vec<usize> = (0..entries.len())
                .filter(|&index| entries[index].category == category)
                .collect();
            if members.is_empty() && category != Category::Custom {
                continue;
            }
            let mut chips = slint::VecModel::<ChipData>::default();
            let (dark, light): (Vec<usize>, Vec<usize>) = members
                .iter()
                .partition(|&&index| entries[index].look.palette.dark);
            let rows: Vec<&[usize]> = if members.len() <= columns {
                vec![&members[..]]
            } else {
                dark.chunks(columns).chain(light.chunks(columns)).collect()
            };
            for (row, row_members) in rows.iter().enumerate() {
                for (col, &index) in row_members.iter().enumerate() {
                    let entry = &entries[index];
                    let (ga, gb, fg, dim) = chip_colors(&entry.look);
                    chips.push(ChipData {
                        name: entry.name.clone().into(),
                        grad_a: ga,
                        grad_b: gb,
                        fg,
                        dim,
                        active: current == Some(index),
                        dark: entry.look.palette.dark,
                        index: index as i32,
                        col: col as i32,
                        row: row as i32,
                    });
                }
            }
            groups.push(ChipGroup {
                label: category.label().into(),
                rows: rows.len() as i32,
                chips: slint::ModelRc::from(Rc::new(chips)),
            });
            content_h += style.get_theme_gap() + style.get_theme_group_label_h()
                + rows.len() as f32 * style.get_theme_row_step();
        }
        ui.set_theme_groups(slint::ModelRc::from(Rc::new(groups)));
        ui.set_theme_content_h(content_h);
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

fn rgba(rgb: Rgb, a: u8) -> slint::Color {
    slint::Color::from_argb_u8(a, rgb[0], rgb[1], rgb[2])
}

fn extension_label(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_uppercase())
        .unwrap_or_else(|| "AUDIO".to_owned())
}

fn format_gain(gain: f32) -> String {
    let rounded = (gain * 10.0).round() / 10.0;
    if rounded == 0.0 {
        "+0.0".to_owned()
    } else if rounded > 0.0 {
        format!("+{rounded:.1}")
    } else {
        format!("{rounded:.1}")
    }
}

fn format_frequency(freq: f32) -> String {
    if freq >= 1000.0 {
        format!("{:.1}K", freq / 1000.0)
    } else {
        format!("{:.0}", freq)
    }
}

fn elide_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max_chars).collect();
        format!("{cut}...")
    }
}

fn auto_status(dark: Option<bool>, mood: Option<Mood>) -> String {
    let mood = mood.unwrap_or_default();
    match dark {
        Some(dark) => format!(
            "AUTO {} / {}",
            if dark { "DARK" } else { "LIGHT" },
            mood.kind.label()
        ),
        None => String::new(),
    }
}

/// Fill the dock's seek row: the meter takes whatever width the time label leaves
/// in `dock-content-w`. Glyph strings and the cell count share the style's advance.
fn set_seek_meter(ui: &Main, time_text: &str, shown: f32) {
    let style = ui.global::<Style>();
    let cell_w = style.get_dock_cell_w();
    let time_w = time_text.chars().count() as f32 * cell_w;
    let cells = (((ui.get_dock_content_w() - style.get_dock_time_gap() - time_w)
        / cell_w).floor() as i32 - 2)
        .clamp(style.get_dock_seek_min_cells(), style.get_dock_seek_max_cells()) as usize;
    let filled = ((shown * cells as f32).round() as usize).min(cells);
    ui.set_seek_cells(cells as i32);
    ui.set_seek_filled(filled as i32);
    ui.set_seek_filled_str("#".repeat(filled).into());
    ui.set_seek_rest_str("-".repeat(cells - filled).into());
    ui.set_time_text(time_text.into());
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

thread_local! {
    /// The currently open track context menu: (playlist index, x, y).
    static CONTEXT_SLOT: RefCell<Option<(usize, f32, f32)>> = const { RefCell::new(None) };
}

fn with_player<R>(body: impl FnOnce(&mut Player) -> R) -> Option<R> {
    PLAYER.with(|cell| {
        let mut guard = cell.borrow_mut();
        guard.as_mut().map(body)
    })
}

pub fn run(initial: Option<PathBuf>) {
    // Opt-in offscreen render probe: no native window, audio or saved state.
    if let Some(dir) = std::env::var_os("SONADIAL_RENDER_PREVIEW") {
        preview::render(Path::new(&dir)).expect("render UI previews");
        return;
    }
    register_fonts();

    let ui = Main::new().expect("slint window");

    let mut player = Player::new(initial);

    ui.on_toggle_play(|| {
        with_player(|player| {
            player.toggle_play();
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
    ui.on_stop(|| {
        with_player(|player| player.stop());
    });
    ui.on_seek(|code| {
        with_player(|player| match code {
            code if code < 0.0 => {
                // Arrow keys arrive as sentinel codes: -1 = back, -2 = forward.
                player.seek_relative(if code == -1.0 { -SEEK_STEP } else { SEEK_STEP });
                player.seek_preview = None;
            }
            fraction => {
                let duration = player.duration();
                if duration > 0.0 {
                    let target = (fraction.clamp(0.0, 1.0) as f64) * duration;
                    if player.engine.seek(target) {
                        player.mark_lyric_seek(target);
                    }
                }
                player.seek_preview = None;
            }
        });
    });
    ui.on_seek_preview(|fraction| {
        with_player(|player| player.seek_preview = Some(fraction.clamp(0.0, 1.0)));
    });
    ui.on_set_volume(|code| {
        with_player(|player| match code {
            -1.0 => player.adjust_volume(VOLUME_STEP),
            -2.0 => player.adjust_volume(-VOLUME_STEP),
            value => player.set_volume(value),
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
    ui.on_toggle_sidebar(|| {
        with_player(|player| {
            player.sidebar_open = !player.sidebar_open;
            player.stage_dirty = true;
        });
    });
    ui.on_minimize(|| {
        with_player(|player| {
            if let Some(ui) = player.ui_weak.upgrade() { ui.window().set_minimized(true); }
        });
    });
    ui.on_toggle_maximize(|| {
        with_player(|player| {
            if let Some(ui) = player.ui_weak.upgrade() {
                // Native title-bar gestures and OS window management can
                // change this state without going through our button.
                let window = ui.window();
                window.set_maximized(!window.is_maximized());
                player.stage_dirty = true;
            }
        });
    });
    ui.on_close_app(|| {
        with_player(|player| {
            player.persist();
            if let Some(ui) = player.ui_weak.upgrade() {
                slint::quit_event_loop().ok();
                ui.hide().ok();
            }
        });
    });
    ui.on_toggle_settings(|| { with_player(|player| player.toggle_overlay(0)); });
    ui.on_toggle_eq(|| { with_player(|player| player.toggle_overlay(1)); });
    ui.on_open_download(|| { with_player(|player| player.toggle_overlay(2)); });
    ui.on_toggle_visual_mode(|| { with_player(|player| player.cycle_visual_mode()); });
    ui.on_play_index(|index| {
        with_player(|player| {
            let index = index as usize;
            player.selected_track = Some(index);
            player.play_index(index);
            player.close_context();
            player.save_if_due();
        });
    });
    ui.on_select_track(|index| {
        with_player(|player| {
            player.selected_track = Some(index as usize);
            // No model rebuild here: recreating the row items between the two
            // clicks breaks Slint's double-click detection. The highlight is
            // driven by the selected-index property instead.
        });
    });
    ui.on_queue_next(|index| {
        with_player(|player| {
            player.queue_next(index as usize);
            player.close_context();
        });
    });
    ui.on_import_lrc(|index| {
        with_player(|player| {
            player.import_lrc(index as usize);
            player.close_context();
        });
    });
    ui.on_clear_lrc(|index| {
        with_player(|player| {
            player.clear_lrc(index as usize);
            player.close_context();
        });
    });
    ui.on_open_track_location(|index| {
        with_player(|player| {
            player.open_track_location(index as usize);
            player.close_context();
        });
    });
    ui.on_remove_track(|index| {
        with_player(|player| {
            player.remove_track_at(index as usize, false);
            player.close_context();
        });
    });
    ui.on_delete_track(|index| {
        with_player(|player| {
            let index = index as usize;
            player.pending_delete = player.playlist.get(index).map(|track| track.path.clone());
            player.close_context();
        });
    });
    ui.on_confirm_delete(|| {
        with_player(|player| {
            if let Some(path) = player.pending_delete.take() {
                if let Some(index) = player
                    .playlist
                    .iter()
                    .position(|track| track.path == path)
                {
                    player.remove_track_at(index, true);
                }
            }
        });
    });
    ui.on_cancel_delete(|| {
        with_player(|player| player.pending_delete = None);
    });
    ui.on_open_context(|index, x, y| {
        with_player(|player| {
            let index = index as usize;
            player.selected_track = Some(index);
            player.track_ui_dirty = true;
            CONTEXT_SLOT.with(|cell| {
                *cell.borrow_mut() = Some((index, x, y));
            });
        });
    });
    ui.on_search_changed(|text| {
        with_player(|player| {
            player.search = text.to_string();
            player.track_ui_dirty = true;
            player.auto_scroll_target = None;
            player.scroll_y = 0.0;
        });
    });
    ui.on_reveal_current(|| {
        with_player(|player| {
            if let Some(index) = player.current {
                if !player.sidebar_open {
                    player.sidebar_open = true;
                    player.stage_dirty = true;
                }
                player.auto_scroll_target = Some(index);
            }
        });
    });
    ui.on_scroll_to(|offset| {
        with_player(|player| player.scroll_y = offset as f32);
    });
    ui.on_set_style(|style| { with_player(|player| player.set_style(style as u8)); });
    ui.on_toggle_bg_spectrum(|| {
        with_player(|player| {
            player.state.show_bg_spectrum = !player.state.show_bg_spectrum;
            player.stage_dirty = true;
            player.dirty = true;
        });
    });
    ui.on_toggle_radial(|| {
        with_player(|player| {
            player.state.show_radial = !player.state.show_radial;
            player.stage_dirty = true;
            player.dirty = true;
        });
    });
    ui.on_toggle_core_pulse(|| {
        with_player(|player| {
            player.state.show_core_pulse = !player.state.show_core_pulse;
            player.stage_dirty = true;
            player.dirty = true;
        });
    });
    ui.on_toggle_lyrics(|| {
        with_player(|player| {
            player.state.show_lyrics = !player.state.show_lyrics;
            player.dirty = true;
        });
    });
    ui.on_toggle_peak_hold(|| {
        with_player(|player| {
            let flag = player.peak_hold_flag();
            *flag = !*flag;
            player.stage_dirty = true;
            player.dirty = true;
        });
    });
    ui.on_adjust_radial_scale(|delta| {
        with_player(|player| {
            player.state.radial_scale = (player.state.radial_scale + delta)
                .clamp(0.7, 1.2);
            player.stage_dirty = true;
            player.dirty = true;
        });
    });
    ui.on_select_theme(|index| {
        with_player(|player| {
            let index = index as usize;
            if let Some(entry) = player.library.entry(index) {
                player.state.theme = entry.builtin;
                player.state.theme_name = Some(entry.name.clone());
                player.state.auto_theme = None;
                player.fade = None;
                player.look = entry.look;
                theme::set_look(entry.look);
                player.stage_dirty = true;
                player.theme_ui_dirty = true;
                player.dirty = true;
            }
        });
    });
    ui.on_select_auto_theme(|dark| {
        with_player(|player| {
            player.state.auto_theme = Some(dark);
            let mood = player.state.auto_mood.unwrap_or_default();
            player.look = auto_theme::look(mood, dark);
            theme::set_look(player.look);
            player.stage_dirty = true;
            player.theme_ui_dirty = true;
            player.dirty = true;
        });
    });
    ui.on_reload_themes(|| {
        with_player(|player| {
            player.library.reload();
            if let Some(name) = player.state.theme_name.clone() {
                if player.library.find(&name).is_none() {
                    player.state.theme_name = None;
                    player.state.theme = Some(0);
                }
            }
            player.look = player.resolve_look();
            theme::set_look(player.look);
            player.stage_dirty = true;
            player.theme_ui_dirty = true;
        });
    });
    ui.on_customize_theme(|| {
        with_player(|player| {
            let source = player
                .state
                .theme_name
                .as_deref()
                .and_then(|name| player.library.find(name));
            if let Some(index) = source {
                if let Some(new_index) = player.library.duplicate(index) {
                    if let Some(entry) = player.library.entry(new_index) {
                        player.state.theme = entry.builtin;
                        player.state.theme_name = Some(entry.name.clone());
                        player.state.auto_theme = None;
                        player.look = entry.look;
                        theme::set_look(entry.look);
                        player.stage_dirty = true;
                        player.theme_ui_dirty = true;
                        player.error =
                            Some("THEME EDITOR OPENS IN THE NEXT MILESTONE / COPY SAVED"
                                .to_owned());
                    }
                }
            }
        });
    });
    ui.on_open_data_dir(|| {
        with_player(|_player| {
            let _ = std::process::Command::new("explorer")
                .arg(storage::data_dir())
                .spawn();
        });
    });
    ui.on_set_band(|index, gain| {
        with_player(|player| player.set_eq_band(index as usize, gain));
    });
    ui.on_cycle_eq_preset(|delta| {
        with_player(|player| player.cycle_eq_preset(delta));
    });
    ui.on_toggle_eq_enabled(|| {
        with_player(|player| {
            player.state.eq_enabled = !player.state.eq_enabled;
            player.apply_eq();
            player.eq_ui_dirty = true;
        });
    });
    ui.on_reset_eq(|| { with_player(|player| player.reset_eq()); });
    ui.on_start_download(|| { with_player(|player| player.start_download()); });
    ui.on_cancel_download(|| { with_player(|player| player.cancel_download()); });
    ui.on_open_download_dir(|| {
        with_player(|_player| {
            let dir = storage::download_dir();
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::process::Command::new("explorer").arg(dir).spawn();
        });
    });
    ui.on_use_env_proxy(|| {
        with_player(|player| {
            if let Some(env) = player.env_proxy.clone() {
                player.state.proxy = env;
                player.dirty = true;
            }
        });
    });
    ui.on_set_download_url(|text| {
        with_player(|player| player.download_url = text.to_string());
    });
    ui.on_set_proxy(|text| {
        with_player(|player| {
            player.state.proxy = text.to_string();
            player.dirty = true;
        });
    });
    ui.on_clear_error(|| { with_player(|player| player.error = None); });

    {
        ui.set_download_url(player.download_url.clone().into());
        ui.set_proxy_text(player.state.proxy.clone().into());
        ui.set_state_path(storage::state_path().display().to_string().into());
        ui.set_download_out_dir(storage::download_dir().display().to_string().into());
        let mut shortcuts = slint::VecModel::<ShortcutRow>::default();
        for (index, (keys, desc)) in [
            ("SPACE", "播放 / 暂停"),
            ("UP / DOWN", "音量 + / -"),
            ("LEFT / RIGHT", "后退 / 前进 5 秒"),
            ("PGUP / PGDN", "上一首 / 下一首"),
            ("CTRL + LEFT/RIGHT", "上一首 / 下一首"),
            ("CTRL + O", "添加音频"),
            ("RIGHT-CLICK", "播放 / 导入 LRC / 删除"),
            ("ESC", "关闭弹窗"),
        ]
        .iter()
        .enumerate()
        {
            shortcuts.push(ShortcutRow {
                index: index as i32,
                keys: (*keys).into(),
                desc: (*desc).into(),
            });
        }
        ui.set_shortcuts(slint::ModelRc::from(Rc::new(shortcuts)));
    }

    let weak = ui.as_weak();
    player.ui_weak = weak.clone();

    // Debug helper: SONADIAL_DEBUG_UI=sys|sys2|eq|bili opens an overlay at
    // startup so the panels can be inspected (and screenshot) headlessly.
    match std::env::var("SONADIAL_DEBUG_UI").as_deref() {
        Ok("sys") => player.show_settings = true,
        Ok("sys2") => {
            player.show_settings = true;
            ui.set_settings_page(2);
        }
        Ok("eq") => player.show_eq = true,
        Ok("bili") => player.show_download = true,
        _ => {}
    }

    PLAYER.with(|cell| *cell.borrow_mut() = Some(player));
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
    // Tear down audio while the process is still fully alive. Dropping the
    // cpal stream during TLS shutdown panics: Stream::drop joins its event
    // thread, which no longer answers once LdrShutdownProcess begins.
    PLAYER.with(|cell| {
        let mut guard = cell.borrow_mut();
        if let Some(mut player) = guard.take() {
            player.persist();
            player.engine.stop();
            // `player` drops here, with the event loop still healthy.
        }
    });
}

/// Register the bundled TUI/pixel fonts with Slint's font collection so the
/// .slint font-family names resolve. Uses the unstable fontique hook, same as
/// the wasm gallery demo does for its downloaded CJK font.
fn register_fonts() {
    // Prepend in reverse priority for Han: SC, then Japanese, then Korean.
    // Kana and Hangul each get their own face first.
    const FONTS: &[(&[u8], &[&str])] = &[
        (include_bytes!("../assets/fonts/FusionPixel12-Mono-KO.ttf"), &["Hang", "Hani"]),
        (include_bytes!("../assets/fonts/FusionPixel12-Mono-JA.ttf"), &["Hira", "Kana", "Hani"]),
        (include_bytes!("../assets/fonts/FusionPixel12-Mono-CJK.ttf"), &["Hani"]),
        (include_bytes!("../assets/fonts/FusionPixel12-Mono-TUI.ttf"), &[]),
        (include_bytes!("../assets/fonts/SarasaMonoSC-TUI.ttf"), &[]),
    ];
    use slint::fontique_011::fontique;
    let mut collection = slint::fontique_011::shared_collection();
    for (data, scripts) in FONTS {
        let blob = fontique::Blob::new(Arc::new((*data).to_vec()));
        for (family_id, _infos) in collection.register_fonts(blob, None) {
            // Appending after system fallbacks lets Windows choose a smooth
            // CJK font before the bundled pixel face is ever considered.
            for script in *scripts {
                let key = fontique::FallbackKey::new(
                    fontique::Script::from_str_unchecked(script),
                    None,
                );
                let existing: Vec<_> = collection.fallback_families(key).collect();
                collection.set_fallbacks(key, std::iter::once(family_id).chain(existing));
            }
        }
    }
}
