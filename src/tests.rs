use serde_json::json;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
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
    // Spawn a background runtime + broker before tests start
    std::thread::spawn(|| {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            run_broker().await.unwrap();
        });
    });

    // Wait until TCP port is open
    for _ in 0..50 {
        if std::net::TcpStream::connect("127.0.0.1:5000").is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("Broker did not start listening on 127.0.0.1:5000");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn stress_broker() {
    let mut handles = Vec::new();

    for i in 0..CLIENTS {
        handles.push(task::spawn(async move {
            let mut stream = TcpStream::connect("127.0.0.1:5000").await.unwrap();

            // seed per client from system time + id
            let seed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
                ^ (i as u64);
            let mut rng = SimpleRng::new(seed);

            let my_object = format!("obj_{i}");

            // Send a RegisterObject first
            let reg = RpcRequest::RegisterObject {
                object_name: my_object.clone(),
            };
            let mut buf = serde_json::to_vec(&reg).unwrap();
            stream.write_all(&buf).await.unwrap();

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
                        buf = serde_json::to_vec(&req).unwrap();
                        stream.write_all(&buf).await.unwrap();
                    }
                    "subscribe" => {
                        let req = RpcRequest::Subscribe {
                            topic: format!("topic_{}", i % 10),
                        };
                        buf = serde_json::to_vec(&req).unwrap();
                        stream.write_all(&buf).await.unwrap();
                    }
                    "publish" => {
                        let req = RpcRequest::Publish {
                            topic: format!("topic_{}", i % 10),
                            args: json!({"val": rng.next_u32()}),
                        };
                        buf = serde_json::to_vec(&req).unwrap();
                        stream.write_all(&buf).await.unwrap();
                    }
                    _ => {}
                }

                // Try reading back a response (non-blocking)
                if let Ok(n) = stream.try_read(&mut read_buf) {
                    if n > 0 {
                        let slice = &read_buf[..n];
                        if let Ok(val) = serde_json::from_slice::<RpcResponse>(slice) {
                            println!("Client {i} got response: {val:?}");
                        }
                    }
                }

                ops_done += 1;
                sleep(Duration::from_millis(rng.gen_range(5, 20))).await;
            }

            println!("Client {i} completed {OPS_PER_CLIENT} ops");
        }));
    }

    // Wait for all clients
    for h in handles {
        let _ = h.await;
    }

    println!("Stress test completed with {CLIENTS} clients x {OPS_PER_CLIENT} ops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broker_register_and_call() {
    // === Client A: register object ===
    let mut a = TcpStream::connect("127.0.0.1:5000").await.unwrap();

    let object_name = "test_obj".to_string();
    let reg = RpcRequest::RegisterObject {
        object_name: object_name.clone(),
    };
    a.write_all(&serde_json::to_vec(&reg).unwrap())
        .await
        .unwrap();

    let mut buf = vec![0u8; 4096];
    let n = a.read(&mut buf).await.unwrap();
    let val: RpcResponse = serde_json::from_slice(&buf[..n]).unwrap();

    assert_eq!(val, RpcResponse::Registered { object_name });

    // === Client B: make a call to that object ===
    let mut b = TcpStream::connect("127.0.0.1:5000").await.unwrap();
    let call_id = Uuid::new_v4().to_string();
    let call = RpcRequest::Call {
        call_id: CallId(call_id.clone()),
        object_name: "test_obj".to_string(),
        method: "echo".to_string(),
        args: json!({"msg": "hi"}),
    };
    b.write_all(&serde_json::to_vec(&call).unwrap())
        .await
        .unwrap();

    // Client A should receive forwarded call
    let n = a.read(&mut buf).await.unwrap();
    let forwarded: RpcRequest = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(forwarded, call);

    // === Client A replies ===
    let result = RpcResponse::Result {
        call_id: CallId(call_id.clone()),
        object_name: "test_obj".to_string(),
        value: json!({"msg": "hi"}),
    };
    a.write_all(&serde_json::to_vec(&result).unwrap())
        .await
        .unwrap();

    // Client B should get response
    let n = b.read(&mut buf).await.unwrap();
    let val: RpcResponse = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(val, result);
    println!("Test completed for register and call");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broker_subscribe_and_publish() {
    let mut sub = TcpStream::connect("127.0.0.1:5000").await.unwrap();
    let mut pub_client = TcpStream::connect("127.0.0.1:5000").await.unwrap();

    let topic = "news";

    // subscriber sends subscribe
    let sub_req = RpcRequest::Subscribe {
        topic: topic.to_string(),
    };
    sub.write_all(&serde_json::to_vec(&sub_req).unwrap())
        .await
        .unwrap();

    let mut buf = vec![0u8; 4096];
    let n = sub.read(&mut buf).await.unwrap();
    let resp: RpcResponse = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(
        resp,
        RpcResponse::Subscribed {
            topic: topic.to_string()
        }
    );

    // publisher sends publish
    let publish = RpcRequest::Publish {
        topic: topic.to_string(),
        args: json!({"headline": "broker works!"}),
    };
    pub_client
        .write_all(&serde_json::to_vec(&publish).unwrap())
        .await
        .unwrap();

    // subscriber should receive event
    let n = sub.read(&mut buf).await.unwrap();
    let event: RpcResponse = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(
        event,
        RpcResponse::Event {
            topic: topic.to_string(),
            args: json!({"headline": "broker works!"}),
        }
    );
    println!("Test completed for subscribe and publish");
}
