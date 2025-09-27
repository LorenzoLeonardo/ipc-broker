# IPC Broker (Tokio-based)

A lightweight **inter-process communication (IPC) broker** built with [Tokio](https://tokio.rs/) and Rust.  
It provides **RPC**, **publish/subscribe eventing**, and supports both **TCP** and **Unix domain sockets**.

---

## ✨ Features

- **Dual transport support**:
  - TCP (`127.0.0.1:5000`)
  - Unix socket (`/tmp/ipc_broker.sock`)
- **RPC calls**:
  - Clients can call registered objects exposed by workers.
  - Responses are automatically routed back to the original caller.
- **Object registration**:
  - Workers can register named objects (e.g. `Calculator`).
- **Pub/Sub**:
  - Clients can subscribe to topics and receive published events.
- **Actor-based client handling**:
  - Each connection is managed by a lightweight async actor.
  - Reads and writes are decoupled via channels.

---

## 📦 Components

The project consists of the following parts:

- **Broker** (the server):
  - Accepts TCP and Unix socket connections.
  - Routes RPC requests, responses, and pub/sub messages.

- **Worker**:
  - Connects to the broker and registers objects.
  - Handles incoming RPC calls (`add`, `mul`, etc.).
  - Sends back results to clients.

- **Client**:
  - Issues RPC calls to registered objects.
  - Waits for and prints responses.

- **Publisher**:
  - Publishes events to topics (e.g. `"news"`).

- **Subscriber**:
  - Subscribes to topics and listens for incoming events.

---

## 🚀 Getting Started

### 1. Run the broker
```bash
cargo run --bin broker