use anyhow::{bail, Result};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use webrtc_vad::{SampleRate, Vad, VadMode};

use crate::config;
use crate::daemon;
use crate::output;
use crate::overlay;

// Vad contains *mut Fvad (not Send), but we only use it from a single task.
struct SendVad(Vad);
unsafe impl Send for SendVad {}

impl SendVad {
    fn is_voice_segment(&mut self, samples: &[i16]) -> bool {
        self.0.is_voice_segment(samples).unwrap_or(false)
    }
}

const FRAME_SAMPLES: usize = 480; // 30ms at 16kHz
const FRAME_BYTES: usize = FRAME_SAMPLES * 2;
const SILENCE_THRESHOLD: usize = 27; // 27 * 30ms = 810ms
const MIN_SPEECH_SAMPLES: usize = 4800; // 300ms at 16kHz
const PRE_SPEECH_FRAMES: usize = 5; // 150ms
const DEBOUNCE_FRAMES: usize = 2;

fn encode_wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
    for &s in samples {
        writer.write_sample(s).unwrap();
    }
    writer.finalize().unwrap();
    cursor.into_inner()
}

fn decode_frame(buf: &[u8], frame: &mut [i16; FRAME_SAMPLES]) {
    for (i, chunk) in buf[..FRAME_BYTES].chunks_exact(2).enumerate() {
        frame[i] = i16::from_le_bytes([chunk[0], chunk[1]]);
    }
}

pub async fn stream_vad(
    mut audio_rx: mpsc::Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
    sample_rate: u32,
    provider: &str,
    state: &config::State,
    overlay: overlay::Handle,
    transcript_file: PathBuf,
    history_file: PathBuf,
    chunk_file: PathBuf,
) -> Result<()> {
    if sample_rate != 16000 {
        bail!("VAD requires 16kHz sample rate, got {sample_rate}Hz");
    }

    let mut vad = SendVad(Vad::new_with_rate_and_mode(SampleRate::Rate16kHz, VadMode::Aggressive));

    let mut buf: Vec<u8> = Vec::new();
    let mut frame = [0i16; FRAME_SAMPLES];
    let mut speech_active = false;
    let mut silence_count: usize = 0;
    let mut voice_count: usize = 0;
    let mut speech_samples: Vec<i16> = Vec::new();
    let mut pre_buffer: VecDeque<[i16; FRAME_SAMPLES]> = VecDeque::new();
    let mut full_transcript = String::new();

    let _ = std::fs::write(&transcript_file, "");
    let (_, model) = config::parse_provider_model(&state.model);
    let model = model.to_string();

    while let Some(chunk) = audio_rx.recv().await {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        buf.extend_from_slice(&chunk);

        while buf.len() >= FRAME_BYTES {
            decode_frame(&buf, &mut frame);
            buf.drain(..FRAME_BYTES);

            let is_voice = vad.is_voice_segment(&frame);

            if !speech_active {
                if is_voice {
                    voice_count += 1;
                    if voice_count >= DEBOUNCE_FRAMES {
                        speech_active = true;
                        silence_count = 0;
                        for pre_frame in pre_buffer.drain(..) {
                            speech_samples.extend_from_slice(&pre_frame);
                        }
                        speech_samples.extend_from_slice(&frame);
                    } else {
                        pre_buffer.push_back(frame);
                        if pre_buffer.len() > PRE_SPEECH_FRAMES {
                            pre_buffer.pop_front();
                        }
                    }
                } else {
                    voice_count = 0;
                    pre_buffer.push_back(frame);
                    if pre_buffer.len() > PRE_SPEECH_FRAMES {
                        pre_buffer.pop_front();
                    }
                }
            } else {
                speech_samples.extend_from_slice(&frame);
                if is_voice {
                    silence_count = 0;
                } else {
                    silence_count += 1;
                    if silence_count >= SILENCE_THRESHOLD {
                        if speech_samples.len() >= MIN_SPEECH_SAMPLES {
                            transcribe_chunk(
                                &speech_samples,
                                sample_rate,
                                &chunk_file,
                                provider,
                                &state.lang,
                                &model,
                                &overlay,
                                &state.output,
                                &transcript_file,
                                &mut full_transcript,
                            )
                            .await;
                        }
                        speech_samples.clear();
                        speech_active = false;
                        silence_count = 0;
                        voice_count = 0;
                        pre_buffer.clear();
                    }
                }
            }
        }
    }

    // Flush remaining speech
    if speech_samples.len() >= MIN_SPEECH_SAMPLES {
        transcribe_chunk(
            &speech_samples,
            sample_rate,
            &chunk_file,
            provider,
            &state.lang,
            &model,
            &overlay,
            &state.output,
            &transcript_file,
            &mut full_transcript,
        )
        .await;
    }

    if state.enter && state.output != "clipboard" && !full_transcript.trim().is_empty() {
        output::type_enter();
    }
    output::append_history(&history_file, full_transcript.trim());

    Ok(())
}

async fn transcribe_chunk(
    samples: &[i16],
    sample_rate: u32,
    chunk_file: &PathBuf,
    provider: &str,
    lang: &str,
    model: &str,
    overlay: &overlay::Handle,
    output_mode: &str,
    transcript_file: &PathBuf,
    full_transcript: &mut String,
) {
    let wav = encode_wav(samples, sample_rate);
    if let Err(e) = tokio::fs::write(chunk_file, &wav).await {
        tracing::error!("failed to write chunk: {e}");
        return;
    }

    overlay.processing();

    match daemon::transcribe_with_retry(chunk_file, provider, lang, model).await {
        Ok(transcript) if !transcript.is_empty() => {
            if !full_transcript.is_empty() && !full_transcript.ends_with(' ') {
                full_transcript.push(' ');
            }
            full_transcript.push_str(&transcript);
            output::copy_to_clipboard(full_transcript);
            if output_mode != "clipboard" {
                output::type_text(&transcript);
            }
            let _ = tokio::fs::write(transcript_file, full_transcript.as_str()).await;
            overlay.set_text(full_transcript.clone());
        }
        Err(e) => tracing::error!("vad transcribe error: {e}"),
        _ => {}
    }

    let _ = tokio::fs::remove_file(chunk_file).await;
}
