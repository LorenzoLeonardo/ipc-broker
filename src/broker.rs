//! # RPC Broker
//!
//! This module implements a lightweight, async RPC broker using Tokio.  
//!
//! The broker acts as a message hub where multiple clients can:
//! - Register objects (services).
//! - Call methods on remote objects.
//! - Subscribe and publish events.
//! - Exchange request/response messages across TCP, Unix sockets, or Windows named pipes.
//!
//! ## Features
//! - Supports multiple transports: TCP, Unix domain sockets, Windows named pipes.
//! - Handles client lifecycle, cleanup, and error handling.
//! - Implements an **actor model** per client for safe concurrent message processing.
//! - Provides subscription-based event distribution.
//!
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

#[cfg(unix)]
use crate::activate::{self, ServiceEntry, ServiceState, SharedServices};
// RPC protocol types
use crate::rpc::TCP_ADDR;
use crate::rpc::{CallId, ClientId, RpcRequest, RpcResponse};
use crate::worker::{SharedObject, WorkerBuilder};

/// Type alias for the message channel used by each client actor.
type ClientSender = mpsc::UnboundedSender<ClientMsg>;

/// Shared registry of active clients.
type SharedClients = Arc<Mutex<HashMap<ClientId, ClientSender>>>;

/// Shared registry of registered objects (service name → client owner).
type SharedObjects = Arc<Mutex<HashMap<String, ClientId>>>;

/// Shared subscription state:
/// `object_name -> topic -> set of subscribers`.
type SharedSubscriptions = Arc<Mutex<HashMap<String, HashMap<String, HashSet<ClientId>>>>>;

/// Shared in-flight call map (call ID → original caller client).
type SharedCalls = Arc<Mutex<HashMap<CallId, ClientId>>>;

/// Message sent to a client actor.
///
/// This is the unit of communication used internally between the broker and
/// client-handling tasks.
#[derive(Debug)]
enum ClientMsg {
    /// Outgoing serialized bytes to be written to the client.
    Outgoing(Vec<u8>),
    /// Signal to shutdown the client writer loop.
    _Shutdown,
}

/// Trait alias for supported asynchronous transport streams.
///
/// This includes:
/// - TCP streams
/// - Unix domain sockets
/// - Windows named pipes
trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

pub async fn read_packet<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let len = reader.read_u32().await?;
    log::trace!("Reading packet of length: {len}");
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_packet<W: AsyncWrite + Unpin>(
    writer: &mut W,
    data: &[u8],
) -> std::io::Result<()> {
    let len = data.len() as u32;
    // write length prefix first
    writer.write_u32(len).await?;
    log::trace!("Writing packet of length: {len}");
    // then write actual data
    writer.write_all(data).await?;
    // optionally flush to ensure it's sent immediately
    writer.flush().await?;

    Ok(())
}
/// Represents the per-client actor that owns the client connection.
///
/// Each client is assigned:
/// - A unique [`ClientId`].
/// - A read/write stream.
/// - A channel receiver for outbound messages.
/// - References to shared broker state.
///
/// The actor spawns independent read/write loops and ensures client cleanup
/// when disconnected.
struct ClientActor<S> {
    client_id: ClientId,
    stream: S,
    rx: mpsc::UnboundedReceiver<ClientMsg>,

    objects: SharedObjects,
    clients: SharedClients,
    subscriptions: SharedSubscriptions,
    calls: SharedCalls,
    #[cfg(unix)]
    services: SharedServices,
}

