//! Audio I/O Module for RustyRoom Voice Communication
//!
//! This module handles:
//! - Cross-platform audio capture (microphone)
//! - Cross-platform audio playback (speakers)
//! - Ring buffer management for thread-safe audio transfer
//! - Integration with the voice communication system

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{HeapRb, traits::*};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// Suppress ALSA error messages on Linux
/// These are verbose and not actionable (e.g., "Cannot open device /dev/dsp")
#[cfg(target_os = "linux")]
pub fn suppress_alsa_errors() {
    // This uses the ALSA C library to set a null error handler
    use std::ffi::c_int;
    
    unsafe extern "C" {
        fn snd_lib_error_set_handler(
            handler: Option<extern "C" fn(*const i8, c_int, *const i8, c_int, *const i8)>,
        ) -> c_int;
    }
    
    unsafe {
        snd_lib_error_set_handler(None);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn suppress_alsa_errors() {
    // No-op on non-Linux platforms
}

/// Audio sample rate in Hz (standard for Opus codec)
pub const SAMPLE_RATE: u32 = 48000;

/// Frame size in samples (20ms at 48kHz for Opus)
pub const FRAME_SIZE: usize = 960;

/// Number of audio channels (mono for voice)
pub const CHANNELS: u16 = 1;

/// Ring buffer size (should hold several frames worth of audio)
/// At 48kHz mono, this is about 1.7 seconds of audio
pub const BUFFER_SIZE: usize = 81920;

// ============================================================================
// AUDIO DEVICE INFO
// ============================================================================

/// Information about an audio device
#[derive(Debug, Clone)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub is_input: bool,
    pub is_default: bool,
}

/// Helper to make device names more user-friendly
fn friendly_device_name(name: &str) -> String {
    // Map common audio backend names to friendlier descriptions
    match name {
        "default" => "System Default".to_string(),
        "pipewire" => "PipeWire (System Audio)".to_string(),
        "pulse" => "PulseAudio".to_string(),
        "jack" => "JACK Audio".to_string(),
        "null" => return String::new(), // Filter out
        _ => {
            // For real device names, clean them up slightly
            // Remove common prefixes that are just noise
            let name = name.trim();
            if name.is_empty() {
                return String::new();
            }
            name.to_string()
        }
    }
}

/// Check if this is a real hardware device (not a virtual/backend device)
fn is_hardware_device(name: &str) -> bool {
    !matches!(name, "default" | "pipewire" | "pulse" | "jack" | "null")
}

/// List available audio input devices (microphones)
pub fn list_input_devices() -> Vec<AudioDeviceInfo> {
    let host = cpal::default_host();
    let default_input = host.default_input_device().and_then(|d| d.name().ok());
    
    let mut devices: Vec<AudioDeviceInfo> = host.input_devices()
        .map(|devices| {
            devices
                .filter_map(|d| {
                    let raw_name = d.name().ok()?;
                    let friendly_name = friendly_device_name(&raw_name);
                    if friendly_name.is_empty() {
                        return None;
                    }
                    Some(AudioDeviceInfo {
                        is_default: Some(&raw_name) == default_input.as_ref(),
                        name: friendly_name,
                        is_input: true,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    
    // Sort: put hardware devices first, then virtual/backend devices
    devices.sort_by(|a, b| {
        let a_hw = is_hardware_device(&a.name);
        let b_hw = is_hardware_device(&b.name);
        match (a_hw, b_hw) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => {
                // Put default first within each group
                if a.is_default { std::cmp::Ordering::Less }
                else if b.is_default { std::cmp::Ordering::Greater }
                else { a.name.cmp(&b.name) }
            }
        }
    });
    devices.dedup_by(|a, b| a.name == b.name);
    
    // If no devices found, add a default placeholder
    if devices.is_empty() {
        devices.push(AudioDeviceInfo {
            name: "System Default".to_string(),
            is_input: true,
            is_default: true,
        });
    }
    
    devices
}

/// List available audio output devices (speakers/headphones)
pub fn list_output_devices() -> Vec<AudioDeviceInfo> {
    let host = cpal::default_host();
    let default_output = host.default_output_device().and_then(|d| d.name().ok());
    
    let mut devices: Vec<AudioDeviceInfo> = host.output_devices()
        .map(|devices| {
            devices
                .filter_map(|d| {
                    let raw_name = d.name().ok()?;
                    let friendly_name = friendly_device_name(&raw_name);
                    if friendly_name.is_empty() {
                        return None;
                    }
                    Some(AudioDeviceInfo {
                        is_default: Some(&raw_name) == default_output.as_ref(),
                        name: friendly_name,
                        is_input: false,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    
    // Sort: put hardware devices first, then virtual/backend devices
    devices.sort_by(|a, b| {
        let a_hw = is_hardware_device(&a.name);
        let b_hw = is_hardware_device(&b.name);
        match (a_hw, b_hw) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => {
                // Put default first within each group
                if a.is_default { std::cmp::Ordering::Less }
                else if b.is_default { std::cmp::Ordering::Greater }
                else { a.name.cmp(&b.name) }
            }
        }
    });
    devices.dedup_by(|a, b| a.name == b.name);
    
    // If no devices found, add a default placeholder
    if devices.is_empty() {
        devices.push(AudioDeviceInfo {
            name: "System Default".to_string(),
            is_input: false,
            is_default: true,
        });
    }
    
    devices
}

// ============================================================================
// AUDIO STATISTICS
// ============================================================================

/// Audio statistics for monitoring
#[derive(Debug, Default)]
pub struct AudioStats {
    /// Samples captured from microphone
    pub samples_captured: AtomicU32,
    /// Samples played to speakers
    pub samples_played: AtomicU32,
    /// Input buffer overflows (samples dropped)
    pub input_overflows: AtomicU32,
    /// Output buffer underflows (silence inserted)
    pub output_underflows: AtomicU32,
}

impl AudioStats {
    /// Create new audio statistics
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Get snapshot of current statistics
    pub fn snapshot(&self) -> AudioStatsSnapshot {
        AudioStatsSnapshot {
            samples_captured: self.samples_captured.load(Ordering::Relaxed),
            samples_played: self.samples_played.load(Ordering::Relaxed),
            input_overflows: self.input_overflows.load(Ordering::Relaxed),
            output_underflows: self.output_underflows.load(Ordering::Relaxed),
        }
    }

    /// Reset statistics
    pub fn reset(&self) {
        self.samples_captured.store(0, Ordering::Relaxed);
        self.samples_played.store(0, Ordering::Relaxed);
        self.input_overflows.store(0, Ordering::Relaxed);
        self.output_underflows.store(0, Ordering::Relaxed);
    }
}

/// Snapshot of audio statistics (non-atomic copy)
#[derive(Debug, Clone, Copy)]
pub struct AudioStatsSnapshot {
    pub samples_captured: u32,
    pub samples_played: u32,
    pub input_overflows: u32,
    pub output_underflows: u32,
}

// ============================================================================
// AUDIO MANAGER
// ============================================================================

/// Audio I/O manager for voice communication
///
/// Handles microphone capture and speaker playback using
/// ring buffers for thread-safe communication with the
/// voice processing pipeline.
pub struct AudioManager {
    /// Input stream (microphone)
    _input_stream: cpal::Stream,
    /// Output stream (speakers)
    _output_stream: cpal::Stream,
    /// Running state flag
    running: Arc<AtomicBool>,
    /// Audio statistics
    stats: Arc<AudioStats>,
    /// Input device name
    pub input_device_name: String,
    /// Output device name
    pub output_device_name: String,
}

impl AudioManager {
    /// Create a new audio manager with default devices
    ///
    /// # Returns
    /// Tuple of (AudioManager, input_consumer, output_producer)
    /// - input_consumer: Receives audio samples from microphone
    /// - output_producer: Sends audio samples to speakers
    pub fn new()
    -> Result<(Self, impl Consumer<Item = f32>, impl Producer<Item = f32>), anyhow::Error> {
        Self::with_devices(None, None)
    }

    /// Create audio manager with specified devices
    ///
    /// # Arguments
    /// * `input_device_name` - Optional input device name (None for default)
    /// * `output_device_name` - Optional output device name (None for default)
    pub fn with_devices(
        input_device_name: Option<&str>,
        output_device_name: Option<&str>,
    ) -> Result<(Self, impl Consumer<Item = f32>, impl Producer<Item = f32>), anyhow::Error> {
        let host = cpal::default_host();

        // Get input device
        let input_device = match input_device_name {
            Some(name) => host
                .input_devices()?
                .find(|d| d.name().ok().as_deref() == Some(name))
                .ok_or_else(|| anyhow::anyhow!("Input device '{}' not found", name))?,
            None => host
                .default_input_device()
                .ok_or_else(|| anyhow::anyhow!("No default input device found"))?,
        };

        // Get output device
        let output_device = match output_device_name {
            Some(name) => host
                .output_devices()?
                .find(|d| d.name().ok().as_deref() == Some(name))
                .ok_or_else(|| anyhow::anyhow!("Output device '{}' not found", name))?,
            None => host
                .default_output_device()
                .ok_or_else(|| anyhow::anyhow!("No default output device found"))?,
        };

        let input_dev_name = input_device.name().unwrap_or_else(|_| "Unknown".to_string());
        let output_dev_name = output_device.name().unwrap_or_else(|_| "Unknown".to_string());

        // Configure audio streams for voice (mono, 48kHz)
        // Use default buffer size to let the audio system choose optimal size
        let config = cpal::StreamConfig {
            channels: CHANNELS,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        // Create ring buffers for audio transfer
        let input_rb = HeapRb::<f32>::new(BUFFER_SIZE);
        let (mut input_prod, input_cons) = input_rb.split();

        let output_rb = HeapRb::<f32>::new(BUFFER_SIZE);
        let (output_prod, mut output_cons) = output_rb.split();

        // Shared state
        let running = Arc::new(AtomicBool::new(true));
        let stats = AudioStats::new();
        
        let running_input = Arc::clone(&running);
        let stats_input = Arc::clone(&stats);

        // Build input stream (microphone capture)
        let input_stream = input_device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if !running_input.load(Ordering::Relaxed) {
                    return;
                }
                
                let mut pushed = 0u32;
                for &sample in data {
                    if input_prod.try_push(sample).is_ok() {
                        pushed += 1;
                    } else {
                        stats_input.input_overflows.fetch_add(1, Ordering::Relaxed);
                    }
                }
                stats_input.samples_captured.fetch_add(pushed, Ordering::Relaxed);
            },
            |err| {
                // Suppress common ALSA underrun messages
                let msg = err.to_string();
                if !msg.contains("underrun") {
                    eprintln!("[Audio] Input stream error: {}", err);
                }
            },
            None,
        )?;

        let running_output = Arc::clone(&running);
        let stats_output = Arc::clone(&stats);
        
        // Track if we've started receiving audio data
        let output_active = Arc::new(AtomicBool::new(false));
        let output_active_cb = Arc::clone(&output_active);

        // Build output stream (speaker playback)
        let output_stream = output_device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                if !running_output.load(Ordering::Relaxed) {
                    data.fill(0.0);
                    return;
                }
                
                let mut played = 0u32;
                for sample in data.iter_mut() {
                    if let Some(s) = output_cons.try_pop() {
                        *sample = s;
                        played += 1;
                        // Mark that we've received audio data
                        if !output_active_cb.load(Ordering::Relaxed) {
                            output_active_cb.store(true, Ordering::Relaxed);
                        }
                    } else {
                        // Only count as underflow if we were actively playing
                        *sample = 0.0;
                        if output_active_cb.load(Ordering::Relaxed) {
                            stats_output.output_underflows.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                stats_output.samples_played.fetch_add(played, Ordering::Relaxed);
            },
            |err| {
                // Suppress common ALSA underrun messages
                let msg = err.to_string();
                if !msg.contains("underrun") {
                    eprintln!("[Audio] Output stream error: {}", err);
                }
            },
            None,
        )?;

        // Start streams
        input_stream.play()?;
        output_stream.play()?;

        println!("[Audio] Input device: {}", input_dev_name);
        println!("[Audio] Output device: {}", output_dev_name);
        println!("[Audio] Sample rate: {} Hz, Channels: {}", SAMPLE_RATE, CHANNELS);

        Ok((
            AudioManager {
                _input_stream: input_stream,
                _output_stream: output_stream,
                running,
                stats,
                input_device_name: input_dev_name,
                output_device_name: output_dev_name,
            },
            input_cons,
            output_prod,
        ))
    }

    /// Get audio statistics
    pub fn stats(&self) -> &Arc<AudioStats> {
        &self.stats
    }

    /// Check if audio manager is running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Stop audio streams
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Drop for AudioManager {
    fn drop(&mut self) {
        self.stop();
    }
}

// ============================================================================
// AUDIO FRAME COLLECTOR
// ============================================================================

/// Collects audio samples into fixed-size frames for Opus encoding
pub struct FrameCollector {
    /// Buffer for accumulating samples
    buffer: Vec<f32>,
    /// Target frame size
    frame_size: usize,
}

impl FrameCollector {
    /// Create a new frame collector
    pub fn new(frame_size: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(frame_size * 2),
            frame_size,
        }
    }

    /// Add samples to the collector
    pub fn push_samples(&mut self, samples: &[f32]) {
        self.buffer.extend_from_slice(samples);
    }

    /// Try to extract a complete frame
    ///
    /// Returns Some(frame) if enough samples are available
    pub fn pop_frame(&mut self) -> Option<Vec<f32>> {
        if self.buffer.len() >= self.frame_size {
            let frame: Vec<f32> = self.buffer.drain(..self.frame_size).collect();
            Some(frame)
        } else {
            None
        }
    }

    /// Get remaining samples count
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// Clear the buffer
    pub fn clear(&mut self) {
        self.buffer.clear();
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_collector() {
        let mut collector = FrameCollector::new(960);
        
        // Add partial frame
        collector.push_samples(&[0.0f32; 500]);
        assert!(collector.pop_frame().is_none());
        assert_eq!(collector.pending(), 500);
        
        // Add remaining samples
        collector.push_samples(&[0.0f32; 500]);
        let frame = collector.pop_frame().unwrap();
        assert_eq!(frame.len(), 960);
        assert_eq!(collector.pending(), 40);
    }

    #[test]
    fn test_list_devices() {
        // Just ensure these don't panic
        let inputs = list_input_devices();
        let outputs = list_output_devices();
        println!("Input devices: {:?}", inputs);
        println!("Output devices: {:?}", outputs);
    }
}
