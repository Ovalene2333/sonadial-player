use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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

#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub path: PathBuf,
    pub duration: f64,
    pub sample_rate: u32,
    pub channels: usize,
}

#[derive(Debug)]
enum Command {
    Seek { target: f64, fallback: f64 },
    Stop,
}

struct Shared {
    paused: AtomicBool,
    volume_bits: AtomicU32,
    position_frames: AtomicU64,
    seek_epoch: AtomicU64,
    done: AtomicBool,
    stop: AtomicBool,
    out_rate: u32,
    eq: Mutex<Arc<EqParams>>,
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
                done: AtomicBool::new(true),
                stop: AtomicBool::new(true),
                out_rate: 48_000,
                eq: Mutex::new(Arc::new(EqParams::flat(48_000))),
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
        }
    }

    pub fn play(&mut self, path: &Path) -> Result<()> {
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
            paused: AtomicBool::new(false),
            volume_bits: AtomicU32::new(self.volume().to_bits()),
            position_frames: AtomicU64::new(0),
            seek_epoch: AtomicU64::new(0),
            done: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            out_rate: self.out_rate,
            eq: Mutex::new(eq_params.clone()),
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

                    let epoch = cb_shared.seek_epoch.load(Ordering::Relaxed);
                    if epoch != last_epoch {
                        out_cons.clear();
                        eq_processor = EqProcessor::new();
                        analysis_resampler.reset();
                        gain = 0.0;
                        last_epoch = epoch;
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

    pub fn seek(&self, seconds: f64) {
        let duration = self.duration();
        let target = seconds.clamp(0.0, duration.max(0.0));
        let fallback = self.position();
        self.shared.position_frames.store(
            (target * self.out_rate as f64).round() as u64,
            Ordering::Relaxed,
        );
        self.shared.seek_epoch.fetch_add(1, Ordering::Relaxed);
        self.shared.done.store(false, Ordering::Relaxed);
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(Command::Seek { target, fallback });
        }
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
        self.info.is_some()
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

    pub fn take_error(&self) -> Option<String> {
        self.error.lock().unwrap().take()
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

    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        loop {
            match rx.try_recv() {
                Ok(Command::Stop) => return Ok(()),
                Ok(Command::Seek { target, fallback }) => {
                    let time = Time::try_from_secs_f64(target).unwrap_or(Time::ZERO);
                    match format.seek(
                        SeekMode::Accurate,
                        SeekTo::Time {
                            time,
                            track_id: Some(track_id),
                        },
                    ) {
                        Ok(seeked) => {
                            // Accurate mode seeks to the packet at or before the
                            // requested time. Keep the requested position and
                            // trim the leading frames so audio does not lag it.
                            let actual = time_base
                                .and_then(|tb| tb.calc_time(seeked.actual_ts))
                                .map(|time| time.as_secs_f64())
                                .unwrap_or(target)
                                .clamp(0.0, target);
                            trim_frames =
                                ((target - actual).max(0.0) * input_rate as f64).round() as u64;
                            decoder.reset();
                            resampler.reset();
                            shared.position_frames.store(
                                (target * shared.out_rate as f64).round() as u64,
                                Ordering::Relaxed,
                            );
                            shared.seek_epoch.fetch_add(1, Ordering::Relaxed);
                            shared.done.store(false, Ordering::Relaxed);
                        }
                        Err(_) => {
                            // The decoder is still at the old packet. Restore
                            // the old position instead of claiming the seek
                            // succeeded.
                            trim_frames = 0;
                            shared.position_frames.store(
                                (fallback.max(0.0) * shared.out_rate as f64).round() as u64,
                                Ordering::Relaxed,
                            );
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
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
                            if !resampler.push(left, right, out, shared) {
                                return Ok(());
                            }
                        }
                    }
                    Err(SymError::DecodeError(_)) => continue,
                    Err(SymError::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => {
                        break;
                    }
                    Err(SymError::ResetRequired) => {
                        decoder.reset();
                    }
                    Err(err) => return Err(err.into()),
                }
            }
            Ok(None) => break,
            Err(SymError::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => {
                decoder.reset();
            }
            Err(err) => return Err(err.into()),
        }
    }

    Ok(())
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
    ) -> bool {
        if self.identity {
            return push_stereo(out, left, right, shared);
        }

        let current_index = self.frame_index as f64;
        if let Some((prev_left, prev_right)) = self.prev {
            while self.next_pos <= current_index {
                let t = ((self.next_pos - (current_index - 1.0)).clamp(0.0, 1.0)) as f32;
                let out_left = prev_left + (left - prev_left) * t;
                let out_right = prev_right + (right - prev_right) * t;
                if !push_stereo(out, out_left, out_right, shared) {
                    return false;
                }
                self.next_pos += self.step;
            }
        } else {
            if !push_stereo(out, left, right, shared) {
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

fn push_stereo(out: &mut HeapProd<f32>, left: f32, right: f32, shared: &Arc<Shared>) -> bool {
    while out.vacant_len() < 2 {
        if shared.stop.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let _ = out.push_slice(&[left, right]);
    true
}
