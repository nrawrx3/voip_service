use cpal::{SampleRate, SupportedStreamConfigRange};
use log::info;

pub fn find_suitable_stream_config(
    stream_configs: &mut dyn Iterator<Item = SupportedStreamConfigRange>,
    sample_rate: usize,
    channels: usize,
    buffer_size_in_samples: usize,
) -> Option<cpal::StreamConfig> {
    let mut must_have_config = None;

    let buffer_size_in_bytes = buffer_size_in_samples * channels * size_of::<f32>();

    for config in stream_configs {
        info!("Have supported config: {:?}", config);

        if must_have_config.is_some() {
            continue;
        }

        if channels != config.channels() as usize {
            continue;
        }

        let sr_min = config.min_sample_rate();
        let sr_max = config.max_sample_rate();

        let sr_buffer_size = match config.buffer_size().clone() {
            cpal::SupportedBufferSize::Range { min, max } => {
                if min <= buffer_size_in_bytes as u32 && buffer_size_in_bytes as u32 <= max {
                    Some(buffer_size_in_bytes)
                } else {
                    None
                }
            }

            cpal::SupportedBufferSize::Unknown => None,
        };

        // Usually we will have 48k sample rate and 2 channels. Hardcoding it for that.
        if sr_min == SampleRate(sample_rate as u32)
            && sr_max == SampleRate(sample_rate as u32)
            && sr_buffer_size.is_some()
        {
            must_have_config = Some(cpal::StreamConfig {
                channels: channels as u16,
                sample_rate: SampleRate(sample_rate as u32),
                buffer_size: cpal::BufferSize::Fixed(buffer_size_in_samples as u32),
            });
        }
    }

    return must_have_config;
}
