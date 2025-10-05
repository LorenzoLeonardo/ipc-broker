use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const BUF_SIZE: usize = (u16::MAX as usize) + 1;
pub const TCP_ADDR: &str = "0.0.0.0:5123";
#[cfg(unix)]
pub const UNIX_PATH: &str = "/tmp/ipc_broker.sock";
#[cfg(windows)]
pub const PIPE_PATH: &str = r"\\.\pipe\ipc_broker";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClientId(String);

impl From<Uuid> for ClientId {
    fn from(uuid: Uuid) -> Self {
        ClientId(uuid.to_string())
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Adjust this to however ClientId should be displayed, for example if it wraps a Uuid:
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallId(String);

impl From<Uuid> for CallId {
    fn from(uuid: Uuid) -> Self {
        CallId(uuid.to_string())
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub enum RpcRequest {
    RegisterObject {
        object_name: String,
    },
    Call {
        call_id: CallId,
        object_name: String,
        method: String,
        args: serde_json::Value,
    },
    Subscribe {
        object_name: String,
        topic: String,
    },
    Publish {
        object_name: String,
        topic: String,
        args: serde_json::Value,
    },
    HasObject {
        object_name: String,
    },
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub enum RpcResponse {
    Registered {
        object_name: String,
    },
    Result {
        call_id: CallId,
        object_name: String,
        value: serde_json::Value,
    },
    Error {
        call_id: Option<CallId>,
        object_name: String,
        message: String,
    },
    Event {
        object_name: String,
        topic: String,
        args: serde_json::Value,
    },
    Subscribed {
        object_name: String,
        topic: String,
    },
    HasObjectResult {
        object_name: String,
        exists: bool,
    },
}
