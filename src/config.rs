pub struct Config {
    pub grpc_listen_port: u16,
    pub livekit_endpoint: String,
    pub livekit_token_endpoint: String,
    pub disable_remote_audio_playback: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            grpc_listen_port: 50051,
            livekit_endpoint: "https://livekit.nrawrx3.xyz".to_string(),
            livekit_token_endpoint: "https://lktoken.nrawrx3.xyz/livekit/join-token".to_string(),
            disable_remote_audio_playback: false,
        }
    }
}

impl Config {
    pub fn from_env_variables() -> Self {
        Config {
            grpc_listen_port: std::env::var("GRPC_LISTEN_PORT")
                .unwrap_or("50051".to_string())
                .parse()
                .expect("GRPC_LISTEN_PORT must be a valid port number"),

            livekit_endpoint: std::env::var("LIVEKIT_ENDPOINT")
                .unwrap_or("https://livekit.nrawrx3.xyz".to_string()),

            livekit_token_endpoint: std::env::var("LIVEKIT_TOKEN_ENDPOINT")
                .unwrap_or("https://lktoken.nrawrx3.xyz/livekit/join-token".to_string()),

            disable_remote_audio_playback: std::env::var("DISABLE_REMOTE_AUDIO_PLAYBACK")
                .unwrap_or("false".to_string())
                .parse()
                .expect("DISABLE_REMOTE_AUDIO_PLAYBACK must be a boolean"),
        }
    }
}
