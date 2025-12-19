use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::{mpsc, oneshot},
};
use uuid::Uuid;

use crate::{
    broker::{read_packet, write_packet},
    rpc::{CallId, RpcRequest, RpcResponse},
};

/// A trait alias for any asynchronous stream type
/// that implements both [`AsyncRead`] and [`AsyncWrite`].
pub trait AsyncStream: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite + Unpin> AsyncStream for T {}

/// Internal message sent to the client actor.
///
/// This is used to decouple the `IPCClient` (public API)
/// from the background task that drives I/O with the server.
enum ClientMsg {
    /// Request a remote procedure call, expecting a response.
    Request {
        req: RpcRequest,
        resp_tx: oneshot::Sender<std::io::Result<RpcResponse>>,
    },
    /// Subscribe to a publish/subscribe topic on a remote object.
    Subscribe {
        object_name: String,
        topic: String,
        updates: mpsc::UnboundedSender<serde_json::Value>,
    },
}

/// A handle to a client connection.
///
/// This is the main entrypoint for making RPC calls,
/// publishing events, or subscribing to topics.
/// Internally it communicates with a background task (the “client actor”)
/// that manages the network connection.
#[derive(Clone)]
pub struct IPCClient {
    tx: mpsc::UnboundedSender<ClientMsg>,
}

impl IPCClient {
    /// Connect to the broker and spawn the client actor.
    ///
    /// - If `BROKER_ADDR` environment variable is set, it connects via TCP.
    /// - Otherwise it attempts local IPC:
    ///   - On Unix: connects to a Unix socket at `rpc::UNIX_PATH`.
    ///   - On Windows: connects to a named pipe at `rpc::PIPE_PATH`.
    ///
    /// Returns a [`IPCClient`] that can be used to issue requests.
    pub async fn connect() -> std::io::Result<Self> {
        // Pick transport
        let stream: Box<dyn AsyncStream + Send + Unpin> =
            if let Ok(ip) = std::env::var("BROKER_ADDR") {
                let tcp = TcpStream::connect(ip.as_str()).await?;
                log::info!("Connected into TCP: {ip}");
                Box::new(tcp)
            } else {
                // Local IPC depending on OS
                #[cfg(unix)]
                {
                    use crate::rpc::UNIX_PATH;

                    let unix = UnixStream::connect(UNIX_PATH).await?;
                    log::info!("Connected into Unix: {UNIX_PATH}");
                    Box::new(unix)
                }

                #[cfg(windows)]
                {
                    use crate::rpc::PIPE_PATH;
                    loop {
                        let res = match ClientOptions::new().open(PIPE_PATH) {
                            Ok(pipe) => {
                                log::info!("Connected into NamedPipe: {PIPE_PATH}");
                                Box::new(pipe)
                            }
                            Err(e) if e.raw_os_error() == Some(231) => {
                                // All pipe instances busy → wait and retry

                                use std::time::Duration;

                                log::error!("All pipe instances busy, retrying...");
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                continue;
                            }
                            Err(e) => {
                                use std::time::Duration;
                                log::error!("Failed to connect to pipe: {}", e);
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                continue;
                            }
                        };
                        break res;
                    }
                }
            };

        // channel for handles -> actor
        let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();

        // Spawn the client actor task
        tokio::spawn(async move {
            let mut stream = stream;
            let mut subs: std::collections::HashMap<
                (String, String),
                Vec<mpsc::UnboundedSender<serde_json::Value>>,
            > = std::collections::HashMap::new();

            loop {
                tokio::select! {
                    Some(msg) = rx.recv() => {
                        match msg {
                            ClientMsg::Request { req, resp_tx } => {
                                // Serialize and send
                                let data = match serde_json::to_vec(&req) {
                                    Ok(d) => d,
                                    Err(e) => {
                                        let _ = resp_tx.send(Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)));
                                        continue;
                                    }
                                };

                                if let Err(e) = write_packet(&mut stream, &data).await {
                                    let _ = resp_tx.send(Err(e));
                                    continue;
                                }

                                // Wait for one response
                                match req {
                                    RpcRequest::Call { .. } | RpcRequest::RegisterObject { .. } | RpcRequest::HasObject { .. } | RpcRequest::RegisterService { .. } => {
                                        // Only these expect a response
                                        match read_packet(&mut stream).await {
                                            Ok(data) => {
                                                let resp: Result<RpcResponse, _> = serde_json::from_slice(&data);
                                                match resp {
                                                    Ok(r) => { let _ = resp_tx.send(Ok(r)); }
                                                    Err(e) => {
                                                        let _ = resp_tx.send(Err(std::io::Error::new(
                                                            std::io::ErrorKind::InvalidData,
                                                            e,
                                                        )));
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                let _ = resp_tx.send(Err(e));
                                            }
                                         }
                                    }
                                    RpcRequest::Publish { .. } | RpcRequest::Subscribe { .. } => {
                                        log::trace!("Fire-and-forget: do not await a response");
                                        let _ = resp_tx.send(Ok(RpcResponse::Event {
                                            object_name: "".into(),
                                            topic: "".into(),
                                            args: serde_json::Value::Null,
                                        }));
                                    }
                                }
                            }
                            ClientMsg::Subscribe { object_name, topic, updates } => {
                                log::debug!("Client subscribing to {object_name}/{topic}");
                                subs.entry((object_name.clone(), topic.clone()))
                                    .or_default()
                                    .push(updates);
                                let data = serde_json::to_vec(&RpcRequest::Subscribe { object_name, topic }).unwrap();

                                let _ = write_packet(&mut stream, &data).await;
                            }

                        }
                    }
                    Ok(data) = read_packet(&mut stream) => {
                        if data.is_empty() {
                            break;
                        }
                        log::debug!("Data {}", String::from_utf8_lossy(&data));
                        match serde_json::from_slice::<RpcResponse>(&data) {
                            Ok(resp) => {
                                // Handle Publish notifications
                                if let RpcResponse::Event { object_name, topic, args } = resp {
                                    if let Some(subscribers) = subs.get(&(object_name.clone(), topic.clone())) {
                                        for tx in subscribers {
                                            let _ = tx.send(args.clone());
                                        }
                                    }
                                } else{
                                    log::trace!("Other responses are ignored here; handled elsewhere");
                                }
                                continue;
                            }
                            Err(_) => {
                                log::trace!("Partial JSON, fallthrough to buffer handling");
                            }
                        }
                    }
                }
            }
        });

        Ok(Self { tx })
    }

