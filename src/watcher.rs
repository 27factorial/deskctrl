use crate::{
    data::{
        DiskInfo, DiskUsage, MemoryUsage, NetworkSample, NetworkUnit, NetworkUsage, StorageUnit,
    },
    notification::{self, AgsNotification, DBusNotification},
    ringbuf::RingBuf,
    type_map::{TypeMap, TypeMapKey},
};
use anyhow::{bail, Context as _};
use hyprland::event_listener::{AsyncEventListener, WindowEventData};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use sysinfo::{CpuExt as _, DiskExt as _, NetworkExt as _, System, SystemExt as _};
use tokio::{
    io::AsyncWriteExt as _,
    sync::{
        mpsc::{Receiver, Sender},
        Mutex, RwLock,
    },
    time::MissedTickBehavior,
};
use zbus::{
    export::futures_util::{StreamExt as _, TryStreamExt as _},
    Connection, MatchRule, MessageStream,
};

pub trait Watcher {
    type Context: Send + Sync;

    async fn watch(self, ctx: Self::Context) -> anyhow::Result<()>;
}

pub struct DaemonContext {
    data: RwLock<TypeMap>,
    updates: Sender<Update>,
}

impl DaemonContext {
    pub fn new(updates: Sender<Update>) -> Self {
        Self {
            data: RwLock::new(TypeMap::new()),
            updates,
        }
    }

    pub fn data(&self) -> &RwLock<TypeMap> {
        &self.data
    }

    pub async fn send_update(&self, update: Update) -> anyhow::Result<()> {
        self.updates.send(update).await?;
        Ok(())
    }
}

pub struct HyprlandContext {
    updates: Sender<Update>,
    errors: Sender<anyhow::Error>,
}

impl HyprlandContext {
    pub fn new(updates: Sender<Update>, errors: Sender<anyhow::Error>) -> Self {
        Self { updates, errors }
    }

    pub async fn send_update(&self, update: Update) -> anyhow::Result<()> {
        self.updates.send(update).await?;
        Ok(())
    }

    pub async fn send_error(&self, error: anyhow::Error) -> anyhow::Result<()> {
        self.errors.send(error).await?;
        Ok(())
    }
}

pub enum Update {
    Network(String, NetworkUsage),
    Disk(BTreeMap<String, DiskInfo>),
    Memory(MemoryUsage),
    Cpu(f64),
    Notification(AgsNotification),
    ActiveWindow(WindowEventData),
}

pub struct SystemKey;

impl TypeMapKey for SystemKey {
    type Value = Mutex<System>;
}

pub struct NotificationKey;

impl TypeMapKey for NotificationKey {
    type Value = Mutex<VecDeque<AgsNotification>>;
}

pub struct NetworkWatcher {
    period: Duration,
    sample_count: usize,
    sample_map: HashMap<String, RingBuf<NetworkSample>>,
}

impl NetworkWatcher {
    pub fn new(period: Duration, sample_count: usize) -> Self {
        Self {
            period,
            sample_count,
            sample_map: HashMap::new(),
        }
    }
}

impl Watcher for NetworkWatcher {
    type Context = Arc<DaemonContext>;

    async fn watch(mut self, ctx: Arc<DaemonContext>) -> anyhow::Result<()> {
        const UNITS: [NetworkUnit; 4] = [
            NetworkUnit::Kilobit,
            NetworkUnit::Megabit,
            NetworkUnit::Gigabit,
            NetworkUnit::Terabit,
        ];

        let mut loop_idx = 0;

        let mut interval = tokio::time::interval(self.period);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            let sample_start = Instant::now();
            interval.tick().await;

            let data = ctx.data().read().await;
            let mut guard = data
                .get::<SystemKey>()
                .context("Failed to get System from data map")?
                .lock()
                .await;

            guard.refresh_networks();
            let elapsed = sample_start.elapsed();

            for (name, data) in guard.networks() {
                let name = name.clone();

                let samples = self
                    .sample_map
                    .entry(name)
                    .or_insert_with(|| RingBuf::new(self.sample_count));

                let sample = NetworkSample {
                    rx: data.received(),
                    tx: data.transmitted(),
                    time: elapsed,
                };

                samples.push(sample);
            }

            // Every `sample_count` loops, I'll send a message containing the network usage stats.
            if loop_idx == self.sample_count - 1 {
                guard.refresh_networks_list();
                for (name, samples) in self.sample_map.iter() {
                    let secs = samples
                        .iter()
                        .map(|sample| sample.time.as_secs_f64())
                        .sum::<f64>();

                    // Get the raw up/download speed in bytes to display on the graph.
                    let (mut rx_raw, mut tx_raw) = samples
                        .iter()
                        .map(|sample| (sample.rx as f64, sample.tx as f64))
                        .fold((0.0, 0.0), |(rx_acc, tx_acc), (rx_next, tx_next)| {
                            (rx_acc + rx_next, tx_acc + tx_next)
                        });

                    rx_raw /= secs;
                    tx_raw /= secs;

                    let mut rx_converted = rx_raw;
                    let mut tx_converted = tx_raw;

                    let mut rx_unit = NetworkUnit::Byte;
                    let mut tx_unit = NetworkUnit::Byte;

                    for unit in UNITS {
                        if rx_converted > rx_unit.scale_factor() {
                            rx_converted /= rx_unit.scale_factor();
                            rx_unit = unit;
                        } else {
                            break;
                        }
                    }

                    for unit in UNITS {
                        if tx_converted > tx_unit.scale_factor() {
                            tx_converted /= tx_unit.scale_factor();
                            tx_unit = unit;
                        } else {
                            break;
                        }
                    }

                    let usage = NetworkUsage {
                        tx: (tx_converted, tx_unit),
                        tx_raw,
                        rx: (rx_converted, rx_unit),
                        rx_raw,
                    };

                    ctx.send_update(Update::Network(name.clone(), usage))
                        .await?;
                }
            }

            loop_idx = (loop_idx + 1) % self.sample_count
        }
    }
}

