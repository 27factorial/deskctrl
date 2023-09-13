use crate::{
    data::{DiskInfo, MemoryUsage, NetworkUsage},
    ipc::{IpcRequest, IpcResponse},
    notification::{self, EwwNotification, NotificationGroup, NOTIFICATION_LIMIT},
    watcher::{
        CpuWatcher, DaemonContext, DiskWatcher, HyprlandContext, HyprlandWatcher, MemoryWatcher,
        NetworkWatcher, NotificationWatcher, SystemKey, Update, Watcher,
    },
    NotificationCommand,
};
use anyhow::Context as _;
use hyprland::{data::Client, shared::HyprDataActiveOptional};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime},
};
use sysinfo::{System, SystemExt};
use tokio::{
    fs::File,
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
    map: HashMap<Arc<str>, NotificationGroup>,
    image_path_cache: HashMap<Arc<Path>, usize>,
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
            map: HashMap::new(),
            image_path_cache: HashMap::with_capacity(NOTIFICATION_LIMIT),
            json_bytes: Vec::with_capacity(8192),
            json_file,
        })
    }

    pub async fn insert_or_replace(&mut self, notification: EwwNotification) -> anyhow::Result<()> {
        // the notification ID corresponds to replace_id in the DBus notification, so this check
        // will replace the old notification with the new one if necessary. This also cuts down on
        // disk usage for things like spotify, where only one song needs to be in the notification
        // queue.
        let group = self
            .map
            .entry(Arc::clone(&notification.app_name))
            .or_default();

        if notification.tmp_image {
            if let Some(ref image_path) = notification.image_path {
                self.image_path_cache
                    .entry(Arc::clone(image_path))
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
            }
        }

        if let Some(idx) = group
            .notifications
            .iter()
            .position(|replace| replace.id == notification.id)
        {
            if let Some(old) = group.notifications.remove(idx) {
                notification::clean_image(&mut self.image_path_cache, old).await?;
            }
        }

        group.notifications.push_front(notification);

        if group.notifications.len() > NOTIFICATION_LIMIT {
            group.notifications.pop_back();
        }

        group.timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("failed to get duration since Unix epoch")
            .as_nanos();

        self.write_notifications().await?;

        Ok(())
    }

    pub async fn remove(&mut self, id: u32) -> anyhow::Result<()> {
        for group in self.map.values_mut() {
            let position = group
                .notifications
                .iter()
                .position(|notification| notification.id == id);

            if let Some(idx) = position {
                if let Some(notification) = group.notifications.remove(idx) {
                    notification::clean_image(&mut self.image_path_cache, notification).await?;
                    self.write_notifications().await?;
                    break;
                }
            }
        }

        Ok(())
    }

    // TODO: Switch this to extract_if when that's available on VecDeque (if ever)
    pub async fn clear_class(&mut self, class: &str) -> anyhow::Result<()> {
        let class = class.to_lowercase();
        let mut write_notifications = false;

        for group in self.map.values_mut() {
            let mut i = 0;
            while i < group.notifications.len() {
                if group.notifications[i].app_class == class {
                    write_notifications = true;
                    let old = group.notifications.remove(i).unwrap();

                    notification::clean_image(&mut self.image_path_cache, old).await?;
                } else {
                    i += 1;
                }
            }
        }

        // If no notifications were cleared, there's no need to write to the notifications file.
        // This fixes the constant updating in ags/eww when switching windows.
        if write_notifications {
            self.write_notifications().await?;
        }

        Ok(())
    }

    pub async fn cleanup(mut self) -> anyhow::Result<()> {
        for mut group in self.map.into_values() {
            // Unfortunately I can't use a for-loop here because of lifetime issues.
            while let Some(notification) = group.notifications.pop_back() {
                notification::clean_image(&mut self.image_path_cache, notification).await?;
            }
        }

        Ok(())
    }

    async fn write_notifications(&mut self) -> anyhow::Result<()> {
        self.json_bytes.clear();

        let mut serializer = serde_json::Serializer::new(&mut self.json_bytes);

        notification::serialize_map_to_filtered_sorted_vec(&self.map, &mut serializer)
            .context("Failed to serialize to json_bytes buffer")?;
        self.json_bytes.push(b'\n');

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
}

