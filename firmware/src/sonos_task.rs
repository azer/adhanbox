//! The Sonos worker thread. All network calls to speakers happen here, so the main loop (clock,
//! buttons, the box's own speaker) never waits on the network. The main loop owns every
//! deadline: it only reads `state()` and decides when to give up and play locally.
use crate::sonos::{self, Poll, Session, Speaker};
use crate::store::Room;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Idle = 0,
    /// Looking for the ticked speakers.
    Preparing = 1,
    /// Ticked speakers found; `play` may be called.
    Ready = 2,
    /// None of the ticked speakers could be found.
    NotReady = 3,
    /// Play requested; not confirmed audible yet.
    Starting = 4,
    /// A speaker confirmed it is playing our audio and the position is advancing.
    Playing = 5,
    /// Never became audible.
    Failed = 6,
    /// Was audible, then stopped early without being asked.
    Lost = 7,
    /// Played to the end, or was stopped on request.
    Ended = 8,
}

enum Msg {
    Prepare(Vec<Room>, Ipv4Addr),
    Play { slot: u8, volume: u8 },
    Volume(u8),
    Release,
}

/// Counts as "played to the end" when it stops within this many seconds of the end.
const END_SLACK_S: u32 = 15;
/// Confirmed playback with no progress for this long is lost.
const STALL: Duration = Duration::from_secs(6);
/// Unconfirmed for this long is a failure (the main loop gives up earlier, at its own deadline).
const START_LIMIT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Player {
    tx: Sender<Msg>,
    state: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}

impl Player {
    pub fn start(source: sonos::Source) -> Self {
        let (tx, rx) = channel();
        let state = Arc::new(AtomicU8::new(State::Idle as u8));
        let stop = Arc::new(AtomicBool::new(false));
        let busy = Arc::new(AtomicBool::new(false));
        let w = Worker { rx, state: state.clone(), stop: stop.clone(), busy: busy.clone(), source, server: None, rooms: Vec::new(), speakers: Vec::new(), local: Ipv4Addr::UNSPECIFIED };
        std::thread::Builder::new().name("sonos".into()).stack_size(16 * 1024).spawn(move || w.run()).expect("sonos thread");
        Self { tx, state, stop, busy }
    }

    pub fn state(&self) -> State {
        match self.state.load(Ordering::SeqCst) {
            1 => State::Preparing,
            2 => State::Ready,
            3 => State::NotReady,
            4 => State::Starting,
            5 => State::Playing,
            6 => State::Failed,
            7 => State::Lost,
            8 => State::Ended,
            _ => State::Idle,
        }
    }

    /// A request is queued or running, or speakers are still being put back.
    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    fn send(&self, m: Msg) {
        self.busy.store(true, Ordering::SeqCst);
        let _ = self.tx.send(m);
    }

    /// Find the ticked speakers and start the file server. Ends in `Ready` or `NotReady`.
    /// `local` is the box's own address, which the speakers fetch the recording from.
    pub fn prepare(&self, rooms: &[Room], local: Ipv4Addr) {
        self.state.store(State::Preparing as u8, Ordering::SeqCst);
        self.send(Msg::Prepare(rooms.to_vec(), local));
    }

    /// Play the adhan in `slot` at Sonos volume `volume` (0..=100). Needs `Ready`.
    pub fn play(&self, slot: u8, volume: u8) {
        self.stop.store(false, Ordering::SeqCst);
        self.state.store(State::Starting as u8, Ordering::SeqCst);
        self.send(Msg::Play { slot, volume });
    }

    /// Change the volume of what is playing (volume screen).
    pub fn volume(&self, volume: u8) {
        self.send(Msg::Volume(volume));
    }

    /// Stop playback and put the speakers back. Returns at once; watch `busy()`.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Stop the file server and forget the speakers (before Wi-Fi goes off).
    pub fn release(&self) {
        self.stop();
        self.send(Msg::Release);
    }

