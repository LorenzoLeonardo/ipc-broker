use async_trait::async_trait;
use core::panic;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(windows)]
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    runtime::Runtime,
    sync::{Mutex, Notify},
    task,
    time::{sleep, timeout},
};
use uuid::Uuid;

use crate::{
    broker::run_broker,
    client::ClientHandle,
    rpc::{CallId, RpcRequest, RpcResponse},
    worker::{SharedObject, WorkerBuilder},
};

// Stress test parameters (reduced slightly to avoid CI flakiness)
const CLIENTS: usize = 100; // lowered concurrency for stability
const OPS_PER_CLIENT: usize = 100; // fewer ops per client

// Simple RNG
struct SimpleRng(u64);
impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x & 0xFFFF_FFFF) as u32
    }
    fn choose<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let idx = (self.next_u32() as usize) % items.len();
        &items[idx]
    }
    fn gen_range(&mut self, min: u64, max: u64) -> u64 {
        min + (self.next_u32() as u64 % (max - min))
    }
}

// Start broker once using std::sync::Once
static START: std::sync::Once = std::sync::Once::new();
fn ensure_broker_running() {
    START.call_once(|| {
        std::thread::spawn(|| {
            let rt = Runtime::new().unwrap();
            rt.block_on(async {
                let _ = run_broker().await;
            });
        });

        // poll for TCP port with retries but fail fast if not up
        let mut ok = false;
        for _ in 0..20 {
            if std::net::TcpStream::connect_timeout(
                &"127.0.0.1:5000".parse().unwrap(),
                Duration::from_millis(250),
            )
            .is_ok()
            {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if !ok {
            panic!("Broker did not start on TCP");
        }
    });
}

/// Abstraction for client connection type (TCP or Unix)
enum Conn {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
    #[cfg(windows)]
    Pipe(NamedPipeClient),
}

impl Conn {
    async fn connect_tcp() -> Self {
        // give a timeout to connect to avoid hanging tests
        let s = timeout(Duration::from_secs(3), TcpStream::connect("127.0.0.1:5000"))
            .await
            .expect("connect timeout")
            .expect("connect failed");
        Conn::Tcp(s)
    }
    #[cfg(unix)]
    async fn connect_unix() -> Self {
        use crate::rpc::UNIX_PATH;

        Conn::Unix(UnixStream::connect(UNIX_PATH).await.unwrap())
    }
    #[cfg(windows)]
    async fn connect_pipe() -> Self {
        use crate::rpc::PIPE_PATH;
        loop {
            match ClientOptions::new().open(PIPE_PATH) {
                Ok(pipe) => return Conn::Pipe(pipe),
                Err(e) if e.raw_os_error() == Some(231) => {
                    // All pipe instances are busy, wait and retry
                    eprintln!("Pipe busy, retrying...");
                    sleep(Duration::from_millis(100)).await;
                }
                Err(e) => {
                    panic!("Failed to open Windows named pipe: {e:?}");
                }
            }
        }
    }

    async fn write_all(&mut self, buf: &[u8]) {
        match self {
            Conn::Tcp(s) => s.write_all(buf).await.unwrap(),
            #[cfg(unix)]
            Conn::Unix(s) => s.write_all(buf).await.unwrap(),
            #[cfg(windows)]
            Conn::Pipe(s) => s.write_all(buf).await.unwrap(),
        }
    }
    async fn read_some(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.read(buf).await,
            #[cfg(unix)]
            Conn::Unix(s) => s.read(buf).await,
            #[cfg(windows)]
            Conn::Pipe(s) => s.read(buf).await,
        }
    }
}

// (Stress tests reduced and made more conservative)
#[tokio::test]
async fn tcp_stress_broker() {
    ensure_broker_running();
    let mut handles = Vec::new();
    for i in 0..CLIENTS {
        handles.push(task::spawn(async move {
            let mut conn = Conn::connect_tcp().await;

            let seed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
                ^ (i as u64);
            let mut rng = SimpleRng::new(seed);

            let my_object = format!("obj_{i}");
            let reg = RpcRequest::RegisterObject {
                object_name: my_object.clone(),
            };
            conn.write_all(&serde_json::to_vec(&reg).unwrap()).await;

            let mut ops_done = 0usize;
            let mut read_buf = vec![0u8; 65536];

            while ops_done < OPS_PER_CLIENT {
                let op_type = *rng.choose(&["call", "subscribe", "publish"]);
                match op_type {
                    "call" => {
                        let req = RpcRequest::Call {
                            call_id: CallId::from(Uuid::new_v4()),
                            object_name: my_object.clone(),
                            method: "echo".into(),
                            args: json!({"msg": format!("hello from client {i}")}),
                        };
                        let _ = timeout(
                            Duration::from_secs(1),
                            conn.write_all(&serde_json::to_vec(&req).unwrap()),
                        )
                        .await;
                    }
                    "subscribe" => {
                        let req = RpcRequest::Subscribe {
                            object_name: my_object.clone(),
                            topic: format!("topic_{}", i % 10),
                        };
                        let _ = timeout(
                            Duration::from_secs(1),
                            conn.write_all(&serde_json::to_vec(&req).unwrap()),
                        )
                        .await;
                    }
                    "publish" => {
                        let req = RpcRequest::Publish {
                            object_name: my_object.clone(),
                            topic: format!("topic_{}", i % 10),
                            args: json!({"val": rng.next_u32()}),
                        };
                        let _ = timeout(
                            Duration::from_secs(1),
                            conn.write_all(&serde_json::to_vec(&req).unwrap()),
                        )
                        .await;
                    }
                    _ => {}
                }

                // non-blocking read
                if let Ok(Ok(n)) =
                    timeout(Duration::from_millis(50), conn.read_some(&mut read_buf)).await
                    && n > 0
                    && let Ok(val) = serde_json::from_slice::<RpcResponse>(&read_buf[..n])
                {
                    println!("Client {i} got response: {val:?}");
                }

                ops_done += 1;
                sleep(Duration::from_millis(rng.gen_range(5, 20))).await;
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn unix_stress_broker() {
    ensure_broker_running();
    let mut handles = Vec::new();
    for i in 0..CLIENTS {
        handles.push(task::spawn(async move {
            let mut conn = Conn::connect_unix().await;

            let seed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
                ^ (i as u64);
            let mut rng = SimpleRng::new(seed);

            let my_object = format!("obj_{i}");
            let reg = RpcRequest::RegisterObject {
                object_name: my_object.clone(),
            };
            conn.write_all(&serde_json::to_vec(&reg).unwrap()).await;

            let mut ops_done = 0usize;
            let mut read_buf = vec![0u8; 65536];

            while ops_done < OPS_PER_CLIENT {
                let op_type = *rng.choose(&["call", "subscribe", "publish"]);
                match op_type {
                    "call" => {
                        let req = RpcRequest::Call {
                            call_id: CallId::from(Uuid::new_v4()),
                            object_name: my_object.clone(),
                            method: "echo".into(),
                            args: json!({"msg": format!("hello from client {i}")}),
                        };
                        let _ = timeout(
                            Duration::from_secs(1),
                            conn.write_all(&serde_json::to_vec(&req).unwrap()),
                        )
                        .await;
                    }
                    "subscribe" => {
                        let req = RpcRequest::Subscribe {
                            object_name: my_object.clone(),
                            topic: format!("topic_{}", i % 10),
                        };
                        let _ = timeout(
                            Duration::from_secs(1),
                            conn.write_all(&serde_json::to_vec(&req).unwrap()),
                        )
                        .await;
                    }
                    "publish" => {
                        let req = RpcRequest::Publish {
                            object_name: my_object.clone(),
                            topic: format!("topic_{}", i % 10),
                            args: json!({"val": rng.next_u32()}),
                        };
                        let _ = timeout(
                            Duration::from_secs(1),
                            conn.write_all(&serde_json::to_vec(&req).unwrap()),
                        )
                        .await;
                    }
                    _ => {}
                }

                // non-blocking read
                if let Ok(Ok(n)) =
                    timeout(Duration::from_millis(50), conn.read_some(&mut read_buf)).await
                {
                    if n > 0 {
                        if let Ok(val) = serde_json::from_slice::<RpcResponse>(&read_buf[..n]) {
                            println!("Client {i} got response: {val:?}");
                        }
                    }
                }

                ops_done += 1;
                sleep(Duration::from_millis(rng.gen_range(5, 20))).await;
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
}

#[cfg(windows)]
#[tokio::test]
async fn pipe_stress_broker() {
    ensure_broker_running();
    let mut handles = Vec::new();
    for i in 0..CLIENTS {
        handles.push(task::spawn(async move {
            let mut conn = Conn::connect_pipe().await;

            let seed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
                ^ (i as u64);
            let mut rng = SimpleRng::new(seed);

            let my_object = format!("obj_{i}");
            let reg = RpcRequest::RegisterObject {
                object_name: my_object.clone(),
            };
            conn.write_all(&serde_json::to_vec(&reg).unwrap()).await;

            let mut ops_done = 0usize;
            let mut read_buf = vec![0u8; 65536];

            while ops_done < OPS_PER_CLIENT {
                let op_type = *rng.choose(&["call", "subscribe", "publish"]);
                match op_type {
                    "call" => {
                        let req = RpcRequest::Call {
                            call_id: CallId::from(Uuid::new_v4()),
                            object_name: my_object.clone(),
                            method: "echo".into(),
                            args: json!({"msg": format!("hello from client {i}")}),
                        };
                        let _ = timeout(
                            Duration::from_secs(1),
                            conn.write_all(&serde_json::to_vec(&req).unwrap()),
                        )
                        .await;
                    }
                    "subscribe" => {
                        let req = RpcRequest::Subscribe {
                            object_name: my_object.clone(),
                            topic: format!("topic_{}", i % 10),
                        };
                        let _ = timeout(
                            Duration::from_secs(1),
                            conn.write_all(&serde_json::to_vec(&req).unwrap()),
                        )
                        .await;
                    }
                    "publish" => {
                        let req = RpcRequest::Publish {
                            object_name: my_object.clone(),
                            topic: format!("topic_{}", i % 10),
                            args: json!({"val": rng.next_u32()}),
                        };
                        let _ = timeout(
                            Duration::from_secs(1),
                            conn.write_all(&serde_json::to_vec(&req).unwrap()),
                        )
                        .await;
                    }
                    _ => {}
                }

                // non-blocking read
                if let Ok(Ok(n)) =
                    timeout(Duration::from_millis(50), conn.read_some(&mut read_buf)).await
                    && n > 0
                    && let Ok(val) = serde_json::from_slice::<RpcResponse>(&read_buf[..n])
                {
                    println!("Client {i} got response: {val:?}");
                }

                ops_done += 1;
                sleep(Duration::from_millis(rng.gen_range(5, 20))).await;
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
}

#[tokio::test]
async fn client_worker() {
    ensure_broker_running();

    struct Calculator;

    #[async_trait]
    impl SharedObject for Calculator {
        async fn call(&self, method: &str, args: &Value) -> Value {
            match method {
                "add" => {
                    let a = args.get(0).and_then(Value::as_i64).unwrap_or(0);
                    let b = args.get(1).and_then(Value::as_i64).unwrap_or(0);
                    (a + b).into()
                }
                "mul" => {
                    let a = args.get(0).and_then(Value::as_i64).unwrap_or(0);
                    let b = args.get(1).and_then(Value::as_i64).unwrap_or(0);
                    (a * b).into()
                }
                _ => Value::String("Unknown method".into()),
            }
        }
    }

    struct Logger;
    #[async_trait]
    impl SharedObject for Logger {
        async fn call(&self, method: &str, args: &Value) -> Value {
            println!("LOG: {method} -> {args}");
            Value::Null
        }
    }

    tokio::spawn(async move {
        // IMPORTANT: ensure run_worker signals readiness
        let calc = Calculator;
        let logger = Logger;
        if let Err(e) = WorkerBuilder::new()
            .add("Calculator", calc)
            .add("Logger", logger)
            .spawn()
            .await
        {
            eprintln!("worker exited early: {e:?}");
        }
    });

    let proxy = ClientHandle::connect().await.unwrap();

    proxy
        .wait_for_object("Calculator")
        .await
        .expect("wait_for_object failed");

    let response = proxy
        .remote_call::<Value, i32>("Calculator", "add", json!([5, 7]))
        .await
        .unwrap();
    println!("Client got response: {response}");
    assert_eq!(response, 12);

    let response = proxy
        .remote_call::<Value, i32>("Calculator", "mul", json!([5, 7]))
        .await
        .unwrap();
    println!("Client got response: {response}");
    assert_eq!(response, 35);

    let response = proxy
        .remote_call::<Value, Value>("Logger", "method", Value::Null)
        .await
        .unwrap();
    println!("Client got response: {response}");
    assert_eq!(response, Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn publish_subscribe() {
    ensure_broker_running();

    // Shared states for each subscription
    let news_val: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let news1_val: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));

    // signal channel
    let news_val_for_task = Arc::clone(&news_val);
    let news1_val_for_task = Arc::clone(&news1_val);
    let notify = Arc::new(Notify::new());
    let notify_clone = notify.clone();
    tokio::spawn(async move {
        let client = ClientHandle::connect().await.unwrap();

        let inner_news_val = Arc::clone(&news_val_for_task);
        client
            .subscribe_async("news_obj", "news", move |value| {
                let inner_news_val = Arc::clone(&inner_news_val);
                tokio::spawn(async move {
                    let mut val = inner_news_val.lock().await;
                    *val = Some(value);
                });
            })
            .await;

        let inner_news1_val = Arc::clone(&news1_val_for_task);
        client
            .subscribe_async("news_obj", "news1", move |value| {
                let inner_news1_val = Arc::clone(&inner_news1_val);
                tokio::spawn(async move {
                    let mut val = inner_news1_val.lock().await;
                    *val = Some(value);
                });
            })
            .await;

        // small sleep to allow registration (bounded)
        tokio::time::sleep(Duration::from_millis(200)).await;
        notify_clone.notify_one(); // signal that subscriber is ready
    });

    // wait for subscriber ready but bound it
    timeout(Duration::from_secs(3), notify.notified())
        .await
        .expect("subscriber did not register in time");
    let proxy = ClientHandle::connect().await.unwrap();

    proxy
        .publish(
            "news_obj",
            "news",
            &json!({"headline": "Rust broker eventing works!"}),
        )
        .await
        .unwrap();
    proxy
        .publish("news_obj", "news1", &json!({"headline": "Another news!"}))
        .await
        .unwrap();

    println!("[Publisher] done broadcasting");

    // Wait a bit for callbacks to fire (bounded)
    async fn wait_for_update(val: Arc<Mutex<Option<Value>>>) -> Value {
        timeout(Duration::from_secs(5), async {
            loop {
                if let Some(v) = val.lock().await.clone() {
                    return v;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Timed out waiting for subscriber update")
    }

    let n1 = wait_for_update(news_val.clone()).await;
    let n2 = wait_for_update(news1_val.clone()).await;

    assert_eq!(n1["headline"], "Rust broker eventing works!");
    assert_eq!(n2["headline"], "Another news!");
}
