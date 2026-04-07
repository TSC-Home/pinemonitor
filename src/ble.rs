use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::Manager;
use uuid::Uuid;

use crate::pinecil::{self, LiveData};

#[derive(Debug, Clone)]
pub struct FoundDevice {
    pub name: String,
    pub id: String,
}

#[derive(Debug, Clone)]
pub enum BleEvent {
    DeviceFound(FoundDevice),
    DeviceLost(String),
    Connected(String, String),   // (id, device_name)
    ConnectFailed(String),       // id — connect or discovery failed, allow retry
    Disconnected(String),        // id
    LiveData(String, Box<LiveData>),
    BuildInfo(String, String),   // (id, build_info)
    ScanTick,                    // scan cycle completed — redraw scan list
    Error(String),               // fatal / BLE adapter errors only
}

#[allow(dead_code)]
pub enum BleCommand {
    Connect(String),
    Disconnect(String),
    SetTemperature(String, u16),
}

pub fn start_ble_thread(
    events: Arc<Mutex<Vec<BleEvent>>>,
    commands: std::sync::mpsc::Receiver<BleCommand>,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(ble_main(events, commands));
    });
}

async fn ble_main(
    events: Arc<Mutex<Vec<BleEvent>>>,
    commands: std::sync::mpsc::Receiver<BleCommand>,
) {
    let push = |event: BleEvent| {
        if let Ok(mut g) = events.lock() {
            g.push(event);
        }
    };

    let manager = match Manager::new().await {
        Ok(m) => m,
        Err(e) => {
            push(BleEvent::Error(format!("BLE manager: {e}")));
            return;
        }
    };
    let adapters = match manager.adapters().await {
        Ok(a) if !a.is_empty() => a,
        _ => {
            push(BleEvent::Error("No BLE adapter found".into()));
            return;
        }
    };
    let adapter = adapters.into_iter().next().unwrap();

    // Start continuous passive scan
    let _ = adapter.start_scan(ScanFilter::default()).await;

    // id → (name, last_seen)
    let mut seen: HashMap<String, (String, Instant)> = HashMap::new();
    // id → Peripheral
    let mut connected: HashMap<String, btleplug::platform::Peripheral> = HashMap::new();

    let mut last_scan = Instant::now() - Duration::from_secs(10); // scan immediately

    loop {
        // ── drain incoming commands ────────────────────────────────────
        loop {
            match commands.try_recv() {
                Ok(BleCommand::Connect(id)) => {
                    if connected.contains_key(&id) {
                        continue; // already connected — drain remaining commands
                    }
                    let peripherals = adapter.peripherals().await.unwrap_or_default();
                    if let Some(p) = peripherals.into_iter().find(|p| p.id().to_string() == id) {
                        match p.connect().await {
                            Ok(()) => {
                                if p.discover_services().await.is_err() {
                                    push(BleEvent::ConnectFailed(id));
                                } else {
                                    let name = device_name(&p).await;
                                    let chars = p.characteristics();
                                    let bi_uuid = Uuid::parse_str(pinecil::BULK_BUILD_INFO).unwrap();
                                    if let Some(c) = chars.iter().find(|c| c.uuid == bi_uuid) {
                                        if let Ok(b) = p.read(c).await {
                                            push(BleEvent::BuildInfo(
                                                id.clone(),
                                                String::from_utf8_lossy(&b).to_string(),
                                            ));
                                        }
                                    }
                                    push(BleEvent::Connected(id.clone(), name));
                                    connected.insert(id, p);
                                }
                            }
                            Err(_) => push(BleEvent::ConnectFailed(id)),
                        }
                    } else {
                        // Peripheral not in adapter cache — fire failed so UI unblocks
                        push(BleEvent::ConnectFailed(id));
                    }
                }

                Ok(BleCommand::Disconnect(id)) => {
                    if let Some(p) = connected.remove(&id) {
                        let _ = p.disconnect().await;
                        push(BleEvent::Disconnected(id));
                    }
                }

                Ok(BleCommand::SetTemperature(id, temp)) => {
                    if let Some(p) = connected.get(&id) {
                        let sp_uuid = pinecil::setting_uuid(pinecil::SETTING_SETPOINT);
                        let save_uuid = Uuid::parse_str(pinecil::SETTINGS_SAVE).unwrap();
                        let chars = p.characteristics();
                        if let Some(c) = chars.iter().find(|c| c.uuid == sp_uuid) {
                            let _ = p.write(c, &temp.to_le_bytes(), WriteType::WithoutResponse).await;
                        }
                        if let Some(c) = chars.iter().find(|c| c.uuid == save_uuid) {
                            let _ = p.write(c, &1u16.to_le_bytes(), WriteType::WithoutResponse).await;
                        }
                    }
                }

                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
            }
        }

        // ── scan refresh every 5 s ─────────────────────────────────────
        if last_scan.elapsed() >= Duration::from_secs(5) {
            last_scan = Instant::now();

            let peripherals = adapter.peripherals().await.unwrap_or_default();
            for p in &peripherals {
                let id = p.id().to_string();
                let name = device_name(p).await;
                if name.starts_with("Pinecil") {
                    let is_new = !seen.contains_key(&id);
                    seen.insert(id.clone(), (name.clone(), Instant::now()));
                    if is_new {
                        push(BleEvent::DeviceFound(FoundDevice { name, id }));
                    } else if let Some(entry) = seen.get_mut(&id) {
                        entry.1 = Instant::now(); // refresh timestamp
                    }
                }
            }

            // remove devices not seen for 10 s (and not connected)
            let stale: Vec<String> = seen
                .iter()
                .filter(|(id, (_, ts))| {
                    !connected.contains_key(*id) && ts.elapsed() > Duration::from_secs(10)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in stale {
                seen.remove(&id);
                push(BleEvent::DeviceLost(id));
            }

            push(BleEvent::ScanTick);
        }

        // ── poll all connected devices ─────────────────────────────────
        let bulk_uuid = Uuid::parse_str(pinecil::BULK_LIVE_DATA).unwrap();
        let ids: Vec<String> = connected.keys().cloned().collect();
        for id in ids {
            if let Some(p) = connected.get(&id) {
                let chars = p.characteristics();
                if let Some(c) = chars.iter().find(|c| c.uuid == bulk_uuid) {
                    match p.read(c).await {
                        Ok(bytes) => {
                            if let Some(data) = LiveData::from_bulk(&bytes) {
                                push(BleEvent::LiveData(id, Box::new(data)));
                            }
                        }
                        Err(_) => {
                            if let Some(p2) = connected.remove(&id) {
                                let _ = p2.disconnect().await;
                            }
                            push(BleEvent::Disconnected(id));
                        }
                    }
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn device_name(p: &btleplug::platform::Peripheral) -> String {
    use btleplug::api::Peripheral as _;
    p.properties()
        .await
        .ok()
        .flatten()
        .and_then(|pr| pr.local_name)
        .unwrap_or_else(|| "Pinecil".to_string())
}
