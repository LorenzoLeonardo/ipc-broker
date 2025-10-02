use ipc_broker::client::ClientHandle;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
pub struct RpcCall {
    pub object: String,
    pub method: String,
    pub signature: String,
    pub args: Value,
}

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
                out.push_str(&format!("{padding}  \"{k}\":{}\n", format_value(v, indent)));
            }
            out
        }
    }
}

fn parse_signature(sig: &str, args: &mut std::slice::Iter<String>) -> Result<Value, String> {
    let mut chars = sig.chars().peekable();
    println!("Parsing signature: {sig}");

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

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.len() < 3 {
        panic!("Usage: rob call <object> <method> [signature] [args...]");
    }

    let command = args.first().expect("missing command");
    let object = args.get(1).expect("missing object");
    let method = args.get(2).expect("missing method");
    let signature = args.get(3).cloned().unwrap_or_default(); // empty if missing

    let mut iter = if args.len() > 4 {
        args[4..].iter()
    } else {
        [].iter() // empty iterator if no args
    };

    let parsed_args = if signature.is_empty() {
        serde_json::Value::Null
    } else {
        parse_signature(&signature, &mut iter).unwrap()
    };
    println!("Parsed args: {parsed_args}");
    // --- pick transport ---
    if command == "call" {
        let proxy = ClientHandle::connect().await.unwrap();

        let response = proxy
            .remote_call::<Value, Value>(object, method, parsed_args)
            .await
            .unwrap();

        println!("Result:\n  {}", format_value(&response, 0));
    } else {
        panic!("Unknown command: {command}");
    }
    Ok(())
}
