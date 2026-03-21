pub mod codec;
pub mod message;

pub use codec::FrameCodec;
pub use message::*;

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024; // 16 MiB
