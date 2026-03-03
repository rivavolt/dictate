use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleRate;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

const SAMPLE_RATE: u32 = 16000;
const CHANNELS: u16 = 1;

pub struct AudioConfig {
    pub device: cpal::Device,
    pub stream_config: cpal::StreamConfig,
    pub sample_rate: u32,
}

pub fn get_audio_config() -> Result<AudioConfig> {
    let host = cpal::default_host();
    let device = host.default_input_device().context("no input device")?;

    let config_range = device
        .supported_input_configs()?
        .filter(|c| c.channels() <= CHANNELS && c.sample_format() == cpal::SampleFormat::F32)
        .min_by_key(|c| {
            let (min, max) = (c.min_sample_rate().0, c.max_sample_rate().0);
            if SAMPLE_RATE >= min && SAMPLE_RATE <= max {
                0
            } else {
                (min as i32 - SAMPLE_RATE as i32)
                    .abs()
                    .min((max as i32 - SAMPLE_RATE as i32).abs())
            }
        })
        .context("no suitable audio config")?;

    let rate =
        if SAMPLE_RATE >= config_range.min_sample_rate().0 && SAMPLE_RATE <= config_range.max_sample_rate().0 {
            SAMPLE_RATE
        } else {
            config_range
                .min_sample_rate()
                .0
                .max(config_range.max_sample_rate().0.min(SAMPLE_RATE))
        };

    let stream_config = config_range.with_sample_rate(SampleRate(rate)).config();
    Ok(AudioConfig {
        device,
        stream_config,
        sample_rate: rate,
    })
}

/// Capture audio into a tokio mpsc channel. Returns the cpal Stream (must be kept alive).
pub fn capture_to_channel(
    stop: Arc<AtomicBool>,
) -> Result<(cpal::Stream, mpsc::Receiver<Vec<u8>>, u32)> {
    let cfg = get_audio_config()?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>(256);

    let stream = cfg.device.build_input_stream(
        &cfg.stream_config,
        move |data: &[f32], _| {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let pcm: Vec<u8> = data
                .iter()
                .flat_map(|&s| ((s * 32767.0).clamp(-32768.0, 32767.0) as i16).to_le_bytes())
                .collect();
            let _ = tx.try_send(pcm);
        },
        |e| tracing::error!("audio capture error: {e}"),
        None,
    )?;

    stream.play()?;
    Ok((stream, rx, cfg.sample_rate))
}

/// Record audio to a WAV file until stop is signaled.
pub fn record_to_file(path: &std::path::Path, stop: Arc<AtomicBool>) -> Result<()> {
    let cfg = get_audio_config()?;
    let spec = hound::WavSpec {
        channels: CHANNELS,
        sample_rate: cfg.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = hound::WavWriter::create(path, spec)?;
    let samples = Arc::new(std::sync::Mutex::new(Vec::<i16>::new()));
    let samples_cb = samples.clone();
    let stop_cb = stop.clone();

    let stream = cfg.device.build_input_stream(
        &cfg.stream_config,
        move |data: &[f32], _| {
            if stop_cb.load(Ordering::Relaxed) {
                return;
            }
            let mut buf = samples_cb.lock().unwrap();
            for &s in data {
                buf.push((s * 32767.0).clamp(-32768.0, 32767.0) as i16);
            }
        },
        |e| tracing::error!("audio capture error: {e}"),
        None,
    )?;

    stream.play()?;

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut buf = samples.lock().unwrap();
        for &s in buf.iter() {
            writer.write_sample(s)?;
        }
        buf.clear();
    }

    let buf = samples.lock().unwrap();
    for &s in buf.iter() {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(())
}
