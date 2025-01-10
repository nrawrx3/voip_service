use std::collections::HashSet;
pub struct DebugStats {
    // We're essentially reverse engineering the audio packet config from the
    // audio packet. Until LiveKit adds some documentation on the audio packet
    // config, this will help understand the behavior of audio handling by
    // livekit's server and native client.
    pub unique_audio_packet_configs: HashSet<AudioPacketConfig>,
}

#[derive(PartialEq, Eq, Hash)]
pub struct AudioPacketConfig {
    pub sample_rate: usize,
    pub channels: usize,
    pub buffer_size_in_samples: usize,
}

pub type SharedDebugStats = std::sync::Arc<std::sync::Mutex<DebugStats>>;

impl DebugStats {
    pub fn print(&self) {
        // Don't add serde_json as a dependency just for this.
        // TODO: Conditionally add serde in debug mode.
        // match serde_json::to_string_pretty(&self) {
        //     Ok(json) => println!("{}", json),
        //     Err(e) => eprintln!("Failed to serialize DebugStats: {}", e),
        // }
    }

    pub fn add_unique_audio_packet_config(&mut self, audio_packet_config: AudioPacketConfig) {
        self.unique_audio_packet_configs.insert(audio_packet_config);
    }
}