impl<S> ClientActor<S>
where
    S: Stream + 'static,
{
    /// Main loop for the client actor.
    ///
    /// Spawns a reader task and runs the writer loop. Cleans up the client
    /// state once the connection closes.
    async fn run(self) {
        let (reader, mut writer) = tokio::io::split(self.stream);
        let client_id = self.client_id.clone();
        let objects = self.objects.clone();
        let clients = self.clients.clone();
        let subscriptions = self.subscriptions.clone();
        let calls = self.calls.clone();
        #[cfg(unix)]
        let services = self.services.clone();
        let mut rx = self.rx;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        // Spawn reader
        let reader_task = tokio::spawn({
            let client_id = client_id.clone();
            let objects = self.objects.clone();
            let clients = self.clients.clone();
            let subs = self.subscriptions.clone();
            let calls = self.calls.clone();
            #[cfg(unix)]
            let services = self.services.clone();
            async move {
                let res = Self::reader_loop(
                    reader,
                    client_id.clone(),
                    objects,
                    clients,
                    subs,
                    calls,
                    #[cfg(unix)]
                    services,
                )
                .await;
                let _ = shutdown_tx.send(()); // notify writer
                res
            }
        });

        // Writer loop (with shutdown reaction)
        Self::writer_loop(&mut writer, client_id.clone(), &mut rx, shutdown_rx).await;

        let _ = reader_task.await;

        // ✅ Cleanup here
        Self::cleanup_client(
            &client_id,
            &clients,
            &objects,
            &subscriptions,
            &calls,
            #[cfg(unix)]
            &services,
        )
        .await;

        log::info!("CONNECTION ENDED: {client_id:?}");
    }

    /// Cleans up broker state when a client disconnects:
    /// - Removes the client from the clients registry.
    /// - Removes objects owned by the client.
    /// - Removes subscriptions from the client.
    /// - Cleans up any pending calls.
    async fn cleanup_client(
        client_id: &ClientId,
        clients: &SharedClients,
        objects: &SharedObjects,
        subscriptions: &SharedSubscriptions,
        calls: &SharedCalls,
        #[cfg(unix)] services: &SharedServices,
    ) {
        log::debug!("Cleaning up client {client_id:?}");

        // 1️⃣ Remove client sender
        clients.lock().await.remove(client_id);

        // 2️⃣ Remove objects owned by this client
        objects.lock().await.retain(|_, owner| owner != client_id);

        // 3️⃣ Remove subscriptions
        {
            let mut subs = subscriptions.lock().await;
            for topic_map in subs.values_mut() {
                for subs_set in topic_map.values_mut() {
                    subs_set.remove(client_id);
                }
            }
        }

        // 4️⃣ Remove calls where this client was the caller
        calls.lock().await.retain(|_, caller| caller != client_id);

        // 5️⃣ Cleanup services
        #[cfg(unix)]
        {
            // Snapshot remaining active calls
            let active_calls: HashSet<CallId> = {
                let calls = calls.lock().await;
                calls.keys().cloned().collect()
            };

            let mut services = services.lock().await;

            for service in services.values_mut() {
                // 🔥 Service process disconnected → reset fully
                if service.owner.as_ref() == Some(client_id) {
                    log::warn!(
                        "Service {} disconnected; resetting state",
                        service.service_name
                    );

                    service.owner = None;
                    service.state = ServiceState::Stopped;
                    service.pending_calls.clear();
                    continue;
                }

                // 🧹 Remove pending calls whose callers disappeared
                let before = service.pending_calls.len();

                let mut retained = Vec::with_capacity(before);
                for req in service.pending_calls.drain(..) {
                    match &req {
                        RpcRequest::Call { call_id, .. } if active_calls.contains(call_id) => {
                            retained.push(req)
                        }
                        RpcRequest::Call { call_id, .. } => {
                            log::debug!(
                                "Dropping stale pending call {:?} for service {}",
                                call_id,
                                service.service_name
                            );
                        }
                        _ => retained.push(req),
                    }
                }

                service.pending_calls = retained;

                // ⚠️ Do NOT reset Starting here – watchdog handles that
            }
        }

        log::debug!("Cleanup complete for client {client_id:?}");
    }

    /// Reads messages from the client stream, parses JSON, and dispatches them
    /// as [`RpcRequest`] or [`RpcResponse`] to the broker [`ServerState`].
    async fn reader_loop<R>(
        mut reader: R,
        client_id: ClientId,
        objects: SharedObjects,
        clients: SharedClients,
        subscriptions: SharedSubscriptions,
        calls: SharedCalls,
        #[cfg(unix)] services: SharedServices,
    ) where
        R: AsyncRead + Unpin,
    {
        let server_state = ServerState {
            objects,
            clients,
            subscriptions,
            calls,
            #[cfg(unix)]
            services,
        };

        loop {
            let buf = match read_packet(&mut reader).await {
                Ok(data) => data,
                Err(err) => {
                    log::error!("READ ERROR: {client_id:?}, {err}");
                    break;
                }
            };
            if let Ok(req) = serde_json::from_slice::<RpcRequest>(&buf) {
                server_state.handle_request(req, &client_id).await;
            } else if let Ok(resp) = serde_json::from_slice::<RpcResponse>(&buf) {
                server_state.handle_response(resp, &client_id).await;
            } else {
                log::error!("Invalid JSON value from {client_id:?}");
            }
        }
    }

    /// Writer loop for sending outbound messages to the client.
    ///
    /// Waits for:
    /// - Outgoing messages on the channel.
    /// - A shutdown signal from the reader task.
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
                log::debug!("Shutdown signal received by writer for {client_id:?}");
            }
            _ = async {
                while let Some(msg) = rx.recv().await {
                    match msg {
                        ClientMsg::Outgoing(bytes) => {
                            if let Err(e) =  write_packet(writer, &bytes).await {
                                log::error!("Write error {client_id}: {e}");
                                break;
                            }
                        }
                        ClientMsg::_Shutdown => {
                            log::debug!("Writer got shutdown for {client_id:?}");
                            break;
                        }
                    }
                }
            } => {}
        }
    }
}

