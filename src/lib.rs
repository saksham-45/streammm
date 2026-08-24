//! streamaid origin: low-latency 1080p30 capture, typed WebSocket media, chunked HTTP fallback.

pub mod capture;
pub mod config;
pub mod encoder;
pub mod frame;
pub mod headers;
pub mod hub;
pub mod protocol;
pub mod publisher;
pub mod server;
pub mod snapshot;
pub mod ws;

pub use config::Config;
pub use encoder::build_ffmpeg_argv;
pub use hub::Hub;
pub use protocol::{pack_media, unpack_media, TYPE_FRAG, TYPE_INIT, TYPE_JPEG, TYPE_SNAP};