pub struct DiskWatcher {
    period: Duration,
}

impl DiskWatcher {
    pub fn new(period: Duration) -> Self {
        Self { period }
    }
}

impl Watcher for DiskWatcher {
    type Context = Arc<DaemonContext>;

    async fn watch(self, ctx: Arc<DaemonContext>) -> anyhow::Result<()> {
        const UNITS: [StorageUnit; 5] = [
            StorageUnit::Kilobyte,
            StorageUnit::Megabyte,
            StorageUnit::Gigabyte,
            StorageUnit::Terabyte,
            StorageUnit::Petabyte,
        ];

        let mut interval = tokio::time::interval(self.period);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            let data = ctx.data().read().await;
            let mut guard = data
                .get::<SystemKey>()
                .context("Failed to get System from data map")?
                .lock()
                .await;

            let mut disk_info = BTreeMap::new();
            guard.refresh_disks_list();
            guard.refresh_disks();

            for disk in guard.disks().iter() {
                let name = disk.name().to_string_lossy().into_owned();
                let mount_point = disk.mount_point().to_string_lossy().into_owned();
                let removable = disk.is_removable();

                let total = disk.total_space();
                let used = total - disk.available_space();

                let mut used_converted = used as f64;
                let mut used_unit = StorageUnit::Byte;
                let mut total_converted = total as f64;
                let mut total_unit = StorageUnit::Byte;

                for unit in UNITS {
                    if used_converted > 1024.0 {
                        used_converted /= 1024.0;
                        used_unit = unit;
                    } else {
                        break;
                    }
                }

                for unit in UNITS {
                    if total_converted > 1024.0 {
                        total_converted /= 1024.0;
                        total_unit = unit;
                    } else {
                        break;
                    }
                }

                let usage = DiskUsage {
                    used: (used_converted, used_unit),
                    total: (total_converted, total_unit),
                    percent: (used as f64 / total as f64) * 100.0,
                };

                disk_info.insert(
                    mount_point.clone(),
                    DiskInfo {
                        name,
                        mount_point,
                        removable,
                        usage,
                    },
                );
            }

            ctx.send_update(Update::Disk(disk_info)).await?;
        }
    }
}

pub struct MemoryWatcher {
    period: Duration,
}

impl MemoryWatcher {
    pub fn new(period: Duration) -> Self {
        Self { period }
    }
}

impl Watcher for MemoryWatcher {
    type Context = Arc<DaemonContext>;

    async fn watch(self, ctx: Arc<DaemonContext>) -> anyhow::Result<()> {
        const UNITS: [StorageUnit; 2] = [StorageUnit::Kilobyte, StorageUnit::Megabyte];

        let mut interval = tokio::time::interval(self.period);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            let data = ctx.data().read().await;
            let mut guard = data
                .get::<SystemKey>()
                .context("Failed to get System from data map")?
                .lock()
                .await;

            guard.refresh_memory();
            let used = guard.used_memory();
            let total = guard.total_memory();

            let mut used_converted = used as f64;
            let mut used_unit = StorageUnit::Byte;
            let mut total_converted = total as f64;
            let mut total_unit = StorageUnit::Byte;

            for unit in UNITS {
                if used_converted > 1024.0 {
                    used_converted /= 1024.0;
                    used_unit = unit;
                } else {
                    break;
                }
            }

            for unit in UNITS {
                if total_converted > 1024.0 {
                    total_converted /= 1024.0;
                    total_unit = unit;
                } else {
                    break;
                }
            }

            let usage = MemoryUsage {
                used: (used_converted, used_unit),
                total: (total_converted, total_unit),
                percent: (used as f64 / total as f64) * 100.0,
            };

            ctx.send_update(Update::Memory(usage)).await?;
        }
    }
}