/// Shared broker state.
///
/// This is cloned into each client actor and used to manage shared resources
/// such as:
/// - Registered objects.
/// - Connected clients.
/// - Subscriptions.
/// - Pending calls.
#[derive(Clone)]
struct ServerState {
    objects: SharedObjects,
    clients: SharedClients,
    subscriptions: SharedSubscriptions,
    calls: SharedCalls,
    #[cfg(unix)]
    services: SharedServices,
}

impl ServerState {
    /// Handles incoming RPC requests from clients and routes them to the
    /// appropriate handler.
    async fn handle_request(&self, req: RpcRequest, client_id: &ClientId) {
        match req {
            RpcRequest::RegisterObject { object_name } => {
                self.handle_register_object(object_name, client_id).await;
            }
            RpcRequest::RegisterService {
                object_name,
                service_name,
            } => {
                #[cfg(unix)]
                self.handle_register_service(object_name, service_name, client_id)
                    .await;

                #[cfg(windows)]
                {
                    unimplemented!("No applicable on windows, {object_name}, {service_name}");
                }
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

            RpcRequest::Subscribe { object_name, topic } => {
                self.handle_subscribe(object_name, topic, client_id).await;
            }

            RpcRequest::Publish {
                object_name,
                topic,
                args,
            } => {
                self.handle_publish(object_name, topic, args).await;
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

    /// Handles RPC responses, routing them back to the original caller.
    async fn handle_response(&self, resp: RpcResponse, _from: &ClientId) {
        match &resp {
            RpcResponse::Result { call_id, .. }
            | RpcResponse::Error {
                call_id: Some(call_id),
                ..
            } => {
                // Look up original caller
                if let Some(caller) = self.calls.lock().await.remove(call_id) {
                    log::debug!("Forwarding response for call_id {call_id:?} to caller {caller:?}");
                    Self::send_to_client(&self.clients, &caller, &resp).await;
                } else {
                    log::error!("No caller found for call_id {call_id:?}");
                }
            }
            _ => {
                // Other response types (Subscribed, Event, etc.) are terminal, not forwarded
                log::error!("Unhandled response type: {resp:?}");
            }
        }
    }

    /// Registers an object owned by a client.
    async fn handle_register_object(&self, object_name: String, client_id: &ClientId) {
        // Register object ownership
        self.objects
            .lock()
            .await
            .insert(object_name.clone(), client_id.clone());

        #[cfg(unix)]
        let pending_calls = {
            let mut services = self.services.lock().await;
            if let Some(service) = services.get_mut(&object_name) {
                service.state = ServiceState::Running(client_id.clone());
                service.owner = Some(client_id.clone());
                std::mem::take(&mut service.pending_calls)
            } else {
                Vec::new()
            }
        };

        #[cfg(unix)]
        // Send queued calls
        for req in pending_calls {
            Self::send_to_client(&self.clients, client_id, &req).await;
        }

        Self::send_to_client(
            &self.clients,
            client_id,
            &RpcResponse::Registered { object_name },
        )
        .await;
    }

    #[cfg(unix)]
    async fn handle_register_service(
        &self,
        object_name: String,
        service_name: String,
        client_id: &ClientId,
    ) {
        let mut services = self.services.lock().await;

        services
            .entry(object_name.clone())
            .and_modify(|svc| {
                // Update service_name if changed (hot reload safe)
                svc.service_name = service_name.clone();

                // If service was in a bad state, reset
                if svc.state != ServiceState::Running(client_id.clone()) {
                    svc.state = ServiceState::Stopped;
                }

                log::debug!(
                    "Updated service activation for {} -> {}",
                    object_name,
                    service_name
                );
            })
            .or_insert_with(|| {
                log::info!(
                    "Registered service activation for {} -> {}",
                    object_name,
                    service_name
                );

                ServiceEntry {
                    service_name,
                    state: ServiceState::Stopped,
                    pending_calls: Vec::new(),
                    owner: None,
                }
            });
    }

    /// Forwards a call to the object’s owning client, or responds with an error
    /// if the object does not exist.
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

        // 1️⃣ Is service already running?
        if let Some(worker_id) = self.objects.lock().await.get(&object_name).cloned() {
            Self::send_to_client(
                &self.clients,
                &worker_id,
                &RpcRequest::Call {
                    call_id,
                    object_name,
                    method,
                    args,
                },
            )
            .await;
            return;
        }
        #[cfg(unix)]
        {
            // Whether we need to start the service
            let mut start_service = false;
            let mut service_name = None;

            // 2️⃣ Known but not running?
            {
                let mut services = self.services.lock().await;

                if let Some(service) = services.get_mut(&object_name) {
                    match service.state {
                        ServiceState::Stopped => {
                            log::info!("Auto-starting service {object_name}");
                            service.state = ServiceState::Starting;
                            service.pending_calls.push(RpcRequest::Call {
                                call_id,
                                object_name,
                                method,
                                args,
                            });

                            start_service = true;
                            service_name = Some(service.service_name.clone());
                        }
                        ServiceState::Starting => {
                            service.pending_calls.push(RpcRequest::Call {
                                call_id,
                                object_name,
                                method,
                                args,
                            });
                        }
                        _ => {}
                    }
                } else {
                    // Unknown service
                    let message = format!("No such object '{object_name}'");
                    Self::send_to_client(
                        &self.clients,
                        client_id,
                        &RpcResponse::Error {
                            call_id: Some(call_id),
                            object_name,
                            message,
                        },
                    )
                    .await;
                    return;
                }
            } // 🔓 lock released here

            // 3️⃣ Start service OUTSIDE the lock
            if start_service && let Some(name) = service_name {
                activate::spawn_service(&name);
            }
        }
        #[cfg(windows)]
        {
            let message = format!("No such object '{object_name}'");
            Self::send_to_client(
                &self.clients,
                client_id,
                &RpcResponse::Error {
                    call_id: Some(call_id),
                    object_name,
                    message,
                },
            )
            .await;
        }
    }

    /// Subscribes a client to a topic under a given object.
    async fn handle_subscribe(&self, object_name: String, topic: String, client_id: &ClientId) {
        log::debug!("Client {client_id:?} subscribing to {object_name}/{topic}");
        self.subscriptions
            .lock()
            .await
            .entry(object_name.clone())
            .or_default()
            .entry(topic.clone())
            .or_default()
            .insert(client_id.clone());

        let resp = RpcResponse::Subscribed { object_name, topic };
        Self::send_to_client(&self.clients, client_id, &resp).await;
    }

    /// Publishes an event to all subscribed clients.
    async fn handle_publish(&self, object_name: String, topic: String, args: serde_json::Value) {
        let subs_list = {
            let subs_guard = self.subscriptions.lock().await;
            subs_guard
                .get(&object_name)
                .and_then(|m| m.get(&topic))
                .map(|s| s.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        };

        if !subs_list.is_empty() {
            let event = RpcResponse::Event {
                object_name: object_name.clone(),
                topic: topic.clone(),
                args,
            };
            let bytes = match serde_json::to_vec(&event) {
                Ok(b) => b,
                Err(e) => {
                    log::error!("[Broker] Failed to serialize event for topic {topic}: {e}");
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

    /// Utility to send a serialized message to a client.
    async fn send_to_client<T: serde::Serialize>(
        clients: &SharedClients,
        client_id: &ClientId,
        msg: &T,
    ) {
        let bytes = match serde_json::to_vec(msg) {
            Ok(b) => b,
            Err(e) => {
                log::error!("[Broker] Failed to serialize message for {client_id:?}: {e}");
                return; // skip this send but keep the broker running
            }
        };
        let clients_guard = clients.lock().await;
        if let Some(tx) = clients_guard.get(client_id) {
            let _ = tx.send(ClientMsg::Outgoing(bytes));
        }
    }
}

fn get_local_ip_port() -> String {
    match local_ip_address::local_ip() {
        Ok(result) => {
            let port = TCP_ADDR
                .rsplit(':')
                .next()
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(5123);
            format!("{result}:{port}")
        }
        Err(e) => {
            log::error!("get_local_ip_port: {e}, defaulting to {TCP_ADDR}");
            TCP_ADDR.to_string()
        }
    }
}

/// Start a TCP listener for incoming broker connections.
async fn start_tcp_listener(
    objects: SharedObjects,
    clients: SharedClients,
    subscriptions: SharedSubscriptions,
    calls: SharedCalls,
    #[cfg(unix)] services: SharedServices,
) -> std::io::Result<JoinHandle<()>> {
    let addr = get_local_ip_port();
    let tcp_listener = TcpListener::bind(addr.as_str()).await?;
    log::info!("Broker listening on TCP {addr}");

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = signal::ctrl_c() => {
                    log::info!("Shutdown signal received, stopping TCP socket listener.");
                    break;
                }
                _ = async {
                    match tcp_listener.accept().await {
                        Ok((stream, _)) => match stream.peer_addr() {
                            Ok(peer) => {
                                log::info!("A Client connected via TCP: {}:{}", peer.ip(), peer.port());
                                spawn_client(
                                    stream,
                                    objects.clone(),
                                    clients.clone(),
                                    subscriptions.clone(),
                                    calls.clone(),
                                    #[cfg(unix)]
                                    services.clone()
                                );
                            }
                            Err(e) => {
                                log::error!("TCP peer error: {e}")
                            }
                        },
                        Err(e) => log::error!("TCP accept error: {e}"),
                    }
                } => {}
            }
        }
    });

    Ok(handle)
}

#[cfg(unix)]
/// Start a Unix domain socket listener for incoming broker connections.
///
/// This is only available on Unix systems.
async fn start_unix_listener(
    objects: SharedObjects,
    clients: SharedClients,
    subscriptions: SharedSubscriptions,
    calls: SharedCalls,
    services: SharedServices,
) -> std::io::Result<JoinHandle<()>> {
    use tokio::net::UnixListener;

    use crate::rpc::UNIX_PATH;

    let _ = std::fs::remove_file(UNIX_PATH); // cleanup old
    let unix_listener = UnixListener::bind(UNIX_PATH)?;
    log::info!("Broker listening on Unix {UNIX_PATH}");

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = signal::ctrl_c() => {
                    log::info!("Shutdown signal received, stopping unix socket listener.");
                    break;
                }
                _ = async {
                    match unix_listener.accept().await {
                        Ok((stream, _)) => match stream.peer_addr() {
                            Ok(_) => {
                                log::info!("A Client connected via Unix: {UNIX_PATH}");
                                spawn_client(
                                    stream,
                                    objects.clone(),
                                    clients.clone(),
                                    subscriptions.clone(),
                                    calls.clone(),
                                    services.clone(),
                                );
                            }
                            Err(e) => {
                                log::error!("Unix peer error: {e}")
                            }
                        },
                        Err(e) => log::error!("Unix accept error: {e}"),
                    }
                } => {}
            }
        }
    });
    Ok(handle)
}

