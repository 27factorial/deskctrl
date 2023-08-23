use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{self, Write as _},
    os::unix::net::UnixStream,
};

use crate::data::{DiskInfo, MemoryUsage, NetworkUsage};

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Serialize, Deserialize)]
pub enum IpcRequest {
    Network,
    Disk,
    Memory,
    Cpu,
    DeleteNotification(u32),
    Kill,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum IpcResponse {
    Network(HashMap<String, NetworkUsage>),
    Disk(Vec<DiskInfo>),
    Memory(MemoryUsage),
    Cpu(f64),
    NotificationDeleted(u32),
    Killed,
}

impl From<EwwResponse> for IpcResponse {
    fn from(value: EwwResponse) -> Self {
        match value {
            EwwResponse::Network(network) => Self::Network(network),
            EwwResponse::Disk(disk) => Self::Disk(disk),
            EwwResponse::Memory(memory) => Self::Memory(memory),
            EwwResponse::Cpu(cpu) => Self::Cpu(cpu),
            EwwResponse::NotificationDeleted(id) => Self::NotificationDeleted(id),
            EwwResponse::Killed => Self::Killed,
        }
    }
}

// This is needed to write the response to stdout in a format that eww expects. This means that I
// need a tagged response for communication between deskctrl processes, and an untagged one for
// getting information into eww. This does mean that the process of serialization between processes
// can be done with a more efficient format than JSON, though.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EwwResponse {
    Network(HashMap<String, NetworkUsage>),
    Disk(Vec<DiskInfo>),
    Memory(MemoryUsage),
    Cpu(f64),
    NotificationDeleted(u32),
    Killed,
}

impl From<IpcResponse> for EwwResponse {
    fn from(value: IpcResponse) -> Self {
        match value {
            IpcResponse::Network(network) => Self::Network(network),
            IpcResponse::Disk(disk) => Self::Disk(disk),
            IpcResponse::Memory(memory) => Self::Memory(memory),
            IpcResponse::Cpu(cpu) => Self::Cpu(cpu),
            IpcResponse::NotificationDeleted(id) => Self::NotificationDeleted(id),
            IpcResponse::Killed => Self::Killed,
        }
    }
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default, Serialize, Deserialize)]
pub struct WindowInfo {
    id: u64,
    wm_class: String,
    title: String,
    idx: usize,
}

pub fn print_update(request: IpcRequest) -> anyhow::Result<()> {
    let mut stdout = io::stdout().lock();
    let mut stream = UnixStream::connect(crate::SOCKET_PATH)
        .context("Failed to connect to unix socket (is the daemon running?)")?;
    bincode::serialize_into(&mut stream, &request)
        .context("Failed to write request to unix stream")?;
    stream.flush()?;

    // Read the response from the unix stream and deserialize it
    let response: IpcResponse = bincode::deserialize_from(&mut stream)
        .context("Failed to deserialize response from unix stream")?;

    let stdout_error = "Failed to write response to stdout";

    match response {
        IpcResponse::Killed => {
            writeln!(stdout, "Killed deskctrl daemon.").context(stdout_error)?;
        }
        IpcResponse::NotificationDeleted(id) => {
            writeln!(stdout, "Deleted Notification {id}").context(stdout_error)?;
        }
        _ => {
            let eww: EwwResponse = response.into();
            serde_json::to_writer(&mut stdout, &eww).context(stdout_error)?;
        }
    }

    Ok(())
}
