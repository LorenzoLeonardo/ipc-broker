use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Deserializer;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(windows)]
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

// Your RPC types
use crate::client::BUF_SIZE;
use crate::rpc::{CallId, ClientId, RpcRequest, RpcResponse};

/// Shared broker state
type ClientSender = mpsc::UnboundedSender<ClientMsg>;
type SharedClients = Arc<Mutex<HashMap<ClientId, ClientSender>>>;
type SharedObjects = Arc<Mutex<HashMap<String, ClientId>>>;
type SharedSubscriptions = Arc<Mutex<HashMap<String, HashSet<ClientId>>>>;
type SharedCalls = Arc<Mutex<HashMap<CallId, ClientId>>>;

/// Message to a client actor
#[derive(Debug)]
enum ClientMsg {
    Outgoing(Vec<u8>),
    _Shutdown,
}

/// Trait alias for supported stream types
trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// Actor that owns a client connection
struct ClientActor<S> {
    client_id: ClientId,
    stream: S,
    rx: mpsc::UnboundedReceiver<ClientMsg>,

    objects: SharedObjects,
    clients: SharedClients,
    subscriptions: SharedSubscriptions,
    calls: SharedCalls,
}

impl<S> ClientActor<S>
where
    S: Stream + 'static,
{
    async fn run(self) {
        let (reader, mut writer) = tokio::io::split(self.stream);
        let client_id = self.client_id.clone();
        let objects = self.objects.clone();
        let clients = self.clients.clone();
        let subscriptions = self.subscriptions.clone();
        let calls = self.calls.clone();
        let mut rx = self.rx;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        // Spawn reader
        let reader_task = tokio::spawn({
            let client_id = client_id.clone();
            let objects = self.objects.clone();
            let clients = self.clients.clone();
            let subs = self.subscriptions.clone();
            let calls = self.calls.clone();
            async move {
                let res =
                    Self::reader_loop(reader, client_id.clone(), objects, clients, subs, calls)
                        .await;
                let _ = shutdown_tx.send(()); // notify writer
                res
            }
        });

        // Writer loop (with shutdown reaction)
        Self::writer_loop(&mut writer, client_id.clone(), &mut rx, shutdown_rx).await;

        let _ = reader_task.await;

        // ✅ Cleanup here
        Self::cleanup_client(&client_id, &clients, &objects, &subscriptions, &calls).await;

        println!("Actor ended for {client_id:?}");
    }

    async fn cleanup_client(
        client_id: &ClientId,
        clients: &SharedClients,
        objects: &SharedObjects,
        subscriptions: &SharedSubscriptions,
        calls: &SharedCalls,
    ) {
        println!("Cleaning up client {client_id:?}");

        // Remove from clients map
        clients.lock().await.remove(client_id);

        // Remove owned objects
        let mut objs = objects.lock().await;
        objs.retain(|_, owner| owner != client_id);

        // Remove subscriptions
        let mut subs = subscriptions.lock().await;
        for subs_set in subs.values_mut() {
            subs_set.remove(client_id);
        }

        // Remove calls where this client was caller
        let mut c = calls.lock().await;
        c.retain(|_, caller| caller != client_id);
    }

    async fn reader_loop<R>(
        mut reader: R,
        client_id: ClientId,
        objects: SharedObjects,
        clients: SharedClients,
        subscriptions: SharedSubscriptions,
        calls: SharedCalls,
    ) where
        R: AsyncRead + Unpin,
    {
        let mut buf = vec![0u8; BUF_SIZE];
        let mut leftover = Vec::new();
        let server_state = ServerState {
            objects,
            clients,
            subscriptions,
            calls,
        };
        loop {
            let n = match reader.read(&mut buf).await {
                Ok(0) => {
                    println!("Client {client_id:?} disconnected");
                    break;
                }
                Ok(n) => n,
                Err(e) => {
                    eprintln!("Read error {client_id:?}: {e:?}");
                    break;
                }
            };

            leftover.extend_from_slice(&buf[..n]);

            let mut slice = leftover.as_slice();
            while !slice.is_empty() {
                let mut de = Deserializer::from_slice(slice).into_iter::<serde_json::Value>();
                match de.next() {
                    Some(Ok(val)) => {
                        let consumed = de.byte_offset();
                        slice = &slice[consumed..];
                        println!("Received from {client_id:?}: {val}");

                        if let Ok(req) = serde_json::from_value::<RpcRequest>(val.clone()) {
                            server_state.handle_request(req, &client_id).await;
                        } else if let Ok(resp) = serde_json::from_value::<RpcResponse>(val) {
                            server_state.handle_response(resp, &client_id).await;
                        } else {
                            eprintln!("Invalid JSON value");
                        }
                    }
                    Some(Err(_)) => {
                        eprintln!("Incomplete JSON from {client_id:?}, waiting for more data");
                        break;
                    }
                    None => break,
                }
            }

            leftover = slice.to_vec();
        }
    }

    async fn writer_loop<W>(
        writer: &mut W,
        client_id: ClientId,
        rx: &mut mpsc::UnboundedReceiver<ClientMsg>,
        mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    ) where
        W: AsyncWrite + Unpin,
    {
        tokio::select! {
            _ = &mut shutdown_rx => {
                println!("Shutdown signal received by writer for {client_id:?}");
            }
            _ = async {
                while let Some(msg) = rx.recv().await {
                    match msg {
                        ClientMsg::Outgoing(bytes) => {
                            if let Err(e) = writer.write_all(&bytes).await {
                                eprintln!("Write error {client_id:?}: {e:?}");
                                break;
                            }
                        }
                        ClientMsg::_Shutdown => {
                            println!("Writer got shutdown for {client_id:?}");
                            break;
                        }
                    }
                }
            } => {}
        }
    }
}

