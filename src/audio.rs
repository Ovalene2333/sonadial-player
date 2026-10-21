use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

use crate::eq::{EqParams, EqProcessor, NUM_BANDS};

// Short gain ramp used for starts, pauses, seeks and track changes. It is long
// enough to remove clicks but still feels immediate.
const FADE_SECS: f32 = 0.12;

// The output callback acknowledges a seek within a few milliseconds. If the
// device stops calling back (unplugged, driver wedged) the decoder must not
// wait forever for that acknowledgement.
const SEEK_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub path: PathBuf,
    pub duration: f64,
    pub sample_rate: u32,
    pub channels: usize,
}

#[derive(Debug)]
enum Command {
    Seek {
        target: f64,
        epoch: u64,
        at_end: bool,
    },
    Stop,
}

struct Shared {
    paused: AtomicBool,
    volume_bits: AtomicU32,
    position_frames: AtomicU64,
    // A seek stops production, asks the callback to flush, then resumes only
    // after that flush is acknowledged. New samples must never be cleared.
    seek_epoch: AtomicU64,
    seek_ready_epoch: AtomicU64,
    seek_flushed_epoch: AtomicU64,
    seek_position_frames: AtomicU64,
    /// Epoch of the latest seek the demuxer refused. The callback adopts it
    /// without discarding buffered audio.
    seek_cancelled_epoch: AtomicU64,
    done: AtomicBool,
    stop: AtomicBool,
    out_rate: u32,
    eq: Mutex<Arc<EqParams>>,
    /// Non-fatal playback notices (for example a rejected seek). Fatal decoder
    /// errors still go through `AudioEngine::error`.
    notice: Mutex<Option<(u64, String)>>,
}

pub struct AudioEngine {
    stream: Option<cpal::Stream>,
    shared: Arc<Shared>,
    cmd_tx: Option<Sender<Command>>,
    decoder: Option<JoinHandle<()>>,
    analysis: Option<HeapCons<f32>>,
    info: Option<TrackInfo>,
    error: Arc<Mutex<Option<String>>>,
    device_name: String,
    current_path: Option<PathBuf>,
    out_rate: u32,
    out_channels: usize,
    eq_gains: [f32; NUM_BANDS],
    eq_enabled: bool,
    pending_seek: Option<(u64, f64)>,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine {
    pub fn new() -> Self {
        Self {
            stream: None,
            shared: Arc::new(Shared {
                paused: AtomicBool::new(true),
                volume_bits: AtomicU32::new(0.78_f32.to_bits()),
                position_frames: AtomicU64::new(0),
                seek_epoch: AtomicU64::new(0),
                seek_ready_epoch: AtomicU64::new(0),
                seek_flushed_epoch: AtomicU64::new(0),
                seek_position_frames: AtomicU64::new(0),
                seek_cancelled_epoch: AtomicU64::new(0),
                done: AtomicBool::new(true),
                stop: AtomicBool::new(true),
                out_rate: 48_000,
                eq: Mutex::new(Arc::new(EqParams::flat(48_000))),
                notice: Mutex::new(None),
            }),
            cmd_tx: None,
            decoder: None,
            analysis: None,
            info: None,
            error: Arc::new(Mutex::new(None)),
            device_name: "NO DEVICE".to_owned(),
            current_path: None,
            out_rate: 48_000,
            out_channels: 2,
            eq_gains: [0.0; NUM_BANDS],
            eq_enabled: true,
            pending_seek: None,
        }
    }

    pub fn play(&mut self, path: &Path) -> Result<()> {
        self.play_with(path, false)
    }

    /// Open the track with the output parked: the decoder fills its buffer but
    /// nothing is audible until `resume()` is called. Used when the user seeks
    /// while nothing is playing.
    pub fn play_paused(&mut self, path: &Path) -> Result<()> {
        self.play_with(path, true)
    }

    fn play_with(&mut self, path: &Path, start_paused: bool) -> Result<()> {
        self.stop();

        let info = probe(path).with_context(|| format!("无法解析 {}", path.display()))?;

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("没有可用的音频输出设备"))?;
        self.device_name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| "DEFAULT OUTPUT".to_owned());

