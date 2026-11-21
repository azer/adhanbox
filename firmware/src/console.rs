//! USB serial console for `cargo xtask setup/reset`. Lines look like `ezan <cmd> <args>`.
use std::io::BufRead;
use std::sync::mpsc::{channel, Receiver};

#[derive(Debug)]
pub enum Cmd {
    Wifi { ssid: String, pass: String },
    City(String),
    Reset,
    Status,
    Sync,
    Setup,
    Play(u8),
    /// Simulated button: (boot?, long?)
    Key(bool, bool),
    Stop,
    /// Menu → Test adhan, from the console.
    Adhan,
    /// Pretend the next prayer is N minutes from now (until restart), to run the real trigger.
    AdhanIn(u16),
    /// `sonos <scan|on|off|tick NAME|untick NAME|vol N|fajr N>`
    Sonos(String),
}

pub fn start() -> Receiver<Cmd> {
    unsafe {
        use esp_idf_svc::sys::*;
        let mut cfg = usb_serial_jtag_driver_config_t { tx_buffer_size: 512, rx_buffer_size: 512 };
        usb_serial_jtag_driver_install(&mut cfg);
        esp_vfs_usb_serial_jtag_use_driver();
    }
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .stack_size(6 * 1024)
        .spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines().map_while(Result::ok) {
                let line = line.trim();
                let Some(rest) = line.strip_prefix("ezan ") else { continue };
                let (cmd, args) = rest.split_once(' ').unwrap_or((rest, ""));
                let c = match cmd {
                    "wifi" => {
                        let (ssid, pass) = args.split_once('\t').unwrap_or((args, ""));
                        Some(Cmd::Wifi { ssid: ssid.to_string(), pass: pass.to_string() })
                    }
                    "city" => Some(Cmd::City(args.trim().to_string())),
                    "reset" => Some(Cmd::Reset),
                    "status" => Some(Cmd::Status),
                    "sync" => Some(Cmd::Sync),
                    "setup" => Some(Cmd::Setup),
                    "play" => Some(Cmd::Play(args.trim().parse().unwrap_or(1))),
                    "stop" => Some(Cmd::Stop),
                    "adhan" => match args.trim().strip_prefix("in ") {
                        Some(n) => Some(Cmd::AdhanIn(n.trim().parse().unwrap_or(2).clamp(1, 30))),
                        None => Some(Cmd::Adhan),
                    },
                    "sonos" => Some(Cmd::Sonos(args.trim().to_string())),
                    "key" => {
                        let mut it = args.split_whitespace();
                        let b = matches!(it.next(), Some("top" | "boot"));
                        let l = it.next() == Some("long");
                        Some(Cmd::Key(b, l))
                    }
                    _ => {
                        println!("ezan> err unknown command");
                        None
                    }
                };
                if let Some(c) = c {
                    if tx.send(c).is_err() {
                        break;
                    }
                }
            }
        })
        .expect("console thread");
    rx
}