#[cfg(windows)]
/// Start a Windows named pipe listener for incoming broker connections.
///
/// This is only available on Windows systems.
async fn start_named_pipe_listener(
    objects: SharedObjects,
    clients: SharedClients,
    subscriptions: SharedSubscriptions,
    calls: SharedCalls,
) -> std::io::Result<JoinHandle<()>> {
    use std::{ffi::c_void, ptr::null_mut, time::Duration};

    use tokio::{net::windows::named_pipe::ServerOptions, signal};
    use windows::{
        Win32::Foundation::{HLOCAL, LocalFree},
        Win32::Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        core::BOOL,
    };

    use crate::rpc::PIPE_PATH;

    log::info!("Broker listening on NamedPipe {}", PIPE_PATH);
    let handle = unsafe {
        let sddl = windows::core::w!("D:(A;;GA;;;WD)");
        let mut p_sd: PSECURITY_DESCRIPTOR = PSECURITY_DESCRIPTOR(null_mut());

        let success = ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl,
            SDDL_REVISION_1,
            &mut p_sd,
            Some(null_mut()),
        );

        if let Err(e) = success {
            log::error!("Failed to create security descriptor: {e}");
            return Err(std::io::Error::other(e.to_string()));
        }

        let sa_box = Box::new(SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: p_sd.0,
            bInheritHandle: BOOL(0),
        });

        let sa_raw_ptr = Box::into_raw(sa_box) as *mut c_void;

        struct SecurityAttributesHolder {
            ptr: *mut c_void,
        }
        unsafe impl Send for SecurityAttributesHolder {}
        unsafe impl Sync for SecurityAttributesHolder {}

        // Since we are not freeing the SECURITY_DESCRIPTOR itself, we need to ensure
        // we free the SECURITY_ATTRIBUTES when done.
        impl Drop for SecurityAttributesHolder {
            fn drop(&mut self) {
                unsafe {
                    if self.ptr.is_null() {
                        return;
                    }
                    let handle = HLOCAL(self.ptr);
                    let res = LocalFree(Some(handle));

                    if !res.0.is_null() {
                        log::warn!("LocalFree failed when freeing SECURITY_DESCRIPTOR");
                    }
                    log::info!("SecurityAttributesHolder dropped and memory freed.");
                }
            }
        }

        let holder = Arc::new(SecurityAttributesHolder { ptr: sa_raw_ptr });
        let holder_clone = holder.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = signal::ctrl_c() => {
                        log::info!("Shutdown signal received, stopping NamedPipe listener.");
                        break;
                    }
                    _ = async {
                        match ServerOptions::new()
                            .create_with_security_attributes_raw(PIPE_PATH, holder_clone.ptr)
                        {
                            Ok(server) => {
                                let objects = objects.clone();
                                let clients = clients.clone();
                                let subs = subscriptions.clone();
                                let calls = calls.clone();

                                match server.connect().await {
                                    Ok(()) => {
                                        log::info!("Client connected via NamedPipe: {PIPE_PATH}");
                                        spawn_client(server, objects, clients, subs, calls);
                                    }
                                    Err(e) => {
                                        log::error!("NamedPipe connection failed: {e}");
                                        tokio::time::sleep(Duration::from_millis(200)).await;
                                    }
                                }
                            }
                            Err(e) => {
                                log::error!("Pipe creation failed: {e}");
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                        }
                    } => {}
                }
            }
        })
    };
    Ok(handle)
}

