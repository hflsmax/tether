pub mod codec;
pub mod message;

pub use codec::FrameCodec;
pub use message::*;

pub const PROTOCOL_VERSION: u8 = 2;
pub const MAX_FRAME_SIZE: u32 = 64 * 1024 * 1024; // 64 MiB
