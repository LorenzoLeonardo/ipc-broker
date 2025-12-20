use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::{fs, process};

use serde::Deserialize;
use tokio::sync::Mutex;

use crate::rpc::{ClientId, RpcRequest};

#[derive(Debug, Deserialize)]
pub struct ServiceActivationFile {
    #[serde(rename = "type")]
    kind: String,

    object_name: String,
    service_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ServiceState {
    Stopped,
    Starting,
    Running(ClientId),
}

#[derive(Debug)]
pub struct ServiceEntry {
    pub state: ServiceState,
    pub service_name: String,
    pub pending_calls: Vec<RpcRequest>,
    pub owner: Option<ClientId>,
}

pub type SharedServices = Arc<Mutex<HashMap<String, ServiceEntry>>>;
pub const BASE_DIR: &str = "service-activation";

fn home_dir() -> PathBuf {
    dirs::home_dir().expect("Could not determine home directory")
}

pub async fn load_service_activations(services: &SharedServices) {
    let base_dir = home_dir().join(BASE_DIR);
    let entries = match fs::read_dir(&base_dir) {
        Ok(e) => e,
        Err(e) => {
            log::warn!(
                "Service activation directory {:?} not accessible: {e}",
                base_dir
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                log::error!("Failed to read {:?}: {e}", path);
                continue;
            }
        };

        let parsed: Vec<ServiceActivationFile> = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                log::error!("Invalid JSON in {:?}: {e}", path);
                continue;
            }
        };

        for item in parsed {
            if item.kind != "RegisterService" {
                log::warn!("Ignoring {:?}: unsupported type {}", path, item.kind);
                continue;
            }

            log::info!(
                "Loaded service activation: {} -> {}",
                item.object_name,
                item.service_name
            );

            services.lock().await.insert(
                item.object_name,
                ServiceEntry {
                    state: ServiceState::Stopped,
                    service_name: item.service_name,
                    pending_calls: Vec::new(),
                    owner: None,
                },
            );
        }
    }
}

pub fn spawn_service(unit: &str) {
    let status = process::Command::new("/usr/bin/sudo")
        .arg("/usr/bin/systemctl")
        .arg("restart")
        .arg(unit)
        .status();

    match status {
        Ok(s) if s.success() => {
            log::info!("Restarted systemd service via sudo: {}", unit);
        }
        Ok(s) => {
            log::error!("sudo systemctl restart {} failed: {}", unit, s);
        }
        Err(e) => {
            log::error!("Failed to execute sudo systemctl: {}", e);
        }
    }
}
