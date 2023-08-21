use std::time::Duration;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
pub struct NetworkSample {
    pub rx: u64,
    pub tx: u64,
    pub time: Duration,
}

#[derive(Clone, PartialEq, PartialOrd, Debug, Default, Serialize, Deserialize)]
pub struct NetworkInfo {
    pub interface: String,
    #[serde(flatten)]
    pub usage: NetworkUsage,
}

#[derive(Copy, Clone, PartialEq, PartialOrd, Debug, Default, Serialize, Deserialize)]
pub struct NetworkUsage {
    pub tx: (f64, NetworkUnit),
    pub tx_raw: f64,
    pub rx: (f64, NetworkUnit),
    pub rx_raw: f64,
}

#[derive(Clone, PartialEq, PartialOrd, Debug, Default, Serialize, Deserialize)]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: String,
    pub removable: bool,
    #[serde(flatten)]
    pub usage: DiskUsage,
}

#[derive(Copy, Clone, PartialEq, PartialOrd, Debug, Default, Serialize, Deserialize)]
pub struct DiskUsage {
    pub used: (f64, StorageUnit),
    pub total: (f64, StorageUnit),
    pub percent: f64,
}

#[derive(Copy, Clone, PartialEq, PartialOrd, Debug, Default, Serialize, Deserialize)]
pub struct MemoryUsage {
    pub used: (f64, StorageUnit),
    pub total: (f64, StorageUnit),
    pub percent: f64,
}

#[derive(
    Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default, Serialize, Deserialize,
)]
pub enum NetworkUnit {
    #[default]
    #[serde(rename = "B")]
    Byte,
    #[serde(rename = "Kb")]
    Kilobit,
    #[serde(rename = "Mb")]
    Megabit,
    #[serde(rename = "Gb")]
    Gigabit,
    #[serde(rename = "Tb")]
    Terabit,
}

impl NetworkUnit {
    pub fn scale_factor(&self) -> f64 {
        if let Self::Byte = self {
            128.0
        } else {
            1024.0
        }
    }
}

#[derive(
    Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default, Serialize, Deserialize,
)]
pub enum StorageUnit {
    #[default]
    #[serde(rename = "B")]
    Byte,
    #[serde(rename = "KiB")]
    Kilobyte,
    #[serde(rename = "MiB")]
    Megabyte,
    #[serde(rename = "GiB")]
    Gigabyte,
    #[serde(rename = "TiB")]
    Terabyte,
    #[serde(rename = "PiB")]
    Petabyte,
}