        let supported = pick_f32_config(&device)?;
        self.out_rate = supported.sample_rate();
        self.out_channels = supported.channels() as usize;
        let mut config: StreamConfig = supported.into();
        // A short fixed WASAPI buffer keeps the driver's allocation small while
        // staying well above the decoder's refill latency.
        config.buffer_size = cpal::BufferSize::Fixed(512);

        let eq_params = Arc::new(EqParams::new(
            &self.eq_gains,
            self.eq_enabled,
            self.out_rate,
        ));
        let shared = Arc::new(Shared {
            paused: AtomicBool::new(start_paused),
            volume_bits: AtomicU32::new(self.volume().to_bits()),
            position_frames: AtomicU64::new(0),
            seek_epoch: AtomicU64::new(0),
            seek_ready_epoch: AtomicU64::new(0),
            seek_flushed_epoch: AtomicU64::new(0),
            seek_position_frames: AtomicU64::new(0),
            seek_cancelled_epoch: AtomicU64::new(0),
            done: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            out_rate: self.out_rate,
            eq: Mutex::new(eq_params.clone()),
            notice: Mutex::new(None),
        });
        *self.error.lock().unwrap() = None;

        // Two seconds of output is enough headroom for decoder hiccups and
        // saves a megabyte over the old three-second buffer.
        let capacity = (self.out_rate as usize * self.out_channels * 2).max(16_384);
        let (out_prod, mut out_cons) = HeapRb::<f32>::new(capacity).split();
        let (analysis_prod, analysis_cons) =
            HeapRb::<f32>::new(crate::spectrum::ANALYSIS_RATE as usize).split();

        let cb_shared = shared.clone();
        let cb_error = self.error.clone();
        let channels = self.out_channels;
        let mut analysis_resampler =
            MonoResampler::new(self.out_rate, crate::spectrum::ANALYSIS_RATE);
        let mut analysis_prod = analysis_prod;
        let mut last_epoch = 0_u64;
        let mut eq_snapshot = eq_params;
        let mut eq_processor = EqProcessor::new();
        // Per-stream output envelope. Starts at silence and follows the
        // requested pause/volume state one sample at a time.
        let mut gain = 0.0_f32;
        let stream = device
            .build_output_stream(
                config,
                move |data: &mut [f32], _| {
                    data.fill(0.0);

                    let epoch = cb_shared.seek_ready_epoch.load(Ordering::Acquire);
                    if epoch > last_epoch {
                        out_cons.clear();
                        eq_processor = EqProcessor::new();
                        analysis_resampler.reset();
                        gain = 0.0;
                        cb_shared.position_frames.store(
                            cb_shared.seek_position_frames.load(Ordering::Relaxed),
                            Ordering::Relaxed,
                        );
                        last_epoch = epoch;
                        cb_shared.seek_flushed_epoch.store(epoch, Ordering::Release);
                    }
                    let requested = cb_shared.seek_epoch.load(Ordering::Acquire);
                    if requested != last_epoch {
                        if cb_shared.seek_cancelled_epoch.load(Ordering::Acquire) == requested {
                            // The demuxer refused this seek, so the queued
                            // audio and the clock are both still valid: resume
                            // without flushing anything.
                            last_epoch = requested;
                        } else {
                            return;
                        }
                    }

                    let paused = cb_shared.paused.load(Ordering::Relaxed);
                    if paused && gain <= 0.0001 {
                        return;
                    }

                    if let Ok(params) = cb_shared.eq.try_lock() {
                        eq_snapshot = params.clone();
                    }
                    let volume = f32::from_bits(cb_shared.volume_bits.load(Ordering::Relaxed));
                    let target_gain = if paused { 0.0 } else { volume };
                    let fade_step = 1.0 / (FADE_SECS * cb_shared.out_rate.max(1) as f32).max(1.0);
                    let mut produced = 0_u64;

                    for frame in data.chunks_mut(channels) {
                        if paused && gain <= 0.0001 {
                            break;
                        }
                        if out_cons.occupied_len() < 2 {
                            break;
                        }
                        let mut left = out_cons.try_pop().unwrap_or(0.0);
                        let mut right = out_cons.try_pop().unwrap_or(0.0);
                        eq_processor.process(&mut left, &mut right, &eq_snapshot);
                        analysis_resampler.push(left, &mut analysis_prod);

                        if gain < target_gain {
                            gain = (gain + fade_step).min(target_gain);
                        } else if gain > target_gain {
                            gain = (gain - fade_step).max(target_gain);
                        }

                        if channels == 1 {
                            frame[0] = (left + right) * 0.5 * gain;
                        } else {
                            frame[0] = left * gain;
                            frame[1] = right * gain;
                        }

                        produced += 1;
                    }

                    if produced > 0 {
                        cb_shared
                            .position_frames
                            .fetch_add(produced, Ordering::Relaxed);
                    }
                },
                move |err| {
                    *cb_error.lock().unwrap() = Some(err.to_string());
                },
                None,
            )
            .context("无法打开音频输出流")?;

