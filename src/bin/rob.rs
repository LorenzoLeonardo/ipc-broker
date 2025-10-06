//! # IPC Broker Client Tool
//!
//! This tool provides a command-line interface for interacting with an IPC-based broker
//! using the [`ipc_broker::client::ClientHandle`] API.
//!
//! It supports three primary operations:
//! - **Remote call** — invoke a method on a remote object and wait for a response.
//! - **Publish (send)** — send an event/notification to a topic without expecting a reply.
//! - **Subscribe (listen)** — listen for incoming published events from a topic.
//!
//! ## Command Syntax
//! ```bash
//! rob <command> <object> <method> [signature] [args...]
//! ```
//!
//! ### Arguments
//! | Name | Description |
//! |------|--------------|
//! | `<command>` | One of `call`, `send`, or `listen` |
//! | `<object>`  | The remote object or topic name |
//! | `<method>`  | The method or event name |
//! | `[signature]` | Optional type signature for argument parsing |
//! | `[args...]` | Optional arguments matching the signature |
//!
//! ### Supported Commands
//!
//! #### 1. `call` — Remote Procedure Call
//! Performs a request-response style operation via `remote_call()`.
//!
//! ```bash
//! rob call <object> <method> [signature] [args...]
//! ```
//!
//! - Sends a structured request to the broker.
//! - Waits for and prints the JSON response.
//!
//! Example:
//! ```bash
//! rob call device_manager get_status
//! ```
//!
//! With arguments and signature:
//! ```bash
//! # Call a method expecting (string, integer)
//! rob call math add "(si)" hello 42
//! ```
//!
//! #### 2. `send` — Publish an Event
//! Sends a message to subscribers without expecting a reply via `publish()`.
//!
//! ```bash
//! rob send <object> <method> [signature] [args...]
//! ```
//!
//! Example:
//! ```bash
//! # Send a string payload
//! rob send system notify "s" "Rebooting in 5 minutes"
//! ```
//!
//! #### 3. `listen` — Subscribe to Events
//! Subscribes to a topic using `subscribe_async()` and prints every message received.
//!
//! ```bash
//! rob listen <object> <method>
//! ```
//!
//! Example:
//! ```bash
//! rob listen system notify
//! ```
//!
//! Output example:
//! ```text
//! Listening for: object=system method=notify. Press ctrl+c to exit.
//
//! Result:
//! Map:
//!   message : :"Rebooting in 5 minutes"
//! ```
//!
//! ## Signature Format
//!
//! The `[signature]` argument describes how to parse `[args...]` into structured JSON.
//! The parser supports nested objects and arrays using symbols similar to D-Bus or GLib GVariant syntax.
//!
//! | Symbol | Meaning | Example |
//! |---------|----------|---------|
//! | `s` | string | `"s"` `"hello"` → `"hello"` |
//! | `i` | integer | `"i"` `42` → `42` |
//! | `n` | null | `"n"` → `null` |
//! | `{ ... }` | object (map) | `"{si}"` `"key1"` `"val1"` `"key2"` `42` |
//! | `( ... )` | array (list) | `"(si)"` `"hello"` `42` |
//!
//! Example complex call:
//! ```bash
//! # Sends a nested object and array
//! rob call data update "{s(i)}" "profile" "hello" 42
//! ```
//!
//! Which produces this JSON:
//! ```json
//! {
//!   "profile": ["hello", 42]
//! }
//! ```
//!
//! ## Implementation Notes
//!
//! - Uses [`serde_json::Value`] as the generic message container.
//! - Connection is automatically created via [`ClientHandle::connect()`].
//! - Message formatting is handled by [`format_value()`] for pretty printing nested values.
//!
//! ## Example Output
//! ```text
//! Result:
//! Map:
//!   status : :"ok"
//!   uptime : :12345
//! ```
//!
//! ## Error Handling
//! - Invalid command, missing arguments, or malformed signatures are gracefully reported.
//! - Signature and argument mismatches return `InvalidInput` errors.
//!
//! ## Example Usage Summary
//! | Command | Example | Description |
//! |----------|----------|-------------|
//! | call | `rob call user_service get_info "(s)" "john"` | Query user info |
//! | send | `rob send sensor update "(i)" 42` | Publish sensor value |
//! | listen | `rob listen sensor update` | Subscribe to sensor updates |
//!
use ipc_broker::client::ClientHandle;
use serde_json::Value;

