# ipc-broker — Block Diagrams

This file contains block diagrams (Mermaid) showing the high-level component topology and common message flows for the `ipc-broker` crate.

## Component Overview

```mermaid
flowchart LR
  subgraph Clients
    C[Client A]
    C2[Client B]
  end

  subgraph Broker
    B[Broker]
    Storage[(Shared State)]
  end

  subgraph Workers
    W[Worker / Service]
  end

  System[System / OS]

  C -->|connect| B
  C2 -->|connect| B
  B -->|reads/writes| Storage
  B -->|forward calls| W
  W -->|registers| B
  B -->|spawn/restart| System
```

## Call Routing (block flow)

```mermaid
flowchart LR
  Caller[Client - caller] -->|Call| Broker
  Broker -->|store call mapping| Storage[(Shared State)]
  Storage -->|lookup owner| Broker
  Broker -->|forward Call| Owner[Client owner / Worker]
  Owner -->|Result| Broker
  Broker -->|lookup mapping| Storage
  Broker -->|forward Result| Caller
```

## Publish / Subscribe (block flow)

```mermaid
flowchart LR
  Publisher[Client - publisher] -->|Publish| Broker
  Broker -->|snapshot subscribers| Storage[(Subscriptions)]
  Storage -->|list| Broker
  Broker -->|send Event| Subscriber1[Client S1]
  Broker -->|send Event| Subscriber2[Client S2]
```

## Service Activation (block flow)

```mermaid
flowchart LR
  Caller[Client] -->|Call for service obj| Broker
  Broker -->|no running owner| ServiceEntry[(ServiceEntry.pending_calls)]
  Broker -->|trigger| System[systemctl / OS action]
  System -->|start process| Worker[Worker process]
  Worker -->|RegisterObject| Broker
  Broker -->|flush pending calls| Worker
```

---

Notes:
- These diagrams are intentionally high-level; use the `Architecture.MD` document for protocol and algorithm details.
- To render the diagrams locally, use a Markdown viewer that supports Mermaid or generate images with `mmdc` (Mermaid CLI).

File: [BlockDiagram.md](BlockDiagram.md)
