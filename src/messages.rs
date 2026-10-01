// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! Application-layer message types: the encrypted `{type, data}` JSON envelope.

use serde::{Deserialize, Serialize};

/// The set this client advertises, and the whole of it.
pub const SUPPORTED_MESSAGE_TYPES: [&str; 6] = [
    "ack",
    "supported_message_types",
    "device_info",
    "file_send_request",
    "file_send_request_reply",
    "text_send",
];

/// The outer envelope carried inside an encrypted record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    #[serde(rename = "type")]
    pub kind: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub device_type: String,
}

/// One entry of a `file_send_request`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDescriptor {
    /// Abstract, `/`-separated name. For a directory `docs/`, members are
    /// `docs/a.txt`, `docs/sub/b.txt`, ...
    pub filename: String,
    /// Authoritative byte count: the receiver reads exactly this many bytes and
    /// there is no in-band end-of-file marker, so it must be exact.
    pub size: u64,
    /// Unix seconds.
    pub last_modified: i64,
    /// Octal string of the low 9 mode bits, e.g. "644".
    pub permissions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSendRequest {
    pub files: Vec<FileDescriptor>,
    #[serde(default)]
    pub supported_compressions: Vec<String>,
    #[serde(default)]
    pub ios_live_photos: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSendRequestReply {
    pub accept: bool,
    #[serde(default)]
    pub compression: String,
}

impl Message {
    fn new(kind: &str, data: serde_json::Value) -> Self {
        Self {
            kind: kind.to_string(),
            data,
        }
    }

    pub fn ack(ok: bool) -> Self {
        Self::new("ack", serde_json::json!({ "ack": ok }))
    }

    pub fn supported_message_types() -> Self {
        Self::new(
            "supported_message_types",
            serde_json::json!({ "types": SUPPORTED_MESSAGE_TYPES }),
        )
    }

    pub fn device_info(name: &str, device_type: &str) -> Self {
        Self::new(
            "device_info",
            serde_json::json!({ "name": name, "type": device_type }),
        )
    }

    pub fn file_send_request(files: &[FileDescriptor]) -> Self {
        Self::new(
            "file_send_request",
            serde_json::json!({
                "files": files,
                "supported_compressions": ["none"],
                "ios_live_photos": [],
            }),
        )
    }

    pub fn file_send_request_reply(accept: bool) -> Self {
        Self::new(
            "file_send_request_reply",
            serde_json::json!({ "accept": accept, "compression": "none" }),
        )
    }

    pub fn text_send(text: &str) -> Self {
        Self::new("text_send", serde_json::json!({ "text": text }))
    }

    pub fn device_info_of(&self) -> Option<DeviceInfo> {
        serde_json::from_value(self.data.clone()).ok()
    }

    pub fn file_send_request_of(&self) -> Option<FileSendRequest> {
        serde_json::from_value(self.data.clone()).ok()
    }

    pub fn file_send_request_reply_of(&self) -> Option<FileSendRequestReply> {
        serde_json::from_value(self.data.clone()).ok()
    }

    pub fn text_of(&self) -> Option<String> {
        self.data
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}