        stream.play().context("无法启动音频输出流")?;

        let (tx, rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let thread_error = self.error.clone();
        let thread_path = path.to_owned();
        let decoder = thread::Builder::new()
            .name("sonadial-decoder".to_owned())
            .stack_size(512 * 1024)
            .spawn(move || {
                decode_loop(thread_path, thread_shared, out_prod, rx, thread_error);
            })
            .context("无法启动解码线程")?;

        self.stream = Some(stream);
        self.shared = shared;
        self.cmd_tx = Some(tx);
        self.decoder = Some(decoder);
        self.analysis = Some(analysis_cons);
        self.info = Some(info);
        self.current_path = Some(path.to_owned());
        Ok(())
    }

    pub fn follow_default_output(&mut self) -> Result<bool> {
        let host = cpal::default_host();
        let Some(device) = host.default_output_device() else {
            return Ok(false);
        };
        let name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| "DEFAULT OUTPUT".to_owned());
        if name == self.device_name {
            return Ok(false);
        }

        self.device_name = name;
        let Some(path) = self.current_path.clone() else {
            return Ok(true);
        };
        let position = self.position();
        let paused = self.is_paused();
        let volume = self.volume();

        self.play(&path)?;
        self.seek(position);
        if paused {
            self.pause();
        } else {
            self.resume();
        }
        self.set_volume(volume);
        Ok(true)
    }

    pub fn stop(&mut self) {
        let had_stream = self.stream.is_some();
        if let Some(tx) = self.cmd_tx.take() {
            self.shared.paused.store(true, Ordering::Relaxed);
            self.shared.stop.store(true, Ordering::Relaxed);
            let _ = tx.send(Command::Stop);
        }
        // Let the output callback ramp the last buffered samples down before
        // the stream is dropped, so stopping does not produce a click.
        if had_stream {
            thread::sleep(Duration::from_secs_f32(FADE_SECS));
        }
        if let Some(handle) = self.decoder.take() {
            let _ = handle.join();
        }
        self.stream = None;
        self.analysis = None;
        self.info = None;
        self.current_path = None;
        self.pending_seek = None;
        self.shared.paused.store(true, Ordering::Relaxed);
        self.shared.done.store(true, Ordering::Relaxed);
        self.shared.position_frames.store(0, Ordering::Relaxed);
    }

    pub fn toggle_pause(&mut self) -> bool {
        if self.info.is_none() {
            return false;
        }
        let paused = !self.shared.paused.load(Ordering::Relaxed);
        self.shared.paused.store(paused, Ordering::Relaxed);
        !paused
    }

    pub fn pause(&mut self) {
        self.shared.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&mut self) {
        self.shared.paused.store(false, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    pub fn seek(&mut self, seconds: f64) -> bool {
        let Some(tx) = &self.cmd_tx else {
            return false;
        };
        let duration = self.duration();
        let target = seconds.clamp(0.0, duration.max(0.0));
        let epoch = self.shared.seek_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        let at_end = duration > 0.0 && target >= duration;
        if tx
            .send(Command::Seek {
                target,
                epoch,
                at_end,
            })
            .is_err()
        {
            *self.error.lock().unwrap() = Some("音频解码线程已停止，无法跳转".to_owned());
            return false;
        }
        self.shared.done.store(false, Ordering::Relaxed);
        self.pending_seek = Some((epoch, target));
        true
    }

    pub fn set_volume(&self, volume: f32) {
        self.shared
            .volume_bits
            .store(volume.clamp(0.0, 1.25).to_bits(), Ordering::Relaxed);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.shared.volume_bits.load(Ordering::Relaxed))
    }

    pub fn set_eq(&mut self, gains: &[f32; NUM_BANDS], enabled: bool) {
        self.eq_gains = *gains;
        self.eq_enabled = enabled;
        let params = Arc::new(EqParams::new(gains, enabled, self.out_rate));
        if let Ok(mut slot) = self.shared.eq.lock() {
            *slot = params;
        }
    }

    pub fn position(&self) -> f64 {
        if let Some((epoch, target)) = self.pending_seek {
            if self.shared.seek_flushed_epoch.load(Ordering::Acquire) != epoch {
                return target;
            }
        }
        self.shared.position_frames.load(Ordering::Relaxed) as f64
            / self.shared.out_rate.max(1) as f64
    }

    pub fn duration(&self) -> f64 {
        self.info.as_ref().map(|i| i.duration).unwrap_or(0.0)
    }

    pub fn info(&self) -> Option<&TrackInfo> {
        self.info.as_ref()
    }

    pub fn is_finished(&self) -> bool {
        let resolved = self
            .shared
            .seek_flushed_epoch
            .load(Ordering::Acquire)
            .max(self.shared.seek_cancelled_epoch.load(Ordering::Acquire));
        self.info.is_some()
            && !self.is_paused()
            && self.shared.seek_epoch.load(Ordering::Acquire) == resolved
            && self.shared.done.load(Ordering::Relaxed)
            && self.position() >= (self.duration() - 0.35).max(0.0)
    }

    pub fn drain_analysis(&mut self, out: &mut Vec<f32>, max: usize) {
        out.clear();
        if let Some(consumer) = &mut self.analysis {
            while out.len() < max {
                match consumer.try_pop() {
                    Some(sample) => out.push(sample),
                    None => break,
                }
            }
        }
    }

    pub fn flush_analysis(&mut self) {
        if let Some(consumer) = &mut self.analysis {
            while consumer.try_pop().is_some() {}
        }
    }

    pub fn take_error(&mut self) -> Option<String> {
        let error = self.error.lock().unwrap().take();
        if error.is_some() {
            // A dead decoder can never acknowledge `pending_seek`; drop it so
            // the UI falls back to the shared clock instead of reporting a
            // target that will never be reached.
            self.pending_seek = None;
        }
        error
    }

    /// Non-fatal playback notice, such as a seek the demuxer refused.
    pub fn take_notice(&mut self) -> Option<String> {
        let notice = self.shared.notice.lock().unwrap().take();
        if let Some((epoch, message)) = notice {
            // The rejected seek never reached the clock, so stop reporting its
            // target unless a newer seek has already been requested.
            if matches!(self.pending_seek, Some((pending, _)) if pending <= epoch) {
                self.pending_seek = None;
            }
            Some(message)
        } else {
            None
        }
    }

    pub fn output_label(&self) -> String {
        format!(
            "{} | {} CH | {} kHz",
            self.device_name,
            self.out_channels,
            self.out_rate as f32 / 1000.0
        )
    }
}

