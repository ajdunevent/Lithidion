//! Pebble Protocol endpoint numbers (section 5).

pub const RECOVERY: u16 = 0;
pub const TIME: u16 = 11;
pub const WATCH_VERSION: u16 = 16;
pub const PHONE_VERSION: u16 = 17;
pub const SYSTEM_MESSAGE: u16 = 18;
pub const MUSIC_CONTROL: u16 = 32;
pub const PHONE_CONTROL: u16 = 33;
pub const APP_MESSAGE: u16 = 48;
pub const APP_RUN_STATE: u16 = 52;
pub const IMAGING: u16 = 53;
pub const PING: u16 = 2001;
pub const LOG_DUMP: u16 = 2002;
pub const RESET: u16 = 2003;
pub const FACTORY_REGISTRY: u16 = 5001;
pub const APP_FETCH: u16 = 6001;
pub const DATA_LOG: u16 = 6778;
pub const PUT_BYTES: u16 = 48879;
pub const BLOBDB_V1: u16 = 45531;
pub const BLOBDB_V2: u16 = 45787;
pub const TIMELINE_ACTIONS: u16 = 11440;
pub const HEALTH_SYNC: u16 = 911;
pub const APP_REORDER: u16 = 0xABCD;
pub const AUDIO_STREAM: u16 = 10000;
pub const VOICE_CONTROL: u16 = 11000;
pub const GET_BYTES: u16 = 9000;

/// A short name for logs.
pub fn name(endpoint: u16) -> &'static str {
    match endpoint {
        RECOVERY => "recovery",
        TIME => "time",
        WATCH_VERSION => "watch-version",
        PHONE_VERSION => "phone-version",
        SYSTEM_MESSAGE => "system-message",
        MUSIC_CONTROL => "music-control",
        PHONE_CONTROL => "phone-control",
        APP_MESSAGE => "app-message",
        APP_RUN_STATE => "app-run-state",
        IMAGING => "imaging",
        PING => "ping",
        LOG_DUMP => "log-dump",
        GET_BYTES => "get-bytes",
        RESET => "reset",
        FACTORY_REGISTRY => "factory-registry",
        APP_FETCH => "app-fetch",
        DATA_LOG => "data-log",
        PUT_BYTES => "put-bytes",
        BLOBDB_V1 => "blobdb-v1",
        BLOBDB_V2 => "blobdb-v2",
        TIMELINE_ACTIONS => "timeline-actions",
        HEALTH_SYNC => "health-sync",
        APP_REORDER => "app-reorder",
        AUDIO_STREAM => "audio-stream",
        VOICE_CONTROL => "voice-control",
        _ => "other",
    }
}
