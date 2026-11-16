//! Host-side tools: `cargo xtask <cmd>`.
//!
//!   cities                 cities.toml → core/src/cities_gen.rs (POSIX TZ from system zoneinfo)
//!   audio                  assets/ezan/*.mp3 → target/audio.bin (trim, normalize, 32 kbps mono MP3)
//!   flash [--release]      build firmware, flash app + audio partition, open monitor
//!   flash-audio            flash only the audio partition
//!   release                build firmware + audio pack into dist/ (what install.sh flashes)
//!   setup --ssid S --pass P [--city KEY]   send Wi-Fi/city to the device over USB serial
//!   reset [--all]          clear device settings over USB serial (--all also erases audio)
//!   status | sync          ask the device for its state / to sync now
//!   monitor                serial monitor (interactive)
//!   log [--secs N] [--reset] [--send CMD]   print device output for N seconds

mod audio;
mod cities;
mod serial;

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

pub fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

pub fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// Offset of the `audio` partition, read from firmware/partitions.csv.
pub fn audio_partition() -> Result<(u32, u32)> {
    let csv = std::fs::read_to_string(root().join("firmware/partitions.csv"))?;
    for line in csv.lines() {
        let cols: Vec<_> = line.split(',').map(str::trim).collect();
        if cols.first() == Some(&"audio") && cols.len() >= 5 {
            let p = |s: &str| -> Result<u32> {
                let s = s.trim();
                Ok(if let Some(h) = s.strip_prefix("0x") {
                    u32::from_str_radix(h, 16)?
                } else if let Some(k) = s.strip_suffix('K') {
                    k.parse::<u32>()? * 1024
                } else if let Some(m) = s.strip_suffix('M') {
                    m.parse::<u32>()? * 1024 * 1024
                } else {
                    s.parse()?
                })
            };
            return Ok((p(cols[3])?, p(cols[4])?));
        }
    }
    bail!("no `audio` row in firmware/partitions.csv")
}

