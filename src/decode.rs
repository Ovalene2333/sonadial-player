//! Shared audio decoding for the offline analysers.
//!
//! Tracks are decoded to mono f32 at a requested rate so beat detection and
//! note transcription can share the same code path.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, Context, Result};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

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
    let mut samples = Vec::new();
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