#[derive(Clone)]
struct ServerState {
    objects: SharedObjects,
    clients: SharedClients,
    subscriptions: SharedSubscriptions,
    calls: SharedCalls,
}

impl ServerState {
    /// Dispatch logic for RpcRequest
    async fn handle_request(&self, req: RpcRequest, client_id: &ClientId) {
        match req {
            RpcRequest::RegisterObject { object_name } => {
                self.handle_register_object(object_name, client_id).await;
            }

            RpcRequest::Call {
                call_id,
                object_name,
                method,
                args,
            } => {
                self.handle_call(call_id, object_name, method, args, client_id)
                    .await;
            }

            RpcRequest::Subscribe { topic } => {
                self.handle_subscribe(topic, client_id).await;
            }

            RpcRequest::Publish { topic, args } => {
                self.handle_publish(topic, args).await;
            }

            RpcRequest::HasObject { object_name } => {
                let exists = { self.objects.lock().await.contains_key(&object_name) };
                let resp = RpcResponse::HasObjectResult {
                    object_name,
                    exists,
                };
                Self::send_to_client(&self.clients, client_id, &resp).await;
            }
        }
    }

    /// Dispatch logic for RpcResponse
    async fn handle_response(&self, resp: RpcResponse, _from: &ClientId) {
        match &resp {
            RpcResponse::Result { call_id, .. }
            | RpcResponse::Error {
                call_id: Some(call_id),
                ..
            } => {
                // Look up original caller
                if let Some(caller) = self.calls.lock().await.remove(call_id) {
                    println!("Forwarding response for call_id {call_id:?} to caller {caller:?}");
                    Self::send_to_client(&self.clients, &caller, &resp).await;
                } else {
                    eprintln!("No caller found for call_id {call_id:?}");
                }
            }
            _ => {
                // Other response types (Subscribed, Event, etc.) are terminal, not forwarded
                eprintln!("Unhandled response type: {resp:?}");
            }
        }
    }

    async fn handle_register_object(&self, object_name: String, client_id: &ClientId) {
        self.objects
            .lock()
            .await
            .insert(object_name.clone(), client_id.clone());

        let resp = RpcResponse::Registered { object_name };
        Self::send_to_client(&self.clients, client_id, &resp).await;
    }

    async fn handle_call(
        &self,
        call_id: CallId,
        object_name: String,
        method: String,
        args: serde_json::Value,
        client_id: &ClientId,
    ) {
        // Track caller for response routing
        self.calls
            .lock()
            .await
            .insert(call_id.clone(), client_id.clone());

        // Lookup worker
        let worker_opt = { self.objects.lock().await.get(&object_name).cloned() };

        if let Some(worker_id) = worker_opt {
            println!("Forwarding call {call_id:?} to worker {worker_id:?}");
            let forwarded = RpcRequest::Call {
                call_id,
                object_name,
                method,
                args,
            };
            Self::send_to_client(&self.clients, &worker_id, &forwarded).await;
        } else {
            let err = RpcResponse::Error {
                call_id: Some(call_id),
                message: "No such object".into(),
            };
            Self::send_to_client(&self.clients, client_id, &err).await;
        }
    }

    async fn handle_subscribe(&self, topic: String, client_id: &ClientId) {
        println!("Client {client_id:?} subscribing to topic '{topic}'");
        self.subscriptions
            .lock()
            .await
            .entry(topic.clone())
            .or_default()
            .insert(client_id.clone());

        let resp = RpcResponse::Subscribed { topic };
        Self::send_to_client(&self.clients, client_id, &resp).await;
    }

    async fn handle_publish(&self, topic: String, args: serde_json::Value) {
        let subs_list = {
            let subs_guard = self.subscriptions.lock().await;
            subs_guard
                .get(&topic)
                .map(|s| s.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        };

        if !subs_list.is_empty() {
            let event = RpcResponse::Event {
                topic: topic.clone(),
                args,
            };
            let bytes = match serde_json::to_vec(&event) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("[Broker] Failed to serialize event for topic {topic}: {e}");
                    return;
                }
            };

            let clients_guard = self.clients.lock().await;
            for sub_id in subs_list {
                if let Some(tx) = clients_guard.get(&sub_id) {
                    let _ = tx.send(ClientMsg::Outgoing(bytes.clone()));
                }
            }
        }
    }

    async fn send_to_client<T: serde::Serialize>(
        clients: &SharedClients,
        client_id: &ClientId,
        msg: &T,
    ) {
        let bytes = match serde_json::to_vec(msg) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[Broker] Failed to serialize message for {client_id:?}: {e}");
                return; // skip this send but keep the broker running
            }
        };
        let clients_guard = clients.lock().await;
        if let Some(tx) = clients_guard.get(client_id) {
            let _ = tx.send(ClientMsg::Outgoing(bytes));
        }
    }
}

