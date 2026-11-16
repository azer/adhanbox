use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

fn find_port() -> Result<String> {
    let ports = serialport::available_ports()?;
    ports
        .iter()
        .map(|p| p.port_name.clone())
        .find(|n| n.contains("usbmodem"))
        .context("no /dev/cu.usbmodem* port found; is the board plugged in?")
}

/// Send console lines (each prefixed with `ezan `) and echo the device's replies.
pub fn send(port: Option<&str>, lines: &str) -> Result<()> {
    let name = port.map(str::to_string).map_or_else(find_port, Ok)?;
    let mut p = serialport::new(&name, 115_200).timeout(Duration::from_millis(200)).open()?;
    let mut reader = BufReader::new(p.try_clone()?);
    for line in lines.lines() {
        p.write_all(format!("ezan {line}\n").as_bytes())?;
        p.flush()?;
        let start = Instant::now();
        let mut buf = String::new();
        while start.elapsed() < Duration::from_secs(5) {
            buf.clear();
            if reader.read_line(&mut buf).is_ok() && !buf.is_empty() {
                let t = buf.trim();
                if let Some(r) = t.split("ezan> ").nth(1) {
                    println!("{r}");
                    if r.starts_with("ok") || r.starts_with("err") {
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Print device output for `secs` seconds (optionally after a reset).
pub fn log(port: Option<&str>, secs: u64, reset: bool, send: Option<&str>) -> Result<()> {
    let name = port.map(str::to_string).map_or_else(find_port, Ok)?;
    let mut p = serialport::new(&name, 115_200).timeout(Duration::from_millis(200)).open()?;
    if reset {
        // USB-Serial-JTAG: RTS pulse with DTR low resets into the app.
        p.write_data_terminal_ready(false)?;
        p.write_request_to_send(true)?;
        std::thread::sleep(Duration::from_millis(100));
        p.write_request_to_send(false)?;
        drop(p);
        std::thread::sleep(Duration::from_millis(1500));
        p = serialport::new(&name, 115_200).timeout(Duration::from_millis(200)).open()?;
    }
    if let Some(cmd) = send {
        p.write_all(format!("ezan {cmd}\n").as_bytes())?;
        p.flush()?;
    }
    let start = Instant::now();
    let mut buf = [0u8; 1024];
    let mut out = std::io::stdout();
    while start.elapsed() < Duration::from_secs(secs) {
        match std::io::Read::read(&mut p, &mut buf) {
            Ok(n) if n > 0 => {
                out.write_all(&buf[..n])?;
                out.flush()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => {
                std::thread::sleep(Duration::from_millis(300));
                if let Ok(np) = serialport::new(&name, 115_200).timeout(Duration::from_millis(200)).open() {
                    p = np;
                }
            }
            _ => {}
        }
    }
    Ok(())
}
