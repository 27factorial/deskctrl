use crate::{
    data::{DiskInfo, MemoryUsage, NetworkUsage},
    ipc::{IpcRequest, IpcResponse},
    notification::{self, EwwNotification},
    watcher::{
        CpuWatcher, DaemonContext, DiskWatcher, MemoryWatcher, NetworkWatcher, NotificationWatcher,
        SystemKey, Update, Watcher,
    },
};
use anyhow::{bail, Context as _};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io,
    sync::Arc,
    time::Duration,
};
use sysinfo::{System, SystemExt};
use tokio::{
    fs::{self, File},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    net::UnixListener,
    sync::{mpsc, Mutex},
};

const CHANNEL_BUFFER: usize = 32;

#[derive(Clone, PartialEq, Debug, Default)]
struct SystemData {
    network: HashMap<String, NetworkUsage>,
    disk: BTreeMap<String, DiskInfo>,
    memory: MemoryUsage,
    cpu: f64,
}

#[derive(Debug)]
struct NotificationData {
    notifications: VecDeque<EwwNotification>,
    json_bytes: Vec<u8>,
    json_file: File,
}

impl NotificationData {
    pub async fn new() -> anyhow::Result<Self> {
        let json_file = notification::create_dir_and_file(format!(
            "{}/{}",
            crate::DATA_PATH,
            notification::NOTIFICATIONS_FILE
        ))
        .await
        .context("Failed to create notifications JSON file")?;

        Ok(Self {
            notifications: VecDeque::with_capacity(notification::NOTIFICATION_LIMIT),
            json_bytes: Vec::with_capacity(8192),
            json_file,
        })
    }

    pub async fn insert_or_replace(&mut self, notification: EwwNotification) -> anyhow::Result<()> {
        // the notification ID corresponds to replace_id in the DBus notification, so this check
        // will replace the old notification with the new one if necessary. This also cuts down on
        // disk usage for things like spotify, where only one song needs to be in the notification
        // queue.
        if let Some(idx) = self
            .notifications
            .iter()
            .position(|replace| replace.id == notification.id)
        {
            if let Some(old) = self.notifications.remove(idx) {
                self.clean_image(old).await?;
            }
        }

        self.notifications.push_front(notification);

        if self.notifications.len() > notification::NOTIFICATION_LIMIT {
            self.notifications.pop_back();
        }

        self.json_bytes.clear();
        serde_json::to_writer(&mut self.json_bytes, &self.notifications)
            .context("Failed to serialize to json_bytes buffer")?;

        self.write_notifications().await?;

        Ok(())
    }

    pub async fn remove(&mut self, id: u32) -> anyhow::Result<()> {
        let position = self
            .notifications
            .iter()
            .position(|notification| notification.id == id);

        if let Some(idx) = position {
            if let Some(notification) = self.notifications.remove(idx) {
                self.clean_image(notification).await?;
                self.write_notifications().await?;
            }
        }

        Ok(())
    }

    pub async fn cleanup(mut self) -> anyhow::Result<()> {
        // Unfortunately I can't use a for-loop here because of lifetime issues.
        while let Some(notification) = self.notifications.pop_back() {
            self.clean_image(notification).await?;
        }

        Ok(())
    }

    async fn write_notifications(&mut self) -> anyhow::Result<()> {
        self.json_file
            .set_len(0)
            .await
            .context("Failed to truncate json_file")?;
        self.json_file
            .rewind()
            .await
            .context("Failed to rewind json_file cursor")?;
        self.json_file
            .write_all(&self.json_bytes)
            .await
            .context("Failed to write json_bytes to json_file")?;
        Ok(())
    }

    async fn clean_image(&self, notification: EwwNotification) -> anyhow::Result<()> {
        if notification.tmp_image {
            let Some(image_path) = notification.image_path else {
                return Ok(());
            };

            let unique_image = self
                .notifications
                .iter()
                .flat_map(|notif| notif.image_path.iter())
                .any(|path| path.as_path() == image_path.as_path());

            if unique_image {
                if let Err(e) = fs::remove_file(image_path).await {
                    if e.kind() != io::ErrorKind::NotFound {
                        bail!("Failed to clean up notification image file: {e:?}")
                    }
                }
            }
        }

        Ok(())
    }
}