/// Spawns a new client actor from a given stream.
///
/// Each client is assigned a unique [`ClientId`].
fn spawn_client<S>(
    stream: S,
    objects: SharedObjects,
    clients: SharedClients,
    subscriptions: SharedSubscriptions,
    calls: SharedCalls,
    #[cfg(unix)] services: SharedServices,
) where
    S: Stream + 'static,
{
    let client_id = ClientId::from(Uuid::new_v4());
    log::info!("CONNECTION STARTED: {client_id:?}");

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
        #[cfg(unix)]
        services,
    };
    tokio::spawn(actor.run());
}

struct ListObjects {
    objects: SharedObjects,
}

#[async_trait]
impl SharedObject for ListObjects {
    async fn call(&self, method: &str, _args: &Value) -> Value {
        match method {
            "listObjects" => {
                let objs = self.objects.lock().await;
                let entries: Vec<String> = objs.keys().cloned().collect();

                serde_json::to_value(entries).unwrap_or(Value::Null)
            }
            _ => Value::Null,
        }
    }
}

/// Entry point for running the broker.
///
/// This function:
/// - Initializes shared state.
/// - Starts transport listeners (TCP + Unix/Named Pipes).
/// - Waits for `Ctrl+C` to gracefully shut down the broker.
pub async fn run_broker() -> std::io::Result<()> {
    let objects: SharedObjects = Arc::new(Mutex::new(HashMap::new()));
    let clients: SharedClients = Arc::new(Mutex::new(HashMap::new()));
    let subscriptions: SharedSubscriptions = Arc::new(Mutex::new(HashMap::new()));
    let calls: SharedCalls = Arc::new(Mutex::new(HashMap::new()));
    #[cfg(unix)]
    let services: SharedServices = Arc::new(Mutex::new(HashMap::new()));
    #[cfg(unix)]
    activate::load_service_activations(&services).await;

    // Spawn listeners
    let mode = std::env::var("APP_MODE").unwrap_or_else(|_| "release".into());
    let mut tcp_listener_handle = None;
    match mode.as_str() {
        "debug" => {
            log::info!("Running in {mode} mode — TCP listener on IP/port is enabled.");
            tcp_listener_handle = Some(
                start_tcp_listener(
                    objects.clone(),
                    clients.clone(),
                    subscriptions.clone(),
                    calls.clone(),
                    #[cfg(unix)]
                    services.clone(),
                )
                .await?,
            );
        }
        "release" => {
            log::info!("Running in {mode} mode — TCP listener on IP/port is disabled.");
        }
        _ => {
            log::error!("Unknown '{mode}' mode, defaulting to production behavior.");
        }
    }

    #[cfg(unix)]
    let listener_handle =
        start_unix_listener(objects.clone(), clients, subscriptions, calls, services).await?;

    #[cfg(windows)]
    let listener_handle =
        start_named_pipe_listener(objects.clone(), clients, subscriptions, calls).await?;

    let rob_handle = tokio::spawn(
        WorkerBuilder::new()
            .add("rob", ListObjects { objects })
            .spawn(),
    );

    if let Some(tcp_handle) = tcp_listener_handle {
        let _ = tokio::join!(listener_handle, rob_handle, tcp_handle);
    } else {
        let _ = tokio::join!(listener_handle, rob_handle);
    }

    log::info!("ipc-broker shutting down...");
    Ok(())
}
