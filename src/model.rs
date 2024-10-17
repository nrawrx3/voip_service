use livekit::participant::ParticipantKind;

#[derive(Debug)]
pub struct ParticipantInfo {
    pub identity: String,
    pub is_connected: bool,
    pub is_speaking: bool,
    pub audio_level: f32,
    pub kind: ParticipantKind,
}

#[derive(Debug)]
pub struct RoomInfo {
    pub local_participant: ParticipantInfo,
    pub remote_participants: Vec<ParticipantInfo>,
}