fn esp_env(cmd: &mut Command) -> &mut Command {
    // `cargo xtask` runs under the host toolchain; strip what it injects so the firmware's
    // rust-toolchain.toml (`esp`) wins, then add the Xtensa env from `espup` (~/export-esp.sh).
    for k in ["RUSTUP_TOOLCHAIN", "RUSTC", "RUSTDOC", "RUSTC_WRAPPER", "CARGO", "CARGO_ENCODED_RUSTFLAGS", "CARGO_TARGET_DIR", "CARGO_MANIFEST_DIR", "CARGO_PKG_NAME"] {
        cmd.env_remove(k);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    if let Ok(sh) = std::fs::read_to_string(format!("{home}/export-esp.sh")) {
        for line in sh.lines() {
            if let Some((k, v)) = line.strip_prefix("export ").and_then(|l| l.split_once('=')) {
                if k != "PATH" {
                    cmd.env(k, v.trim_matches('"'));
                }
            }
        }
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let tools = root().join("firmware/.tools/bin");
    cmd.env("PATH", format!("{}:{home}/.cargo/bin:{path}", tools.display()))
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = esp_env(cmd).status().with_context(|| format!("running {cmd:?}"))?;
    if !status.success() {
        bail!("command failed: {cmd:?}");
    }
    Ok(())
}

fn flash_audio(port: Option<&str>) -> Result<()> {
    let bin = root().join("target/audio.bin");
    if !bin.exists() {
        audio::build()?;
    }
    let (offset, _) = audio_partition()?;
    let mut c = Command::new("espflash");
    c.arg("write-bin");
    if let Some(p) = port {
        c.args(["--port", p]);
    }
    c.arg(format!("0x{offset:x}")).arg(&bin);
    run(&mut c)
}

fn build_firmware(release: bool) -> Result<PathBuf> {
    let mut b = Command::new("cargo");
    b.current_dir(root().join("firmware")).arg("build");
    if release {
        b.arg("--release");
    }
    run(&mut b)?;
    Ok(root().join(format!(
        "firmware/target/xtensa-esp32s3-espidf/{}/ezanbox",
        if release { "release" } else { "debug" }
    )))
}

/// Build everything a no-toolchain install needs into dist/:
///   bootloader.bin @ 0x0, partition-table.bin @ 0x8000, adhanbox.bin @ 0x10000, audio.bin @ audio offset,
/// plus flash.txt (address + file per line) that install.sh and the web installer read.
/// The parts are kept separate (not one merged image) so updates don't wipe the NVS settings.
fn release() -> Result<()> {
    let audio = audio::build()?;
    let elf = build_firmware(true)?;
    let dist = root().join("dist");
    let _ = std::fs::remove_dir_all(&dist);
    std::fs::create_dir_all(&dist)?;
    // One merged image (espflash's bootloader + partition table + app, as `xtask flash` writes them),
    // then split at the fixed ESP32-S3 offsets so each part can be written on its own.
    let merged = dist.join("merged.bin");
    let mut c = Command::new("espflash");
    c.args(["save-image", "--chip", "esp32s3", "--flash-size", "8mb", "--merge", "--skip-padding", "--partition-table"])
        .arg(root().join("firmware/partitions.csv"))
        .arg(&elf)
        .arg(&merged);
    run(&mut c)?;
    let img = std::fs::read(&merged)?;
    std::fs::remove_file(&merged)?;
    if img.len() <= 0x10000 {
        bail!("merged image is only {} bytes", img.len());
    }
    let mut lines = Vec::new();
    for (addr, end, out) in [(0x0usize, 0x8000usize, "bootloader.bin"), (0x8000, 0x9000, "partition-table.bin"), (0x10000, img.len(), "adhanbox.bin")] {
        std::fs::write(dist.join(out), &img[addr..end])?;
        lines.push((addr as u32, out.to_string()));
    }
    let (audio_off, _) = audio_partition()?;
    std::fs::copy(&audio, dist.join("audio.bin"))?;
    lines.push((audio_off, "audio.bin".into()));
    lines.sort();
    if lines.len() != 4 {
        bail!("expected 4 flash parts, got {lines:?}");
    }
    let txt: String = lines.iter().map(|(a, f)| format!("0x{a:x} {f}\n")).collect();
    std::fs::write(dist.join("flash.txt"), &txt)?;
    print!("dist/:\n{txt}");
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let port = arg(&args, "--port");
    match args.first().map(String::as_str) {
        Some("cities") => cities::generate(),
        Some("audio") => audio::build().map(|_| ()),
        Some("flash-audio") => flash_audio(port.as_deref()),
        Some("release") => release(),
        Some("flash") => {
            audio::build()?;
            let release = !flag(&args, "--debug");
            // App first (espflash flashes partition table + app), then audio, then monitor.
            let elf = build_firmware(release)?;
            let mut f = Command::new("espflash");
            f.args(["flash", "--partition-table"])
                .arg(root().join("firmware/partitions.csv"))
                .args(["--flash-size", "8mb"]);
            if let Some(p) = &port {
                f.args(["--port", p]);
            }
            f.arg(&elf);
            run(&mut f)?;
            flash_audio(port.as_deref())?;
            if !flag(&args, "--no-monitor") {
                let mut m = Command::new("espflash");
                m.arg("monitor");
                if let Some(p) = &port {
                    m.args(["--port", p]);
                }
                run(&mut m)?;
            }
            Ok(())
        }
        Some("status") => serial::send(port.as_deref(), "status"),
        Some("sync") => serial::send(port.as_deref(), "sync"),
        Some("key") => {
            let rest = args[1..].join(" ");
            serial::send(port.as_deref(), &format!("key {rest}"))
        }
        Some("setup-ap") => serial::send(port.as_deref(), "setup"),
        Some("log") => {
            let secs = arg(&args, "--secs").and_then(|s| s.parse().ok()).unwrap_or(20);
            serial::log(port.as_deref(), secs, flag(&args, "--reset"), arg(&args, "--send").as_deref())
        }
        Some("monitor") => {
            let mut m = Command::new("espflash");
            m.arg("monitor");
            if let Some(p) = &port {
                m.args(["--port", p]);
            }
            run(&mut m)
        }
        Some("setup") => {
            let ssid = arg(&args, "--ssid").context("--ssid required")?;
            let pass = arg(&args, "--pass").unwrap_or_default();
            let mut line = format!("wifi {}\t{}", ssid, pass);
            if let Some(city) = arg(&args, "--city") {
                line.push_str(&format!("\ncity {city}"));
            }
            serial::send(port.as_deref(), &line)
        }
        Some("reset") => {
            serial::send(port.as_deref(), "reset")?;
            if flag(&args, "--all") {
                let (offset, size) = audio_partition()?;
                let mut c = Command::new("espflash");
                c.arg("erase-region");
                if let Some(p) = &port {
                    c.args(["--port", p]);
                }
                c.arg(format!("0x{offset:x}")).arg(format!("0x{size:x}"));
                run(&mut c)?;
                println!("audio erased; run `cargo xtask flash-audio` to restore it");
            }
            Ok(())
        }
        _ => {
            eprintln!("{}", include_str!("main.rs").lines().take(14).collect::<Vec<_>>().join("\n"));
            Ok(())
        }
    }
}