    /// Block until the worker has nothing left to do (bounded).
    pub fn wait_idle(&self, limit: Duration) -> bool {
        let t = Instant::now();
        while self.busy() {
            if t.elapsed() > limit {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }
}

struct Worker {
    rx: Receiver<Msg>,
    state: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    source: sonos::Source,
    server: Option<sonos::Server>,
    rooms: Vec<Room>,
    speakers: Vec<Speaker>,
    local: Ipv4Addr,
}

impl Worker {
    fn set(&self, s: State) {
        self.state.store(s as u8, Ordering::SeqCst);
    }

    fn run(mut self) {
        loop {
            let msg = match self.rx.try_recv() {
                Ok(m) => m,
                Err(_) => {
                    // Queue empty: nothing left to do.
                    self.busy.store(false, Ordering::SeqCst);
                    let Ok(m) = self.rx.recv() else { return };
                    self.busy.store(true, Ordering::SeqCst);
                    m
                }
            };
            match msg {
                Msg::Prepare(rooms, local) => {
                    self.rooms = rooms;
                    self.local = local;
                    let ok = self.find();
                    // A later `play` may already have moved the state on.
                    if self.state.load(Ordering::SeqCst) == State::Preparing as u8 {
                        self.set(if ok { State::Ready } else { State::NotReady });
                    }
                }
                Msg::Play { slot, volume } => {
                    let end = self.play(slot, volume);
                    self.set(end);
                }
                Msg::Volume(_) => {}
                Msg::Release => {
                    self.server = None;
                    self.speakers.clear();
                    self.set(State::Idle);
                }
            }
        }
    }

    /// Locate the ticked speakers (by id; addresses are only hints) and start serving.
    fn find(&mut self) -> bool {
        if self.server.is_none() {
            match sonos::serve(self.source.clone()) {
                Ok(s) => self.server = Some(s),
                Err(e) => {
                    log::error!("sonos: file server: {e:#}");
                    return false;
                }
            }
        }
        let mut known: Vec<Ipv4Addr> = self.speakers.iter().map(|s| s.ip).collect();
        known.extend(self.rooms.iter().map(|r| r.ip));
        known.dedup();
        let t = Instant::now();
        self.speakers = match sonos::discover(&known) {
            Ok(all) => all.into_iter().filter(|s| self.rooms.iter().any(|r| r.id == s.id)).collect(),
            Err(e) => {
                log::warn!("sonos: discovery failed: {e:#}");
                Vec::new()
            }
        };
        log::info!("sonos: {} of {} ticked speakers found in {} ms", self.speakers.len(), self.rooms.len(), t.elapsed().as_millis());
        !self.speakers.is_empty()
    }

    fn play(&mut self, slot: u8, volume: u8) -> State {
        if self.stop.load(Ordering::SeqCst) {
            return State::Ended;
        }
        // Fresh group information: it may have changed since `prepare`.
        if !self.find() {
            return State::Failed;
        }
        if self.local.is_unspecified() {
            log::warn!("sonos: the box has no network address");
            return State::Failed;
        }
        let url = sonos::adhan_url(self.local, slot);
        let t0 = Instant::now();
        let mut session = match Session::start(&self.speakers, &url, volume) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("sonos: start failed: {e:#}");
                return State::Failed;
            }
        };
        log::info!("sonos: play sent to {:?} at volume {volume} after {} ms", session.names(), t0.elapsed().as_millis());
        let mut confirmed = false;
        let mut progress = (0u32, Instant::now());
        let end = loop {
            if self.stop.load(Ordering::SeqCst) {
                break State::Ended;
            }
            while let Ok(m) = self.rx.try_recv() {
                match m {
                    Msg::Volume(v) => session.set_volume(v),
                    Msg::Release => self.stop.store(true, Ordering::SeqCst),
                    _ => {}
                }
            }
            match session.poll() {
                Poll::Playing { rel, .. } => {
                    if !confirmed {
                        confirmed = true;
                        log::info!("sonos: confirmed playing after {} ms", t0.elapsed().as_millis());
                        self.set(State::Playing);
                    }
                    if rel > progress.0 {
                        progress = (rel, Instant::now());
                    }
                }
                Poll::Stopped if confirmed => {
                    break if session.duration > 0 && session.played() + END_SLACK_S >= session.duration { State::Ended } else { State::Lost };
                }
                // Right after Play the speaker may still report the old source.
                Poll::Stopped if t0.elapsed() > Duration::from_millis(1500) => break State::Failed,
                _ => {}
            }
            if confirmed && progress.1.elapsed() > STALL {
                break if session.duration > 0 && session.played() + END_SLACK_S >= session.duration { State::Ended } else { State::Lost };
            }
            if !confirmed && t0.elapsed() > START_LIMIT {
                break State::Failed;
            }
            // Poll fast until confirmed, then once a second; stay responsive to `stop`.
            let nap = if confirmed { 1000 } else { 250 };
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(nap) && !self.stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        log::info!("sonos: {end:?} after {} s of audio, {} bytes served", session.played(), self.server.as_ref().map(|s| s.sent.load(Ordering::Relaxed)).unwrap_or(0));
        session.finish();
        end
    }
}
