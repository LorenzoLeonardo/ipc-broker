use async_trait::async_trait;
use serde_json::Value;
use std::{collections::HashMap, future, sync::Arc};
#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::watch,
};

use crate::{
    broker::{read_packet, write_packet},
    rpc::{RpcRequest, RpcResponse},
};

/// Abstraction for any asynchronous stream that can both read and write.
///
/// This trait is implemented for all types that implement
/// [`AsyncRead`] and [`AsyncWrite`] with [`Unpin`].
///
/// This allows the worker runtime to remain agnostic of the underlying
/// transport (TCP, Unix socket, or Windows named pipe).
trait AsyncStream: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite + Unpin> AsyncStream for T {}

/// Trait for an object that can be exposed to the RPC worker.
///
/// A `SharedObject` defines how a given object responds to remote
/// procedure calls. Each object must implement a single method:
///
/// - [`call`](SharedObject::call): Handles a method call with JSON arguments
///   and returns a JSON value as a result.
///
/// Objects must be thread-safe (`Send + Sync`) since they may be shared
/// across async tasks.
#[async_trait]
pub trait SharedObject: Send + Sync {
    /// Handle an RPC call for this object.
    ///
    /// # Arguments
    ///
    /// * `method` - The method name to invoke on this object.
    /// * `args` - JSON-encoded arguments to the method.
    ///
    /// # Returns
    ///
    /// JSON value representing the result of the call.
    async fn call(&self, method: &str, args: &Value) -> Value;
}

/// Builder pattern for constructing and spawning a worker runtime.
///
/// A `WorkerBuilder` allows you to register multiple [`SharedObject`]s
/// under a given name, and then spawn a worker that connects to an RPC
/// broker and handles incoming requests.
///
/// # Example
/// ```ignore
/// struct Calculator;
///
/// #[async_trait]
/// impl SharedObject for Calculator {
///     async fn call(&self, method: &str, args: &Value) -> Value {
///         match method {
///             "add" => {
///                 let a = args["a"].as_i64().unwrap();
///                 let b = args["b"].as_i64().unwrap();
///                 Value::from(a + b)
///             }
///             _ => Value::Null,
///         }
///     }
/// }
///
/// WorkerBuilder::new()
///     .add("Calculator", Calculator)
///     .spawn()
///     .await
///     .unwrap();
/// ```
pub struct WorkerBuilder {
    objects: HashMap<String, Arc<dyn SharedObject>>,
    shutdown_rx: Option<watch::Receiver<bool>>,
}

impl WorkerBuilder {
    /// Create a new, empty worker builder.
    pub fn new() -> Self {
        Self {
            objects: HashMap::new(),
            shutdown_rx: None,
        }
    }

    /// Add a named object to the worker.
    ///
    /// # Arguments
    ///
    /// * `name` - The name under which this object will be registered.
    /// * `obj` - The object implementing [`SharedObject`].
    ///
    /// # Returns
    ///
    /// Updated [`WorkerBuilder`] so calls can be chained.
    pub fn add<T>(mut self, name: &str, obj: T) -> Self
    where
        T: SharedObject + 'static,
    {
        self.objects.insert(name.to_string(), Arc::new(obj));
        self
    }

    pub fn with_graceful_shutdown(mut self) -> (Self, watch::Sender<bool>) {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        self.shutdown_rx = Some(shutdown_rx);
        (self, shutdown_tx)
    }

    /// Spawn the worker runtime with all registered objects.
    ///
    /// This connects to the broker (via TCP, Unix socket, or Windows named
    /// pipe depending on platform and environment) and begins handling
    /// RPC requests.
    pub async fn spawn(self) -> std::io::Result<()> {
        worker_supervisor(self.objects, self.shutdown_rx).await
    }
}

