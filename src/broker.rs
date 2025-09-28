use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Deserializer;
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(windows)]
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, mpsc},
};
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
trait Stream: AsyncReadExt + AsyncWriteExt + Unpin + Send {}
impl<T: AsyncReadExt + AsyncWriteExt + Unpin + Send> Stream for T {}

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
        let (mut reader, mut writer) = tokio::io::split(self.stream);
        let client_id = self.client_id.clone();

        // === Reader task ===
        let objects = self.objects.clone();
        let clients = self.clients.clone();
        let subs = self.subscriptions.clone();
        let inner_client_id = client_id.clone();
        let calls = self.calls.clone();
        let mut rx = self.rx;

        let reader_task = tokio::spawn(async move {
            let mut buf = vec![0u8; BUF_SIZE];
            let mut leftover = Vec::new();

            loop {
                let n = match reader.read(&mut buf).await {
                    Ok(0) => {
                        println!("Client {inner_client_id:?} disconnected");
                        break;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("Read error {inner_client_id:?}: {e:?}");
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
                            println!("Received from {inner_client_id:?}: {val}");
                            // Dispatch the JSON
                            if let Ok(req) = serde_json::from_value::<RpcRequest>(val.clone()) {
                                handle_request(
                                    req,
                                    &inner_client_id,
                                    &objects,
                                    &clients,
                                    &subs,
                                    &calls,
                                )
                                .await;
                            } else if let Ok(resp) = serde_json::from_value::<RpcResponse>(val) {
                                handle_response(resp, &inner_client_id, &clients, &calls).await;
                            } else {
                                eprintln!("Invalid JSON value");
                            }
                        }
                        Some(Err(_)) => {
                            // Incomplete JSON, wait for more bytes
                            break;
                        }
                        None => break,
                    }
                }

                leftover = slice.to_vec();
            }
        });

        // === Writer loop ===
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMsg::Outgoing(bytes) => {
                    if let Err(e) = writer.write_all(&bytes).await {
                        eprintln!("Write error {client_id:?}: {e:?}");
                        break;
                    }
                }
                ClientMsg::_Shutdown => break,
            }
        }

        let _ = reader_task.await;
        println!("Actor ended for {client_id:?}");
    }
}

/// Dispatch logic for RpcRequest
async fn handle_request(
    req: RpcRequest,
    client_id: &ClientId,
    objects: &SharedObjects,
    clients: &SharedClients,
    subs: &SharedSubscriptions,
    calls: &SharedCalls,
) {
    match req {
        RpcRequest::RegisterObject { object_name } => {
            objects
                .lock()
                .await
                .insert(object_name.clone(), client_id.clone());
            let resp = RpcResponse::Registered { object_name };
            send_to_client(clients, client_id, resp).await;
        }

        RpcRequest::Call {
            call_id,
            object_name,
            method,
            args,
        } => {
            // Track the caller for this call_id
            calls
                .lock()
                .await
                .insert(call_id.clone(), client_id.clone());

            let worker_opt = { objects.lock().await.get(&object_name).cloned() };
            if let Some(worker_id) = worker_opt {
                println!("Forwarding call {call_id:?} to worker {worker_id:?}");
                let forwarded = RpcRequest::Call {
                    call_id: call_id.clone(),
                    object_name: object_name.clone(),
                    method: method.clone(),
                    args: args.clone(),
                };
                send_raw_to_client(clients, &worker_id, &forwarded).await;
            } else {
                let err = RpcResponse::Error {
                    call_id: Some(call_id),
                    message: "No such object".into(),
                };
                send_to_client(clients, client_id, err).await;
            }
        }

        RpcRequest::Subscribe { topic } => {
            println!("Client {client_id:?} subscribing to topic '{topic}'");
            subs.lock()
                .await
                .entry(topic.clone())
                .or_default()
                .insert(client_id.clone());
            let resp = RpcResponse::Subscribed { topic };
            send_to_client(clients, client_id, resp).await;
        }

        RpcRequest::Publish { topic, args } => {
            let subs_list = {
                let subs = subs.lock().await;
                subs.get(&topic)
                    .map(|s| s.iter().cloned().collect::<Vec<_>>())
                    .unwrap_or_default()
            };

            if !subs_list.is_empty() {
                let event = RpcResponse::Event {
                    topic: topic.clone(),
                    args: args.clone(),
                };
                let bytes = serde_json::to_vec(&event).unwrap();
                let clients_guard = clients.lock().await;
                for sub_id in subs_list {
                    if let Some(tx) = clients_guard.get(&sub_id) {
                        let _ = tx.send(ClientMsg::Outgoing(bytes.clone()));
                    }
                }
            }
        }
    }
}

