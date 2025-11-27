use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{HeapRb, traits::*};

pub const SAMPLE_RATE: u32 = 48000;

pub struct AudioManager {
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
}

impl AudioManager {
    pub fn new()
    -> Result<(Self, impl Consumer<Item = f32>, impl Producer<Item = f32>), anyhow::Error> {
        let host = cpal::default_host();

        let input_device = host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No input device found"))?;

        let output_device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("No output device found"))?;

        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        let input_rb = HeapRb::<f32>::new(8192);
        let (mut input_prod, input_cons) = input_rb.split();

        let output_rb = HeapRb::<f32>::new(8192);
        let (output_prod, mut output_cons) = output_rb.split();

        let input_stream = input_device.build_input_stream(
            &config,
            move |data: &[f32], _: &_| {
                for &sample in data {
                    let _ = input_prod.try_push(sample);
                }
            },
            |err| eprintln!("[Audio] Input error: {}", err),
            None,
        )?;

        let output_stream = output_device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &_| {
                for sample in data {
                    *sample = output_cons.try_pop().unwrap_or(0.0);
                }
            },
            |err| eprintln!("[Audio] Output error: {}", err),
            None,
        )?;

        input_stream.play()?;
        output_stream.play()?;

        Ok((
            AudioManager {
                _input_stream: input_stream,
                _output_stream: output_stream,
            },
            input_cons,
            output_prod,
        ))
    }
}