    /// Call a method on a remote object.
    ///
    /// - `object`: name of the remote object.
    /// - `method`: method name.
    /// - `args`: JSON arguments.
    ///
    /// Returns the deserialized [`RpcResponse`] or an error if
    /// the call failed or connection was lost.
    pub async fn remote_call<U, T>(&self, object: &str, method: &str, args: U) -> std::io::Result<T>
    where
        T: DeserializeOwned,
        U: Serialize + std::any::Any + 'static,
    {
        let args = if let Some(val) = (&args as &dyn std::any::Any).downcast_ref::<Value>() {
            val.clone()
        } else {
            serde_json::to_value(args)?
        };
        let call_id = CallId::from(Uuid::new_v4());

        let req = RpcRequest::Call {
            call_id,
            object_name: object.into(),
            method: method.into(),
            args,
        };

        let (resp_tx, resp_rx) = oneshot::channel();
        let msg = ClientMsg::Request { req, resp_tx };

        // Send request to actor
        self.tx
            .send(msg)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Actor dropped"))?;

        // Wait for reply
        let resp = resp_rx.await.unwrap_or_else(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "Actor task ended",
            ))
        })?;

        // Match the RpcResponse
        match resp {
            RpcResponse::Result { value, .. } => serde_json::from_value(value).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Deserialize error: {e}"),
                )
            }),
            RpcResponse::Error { message, .. } => {
                Err(std::io::Error::other(format!("Remote error: {message}")))
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Unexpected response type",
            )),
        }
    }

    /// Publish an event to a remote object/topic.
    ///
    /// Unlike [`remote_call`], this is fire-and-forget: no response is awaited.
    pub async fn publish(
        &self,
        object: &str,
        topic: &str,
        args: &serde_json::Value,
    ) -> std::io::Result<()> {
        let (resp_tx, _resp_rx) = oneshot::channel();
        let msg = ClientMsg::Request {
            req: RpcRequest::Publish {
                object_name: object.into(),
                topic: topic.into(),
                args: args.clone(),
            },
            resp_tx,
        };

        self.tx
            .send(msg)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Actor dropped"))
    }

    /// Subscribe to events from a remote object/topic.
    ///
    /// Returns an [`mpsc::UnboundedReceiver`] where updates
    /// will be delivered as `serde_json::Value`.
    pub async fn subscribe(
        &self,
        object: &str,
        topic: &str,
    ) -> mpsc::UnboundedReceiver<serde_json::Value> {
        let (tx_updates, rx_updates) = mpsc::unbounded_channel();
        let _ = self.tx.send(ClientMsg::Subscribe {
            object_name: object.into(),
            topic: topic.into(),
            updates: tx_updates,
        });
        rx_updates
    }

    /// Subscribe asynchronously with a callback.
    ///
    /// Spawns a background task that invokes `callback` whenever
    /// a new event is received on the subscription.
    pub async fn subscribe_async<F>(&self, object: &str, topic: &str, callback: F)
    where
        F: Fn(Value) + Send + Sync + 'static,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let callback = Arc::new(callback);

        // Tell the actor to subscribe
        let _ = self.tx.send(ClientMsg::Subscribe {
            object_name: object.into(),
            topic: topic.into(),
            updates: tx,
        });

        // Spawn a task to handle messages and call the callback synchronously
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let cb = callback.clone();
                // Call synchronously
                cb(msg);
            }
        });
    }

    /// Wait until a given object exists on the server.
    ///
    /// This polls periodically using [`has_object`] until success.
    pub async fn wait_for_object(&self, object: &str) -> std::io::Result<()> {
        loop {
            if self.has_object(object).await? {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// Check whether a remote object exists.
    ///
    /// Returns `Ok(true)` if the object is present, `Ok(false)` otherwise.
    async fn has_object(&self, object: &str) -> std::io::Result<bool> {
        let req = RpcRequest::HasObject {
            object_name: object.into(),
        };

        let (resp_tx, resp_rx) = oneshot::channel();
        let msg = ClientMsg::Request { req, resp_tx };

        // Send request to actor
        self.tx
            .send(msg)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Actor dropped"))?;

        // Wait for reply
        match resp_rx.await.unwrap_or_else(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "Actor task ended",
            ))
        })? {
            RpcResponse::HasObjectResult { exists, .. } => Ok(exists),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Unexpected response type",
            )),
        }
    }
}
