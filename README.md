# EwwCtrl

A (fairly) small program for providing data to and controlling my 
[eww (ElKowar's wacky widgets)](https://github.com/elkowar/eww) widgets on my machine. Because this 
is only meant for personal use, documentation is lacking and functionality only extends as far as I
need (read: want) it to for my PC.

## Compilation

To compile ewwctrl, run the following commands:

```shell
git clone https://github.com/27factorial/ewwctrl.git
cd ewwctrl
cargo build --release
```
The generated executable can be found in `target/release/ewwctrl`.

Ewwctrl is only tested on the latest nightly Rust toolchain, although it will probably compile on 
the latest stable or earlier toolchain versions, as it doesn't use any nightly features (yet).

# Paths
Ewwctrl uses a number of files on disk as either data, unix domain sockets, or for stdout/stderr.

- `/dev/shm/ewwctrld.sock` -> The socket file for communication between the daemon and client 
  processes. If your system does not have /dev/shm enabled, you can change the SOCKET_PATH constant
  to some other file path before compilation.
- `/tmp/ewwctrld.{out | err}` -> The stdout and stderr files for the ewwctrl daemon process.
- `/tmp/ewwctrld.pid` -> The PID file for the ewwctrl daemon process.
- `/tmp/ewwctrl/data/notifications.json` -> The file containing notification data for eww to read 
    from (usually using `tail -F`).
- `/tmp/ewwctrl/images/\<hash>.png` -> Images pulled from DBus notifications. Cleaned up
   automatically when running `ewwctrl kill` or when the image is no longer in use.

## Why?
As you may notice when skimming the codebase, much of the data provided by ewwctrl is already built
into eww itself, so then what's the point of this program?

The short answer is "Because I wanted to." The longer answer is:
1. I wanted to try my hand at building a more complex Linux program that utilizes daemons, DBus, 
   and Unix Domain Sockets.
2. Eww provides no way that I can see (aside from modifying the codebase) to change the time between
   updates for the information it provides. For instance, CPU information is updated once per second
   when using the EWW_CPU "magic variable" in yuck, while ewwctrl updates once every 500ms. If I was
   going to dig into the codebase for eww to change a couple of values anyway, I figured I could
   write my own program to provide the same (and more, see below) info with update intervals that
   I had set.
3. I wanted a way to provide notifications to eww that have more information encoded than what tools
   like dunst or mako offer. Neither of these tools offers a way that I can find to obtain images
   from them through their history commands, so I figured I would just add that to my current
   ewwctrl program instead.

## Usage
Run ewwctrl --help for information on how to use the tool.