use serde_json::json;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UnixStream},
    runtime::Runtime,
    task,
    time::sleep,
};
use uuid::Uuid;

use crate::{
    broker::run_broker,
    rpc::{CallId, RpcRequest, RpcResponse},
};

/// Stress test parameters
const CLIENTS: usize = 100; // number of concurrent clients
const OPS_PER_CLIENT: usize = 100; // operations per client
const UNIX_PATH: &str = "/tmp/ipc_broker.sock";

/// Very simple pseudo-random number generator (xorshift)
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

#[ctor::ctor]
fn init_broker() {
    // Remove stale Unix socket
    let _ = std::fs::remove_file(UNIX_PATH);

    std::thread::spawn(|| {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            run_broker().await.unwrap();
        });
    });

    // Wait until TCP is ready
    for _ in 0..50 {
        if std::net::TcpStream::connect("127.0.0.1:5000").is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("Broker did not start on TCP");
}

/// Abstraction for client connection type (TCP or Unix)
enum Conn {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl Conn {
    async fn connect_tcp() -> Self {
        Conn::Tcp(TcpStream::connect("127.0.0.1:5000").await.unwrap())
    }
    async fn connect_unix() -> Self {
        Conn::Unix(UnixStream::connect(UNIX_PATH).await.unwrap())
    }
    async fn write_all(&mut self, buf: &[u8]) {
        match self {
            Conn::Tcp(s) => s.write_all(buf).await.unwrap(),
            Conn::Unix(s) => s.write_all(buf).await.unwrap(),
        }
    }
    async fn read(&mut self, buf: &mut [u8]) -> usize {
        match self {
            Conn::Tcp(s) => s.read(buf).await.unwrap(),
            Conn::Unix(s) => s.read(buf).await.unwrap(),
        }
    }
    fn try_read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Tcp(s) => s.try_read(buf),
            Conn::Unix(s) => s.try_read(buf),
        }
    }
}

/// === TEST HELPERS ===

async fn do_register_and_call(mut a: Conn, mut b: Conn, obj_name: &str) {
    // Register
    let reg = RpcRequest::RegisterObject {
        object_name: obj_name.to_string(),
    };
    a.write_all(&serde_json::to_vec(&reg).unwrap()).await;

    let mut buf = vec![0u8; 4096];
    let n = a.read(&mut buf).await;
    let val: RpcResponse = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(
        val,
        RpcResponse::Registered {
            object_name: obj_name.to_string()
        }
    );

    // Call
    let call_id = Uuid::new_v4().to_string();
    let call = RpcRequest::Call {
        call_id: CallId(call_id.clone()),
        object_name: obj_name.to_string(),
        method: "echo".to_string(),
        args: json!({"msg": "hi"}),
    };
    b.write_all(&serde_json::to_vec(&call).unwrap()).await;

    let n = a.read(&mut buf).await;
    let forwarded: RpcRequest = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(forwarded, call);

    // Reply
    let result = RpcResponse::Result {
        call_id: CallId(call_id.clone()),
        object_name: obj_name.to_string(),
        value: json!({"msg": "hi"}),
    };
    a.write_all(&serde_json::to_vec(&result).unwrap()).await;

    let n = b.read(&mut buf).await;
    let val: RpcResponse = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(val, result);
}

async fn do_subscribe_and_publish(mut sub: Conn, mut pub_client: Conn, topic: &str) {
    // Subscribe
    let sub_req = RpcRequest::Subscribe {
        topic: topic.to_string(),
    };
    sub.write_all(&serde_json::to_vec(&sub_req).unwrap()).await;

    let mut buf = vec![0u8; 4096];
    let n = sub.read(&mut buf).await;
    let resp: RpcResponse = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(
        resp,
        RpcResponse::Subscribed {
            topic: topic.to_string()
        }
    );

    // Publish
    let publish = RpcRequest::Publish {
        topic: topic.to_string(),
        args: json!({"headline": "broker works!"}),
    };
    pub_client
        .write_all(&serde_json::to_vec(&publish).unwrap())
        .await;

    // Event should arrive
    let n = sub.read(&mut buf).await;
    let event: RpcResponse = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(
        event,
        RpcResponse::Event {
            topic: topic.to_string(),
            args: json!({"headline": "broker works!"}),
        }
    );
}

/// === ACTUAL TESTS ===

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tcp_register_and_call() {
    let a = Conn::connect_tcp().await;
    let b = Conn::connect_tcp().await;
    do_register_and_call(a, b, "tcp_obj").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unix_register_and_call() {
    let a = Conn::connect_unix().await;
    let b = Conn::connect_unix().await;
    do_register_and_call(a, b, "unix_obj").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tcp_subscribe_and_publish() {
    let sub = Conn::connect_tcp().await;
    let pubc = Conn::connect_tcp().await;
    do_subscribe_and_publish(sub, pubc, "news_tcp").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unix_subscribe_and_publish() {
    let sub = Conn::connect_unix().await;
    let pubc = Conn::connect_unix().await;
    do_subscribe_and_publish(sub, pubc, "news_unix").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn tcp_stress_broker() {
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
                            call_id: CallId(Uuid::new_v4().to_string()),
                            object_name: my_object.clone(),
                            method: "echo".into(),
                            args: json!({"msg": format!("hello from client {i}")}),
                        };
                        conn.write_all(&serde_json::to_vec(&req).unwrap()).await;
                    }
                    "subscribe" => {
                        let req = RpcRequest::Subscribe {
                            topic: format!("topic_{}", i % 10),
                        };
                        conn.write_all(&serde_json::to_vec(&req).unwrap()).await;
                    }
                    "publish" => {
                        let req = RpcRequest::Publish {
                            topic: format!("topic_{}", i % 10),
                            args: json!({"val": rng.next_u32()}),
                        };
                        conn.write_all(&serde_json::to_vec(&req).unwrap()).await;
                    }
                    _ => {}
                }

                if let Ok(n) = conn.try_read(&mut read_buf) {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn unix_stress_broker() {
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
                            call_id: CallId(Uuid::new_v4().to_string()),
                            object_name: my_object.clone(),
                            method: "echo".into(),
                            args: json!({"msg": format!("hello from client {i}")}),
                        };
                        conn.write_all(&serde_json::to_vec(&req).unwrap()).await;
                    }
                    "subscribe" => {
                        let req = RpcRequest::Subscribe {
                            topic: format!("topic_{}", i % 10),
                        };
                        conn.write_all(&serde_json::to_vec(&req).unwrap()).await;
                    }
                    "publish" => {
                        let req = RpcRequest::Publish {
                            topic: format!("topic_{}", i % 10),
                            args: json!({"val": rng.next_u32()}),
                        };
                        conn.write_all(&serde_json::to_vec(&req).unwrap()).await;
                    }
                    _ => {}
                }

                if let Ok(n) = conn.try_read(&mut read_buf) {
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
