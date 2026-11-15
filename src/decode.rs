//! Shared audio decoding for the offline analysers.
//!
//! Tracks are decoded to mono f32 at a requested rate so beat detection and
//! note transcription can share the same code path. Decoding can either build
//! the whole track buffer (note transcription) or stream small chunks to a
//! sink (beat detection), so the frame pass never holds the entire song.


use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use anyhow::{anyhow, Context, Result};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// The most recently decoded whole-track buffer.
///
/// Note transcription decodes the full track into a buffer here; the beat
/// analyser streams instead and does not touch this slot. The entry is weak:
/// once the analyser is done the buffer is freed.
static SHARED: Mutex<Option<(PathBuf, u32, Weak<Vec<f32>>)>> = Mutex::new(None);
/// Serialises decoding so a second analyser waits for the first one instead of
/// decoding the same file in parallel.
static DECODE_LOCK: Mutex<()> = Mutex::new(());

/// Decodes to mono at `target_rate`, reusing a buffer another analyser already
/// produced for the same file.
pub fn decode_mono_shared(
    path: &Path,
    target_rate: u32,
    cancel: &AtomicBool,
) -> Result<Arc<Vec<f32>>> {
    if let Some(samples) = shared_lookup(path, target_rate) {
        return Ok(samples);
    }

    let _guard = DECODE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(samples) = shared_lookup(path, target_rate) {
        return Ok(samples);
    }

    let samples = Arc::new(decode_mono(path, target_rate, cancel)?);
    if let Ok(mut slot) = SHARED.lock() {
        *slot = Some((path.to_owned(), target_rate, Arc::downgrade(&samples)));
    }
    Ok(samples)
}

fn shared_lookup(path: &Path, target_rate: u32) -> Option<Arc<Vec<f32>>> {
    let slot = SHARED.lock().ok()?;
    let (cached_path, cached_rate, weak) = slot.as_ref()?;
    if *cached_rate != target_rate || cached_path != path {
        return None;
    }
    weak.upgrade()
}

pub fn decode_mono(path: &Path, target_rate: u32, cancel: &AtomicBool) -> Result<Vec<f32>> {
    let file =
        Box::new(fs::File::open(path).with_context(|| format!("无法打开 {}", path.display()))?);
    let mss = MediaSourceStream::new(file, Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .context("无法解析音频文件")?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("文件中没有音频轨"))?;
    let track_id = track.id;
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
        .map(|channels| channels.count())
        .unwrap_or(2)
        .max(1);
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&codec, &AudioDecoderOptions::default())?;

    let mut resampler = MonoResampler::new(input_rate, target_rate);
    let mut samples = reserve_output(track.num_frames, input_rate, target_rate);
    let mut sample_buf: Vec<f32> = Vec::new();

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("audio analysis cancelled"));
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
                        for frame in 0..frames {
                            let base = frame * input_channels;
                            let mut mono = 0.0;
                            for channel in 0..input_channels {
                                mono += sample_buf[base + channel];
                            }
                            resampler.push(mono / input_channels as f32, &mut samples);
                        }
                    }
                    Err(SymError::DecodeError(_)) => continue,
                    Err(SymError::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => {
                        break;
                    }
                    Err(SymError::ResetRequired) => decoder.reset(),
                    Err(err) => return Err(err.into()),
                }
            }
            Ok(None) => break,
            Err(SymError::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => decoder.reset(),
            Err(err) => return Err(err.into()),
        }
    }

    Ok(samples)
}

/// Decodes to mono at `target_rate`, handing sequential chunks to `sink`
/// instead of building the whole track in memory. The sink returns false to
/// stop early; `Ok(false)` then reports that stop. `Ok(true)` means the
/// stream ran to the end of the file.
pub fn decode_mono_streamed(
    path: &Path,
    target_rate: u32,
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(&[f32]) -> bool,
) -> Result<bool> {
    let file =
        Box::new(fs::File::open(path).with_context(|| format!("无法打开 {}", path.display()))?);
    let mss = MediaSourceStream::new(file, Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .context("无法解析音频文件")?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("文件中没有音频轨"))?;
    let track_id = track.id;
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
        .map(|channels| channels.count())
        .unwrap_or(2)
        .max(1);
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&codec, &AudioDecoderOptions::default())?;

    let mut resampler = MonoResampler::new(input_rate, target_rate);
    let mut sample_buf: Vec<f32> = Vec::new();
    let mut out: Vec<f32> = Vec::new();
    let mut ran_to_end = true;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("audio analysis cancelled"));
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
                        out.clear();
                        for frame in 0..frames {
                            let base = frame * input_channels;
                            let mut mono = 0.0;
                            for channel in 0..input_channels {
                                mono += sample_buf[base + channel];
                            }
                            resampler.push(mono / input_channels as f32, &mut out);
                        }
                        if !sink(&out) {
                            ran_to_end = false;
                            break;
                        }
                    }
                    Err(SymError::DecodeError(_)) => continue,
                    Err(SymError::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => {
                        break;
                    }
                    Err(SymError::ResetRequired) => decoder.reset(),
                    Err(err) => return Err(err.into()),
                }
            }
            Ok(None) => break,
            Err(SymError::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => decoder.reset(),
            Err(err) => return Err(err.into()),
        }
    }

    Ok(ran_to_end)
}

/// Pre-size the decoded buffer from the container's frame count so a long
/// track does not grow it by repeated doubling (and briefly hold both the old
/// and the new buffer during each copy). The hint is capped and given a small
/// margin because the count excludes encoder delay and padding frames.
fn reserve_output(num_frames: Option<u64>, input_rate: u32, target_rate: u32) -> Vec<f32> {
    const MAX_SECONDS: u64 = 2 * 60 * 60;
    let Some(frames) = num_frames else {
        return Vec::new();
    };
    let expected = (frames as u128 * target_rate as u128 / input_rate.max(1) as u128) as u64;
    let expected = expected.min(MAX_SECONDS * target_rate as u64) as usize;
    let mut samples = Vec::new();
    samples.reserve(expected.saturating_add(target_rate as usize / 4));
    samples
}

pub struct MonoResampler {
    step: f64,
    next_pos: f64,
    frame_index: u64,
    prev: Option<f32>,
    identity: bool,
}

impl MonoResampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        Self {
            step: input_rate as f64 / output_rate.max(1) as f64,
            next_pos: 0.0,
            frame_index: 0,
            prev: None,
            identity: input_rate == output_rate,
        }
    }

    pub fn push(&mut self, sample: f32, out: &mut Vec<f32>) {
        if self.identity {
            out.push(sample);
            return;
        }
        let current_index = self.frame_index as f64;
        if let Some(previous) = self.prev {
            while self.next_pos <= current_index {
                let fraction = ((self.next_pos - (current_index - 1.0)).clamp(0.0, 1.0)) as f32;
                out.push(previous + (sample - previous) * fraction);
                self.next_pos += self.step;
            }
        } else {
            out.push(sample);
            self.next_pos = self.step;
        }
        self.prev = Some(sample);
        self.frame_index += 1;
    }
}
