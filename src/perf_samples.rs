use serde::{Deserialize, Serialize};

pub struct PerfSamplesFile {
    samples: PerfSamples,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PerfSamples {
    start_time: u64, // Unix timestamp in milliseconds
    thread_type: SampleThreadType,
    participant_name: String,
    samples: Vec<PerfSample>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PerfSample {
    ts: u64, // Nanoseconds
    frame_number: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum SampleThreadType {
    AudioFrameProducerThread,
    AudioFrameConsumerThread,
    CaptureFrameProducerThread,
    CaptureFrameConsumerThread,
}
