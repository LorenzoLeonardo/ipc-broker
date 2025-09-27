use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClientId(pub u128);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallId(pub u128);

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcRequest {
    RegisterObject {
        object_name: String,
    },
    Call {
        call_id: CallId,
        object_name: String,
        method: String,
        args: Vec<i32>,
    },
    Subscribe {
        topic: String,
    },
    Publish {
        topic: String,
        payload: String,
    },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum RpcResponse {
    Registered {
        object_name: String,
    },
    Result {
        call_id: CallId,
        value: i32,
    },
    Error {
        call_id: Option<CallId>,
        message: String,
    },
    Event {
        topic: String,
        payload: String,
    },
    Subscribed {
        topic: String,
    },
}