pub async fn run_daemon() -> anyhow::Result<()> {
    let mut system_data = SystemData::default();
    let mut notification_data = NotificationData::new().await?;

    let mut serde_buffer = Vec::new();

    let (tx, mut rx) = mpsc::channel(CHANNEL_BUFFER);

    let ctx = Arc::new(DaemonContext::new(tx));
    ctx.data()
        .write()
        .await
        .insert::<SystemKey>(Mutex::new(System::new_all()));

    // Network: 4 samples per second(ish), 1 update per second(ish)
    tokio::spawn(NetworkWatcher::new(Duration::from_millis(250), 4).watch(Arc::clone(&ctx)));

    // Disk: 1 sample every 2 seconds(ish), 1 update per 2 seconds(ish)
    tokio::spawn(DiskWatcher::new(Duration::from_secs(2)).watch(Arc::clone(&ctx)));

    // Memory: 1 sample per second(ish), 1 update per second(ish)
    tokio::spawn(MemoryWatcher::new(Duration::from_secs(1)).watch(Arc::clone(&ctx)));

    // CPU: 2 samples per second(ish), 2 updates per second(ish)
    tokio::spawn(CpuWatcher::new(Duration::from_millis(500)).watch(Arc::clone(&ctx)));

    // Notifications: No polling required, writes to JSON file directly when a notification is received.
    tokio::spawn(NotificationWatcher::new().await?.watch(Arc::clone(&ctx)));

    if tokio::fs::try_exists(crate::SOCKET_PATH)
        .await
        .is_ok_and(|id| id)
    {
        tokio::fs::remove_file(crate::SOCKET_PATH)
            .await
            .context("Could not remove old socket file")?;
    }

    let sock = UnixListener::bind(crate::SOCKET_PATH).context("Could not bind unix socket")?;
    let mut killed = false;

    while !killed {
        tokio::select! {
            update = rx.recv() => {
                let Some(update) = update else {
                    return Ok(());
                };

                match update {
                    Update::Network(interface, network) => {
                        system_data.network.insert(interface, network);
                    }
                    Update::Disk(disk) => {
                        system_data.disk = disk
                    }
                    Update::Memory(memory) => system_data.memory = memory,
                    Update::Cpu(cpu) => system_data.cpu = cpu,
                    Update::Notification(notification) => {
                        notification_data.insert_or_replace(notification).await?;
                    }
                }
            },
            res = sock.accept() => {
                let (mut stream, _) = res.context("Could not accept unix stream")?;

                stream.read_buf(&mut serde_buffer).await.context("Failed to read from unix stream")?;

                let request = match bincode::deserialize(&serde_buffer) {
                    Ok(request) => request,
                    Err(e) => {
                        eprintln!("Failed to deserialize request: {e:?}");
                        serde_buffer.clear();
                        continue;
                    }
                };

                let response = match request {
                    IpcRequest::Network => {
                        IpcResponse::Network(system_data.network.clone())
                    },
                    IpcRequest::Disk => {
                        let mut info: Vec<_> = system_data.disk.values().filter(|info| info.name != "systemd-1" && info.mount_point != "/efi").cloned().collect();
                        info.sort_by(|a, b| a.name.cmp(&b.name));

                        IpcResponse::Disk(info)
                    },
                    IpcRequest::Memory => IpcResponse::Memory(system_data.memory),
                    IpcRequest::Cpu => IpcResponse::Cpu(system_data.cpu),
                    IpcRequest::DeleteNotification(id) => {
                        notification_data.remove(id).await?;
                        IpcResponse::NotificationDeleted(id)
                    }
                    IpcRequest::Kill => {
                        killed = true;
                        IpcResponse::Killed
                    }
                };

                serde_buffer.clear();
                bincode::serialize_into(&mut serde_buffer, &response).context("Failed to serialize response to buffer")?;

                stream.write_all(&serde_buffer).await.context("Failed to write response to unix stream")?;
                stream.shutdown().await.context("Failed to shut down unix stream")?;
                serde_buffer.clear();
            }
        }
    }

    notification_data
        .cleanup()
        .await
        .context("Failed to clean up notification image data")?;

    Ok(())
}
