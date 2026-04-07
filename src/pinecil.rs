#![allow(dead_code)]

use uuid::Uuid;

// BLE Service UUIDs (IronOS 2.21+)
pub const BULK_DATA_SERVICE: &str = "9eae1000-9d0d-48c5-aa55-33e27f9bc533";
pub const BULK_LIVE_DATA: &str = "9eae1001-9d0d-48c5-aa55-33e27f9bc533";
pub const BULK_BUILD_INFO: &str = "9eae1003-9d0d-48c5-aa55-33e27f9bc533";

pub const SETTINGS_SERVICE: &str = "f6d80000-5a10-4eba-aa55-33e27f9bc533";
pub const SETTINGS_SAVE: &str = "f6d7ffff-5a10-4eba-aa55-33e27f9bc533";

// Setting characteristic UUID by index: f6d7NNNN-5a10-4eba-aa55-33e27f9bc533
pub fn setting_uuid(index: u16) -> Uuid {
    Uuid::parse_str(&format!(
        "f6d7{:04x}-5a10-4eba-aa55-33e27f9bc533",
        index
    ))
    .unwrap()
}

// Setting indices
pub const SETTING_SETPOINT: u16 = 0;
pub const SETTING_BOOST_TEMP: u16 = 4;
pub const SETTING_SLEEP_TEMP: u16 = 5;
pub const SETTING_SLEEP_TIMEOUT: u16 = 6;
pub const SETTING_BRIGHTNESS: u16 = 14;
pub const SETTING_TEMP_UNIT: u16 = 18;

#[derive(Debug, Clone)]
pub struct LiveData {
    pub tip_temp: f32,
    pub setpoint: f32,
    pub voltage: f32,
    pub handle_temp: f32,
    pub power_pwm: u32,
    pub power_source: PowerSource,
    pub tip_resistance: f32,
    pub uptime_secs: u32,
    pub last_move_secs: u32,
    pub max_temp: f32,
    pub raw_tip: u32,
    pub hall_sensor: i32,
    pub operating_mode: OperatingMode,
    pub estimated_watts: f32,
}

impl LiveData {
    pub fn from_bulk(data: &[u8]) -> Option<Self> {
        if data.len() < 56 {
            return None;
        }
        let u32_at = |off: usize| {
            u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
        };
        // Skip uninitialized frames (characteristic not yet populated right after connect)
        if u32_at(0) == 0 && u32_at(4) == 0 {
            return None;
        }
        Some(LiveData {
            tip_temp:        u32_at(0)  as f32,         // °C directly
            setpoint:        u32_at(4)  as f32,         // °C directly
            voltage:         u32_at(8)  as f32 / 10.0,  // 100 mV units → V
            handle_temp:     u32_at(12) as f32 / 10.0,  // 0.1 °C units → °C
            power_pwm:       u32_at(16),                 // 0–100 %
            power_source:    PowerSource::from(u32_at(20)),
            tip_resistance:  u32_at(24) as f32 / 10.0,  // 0.1 Ω units → Ω
            uptime_secs:     u32_at(28),                 // seconds
            last_move_secs:  u32_at(32),                 // seconds
            max_temp:        u32_at(36) as f32,          // °C directly
            raw_tip:         u32_at(40),
            hall_sensor:     u32_at(44) as i32,
            operating_mode:  OperatingMode::from(u32_at(48)),
            estimated_watts: u32_at(52) as f32 / 10.0,  // 0.1 W units → W
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PowerSource {
    DC,
    QC,
    PDPPS,
    USBPD,
    Unknown(u32),
}

impl From<u32> for PowerSource {
    fn from(v: u32) -> Self {
        match v {
            0 => PowerSource::DC,
            1 => PowerSource::QC,
            2 => PowerSource::PDPPS,
            3 => PowerSource::USBPD,
            other => PowerSource::Unknown(other),
        }
    }
}

impl std::fmt::Display for PowerSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PowerSource::DC => write!(f, "DC"),
            PowerSource::QC => write!(f, "QC"),
            PowerSource::PDPPS => write!(f, "PD PPS"),
            PowerSource::USBPD => write!(f, "USB-PD"),
            PowerSource::Unknown(v) => write!(f, "?({v})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OperatingMode {
    Idle,
    Soldering,
    Boost,
    Sleeping,
    Standby,
    Debug,
    Unknown(u32),
}

impl From<u32> for OperatingMode {
    fn from(v: u32) -> Self {
        match v {
            0 => OperatingMode::Idle,
            1 => OperatingMode::Soldering,
            2 => OperatingMode::Boost,
            3 => OperatingMode::Sleeping,
            4 => OperatingMode::Standby,
            5 => OperatingMode::Debug,
            other => OperatingMode::Unknown(other),
        }
    }
}

impl std::fmt::Display for OperatingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperatingMode::Idle => write!(f, "IDLE"),
            OperatingMode::Soldering => write!(f, "SOLDERING"),
            OperatingMode::Boost => write!(f, "BOOST"),
            OperatingMode::Sleeping => write!(f, "SLEEPING"),
            OperatingMode::Standby => write!(f, "STANDBY"),
            OperatingMode::Debug => write!(f, "DEBUG"),
            OperatingMode::Unknown(v) => write!(f, "?({v})"),
        }
    }
}