fn format_value(value: &Value, indent: usize) -> String {
    let padding = " ".repeat(indent);
    match value {
        Value::Null => format!("{padding}Null"),
        Value::Bool(b) => format!("{padding}{b}"),
        Value::Number(n) => format!("{padding}{n}"),
        Value::String(s) => format!("{padding}\"{s}\""),
        Value::Array(arr) => {
            let mut out = format!("{padding}List:\n");
            for (i, v) in arr.iter().enumerate() {
                out.push_str(&format!(
                    "{padding}  {i} : :{}\n",
                    format_value(v, indent + 1)
                ));
            }
            out
        }
        Value::Object(obj) => {
            let mut out = format!("{padding}Map:\n");
            for (k, v) in obj {
                out.push_str(&format!(
                    "{padding}  {k} : :{}\n",
                    format_value(v, indent + 1)
                ));
            }
            out
        }
    }
}

fn parse_signature(sig: &str, args: &mut std::slice::Iter<String>) -> Result<Value, String> {
    let mut chars = sig.chars().peekable();

    fn parse(
        chars: &mut std::iter::Peekable<std::str::Chars>,
        args: &mut std::slice::Iter<String>,
    ) -> Result<Value, String> {
        match chars.next() {
            Some('s') => {
                let val = args.next().ok_or("expected string argument")?;
                Ok(Value::String(val.clone()))
            }
            Some('i') => {
                let val = args.next().ok_or("expected integer argument")?;
                let num = val
                    .parse::<i64>()
                    .map_err(|_| format!("invalid integer: {val}"))?;
                Ok(Value::Number(num.into()))
            }
            Some('{') => {
                let mut map = serde_json::Map::new();
                while let Some(&c) = chars.peek() {
                    if c == '}' {
                        chars.next(); // consume '}'
                        break;
                    }
                    let key = args.next().ok_or("expected map key")?.clone();
                    let val = parse(chars, args)?;
                    map.insert(key, val);
                }
                Ok(Value::Object(map))
            }
            Some('(') => {
                let mut arr = vec![];
                while let Some(&c) = chars.peek() {
                    if c == ')' {
                        chars.next(); // consume ')'
                        break;
                    }
                    arr.push(parse(chars, args)?);
                }
                Ok(Value::Array(arr))
            }
            Some(c) => {
                if c == 'n' {
                    return Ok(Value::Null);
                }
                Err(format!("unexpected char in signature: {c}"))
            }
            None => Err("unexpected end of signature".into()),
        }
    }

    parse(&mut chars, args)
}

const APP_NAME: &str = "rob";
const APP_VERSION: &str = env!("ROB_VERSION");

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Handle global flags first
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("{APP_NAME} version {APP_VERSION}");
        return Ok(());
    }

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: rob call <object> <method> [signature] [args...]");
        return Ok(());
    }

    if args.len() < 3 {
        log::error!("Usage: rob call <object> <method> [signature] [args...]");
        return Ok(());
    }

    let command = args
        .first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing command"))?;
    let object = args.get(1).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing object name")
    })?;
    let method = args.get(2).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing method name")
    })?;
    let signature = args.get(3).cloned().unwrap_or_default(); // empty if missing

    let mut iter = if args.len() > 4 {
        args[4..].iter()
    } else {
        [].iter() // empty iterator if no args
    };

    let parsed_args = if signature.is_empty() {
        serde_json::Value::Null
    } else {
        parse_signature(&signature, &mut iter)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?
    };

    // --- pick transport ---
    if command == "call" {
        let proxy = ClientHandle::connect().await?;

        let response = proxy
            .remote_call::<Value, Value>(object, method, parsed_args)
            .await?;

        println!("Result:\n{}", format_value(&response, 0));
    } else if command == "listen" {
        println!("Listening for: object={object} method={method}. Press ctrl+c to exit.\n\n");
        let proxy = ClientHandle::connect().await?;

        proxy
            .subscribe_async(object, method, |param| {
                println!("Result:\n{}", format_value(&param, 0));
            })
            .await;
        tokio::signal::ctrl_c().await?;
    } else if command == "send" {
        let proxy = ClientHandle::connect().await?;
        println!("Sending: {parsed_args}");
        proxy.publish(object, method, &parsed_args).await?;
    } else {
        eprintln!("Unknown command: {command}");
    }
    Ok(())
}