pub struct CpuWatcher {
    period: Duration,
}

impl CpuWatcher {
    pub fn new(period: Duration) -> Self {
        Self { period }
    }
}

impl Watcher for CpuWatcher {
    type Context = Arc<DaemonContext>;

    async fn watch(self, ctx: Arc<DaemonContext>) -> anyhow::Result<()> {
        let period = self.period.max(System::MINIMUM_CPU_UPDATE_INTERVAL);

        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        // This ensures that at least 2 refreshes are done before sending data to the manager task.
        {
            let data = ctx.data().read().await;
            let mut guard = data
                .get::<SystemKey>()
                .context("Failed to get System from data map")?
                .lock()
                .await;
            guard.refresh_cpu();
        }

        loop {
            interval.tick().await;
            let data = ctx.data().read().await;
            let mut guard = data
                .get::<SystemKey>()
                .context("Failed to get System from data map")?
                .lock()
                .await;
            let cpu_count = guard.cpus().len();

            guard.refresh_cpu();
            let avg_usage = guard
                .cpus()
                .iter()
                .map(|cpu| cpu.cpu_usage() as f64)
                .sum::<f64>()
                / cpu_count as f64;

            ctx.send_update(Update::Cpu(avg_usage)).await?;
        }
    }
}

pub struct NotificationWatcher {
    connection: Connection,
}

impl NotificationWatcher {
    pub async fn new() -> anyhow::Result<Self> {
        let rule = MatchRule::builder()
            .path("/org/freedesktop/Notifications")?
            .interface("org.freedesktop.Notifications")?
            .member("Notify")?
            .build()
            .to_string();

        let connection = Connection::session()
            .await
            .context("Failed to connect to DBus session bus")?;

        connection
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus.Monitoring"),
                "BecomeMonitor",
                &(&[rule.as_str()] as &[&str], 0u32),
            )
            .await
            .context("Failed to call /org/freedesktop/DBus BecomeMonitor")?;

        Ok(Self { connection })
    }
}

impl Watcher for NotificationWatcher {
    type Context = Arc<DaemonContext>;

    #[allow(clippy::let_unit_value)] // False positive (possibly related to async fn in trait?)
    async fn watch(self, ctx: Arc<DaemonContext>) -> anyhow::Result<()> {
        let mut stream = MessageStream::from(&self.connection).skip(1);

        while let Some(message) = stream
            .try_next()
            .await
            .context("Failed to get next DBus message from stream")?
        {
            let body = match message.body::<DBusNotification>() {
                Ok(notif) => notif,
                Err(e) => {
                    let msg =
                        format!("Error deserializing DBus message to DBusNotification: {e:?}");
                    tokio::io::stderr()
                        .write_all(msg.as_bytes())
                        .await
                        .context("Failed to write error message to stderr")?;
                    continue;
                }
            };

            let notification = notification::make_ags(body)
                .await
                .context("Failed to convert DBus Notification to ags Notification format")?;

            ctx.send_update(Update::Notification(notification)).await?;
        }

        Ok(())
    }
}

pub struct HyprlandWatcher {
    event_listener: AsyncEventListener,
    errors: Receiver<anyhow::Error>,
}

impl HyprlandWatcher {
    pub fn new(errors: Receiver<anyhow::Error>) -> Self {
        Self {
            event_listener: AsyncEventListener::new(),
            errors,
        }
    }
}

impl Watcher for HyprlandWatcher {
    type Context = Arc<HyprlandContext>;

    async fn watch(mut self, ctx: Arc<HyprlandContext>) -> anyhow::Result<()> {
        self.event_listener
            .add_active_window_change_handler(move |data| {
                // Unfortunately, using the event listener in this way incurs an Arc clone every
                // time the handler is run. There isn't much I can do about that given the lifetime
                // requirements on this closure.
                let ctx = Arc::clone(&ctx);

                Box::pin(async move {
                    if let Some(event) = data {
                        if let Err(e) = ctx.send_update(Update::ActiveWindow(event)).await {
                            ctx.send_error(e).await.unwrap();
                        }
                    }
                })
            });

        tokio::select! {
            listener_result = self.event_listener.start_listener_async() => {
                listener_result.context("Failed to run Hyprland event listener")
            }
            update_err = self.errors.recv() => {
                bail!("Failed to send active window update: {update_err:?}")
            }
        }
    }
}