fn pick_f32_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig> {
    let default = device.default_output_config().context("无法读取输出配置")?;

    if let Ok(configs) = device.supported_output_configs() {
        let mut fallback = None;
        for config in configs.filter(|config| config.sample_format() == SampleFormat::F32) {
            if config.channels() == 2 {
                let rate = if config.min_sample_rate() <= 44_100
                    && config.max_sample_rate() >= 44_100
                {
                    44_100
                } else if config.min_sample_rate() <= 48_000 && config.max_sample_rate() >= 48_000 {
                    48_000
                } else {
                    config.max_sample_rate()
                };
                return Ok(config.with_sample_rate(rate));
            }
            if fallback.is_none() {
                fallback = Some(config.with_max_sample_rate());
            }
        }
        if default.sample_format() == SampleFormat::F32 {
            return Ok(default);
        }
        if let Some(config) = fallback {
            return Ok(config);
        }
    } else if default.sample_format() == SampleFormat::F32 {
        return Ok(default);
    }

    Err(anyhow!(
        "输出设备不支持 f32 采样（当前为 {:?}）",
        default.sample_format()
    ))
}

pub fn probe_track(path: &Path) -> Result<TrackInfo> {
    probe(path)
}

fn probe(path: &Path) -> Result<TrackInfo> {
    let file = Box::new(File::open(path)?);
    let mss = MediaSourceStream::new(file, Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let format = symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("文件中没有音频轨"))?;
    let codec = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or_else(|| anyhow!("无法读取音频参数"))?;

    let sample_rate = codec.sample_rate.unwrap_or(44_100);
    let channels = codec.channels.as_ref().map(|c| c.count()).unwrap_or(2);

    let duration = track
        .duration
        .and_then(|duration| track.time_base.map(|tb| tb.calc_duration(duration)))
        .flatten()
        .map(|time| time.as_secs_f64())
        .or_else(|| {
            track
                .num_frames
                .map(|frames| frames as f64 / sample_rate.max(1) as f64)
        })
        .unwrap_or(0.0);

    Ok(TrackInfo {
        path: path.to_owned(),
        duration,
        sample_rate,
        channels,
    })
}