async fn handle_response(
    resp: RpcResponse,
    _from: &ClientId,
    clients: &SharedClients,
    calls: &SharedCalls,
) {
    match &resp {
        RpcResponse::Result { call_id, .. }
        | RpcResponse::Error {
            call_id: Some(call_id),
            ..
        } => {
            // Look up original caller
            if let Some(caller) = calls.lock().await.remove(call_id) {
                println!("Forwarding response for call_id {call_id:?} to caller {caller:?}");
                send_to_client(clients, &caller, resp).await;
            } else {
                eprintln!("No caller found for call_id {call_id:?}");
            }
        }
        _ => {
            // Other response types (Subscribed, Event, etc.) are terminal, not forwarded
            println!("Unhandled response type: {resp:?}");
        }
    }
}

/// Send a typed response to a client
async fn send_to_client(clients: &SharedClients, client_id: &ClientId, msg: RpcResponse) {
    let bytes = serde_json::to_vec(&msg).unwrap();
    let clients_guard = clients.lock().await;
    if let Some(tx) = clients_guard.get(client_id) {
        let _ = tx.send(ClientMsg::Outgoing(bytes));
    }
}

/// Send a typed request to a client
async fn send_raw_to_client<T: serde::Serialize>(
    clients: &SharedClients,
    client_id: &ClientId,
    msg: &T,
) {
    let bytes = serde_json::to_vec(msg).unwrap();
    let clients_guard = clients.lock().await;
    if let Some(tx) = clients_guard.get(client_id) {
        let _ = tx.send(ClientMsg::Outgoing(bytes));
    }
}

pub async fn run_broker() -> std::io::Result<()> {
    let objects: SharedObjects = Arc::new(Mutex::new(HashMap::new()));
    let clients: SharedClients = Arc::new(Mutex::new(HashMap::new()));
    let subscriptions: SharedSubscriptions = Arc::new(Mutex::new(HashMap::new()));
    let calls: SharedCalls = Arc::new(Mutex::new(HashMap::new()));

    // --- TCP listener ---
    let tcp_listener = TcpListener::bind("0.0.0.0:5000").await?;
    let tcp_objects = objects.clone();
    let tcp_clients = clients.clone();
    let tcp_subs = subscriptions.clone();
    let tcp_calls = calls.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = tcp_listener.accept().await.unwrap();
            let client_id = ClientId(Uuid::new_v4().to_string());
            println!("New TCP connection: {client_id:?}");

            let (tx, rx) = mpsc::unbounded_channel::<ClientMsg>();
            tcp_clients.lock().await.insert(client_id.clone(), tx);

            let actor = ClientActor {
                client_id,
                stream,
                rx,
                objects: tcp_objects.clone(),
                clients: tcp_clients.clone(),
                subscriptions: tcp_subs.clone(),
                calls: tcp_calls.clone(),
            };
            tokio::spawn(actor.run());
        }
    });

    // --- Unix listener (Unix only) ---
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file("/tmp/ipc_broker.sock"); // cleanup old
        let unix_listener = UnixListener::bind("/tmp/ipc_broker.sock")?;
        println!("Broker listening on TCP 0.0.0.0:5000 and /tmp/ipc_broker.sock");

        loop {
            let (stream, _) = unix_listener.accept().await?;
            let client_id = ClientId(Uuid::new_v4().to_string());
            println!("New Unix connection: {client_id:?}");

            let (tx, rx) = mpsc::unbounded_channel::<ClientMsg>();
            clients.lock().await.insert(client_id.clone(), tx);

            let actor = ClientActor {
                client_id,
                stream,
                rx,
                objects: objects.clone(),
                clients: clients.clone(),
                subscriptions: subscriptions.clone(),
                calls: calls.clone(),
            };
            tokio::spawn(actor.run());
        }
    }

    // --- Named pipe listener (Windows only) ---
    #[cfg(windows)]
    {
        let pipe_name = r"\\.\pipe\ipc_broker";
        println!("Broker listening on TCP 0.0.0.0:5000 and named pipe {pipe_name}");

        loop {
            let server = ServerOptions::new()
                .first_pipe_instance(true)
                .create(pipe_name)?;

            let objects = objects.clone();
            let clients = clients.clone();
            let subs = subscriptions.clone();
            let calls = calls.clone();

            tokio::spawn(async move {
                match server.connect().await {
                    Ok(stream) => {
                        let client_id = ClientId(Uuid::new_v4().to_string());
                        println!("New NamedPipe connection: {client_id:?}");

                        let (tx, rx) = mpsc::unbounded_channel::<ClientMsg>();
                        clients.lock().await.insert(client_id.clone(), tx);

                        let actor = ClientActor {
                            client_id,
                            stream,
                            rx,
                            objects,
                            clients,
                            subscriptions: subs,
                            calls,
                        };
                        tokio::spawn(actor.run());
                    }
                    Err(e) => eprintln!("NamedPipe connection error: {e:?}"),
                }
            });
        }
    }
}
