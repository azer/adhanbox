//! Adhan pack: trim + loudness-normalize + 22.05 kHz mono 32 kbps MP3 per prayer,
//! concatenated with a small index. Uses ffmpeg for the transcoding.
//!
//! Format (little-endian):
//!   "EZAN" | u32 version=1 | u32 count | count × { u8 slot, 3×pad, u32 offset, u32 len }
//!   then the MP3 payloads. Offsets are from the start of the pack.
//! Slots: 0 Fajr, 1 Dhuhr, 2 Asr, 3 Maghrib, 4 Isha (same as the adhan bit mask).
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

const FILES: [(&str, u8); 5] =
    [("sabah", 0), ("ogle", 1), ("ikindi", 2), ("aksam", 3), ("yatsi", 4)];

fn encode(src: &PathBuf, dst: &PathBuf) -> Result<()> {
    let filter = concat!(
        "silenceremove=start_periods=1:start_threshold=-45dB:start_silence=0.2,",
        "areverse,silenceremove=start_periods=1:start_threshold=-45dB:start_silence=0.5,areverse,",
        "loudnorm=I=-16:TP=-1.5:LRA=11,",
        "afade=t=in:d=0.05"
    );
    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        .args(["-af", filter, "-ac", "1", "-ar", "22050", "-c:a", "libmp3lame", "-b:a", "32k"])
        .args(["-map_metadata", "-1", "-id3v2_version", "0", "-write_xing", "0"])
        .arg(dst)
        .status()
        .context("running ffmpeg (brew install ffmpeg)")?;
    if !st.success() {
        bail!("ffmpeg failed on {}", src.display());
    }
    Ok(())
}

pub fn build() -> Result<PathBuf> {
    let root = crate::root();
    let dir = root.join("assets/ezan");
    let tmp = root.join("target/audio");
    std::fs::create_dir_all(&tmp)?;
    let mut entries = Vec::new();
    for (name, slot) in FILES {
        let src = dir.join(format!("{name}.mp3"));
        if !src.exists() {
            println!("  (skip) {name}.mp3 not found");
            continue;
        }
        let dst = tmp.join(format!("{name}.mp3"));
        let fresh = dst.exists()
            && dst.metadata()?.modified()? >= src.metadata()?.modified()?;
        if !fresh {
            encode(&src, &dst)?;
        }
        let data = std::fs::read(&dst)?;
        println!("  {name:<7} {:>6} KB", data.len() / 1024);
        entries.push((slot, data));
    }
    if entries.is_empty() {
        bail!("no adhan files in assets/ezan/");
    }
    let header = 12 + entries.len() * 12;
    let mut out = Vec::new();
    out.extend_from_slice(b"EZAN");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    let mut off = header;
    for (slot, data) in &entries {
        out.extend_from_slice(&[*slot, 0, 0, 0]);
        out.extend_from_slice(&(off as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        off += data.len();
    }
    for (_, data) in &entries {
        out.extend_from_slice(data);
    }
    let (_, part_size) = crate::audio_partition()?;
    if out.len() as u32 > part_size {
        bail!(
            "audio pack is {} KB but the audio partition is {} KB; shorten or drop a file",
            out.len() / 1024,
            part_size / 1024
        );
    }
    let path = root.join("target/audio.bin");
    std::fs::write(&path, &out)?;
    println!("audio.bin: {} KB of {} KB partition", out.len() / 1024, part_size / 1024);
    Ok(path)
}
