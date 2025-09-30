use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClientId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallId(pub String);

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
        topic: String,
    },
    Publish {
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
        message: String,
    },
    Event {
        topic: String,
        args: serde_json::Value,
    },
    Subscribed {
        topic: String,
    },
    HasObjectResult {
        object_name: String,
        exists: bool,
    },
}