async fn start_tcp_listener(
    objects: SharedObjects,
    clients: SharedClients,
    subscriptions: SharedSubscriptions,
    calls: SharedCalls,
) -> std::io::Result<()> {
    let tcp_listener = TcpListener::bind("0.0.0.0:5000").await?;
    println!("Broker listening on TCP 0.0.0.0:5000");

    tokio::spawn(async move {
        loop {
            match tcp_listener.accept().await {
                Ok((stream, _)) => {
                    spawn_client(
                        stream,
                        objects.clone(),
                        clients.clone(),
                        subscriptions.clone(),
                        calls.clone(),
                    );
                }
                Err(e) => eprintln!("TCP accept error: {e:?}"),
            }
        }
    });

    Ok(())
}

#[cfg(unix)]
async fn start_unix_listener(
    objects: SharedObjects,
    clients: SharedClients,
    subscriptions: SharedSubscriptions,
    calls: SharedCalls,
) -> std::io::Result<()> {
    let _ = std::fs::remove_file("/tmp/ipc_broker.sock"); // cleanup old
    let unix_listener = UnixListener::bind("/tmp/ipc_broker.sock")?;
    println!("Broker also listening on /tmp/ipc_broker.sock");

    tokio::spawn(async move {
        loop {
            match unix_listener.accept().await {
                Ok((stream, _)) => {
                    spawn_client(
                        stream,
                        objects.clone(),
                        clients.clone(),
                        subscriptions.clone(),
                        calls.clone(),
                    );
                }
                Err(e) => eprintln!("Unix accept error: {e:?}"),
            }
        }
    });

    Ok(())
}

#[cfg(windows)]
fn start_named_pipe_listener(
    objects: SharedObjects,
    clients: SharedClients,
    subscriptions: SharedSubscriptions,
    calls: SharedCalls,
) {
    let pipe_name = r"\\.\pipe\ipc_broker";
    println!("Broker also listening on named pipe {pipe_name}");

    tokio::spawn(async move {
        loop {
            let server = match ServerOptions::new()
                .first_pipe_instance(false) // multiple instances
                .create(pipe_name)
            {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Pipe creation failed: {e:?}");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
            };

            let objects = objects.clone();
            let clients = clients.clone();
            let subs = subscriptions.clone();
            let calls = calls.clone();

            tokio::spawn(async move {
                match server.connect().await {
                    Ok(()) => spawn_client(server, objects, clients, subs, calls),
                    Err(e) => eprintln!("NamedPipe connection failed: {e:?}"),
                }
            });
        }
    });
}

fn spawn_client<S>(
    stream: S,
    objects: SharedObjects,
    clients: SharedClients,
    subscriptions: SharedSubscriptions,
    calls: SharedCalls,
) where
    S: Stream + 'static,
{
    let client_id = ClientId(Uuid::new_v4().to_string());
    println!("New connection: {client_id:?}");

    let (tx, rx) = mpsc::unbounded_channel::<ClientMsg>();
    let inner_client_id = client_id.clone();
    tokio::spawn({
        let clients = clients.clone();
        async move {
            clients.lock().await.insert(inner_client_id, tx);
        }
    });

    let actor = ClientActor {
        client_id,
        stream,
        rx,
        objects,
        clients,
        subscriptions,
        calls,
    };
    tokio::spawn(actor.run());
}

pub async fn run_broker() -> std::io::Result<()> {
    let objects: SharedObjects = Arc::new(Mutex::new(HashMap::new()));
    let clients: SharedClients = Arc::new(Mutex::new(HashMap::new()));
    let subscriptions: SharedSubscriptions = Arc::new(Mutex::new(HashMap::new()));
    let calls: SharedCalls = Arc::new(Mutex::new(HashMap::new()));

    // Spawn listeners
    start_tcp_listener(
        objects.clone(),
        clients.clone(),
        subscriptions.clone(),
        calls.clone(),
    )
    .await?;

    #[cfg(unix)]
    start_unix_listener(
        objects.clone(),
        clients.clone(),
        subscriptions.clone(),
        calls.clone(),
    )
    .await?;

    #[cfg(windows)]
    start_named_pipe_listener(
        objects.clone(),
        clients.clone(),
        subscriptions.clone(),
        calls.clone(),
    );

    // Wait for shutdown
    tokio::signal::ctrl_c().await?;
    println!("Broker shutting down...");
    Ok(())
}