fn decode_loop(
    path: PathBuf,
    shared: Arc<Shared>,
    mut out: HeapProd<f32>,
    rx: Receiver<Command>,
    error: Arc<Mutex<Option<String>>>,
) {
    let result = decode_inner(&path, &shared, &mut out, &rx);

    if let Err(err) = result {
        if !shared.stop.load(Ordering::Relaxed) {
            *error.lock().unwrap() = Some(err.to_string());
        }
    }
    shared.done.store(true, Ordering::Relaxed);
}

/// Wait until the output callback has discarded the pre-seek audio and rebased
/// the playback clock. Timing out means the device stopped calling back, which
/// the caller reports as a fatal error instead of hanging the decoder thread.
fn wait_for_seek_flush(shared: &Arc<Shared>, epoch: u64) -> Result<()> {
    let started = Instant::now();
    while shared.seek_flushed_epoch.load(Ordering::Acquire) != epoch {
        if shared.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if started.elapsed() >= SEEK_FLUSH_TIMEOUT {
            return Err(anyhow!("音频输出未响应跳转，已停止播放"));
        }
        thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

fn decode_inner(
    path: &Path,
    shared: &Arc<Shared>,
    out: &mut HeapProd<f32>,
    rx: &Receiver<Command>,
) -> Result<()> {
    let file = Box::new(File::open(path)?);
    let mss = MediaSourceStream::new(file, Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("文件中没有音频轨"))?;
    let track_id = track.id;
    let time_base = track.time_base;
    let codec = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .cloned()
        .ok_or_else(|| anyhow!("无法读取音频参数"))?;

    let input_rate = codec.sample_rate.unwrap_or(44_100);
    let input_channels = codec
        .channels
        .as_ref()
        .map(|c| c.count())
        .unwrap_or(2)
        .max(1);
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&codec, &AudioDecoderOptions::default())?;

    let mut resampler = LinearResampler::new(input_rate, shared.out_rate);
    let mut sample_buf: Vec<f32> = Vec::new();
    // Accurate seeks land on the packet before the requested time. Drop the
    // leading decoded frames so the audible stream really starts at `target`.
    let mut trim_frames = 0_u64;
    let mut epoch = 0_u64;
    let mut eof = false;

    'decode: loop {
        if shared.stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        loop {
            match rx.try_recv() {
                Ok(Command::Stop) => return Ok(()),
                Ok(Command::Seek {
                    target,
                    epoch: requested_epoch,
                    at_end,
                }) => {
                    if requested_epoch != shared.seek_epoch.load(Ordering::Acquire) {
                        continue;
                    }
                    let seek = if at_end {
                        // Some demuxers reject a timestamp exactly at EOF.
                        // Flushing to the end needs no packet from the file.
                        Ok(target)
                    } else {
                        let time = Time::try_from_secs_f64(target).unwrap_or(Time::ZERO);
                        format
                            .seek(
                                SeekMode::Accurate,
                                SeekTo::Time {
                                    time,
                                    track_id: Some(track_id),
                                },
                            )
                            .map(|seeked| {
                                time_base
                                    .and_then(|tb| tb.calc_time(seeked.actual_ts))
                                    .map(|time| time.as_secs_f64())
                                    .unwrap_or(target)
                                    .clamp(0.0, target)
                            })
                            .map_err(|err| anyhow!("无法跳转到指定播放位置：{err}"))
                    };

                    match seek {
                        Ok(actual) => {
                            trim_frames = ((target - actual) * input_rate as f64).round() as u64;
                            decoder.reset();
                            resampler.reset();
                            epoch = requested_epoch;
                            eof = at_end;
                            shared.done.store(eof, Ordering::Relaxed);
                            shared.seek_position_frames.store(
                                (target * shared.out_rate as f64).round() as u64,
                                Ordering::Relaxed,
                            );
                            shared.seek_ready_epoch.store(epoch, Ordering::Release);
                            // The callback owns both the queue consumer and the
                            // playback clock. It must discard old audio and
                            // rebase that clock before we may enqueue new audio.
                            wait_for_seek_flush(shared, epoch)?;
                            if shared.stop.load(Ordering::Relaxed) {
                                return Ok(());
                            }
                        }
                        Err(err) => {
                            // The demuxer stayed on the old packet, so the
                            // buffered audio is still valid. Tell the callback
                            // to resume without flushing and leave the clock
                            // alone; only the UI has to drop the seek target.
                            epoch = requested_epoch;
                            shared.seek_cancelled_epoch.store(epoch, Ordering::Release);
                            shared.done.store(eof, Ordering::Relaxed);
                            *shared.notice.lock().unwrap() = Some((epoch, err.to_string()));
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }

        if eof || epoch != shared.seek_epoch.load(Ordering::Acquire) {
            // Keep servicing seeks after EOF, including when the final output
            // buffer is still playing or playback is paused near the end.
            thread::sleep(Duration::from_millis(10));
            continue;
        }

        match format.next_packet() {
            Ok(Some(packet)) => {
                if packet.track_id != track_id {
                    continue;
                }

                match decoder.decode(&packet) {
                    Ok(audio) => {
                        sample_buf.resize(audio.samples_interleaved(), 0.0);
                        audio.copy_to_slice_interleaved(&mut sample_buf);

                        let frames = sample_buf.len() / input_channels;
                        let mut start = 0;
                        if trim_frames > 0 {
                            let drop = (trim_frames as usize).min(frames);
                            trim_frames -= drop as u64;
                            start = drop;
                        }
                        for frame in start..frames {
                            let base = frame * input_channels;
                            let left = sample_buf[base];
                            let right = if input_channels >= 2 {
                                sample_buf[base + 1]
                            } else {
                                left
                            };
                            if !resampler.push(left, right, out, shared, epoch) {
                                continue 'decode;
                            }
                        }
                    }
                    Err(SymError::DecodeError(_)) => continue,
                    Err(SymError::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => {
                        eof = true;
                        shared.done.store(true, Ordering::Relaxed);
                    }
                    Err(SymError::ResetRequired) => {
                        decoder.reset();
                    }
                    Err(err) => return Err(err.into()),
                }
            }
            Ok(None) => {
                eof = true;
                shared.done.store(true, Ordering::Relaxed);
            }
            Err(SymError::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => {
                eof = true;
                shared.done.store(true, Ordering::Relaxed);
            }
            Err(SymError::ResetRequired) => {
                decoder.reset();
            }
            Err(err) => return Err(err.into()),
        }
    }
}

struct LinearResampler {
    step: f64,
    next_pos: f64,
    frame_index: u64,
    prev: Option<(f32, f32)>,
    identity: bool,
}

impl LinearResampler {
    fn new(input_rate: u32, output_rate: u32) -> Self {
        let step = input_rate as f64 / output_rate.max(1) as f64;
        Self {
            step,
            next_pos: 0.0,
            frame_index: 0,
            prev: None,
            identity: (input_rate == output_rate),
        }
    }

    fn reset(&mut self) {
        self.next_pos = 0.0;
        self.frame_index = 0;
        self.prev = None;
    }

    fn push(
        &mut self,
        left: f32,
        right: f32,
        out: &mut HeapProd<f32>,
        shared: &Arc<Shared>,
        epoch: u64,
    ) -> bool {
        if self.identity {
            return push_stereo(out, left, right, shared, epoch);
        }

        let current_index = self.frame_index as f64;
        if let Some((prev_left, prev_right)) = self.prev {
            while self.next_pos <= current_index {
                let t = ((self.next_pos - (current_index - 1.0)).clamp(0.0, 1.0)) as f32;
                let out_left = prev_left + (left - prev_left) * t;
                let out_right = prev_right + (right - prev_right) * t;
                if !push_stereo(out, out_left, out_right, shared, epoch) {
                    return false;
                }
                self.next_pos += self.step;
            }
        } else {
            if !push_stereo(out, left, right, shared, epoch) {
                return false;
            }
            self.next_pos = self.step;
        }

        self.prev = Some((left, right));
        self.frame_index += 1;
        true
    }
}

struct MonoResampler {
    step: f64,
    next_pos: f64,
    frame_index: u64,
    prev: Option<f32>,
    identity: bool,
}

impl MonoResampler {
    fn new(input_rate: u32, output_rate: u32) -> Self {
        let step = input_rate as f64 / output_rate.max(1) as f64;
        Self {
            step,
            next_pos: 0.0,
            frame_index: 0,
            prev: None,
            identity: input_rate == output_rate,
        }
    }

    fn reset(&mut self) {
        self.next_pos = 0.0;
        self.frame_index = 0;
        self.prev = None;
    }

    fn push(&mut self, sample: f32, out: &mut HeapProd<f32>) {
        if self.identity {
            let _ = out.try_push(sample);
            return;
        }

        let current_index = self.frame_index as f64;
        if let Some(previous) = self.prev {
            while self.next_pos <= current_index {
                let t = ((self.next_pos - (current_index - 1.0)).clamp(0.0, 1.0)) as f32;
                let value = previous + (sample - previous) * t;
                let _ = out.try_push(value);
                self.next_pos += self.step;
            }
        } else {
            let _ = out.try_push(sample);
            self.next_pos = self.step;
        }

        self.prev = Some(sample);
        self.frame_index += 1;
    }
}

fn push_stereo(
    out: &mut HeapProd<f32>,
    left: f32,
    right: f32,
    shared: &Arc<Shared>,
    epoch: u64,
) -> bool {
    loop {
        if shared.stop.load(Ordering::Relaxed) || shared.seek_epoch.load(Ordering::Acquire) != epoch
        {
            return false;
        }
        if out.vacant_len() >= 2 {
            let _ = out.push_slice(&[left, right]);
            return true;
        }
        thread::sleep(Duration::from_millis(2));
    }
}
