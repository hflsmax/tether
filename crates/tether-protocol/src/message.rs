use std::collections::HashMap;
use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

// -- Message type IDs --

pub const MSG_HELLO: u8 = 0x01;
pub const MSG_HELLO_OK: u8 = 0x02;
pub const MSG_ERROR: u8 = 0x03;
pub const MSG_SESSION_CREATE: u8 = 0x10;
pub const MSG_SESSION_CREATED: u8 = 0x11;
pub const MSG_SESSION_ATTACH: u8 = 0x12;
pub const MSG_SESSION_DETACH: u8 = 0x13;
pub const MSG_SESSION_DESTROY: u8 = 0x14;
pub const MSG_SESSION_LIST: u8 = 0x15;
pub const MSG_SESSION_LIST_RESP: u8 = 0x16;
pub const MSG_SESSION_STATE: u8 = 0x17;
pub const MSG_DATA: u8 = 0x20;
pub const MSG_RESIZE: u8 = 0x21;
pub const MSG_PING: u8 = 0x30;
pub const MSG_PONG: u8 = 0x31;
pub const MSG_SESSION_EXITED: u8 = 0x40;

// -- Terminal state types --

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenMode {
    Main,
    Alternate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorState {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    pub shape: CursorShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub kind: ColorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColorKind {
    #[default]
    Default,
    Indexed(u8),
    Rgb,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct CellFlags: u16 {
        const BOLD          = 0b0000_0001;
        const ITALIC        = 0b0000_0010;
        const UNDERLINE     = 0b0000_0100;
        const INVERSE       = 0b0000_1000;
        const STRIKETHROUGH = 0b0001_0000;
        const DIM           = 0b0010_0000;
        const HIDDEN        = 0b0100_0000;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            c: ' ',
            fg: Color::default(),
            bg: Color::default(),
            flags: CellFlags::empty(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub cols: u16,
    pub rows: u16,
    pub screen_mode: ScreenMode,
    pub visible_rows: Vec<Row>,
    pub cursor: CursorState,
    pub scrollback: Vec<Row>,
    pub viewport_offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub cols: u16,
    pub rows: u16,
    pub attached: bool,
    pub idle_secs: u64,
    pub created_secs: u64,
    pub cmd: String,
    pub cwd: String,
    /// Name of the foreground process (e.g. "vim", "cargo")
    pub foreground_proc: String,
}

// -- Protocol messages --

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    Hello {
        version: u8,
        term: String,
        cols: u16,
        rows: u16,
    },
    HelloOk {
        version: u8,
    },
    Error {
        code: u16,
        message: String,
    },
    SessionCreate {
        id: Option<String>,
        cmd: Option<String>,
        cols: u16,
        rows: u16,
        env: HashMap<String, String>,
    },
    SessionCreated {
        id: String,
    },
    SessionAttach {
        id: String,
    },
    SessionDetach,
    SessionDestroy {
        id: String,
    },
    SessionList,
    SessionListResp {
        sessions: Vec<SessionInfo>,
    },
    SessionState(SessionState),
    Data(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    Ping {
        seq: u32,
    },
    Pong {
        seq: u32,
    },
    SessionExited {
        id: String,
        exit_code: i32,
    },
}

impl Message {
    pub fn type_id(&self) -> u8 {
        match self {
            Message::Hello { .. } => MSG_HELLO,
            Message::HelloOk { .. } => MSG_HELLO_OK,
            Message::Error { .. } => MSG_ERROR,
            Message::SessionCreate { .. } => MSG_SESSION_CREATE,
            Message::SessionCreated { .. } => MSG_SESSION_CREATED,
            Message::SessionAttach { .. } => MSG_SESSION_ATTACH,
            Message::SessionDetach => MSG_SESSION_DETACH,
            Message::SessionDestroy { .. } => MSG_SESSION_DESTROY,
            Message::SessionList => MSG_SESSION_LIST,
            Message::SessionListResp { .. } => MSG_SESSION_LIST_RESP,
            Message::SessionState(_) => MSG_SESSION_STATE,
            Message::Data(_) => MSG_DATA,
            Message::Resize { .. } => MSG_RESIZE,
            Message::Ping { .. } => MSG_PING,
            Message::Pong { .. } => MSG_PONG,
            Message::SessionExited { .. } => MSG_SESSION_EXITED,
        }
    }

    /// Encode this message to bytes: type_id byte + bincode payload.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let type_id = self.type_id();
        let payload: Vec<u8> = match self {
            Message::Data(data) => data.clone(),
            Message::Hello { version, term, cols, rows } => {
                bincode::serialize(&(version, term, cols, rows)).map_err(EncodeError::Bincode)?
            }
            Message::HelloOk { version } => {
                bincode::serialize(version).map_err(EncodeError::Bincode)?
            }
            Message::Error { code, message } => {
                bincode::serialize(&(code, message)).map_err(EncodeError::Bincode)?
            }
            Message::SessionCreate { id, cmd, cols, rows, env } => {
                bincode::serialize(&(id, cmd, cols, rows, env)).map_err(EncodeError::Bincode)?
            }
            Message::SessionCreated { id } => {
                bincode::serialize(id).map_err(EncodeError::Bincode)?
            }
            Message::SessionAttach { id } => {
                bincode::serialize(id).map_err(EncodeError::Bincode)?
            }
            Message::SessionDetach => vec![],
            Message::SessionDestroy { id } => {
                bincode::serialize(id).map_err(EncodeError::Bincode)?
            }
            Message::SessionList => vec![],
            Message::SessionListResp { sessions } => {
                bincode::serialize(sessions).map_err(EncodeError::Bincode)?
            }
            Message::SessionState(state) => {
                let raw = bincode::serialize(state).map_err(EncodeError::Bincode)?;
                let mut encoder =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
                encoder.write_all(&raw).map_err(EncodeError::Io)?;
                encoder.finish().map_err(EncodeError::Io)?
            }
            Message::Resize { cols, rows } => {
                bincode::serialize(&(cols, rows)).map_err(EncodeError::Bincode)?
            }
            Message::Ping { seq } => {
                bincode::serialize(seq).map_err(EncodeError::Bincode)?
            }
            Message::Pong { seq } => {
                bincode::serialize(seq).map_err(EncodeError::Bincode)?
            }
            Message::SessionExited { id, exit_code } => {
                bincode::serialize(&(id, exit_code)).map_err(EncodeError::Bincode)?
            }
        };
        let mut buf = Vec::with_capacity(1 + payload.len());
        buf.push(type_id);
        buf.extend_from_slice(&payload);
        Ok(buf)
    }

    /// Decode from bytes: first byte is type_id, rest is payload.
    pub fn decode(data: &[u8]) -> Result<Self, DecodeError> {
        if data.is_empty() {
            return Err(DecodeError::Empty);
        }
        let type_id = data[0];
        let payload = &data[1..];

        match type_id {
            MSG_DATA => Ok(Message::Data(payload.to_vec())),
            _ => decode_typed(type_id, payload),
        }
    }
}

fn decode_typed(type_id: u8, payload: &[u8]) -> Result<Message, DecodeError> {
    match type_id {
        MSG_HELLO => {
            let (version, term, cols, rows) = bincode::deserialize(payload)?;
            Ok(Message::Hello { version, term, cols, rows })
        }
        MSG_HELLO_OK => {
            let version = bincode::deserialize(payload)?;
            Ok(Message::HelloOk { version })
        }
        MSG_ERROR => {
            let (code, message) = bincode::deserialize(payload)?;
            Ok(Message::Error { code, message })
        }
        MSG_SESSION_CREATE => {
            let (id, cmd, cols, rows, env) = bincode::deserialize(payload)?;
            Ok(Message::SessionCreate { id, cmd, cols, rows, env })
        }
        MSG_SESSION_CREATED => {
            let id = bincode::deserialize(payload)?;
            Ok(Message::SessionCreated { id })
        }
        MSG_SESSION_ATTACH => {
            let id = bincode::deserialize(payload)?;
            Ok(Message::SessionAttach { id })
        }
        MSG_SESSION_DETACH => Ok(Message::SessionDetach),
        MSG_SESSION_DESTROY => {
            let id = bincode::deserialize(payload)?;
            Ok(Message::SessionDestroy { id })
        }
        MSG_SESSION_LIST => Ok(Message::SessionList),
        MSG_SESSION_LIST_RESP => {
            let sessions = bincode::deserialize(payload)?;
            Ok(Message::SessionListResp { sessions })
        }
        MSG_SESSION_STATE => {
            let mut decoder = flate2::read::DeflateDecoder::new(payload);
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed).map_err(DecodeError::Io)?;
            let state = bincode::deserialize(&decompressed)?;
            Ok(Message::SessionState(state))
        }
        MSG_DATA => Ok(Message::Data(payload.to_vec())),
        MSG_RESIZE => {
            let (cols, rows) = bincode::deserialize(payload)?;
            Ok(Message::Resize { cols, rows })
        }
        MSG_PING => {
            let seq = bincode::deserialize(payload)?;
            Ok(Message::Ping { seq })
        }
        MSG_PONG => {
            let seq = bincode::deserialize(payload)?;
            Ok(Message::Pong { seq })
        }
        MSG_SESSION_EXITED => {
            let (id, exit_code) = bincode::deserialize(payload)?;
            Ok(Message::SessionExited { id, exit_code })
        }
        _ => Err(DecodeError::UnknownType(type_id)),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("bincode encode error: {0}")]
    Bincode(bincode::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("empty frame")]
    Empty,
    #[error("unknown message type: 0x{0:02x}")]
    UnknownType(u8),
    #[error("bincode decode error: {0}")]
    Bincode(#[from] bincode::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_hello() {
        let msg = Message::Hello {
            version: 1,
            term: "xterm-256color".into(),
            cols: 80,
            rows: 24,
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_hello_ok() {
        let msg = Message::HelloOk { version: 1 };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_error() {
        let msg = Message::Error {
            code: 404,
            message: "session not found".into(),
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_data() {
        let msg = Message::Data(b"hello world\x1b[31m".to_vec());
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_session_create() {
        let mut env = HashMap::new();
        env.insert("LANG".into(), "en_US.UTF-8".into());
        let msg = Message::SessionCreate {
            id: Some("my-session".into()),
            cmd: Some("/bin/bash".into()),
            cols: 120,
            rows: 40,
            env,
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_session_state() {
        let state = SessionState {
            cols: 80,
            rows: 24,
            screen_mode: ScreenMode::Main,
            visible_rows: vec![Row {
                cells: vec![
                    Cell {
                        c: 'A',
                        fg: Color { r: 255, g: 255, b: 255, kind: ColorKind::Rgb },
                        bg: Color::default(),
                        flags: CellFlags::BOLD,
                    },
                    Cell::default(),
                ],
            }],
            cursor: CursorState {
                row: 0,
                col: 1,
                visible: true,
                shape: CursorShape::Block,
            },
            scrollback: vec![],
            viewport_offset: 0,
        };
        let msg = Message::SessionState(state);
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_session_list() {
        let msg = Message::SessionList;
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_session_list_resp() {
        let msg = Message::SessionListResp {
            sessions: vec![
                SessionInfo {
                    id: "bright-fox".into(),
                    cols: 80,
                    rows: 24,
                    attached: true,
                    idle_secs: 0,
                    created_secs: 120,
                    cmd: "/bin/zsh".into(),
                    cwd: "/home/user".into(),
                    foreground_proc: "vim".into(),
                },
                SessionInfo {
                    id: "calm-river".into(),
                    cols: 120,
                    rows: 40,
                    attached: false,
                    idle_secs: 3600,
                    created_secs: 7200,
                    cmd: "/bin/bash".into(),
                    cwd: "/tmp".into(),
                    foreground_proc: "cargo".into(),
                },
            ],
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_ping_pong() {
        let ping = Message::Ping { seq: 42 };
        let pong = Message::Pong { seq: 42 };
        assert_eq!(ping, Message::decode(&ping.encode().unwrap()).unwrap());
        assert_eq!(pong, Message::decode(&pong.encode().unwrap()).unwrap());
    }

    #[test]
    fn round_trip_resize() {
        let msg = Message::Resize { cols: 200, rows: 50 };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_session_exited() {
        let msg = Message::SessionExited {
            id: "test".into(),
            exit_code: 127,
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn unknown_type_id_errors() {
        let data = [0xFF, 0x00];
        assert!(Message::decode(&data).is_err());
    }

    #[test]
    fn empty_frame_errors() {
        assert!(Message::decode(&[]).is_err());
    }
}