impl Default for WorkerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// This supervisor will manage the worker whether to retry if connection failed or stop gracefully
/// with an OS signal to stop.
async fn worker_supervisor(
    objects: HashMap<String, Arc<dyn SharedObject>>,
    shutdown_rx: Option<watch::Receiver<bool>>,
) -> std::io::Result<()> {
    use std::time::Duration;

    loop {
        let shutdown_rx = shutdown_rx.clone();
        match run_worker(objects.clone(), shutdown_rx.clone()).await {
            Ok(_) => {
                log::warn!("Worker exited normally, ending service now...");
                break;
            }
            Err(e) => {
                log::error!("Worker has failed to connect: {e}, retrying. . .");
            }
        }

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                log::info!("Supervisor shutting down");
                break;
            }
            // ✅ Optional graceful shutdown
            _ = async {
                if let Some(mut rx) = shutdown_rx {
                    let _ = rx.changed().await;
                } else {
                    future::pending::<()>().await;
                }
            } => {
                log::info!("Shutdown signal received (watch)");
                break;
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }

    Ok(())
}

/// Worker runtime function that handles broker connection and RPC calls.
///
/// This function is not intended to be used directly. Instead, use
/// [`WorkerBuilder::spawn`].
///
/// The worker will:
/// 1. Connect to the broker (TCP if `BROKER_ADDR` env var is set, otherwise
///    Unix socket on Unix or named pipe on Windows).
/// 2. Register all shared objects.
/// 3. Enter a loop processing incoming requests:
///    - [`RpcRequest::Call`] will invoke the corresponding object’s method.
///    - Results are sent back as [`RpcResponse::Result`].
///    - Other requests and responses are logged.
///
/// # Errors
///
/// Returns an error if the connection to the broker fails or if I/O errors occur.
async fn run_worker(
    objects: HashMap<String, Arc<dyn SharedObject>>,
    shutdown_rx: Option<watch::Receiver<bool>>,
) -> std::io::Result<()> {
    let mut stream: Box<dyn AsyncStream + Send + Unpin> =
        if let Ok(ip) = std::env::var("BROKER_ADDR") {
            let tcp = TcpStream::connect(ip.as_str()).await?;
            log::info!("Connected into TCP: {ip}");
            Box::new(tcp)
        } else {
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

    // Register all objects
    for name in objects.keys() {
        let reg = RpcRequest::RegisterObject {
            object_name: name.clone(),
        };
        let data = serde_json::to_vec(&reg).unwrap();
        write_packet(&mut stream, &data).await?;
    }

    let mut error = None;
    loop {
        let shutdown_rx = shutdown_rx.clone();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                log::info!("Ctrl-C received, shutting down worker loop.");
                break;
            }
            // ✅ Optional graceful shutdown
            _ = async {
                if let Some(mut rx) = shutdown_rx {
                    let _ = rx.changed().await;
                } else {
                    future::pending::<()>().await;
                }
            } => {
                log::info!("Shutdown signal received (watch)");
                break;
            }
            result = read_packet(&mut stream) => {
                let buf = match result {
                    Ok(data) => data,
                    Err(err) => {
                        // ✅ Log once
                        log::error!("Connection lost: {err}");
                        // ✅ Exit loop
                        error = Some(err);
                        break;
                    }
                };

                if let Ok(req) = serde_json::from_slice::<RpcRequest>(&buf) {
                    match req {
                        RpcRequest::Call {
                            call_id,
                            object_name,
                            method,
                            args,
                        } => {
                            if let Some(obj) = objects.get(&object_name) {
                                let result = obj.call(&method, &args).await;

                                let resp = RpcResponse::Result {
                                    call_id,
                                    object_name,
                                    value: result,
                                };

                                let resp_bytes = serde_json::to_vec(&resp).unwrap();

                                if let Err(err) = write_packet(&mut stream, &resp_bytes).await {
                                    log::error!("Write error (closing connection): {err}");
                                    error = Some(err);
                                    break;
                                }
                            } else {
                                log::error!("Unknown object: {object_name}");
                            }
                        }
                        _ => {
                            log::debug!("Unsupported request: {req:?}");
                        }
                    }
                } else if let Ok(resp) = serde_json::from_slice::<RpcResponse>(&buf) {
                    log::debug!("Worker got response: {resp:?}");
                } else {
                    log::error!("Invalid JSON value from broker");
                }
            }
        }
    }

    if let Some(error) = error {
        Err(error)
    } else {
        Ok(())
    }
}
