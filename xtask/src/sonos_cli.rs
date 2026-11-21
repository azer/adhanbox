//! `cargo xtask sonos …`: drive the firmware's Sonos module from this computer, for testing
//! against real speakers without flashing.
use crate::sonos::{self, Poll, Session};
use anyhow::{bail, Context, Result};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Stderr;
impl log::Log for Stderr {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        eprintln!("[{}] {}", r.level(), r.args());
    }
    fn flush(&self) {}
}

pub fn run(args: &[String]) -> Result<()> {
    let _ = log::set_logger(&Stderr).map(|_| log::set_max_level(log::LevelFilter::Debug));
    let t = Instant::now();
    let speakers = sonos::discover(&[])?;
    println!("found {} speakers in {:.2}s", speakers.len(), t.elapsed().as_secs_f32());
    match args.first().map(String::as_str) {
        Some("play") => {
            let want = args.get(1).context("room name(s) required")?.to_lowercase();
            let slot: u8 = crate::arg(args, "--slot").and_then(|s| s.parse().ok()).unwrap_or(1);
            let step: u8 = crate::arg(args, "--step").and_then(|s| s.parse().ok()).unwrap_or(2);
            let secs: u64 = crate::arg(args, "--secs").and_then(|s| s.parse().ok()).unwrap_or(8);
            let chosen: Vec<_> = want.split(',').filter_map(|w| speakers.iter().find(|s| s.name.to_lowercase() == w.trim()).cloned()).collect();
            if chosen.is_empty() {
                bail!("no such room");
            }
            let files = ["sabah", "ogle", "ikindi", "aksam", "yatsi"];
            let source: sonos::Source = Arc::new(move |slot| {
                let p = crate::root().join(format!("target/audio/{}.mp3", files.get(slot as usize)?));
                Some(&*Box::leak(std::fs::read(p).ok()?.into_boxed_slice()))
            });
            let server = sonos::serve(source)?;
            let url = sonos::adhan_url(sonos::local_ip(chosen[0].ip)?, slot);
            let t = Instant::now();
            let mut s = Session::start(&chosen, &url, sonos::step_volume(step))?;
            println!("+{:.2}s started on {:?} at volume {}: {url}", t.elapsed().as_secs_f32(), s.names(), sonos::step_volume(step));
            let mut last = None;
            while t.elapsed() < Duration::from_secs(secs) {
                let p = s.poll();
                if last != Some(p) {
                    println!("+{:.2}s {p:?} (served {} bytes)", t.elapsed().as_secs_f32(), server.sent.load(std::sync::atomic::Ordering::Relaxed));
                    last = Some(p);
                }
                if p == Poll::Stopped {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            let t = Instant::now();
            s.finish();
            println!("restored in {:.2}s", t.elapsed().as_secs_f32());
            Ok(())
        }
        _ => {
            for s in &speakers {
                println!("{:<16} {:<15} fixed={} group={} buddy={:?}", s.name, s.ip, sonos::output_fixed(s.ip), s.coordinator == s.id, s.buddy.is_some());
            }
            Ok(())
        }
    }
}