pub async fn run_daemon() -> anyhow::Result<()> {
    let mut system_data = SystemData::default();
    let mut notification_data = NotificationData::new().await?;

    let mut serde_buffer = Vec::new();

    let (update_tx, mut update_rx) = mpsc::channel(CHANNEL_BUFFER);
    let (hyprland_error_tx, hyprland_error_rx) = mpsc::channel(1);

    let daemon_ctx = Arc::new(DaemonContext::new(update_tx.clone()));
    let hyprland_ctx = Arc::new(HyprlandContext::new(update_tx, hyprland_error_tx));

    daemon_ctx
        .data()
        .write()
        .await
        .insert::<SystemKey>(Mutex::new(System::new_all()));

    // Network: 4 samples per second(ish), 1 update per second(ish)
    tokio::spawn(NetworkWatcher::new(Duration::from_millis(250), 4).watch(Arc::clone(&daemon_ctx)));

    // Disk: 1 sample every 2 seconds(ish), 1 update per 2 seconds(ish)
    tokio::spawn(DiskWatcher::new(Duration::from_secs(2)).watch(Arc::clone(&daemon_ctx)));

    // Memory: 1 sample per second(ish), 1 update per second(ish)
    tokio::spawn(MemoryWatcher::new(Duration::from_secs(1)).watch(Arc::clone(&daemon_ctx)));

    // CPU: 2 samples per second(ish), 2 updates per second(ish)
    tokio::spawn(CpuWatcher::new(Duration::from_millis(500)).watch(Arc::clone(&daemon_ctx)));

    // Notifications: No polling required, writes to JSON file directly when a notification is received.
    tokio::spawn(
        NotificationWatcher::new()
            .await?
            .watch(Arc::clone(&daemon_ctx)),
    );

    tokio::spawn(HyprlandWatcher::new(hyprland_error_rx).watch(hyprland_ctx));

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
            update = update_rx.recv() => {
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
                        // We only want to add the notification to the queue if the window the
                        // notification came from isn't focused already.
                        let client = Client::get_active_async().await?;

                        if let Some(active) = client {
                            let active_class = active.class.to_lowercase();

                            if notification.app_class == active_class {
                                continue;
                            }
                        }

                        notification_data.insert_or_replace(notification).await?;
                    }
                    Update::ActiveWindow(event) => {
                        // When switching to a new window, notifications are cleared for that window
                        // class, since presumably you're going to check them.
                        notification_data.clear_class(&event.window_class).await?;
                    }
                }
            },
            res = sock.accept() => {
                let (mut stream, _) = res.context("Could not accept unix stream")?;

                stream.read_buf(&mut serde_buffer)
                    .await
                    .context("Failed to read from unix stream")?;

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
                        let mut info: Vec<_> = system_data.disk
                            .values()
                            .filter(|info| info.name != "systemd-1" && info.mount_point != "/efi")
                            .cloned()
                            .collect();

                        info.sort_by(|a, b| a.name.cmp(&b.name));

                        IpcResponse::Disk(info)
                    },
                    IpcRequest::Memory => IpcResponse::Memory(system_data.memory),
                    IpcRequest::Cpu => IpcResponse::Cpu(system_data.cpu),
                    IpcRequest::Notification(command) => {
                        handle_notification_command(command, &mut notification_data).await?;
                        IpcResponse::Notification
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

async fn handle_notification_command(
    command: NotificationCommand,
    data: &mut NotificationData,
) -> anyhow::Result<()> {
    match command {
        NotificationCommand::Delete { id } => data.remove(id).await,
    }
}
