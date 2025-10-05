# IPC Broker (Tokio-based)

A lightweight **inter-process communication (IPC) broker** built with [Tokio](https://tokio.rs/) and Rust.
It provides **RPC**, **publish/subscribe eventing**, and supports both **TCP** and **Unix domain sockets** for Linux and macOS,
and both **TCP** and **Named Pipes** for Windows.

---

## ✨ Features

* **Dual transport support**

  * TCP (`127.0.0.1:5000`)
  * Unix socket (`/tmp/ipc_broker.sock`)
  * Named Pipes (`\\.\pipe\ipc_broker`)
* **RPC calls**

  * Clients can call registered objects exposed by workers.
  * Responses are automatically routed back to the original caller.
* **Object registration**

  * Workers can register named objects.
* **Pub/Sub**

  * Clients can subscribe to topics and receive published events.
* **Actor-based connection handling**

  * Each connection runs as an async actor.
  * Read/write are decoupled through channels for maximum concurrency.

---

## 🧩 Architecture Overview

```
+-------------+        +-------------+        +-------------+
|   Client    | <----> |   Broker    | <----> |   Worker    |
|-------------|        |-------------|        |-------------|
| RPC calls   |        | Routes RPCs |        | Exposes RPC |
| Pub/Sub     |        | Routes Pubs |        | Handles req |
+-------------+        +-------------+        +-------------+
```

* **Broker** – Routes all communication between clients and workers.
* **Worker** – Registers objects and executes requested methods.
* **Client** – Invokes remote methods, publishes events, or subscribes to topics.

---

## 🚀 Getting Started (Development)

### 1. Run the Broker

```bash
cargo run --bin ipc-broker
```

The broker will listen on:

* TCP: `127.0.0.1:5000`
* Unix socket: `/tmp/ipc_broker.sock` (on Linux/macOS)
* Named pipe: `\\.\pipe\ipc_broker` (on Windows)

---

### 2. Run a Worker

Your worker connects to the broker and registers callable objects.

Example:

```rust
use async_trait::async_trait;
use ipc_broker::worker::{SharedObject, WorkerBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize, Serialize)]
struct Args {
    a: i32,
    b: i32,
}

pub struct Calculator;
#[async_trait]
impl SharedObject for Calculator {
    async fn call(&self, method: &str, args: &Value) -> Value {
        let parsed = serde_json::from_value(args.clone());

        match (method, parsed) {
            ("add", Ok(Args { a, b })) => json!({"sum": (a + b)}),
            ("mul", Ok(Args { a, b })) => json!({"product": (a * b)}),
            (_, Err(e)) => Value::String(format!("Invalid args: {e}")),
            _ => Value::String("Unknown method".into()),
        }
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let calc = Calculator;

    WorkerBuilder::new().add("math", calc).spawn().await?;
    Ok(())
}
```

---

### 3. Use the `rob` Command-Line Tool

The **`rob`** utility is a helper CLI for interacting with the broker.

#### 🧪 Syntax

```bash
rob <command> <object> <method> [signature] [args...]
```

| Command  | Description                                 |
| -------- | ------------------------------------------- |
| `call`   | Perform an RPC call and wait for a response |
| `send`   | Publish an event to a topic                 |
| `listen` | Subscribe to a topic and listen for events  |

---

## 🧪 Examples

### 🧩 Example 1 – RPC Call

Call a registered worker method:

```bash
rob call math add {ii} a 10 b 32
```

**Explanation:**

* `{ii}` means a map with keys `a` and `b`, both integers.
* The arguments `a 10 b 32` correspond to the values.

**Expected Output:**

```
Result:
Map:
  sum :: 42
```

---

### 📢 Example 2 – Publish an Event

Publish a message to subscribers:

```bash
rob send sensor temperature s "25.6°C"
```

**Signature** `s` = string argument
Publishes a temperature event with the value `"25.6°C"`.

---

### 📢 Example 3 – Subscribe to a Topic

Listen for published events:

```bash
rob listen sensor temperature
```

**Output:**

```
Listening for: object=sensor method=temperature. Press ctrl+c to exit.

Result:
"25.6°C"
```

---

## 🧰 Signature Syntax Reference

| Symbol    | Type    | Example                     | Description               |
| --------- | ------- | ----------------------------| --------------------------|
| `s`       | String  | `s "hello"`                 | Text value                |
| `i`       | Integer | `i 42`                      | 64-bit integer            |
| `n`       | Null    | `n`                         | JSON null/ No parameter   |
| `{ ... }` | Map     | `{is} key 123 value "text"` | Key-value pairs           |
| `( ... )` | Array   | `(sss) "one" "two" "three"` | List of values            |

Examples:

```bash
# Map example
rob call user info {i} id 123

# Array example
rob call math sum (ii) 1 2
```

---

## 🛠 Building

```bash
cargo build --release
```

The resulting binaries will be placed in `target/release/`.

---

## 🪟 Platform Notes

| Platform    | Supported Transports |
| ----------- | -------------------- |
| Linux/macOS | TCP + Unix socket    |
| Windows     | TCP + Named Pipe     |

---

## 📄 License

MIT License © 2025
