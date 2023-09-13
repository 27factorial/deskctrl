#![feature(async_fn_in_trait)]

mod daemon;
mod data;
mod ipc;
mod notification;
mod ringbuf;
mod type_map;
mod watcher;

use anyhow::bail;
use anyhow::Context as _;
use clap::Parser;
use clap::Subcommand;
use daemonize::Daemonize;
use ipc::IpcRequest;
use nix::unistd::Uid;
use serde::{Deserialize, Serialize};
use std::fs::File;

const SOCKET_PATH: &str = "/dev/shm/deskctrld.sock";
const IMAGE_PATH: &str = "/tmp/deskctrl/images";
const DATA_PATH: &str = "/tmp/deskctrl/data";

#[derive(Parser, Debug)]
#[command(author, version, about = "Factorial's controller program for eww widgets", long_about = None)]
pub enum Mode {
    /// Get information about network speed and usage
    Network,
    /// Get information about attached disks
    Disk,
    /// Get information about memory usage
    Memory,
    /// Get information about CPU usage
    Cpu,
    /// Kill the deskctrl daemon
    Kill,
    /// Run a command to change or remove notification groups
    #[clap(flatten)]
    Notification(NotificationCommand),
    /// Provides daemon functionality without actually spawning deskctrl as a daemon
    TestDaemon,
    /// Start the deskctrl daemon
    Daemon,
}

#[derive(
    Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Serialize, Deserialize, Subcommand,
)]
pub enum NotificationCommand {
    Delete { id: u32 },
}

fn main() -> anyhow::Result<()> {
    if Uid::current().is_root() || Uid::effective().is_root() {
        bail!("Please do not run this program with root privileges.");
    }

    match Mode::parse() {
        Mode::Network => ipc::print_update(IpcRequest::Network),
        Mode::Disk => ipc::print_update(IpcRequest::Disk),
        Mode::Memory => ipc::print_update(IpcRequest::Memory),
        Mode::Cpu => ipc::print_update(IpcRequest::Cpu),
        Mode::Kill => ipc::print_update(IpcRequest::Kill),
        Mode::Notification(command) => ipc::print_update(IpcRequest::Notification(command)),
        Mode::TestDaemon => daemon_main(),
        Mode::Daemon => {
            let stdout =
                File::create("/tmp/deskctrld.out").context("Failed to create daemon stdout")?;
            let stderr =
                File::create("/tmp/deskctrld.err").context("failed to create daemon stderr")?;

            Daemonize::new()
                .pid_file("/tmp/deskctrld.pid")
                .working_directory("/tmp")
                .stdout(stdout)
                .stderr(stderr)
                .start()
                .context("Failed to daemonize process (is another instance already running?)")?;

            match daemon_main() {
                Ok(_) => Ok(()),
                Err(e) => {
                    for (idx, cause) in e.chain().rev().enumerate() {
                        eprintln!("{}: {}", idx, cause);
                    }
                    Err(e)
                }
            }
        }
    }
}

#[tokio::main]
async fn daemon_main() -> anyhow::Result<()> {
    daemon::run_daemon().await
}
