//! Sonos playback over the local UPnP API (no cloud, no login).
//! Design and test notes: docs/proposal-sonos-fajr-ramadan.md.
//!
//! Only `std`, `log` and `anyhow` are used, so `cargo xtask sonos …` runs this same file on the
//! computer against real speakers.
//!
//! Rules this module keeps:
//!   * A 200 from `Play` proves nothing. Playback counts only when the speaker reports PLAYING on
//!     our URL with the position advancing.
//!   * A speaker never plays at a volume we did not set and read back.
//!   * The old volume comes back only after the speaker has confirmed it stopped.
use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(2);
pub const SERVE_PORT: u16 = 8080;
/// Sonos volume (0..=100) per step 1..=6. The last entry is the hard ceiling.
pub const VOLUME_STEPS: [u8; 7] = [0, 4, 7, 10, 14, 18, 23];
pub const VOLUME_CEILING: u8 = 23;

pub fn step_volume(step: u8) -> u8 {
    VOLUME_STEPS[(step as usize).min(6)].min(VOLUME_CEILING)
}

// ───────────────────────────── XML / HTTP helpers ─────────────────────────────

pub fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Text of the first `<name>…</name>`, unescaped once.
pub fn tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let a = xml.find(&open)? + open.len();
    let b = xml[a..].find(&format!("</{name}>"))? + a;
    Some(unescape(&xml[a..b]))
}

/// Value of `name="…"` inside one element's attribute text.
pub fn attr<'a>(el: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!(" {name}=\"");
    let a = el.find(&pat)? + pat.len();
    let b = el[a..].find('"')? + a;
    Some(&el[a..b])
}

/// "H:MM:SS" → seconds.
pub fn hms(s: &str) -> Option<u32> {
    let mut it = s.split(':');
    let (h, m, sec) = (it.next()?, it.next()?, it.next()?);
    let sec = sec.split('.').next()?;
    Some(h.parse::<u32>().ok()? * 3600 + m.parse::<u32>().ok()? * 60 + sec.parse::<u32>().ok()?)
}

fn http_post(ip: Ipv4Addr, path: &str, headers: &str, body: &str) -> Result<(u16, String)> {
    let addr = SocketAddr::new(IpAddr::V4(ip), 1400);
    let mut s = TcpStream::connect_timeout(&addr, TIMEOUT).context("connect")?;
    s.set_read_timeout(Some(TIMEOUT))?;
    s.set_write_timeout(Some(TIMEOUT))?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {ip}:1400\r\nConnection: close\r\nContent-Type: text/xml; charset=\"utf-8\"\r\n{headers}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes())?;
    let mut out = Vec::with_capacity(1024);
    let mut buf = [0u8; 1024];
    let start = Instant::now();
    loop {
        let n = s.read(&mut buf).context("read")?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() > 256 * 1024 || start.elapsed() > TIMEOUT * 2 {
            bail!("response too large or too slow");
        }
        // Stop at Content-Length rather than waiting for the close.
        if let Some(h) = find(&out, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&out[..h]).to_ascii_lowercase();
            if let Some(len) = head.split("content-length:").nth(1).and_then(|v| v.lines().next()).and_then(|v| v.trim().parse::<usize>().ok()) {
                if out.len() >= h + 4 + len {
                    break;
                }
            }
        }
    }
    let text = String::from_utf8_lossy(&out).into_owned();
    let status = text.split(' ').nth(1).and_then(|c| c.parse().ok()).context("bad HTTP status")?;
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    Ok((status, body))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[derive(Clone, Copy)]
enum Svc {
    Transport,
    Rendering,
    Topology,
}

fn soap(ip: Ipv4Addr, svc: Svc, action: &str, args: &str) -> Result<String> {
    let (path, urn) = match svc {
        Svc::Transport => ("/MediaRenderer/AVTransport/Control", "urn:schemas-upnp-org:service:AVTransport:1"),
        Svc::Rendering => ("/MediaRenderer/RenderingControl/Control", "urn:schemas-upnp-org:service:RenderingControl:1"),
        Svc::Topology => ("/ZoneGroupTopology/Control", "urn:schemas-upnp-org:service:ZoneGroupTopology:1"),
    };
    let inst = if matches!(svc, Svc::Topology) { "" } else { "<InstanceID>0</InstanceID>" };
    let body = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{urn}\">{inst}{args}</u:{action}></s:Body></s:Envelope>"
    );
    let (status, body) = http_post(ip, path, &format!("SOAPACTION: \"{urn}#{action}\"\r\n"), &body).with_context(|| format!("{action} @ {ip}"))?;
    if status != 200 {
        bail!("{action} @ {ip}: HTTP {status} {}", tag(&body, "errorCode").unwrap_or_default());
    }
    Ok(body)
}

// ───────────────────────────── discovery ─────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Speaker {
    /// "RINCON_…": stable across address changes.
    pub id: String,
    pub name: String,
    pub ip: Ipv4Addr,
    /// The coordinator of the group this speaker is in right now (its own id when alone).
    pub coordinator: String,
    /// Another member of that group, if any (used to rejoin it afterwards).
    pub buddy: Option<String>,
}

/// Parse `GetZoneGroupState`. Only visible players (no surrounds, subs or stereo-pair seconds).
pub fn parse_topology(soap_body: &str) -> Vec<Speaker> {
    let state = tag(soap_body, "ZoneGroupState").unwrap_or_default();
    let mut out = Vec::new();
    for group in state.split("<ZoneGroup ").skip(1) {
        let group = group.split("</ZoneGroup>").next().unwrap_or(group);
        let Some(coord) = attr(&format!(" {group}"), "Coordinator").map(str::to_string) else { continue };
        let mut members = Vec::new();
        for m in group.split("<ZoneGroupMember").skip(1) {
            let el = m.split('>').next().unwrap_or(m);
            if attr(el, "Invisible") == Some("1") {
                continue;
            }
            let (Some(id), Some(name), Some(loc)) = (attr(el, "UUID"), attr(el, "ZoneName"), attr(el, "Location")) else { continue };
            let Some(ip) = loc.strip_prefix("http://").and_then(|l| l.split(':').next()).and_then(|h| h.parse().ok()) else { continue };
            members.push((id.to_string(), unescape(name), ip));
        }
        for (id, name, ip) in &members {
            let buddy = if *id != coord && members.iter().any(|m| m.0 == coord) {
                Some(coord.clone())
            } else {
                members.iter().find(|m| m.0 != *id).map(|m| m.0.clone())
            };
            out.push(Speaker { id: id.clone(), name: name.clone(), ip: *ip, coordinator: coord.clone(), buddy });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
    out
}

pub fn topology(ip: Ipv4Addr) -> Result<Vec<Speaker>> {
    let v = parse_topology(&soap(ip, Svc::Topology, "GetZoneGroupState", "")?);
    if v.is_empty() {
        bail!("no speakers in topology from {ip}");
    }
    Ok(v)
}

/// SSDP search: the address of any one Sonos player.
fn ssdp() -> Result<Ipv4Addr> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_read_timeout(Some(Duration::from_millis(300)))?;
    let msg = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: urn:schemas-upnp-org:device:ZonePlayer:1\r\n\r\n";
    let mut buf = [0u8; 1024];
    for _ in 0..4 {
        // Multicast first; some routers only pass broadcast.
        let _ = sock.send_to(msg.as_bytes(), "239.255.255.250:1900");
        let _ = sock.set_broadcast(true).and_then(|_| sock.send_to(msg.as_bytes(), "255.255.255.255:1900"));
        let until = Instant::now() + Duration::from_millis(1200);
        while Instant::now() < until {
            if let Ok((n, SocketAddr::V4(from))) = sock.recv_from(&mut buf) {
                if String::from_utf8_lossy(&buf[..n]).contains("Sonos") {
                    return Ok(*from.ip());
                }
            }
        }
    }
    bail!("no Sonos speaker answered the search")
}

/// All speakers on the network. `known` addresses (from last time) are asked first, which is
/// faster and does not depend on multicast; the search is the fallback.
pub fn discover(known: &[Ipv4Addr]) -> Result<Vec<Speaker>> {
    for ip in known {
        match topology(*ip) {
            Ok(v) => return Ok(v),
            Err(e) => log::warn!("sonos: {ip} did not answer: {e:#}"),
        }
    }
    topology(ssdp()?)
}

// ───────────────────────────── single-speaker calls ─────────────────────────────

fn volume(ip: Ipv4Addr) -> Result<u8> {
    let x = soap(ip, Svc::Rendering, "GetVolume", "<Channel>Master</Channel>")?;
    tag(&x, "CurrentVolume").and_then(|v| v.parse().ok()).context("no volume in reply")
}

fn set_volume(ip: Ipv4Addr, v: u8) -> Result<()> {
    soap(ip, Svc::Rendering, "SetVolume", &format!("<Channel>Master</Channel><DesiredVolume>{v}</DesiredVolume>")).map(|_| ())
}

fn mute(ip: Ipv4Addr) -> Result<bool> {
    Ok(tag(&soap(ip, Svc::Rendering, "GetMute", "<Channel>Master</Channel>")?, "CurrentMute").as_deref() == Some("1"))
}

fn set_mute(ip: Ipv4Addr, m: bool) -> Result<()> {
    soap(ip, Svc::Rendering, "SetMute", &format!("<Channel>Master</Channel><DesiredMute>{}</DesiredMute>", m as u8)).map(|_| ())
}

/// Line-out players (Port, Connect) can be set to a fixed level that ignores volume commands.
pub fn output_fixed(ip: Ipv4Addr) -> bool {
    soap(ip, Svc::Rendering, "GetOutputFixed", "").ok().and_then(|x| tag(&x, "CurrentFixed")).as_deref() == Some("1")
}

fn transport_state(ip: Ipv4Addr) -> Result<String> {
    tag(&soap(ip, Svc::Transport, "GetTransportInfo", "")?, "CurrentTransportState").context("no transport state")
}

struct Position {
    uri: String,
    rel: u32,
    duration: u32,
    track: u32,
}

fn position(ip: Ipv4Addr) -> Result<Position> {
    let x = soap(ip, Svc::Transport, "GetPositionInfo", "")?;
    Ok(Position {
        uri: tag(&x, "TrackURI").unwrap_or_default(),
        rel: tag(&x, "RelTime").and_then(|t| hms(&t)).unwrap_or(0),
        duration: tag(&x, "TrackDuration").and_then(|t| hms(&t)).unwrap_or(0),
        track: tag(&x, "Track").and_then(|t| t.parse().ok()).unwrap_or(0),
    })
}

fn set_uri(ip: Ipv4Addr, uri: &str, meta: &str) -> Result<()> {
    soap(
        ip,
        Svc::Transport,
        "SetAVTransportURI",
        &format!("<CurrentURI>{}</CurrentURI><CurrentURIMetaData>{}</CurrentURIMetaData>", escape(uri), escape(meta)),
    )
    .map(|_| ())
}

fn play(ip: Ipv4Addr) -> Result<()> {
    soap(ip, Svc::Transport, "Play", "<Speed>1</Speed>").map(|_| ())
}

fn stop(ip: Ipv4Addr) -> Result<()> {
    soap(ip, Svc::Transport, "Stop", "").map(|_| ())
}

fn standalone(ip: Ipv4Addr) -> Result<()> {
    soap(ip, Svc::Transport, "BecomeCoordinatorOfStandaloneGroup", "").map(|_| ())
}

fn seek(ip: Ipv4Addr, unit: &str, target: &str) -> Result<()> {
    soap(ip, Svc::Transport, "Seek", &format!("<Unit>{unit}</Unit><Target>{target}</Target>")).map(|_| ())
}

// ───────────────────────────── a playback session ─────────────────────────────

/// What a speaker was doing before we took it over.
struct Before {
    volume: u8,
    muted: bool,
    /// Was in a group with other speakers: rejoin via this member afterwards.
    buddy: Option<String>,
    uri: String,
    meta: String,
    track: u32,
    rel: u32,
    seekable: bool,
    playing: bool,
}

struct Member {
    sp: Speaker,
    before: Before,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Poll {
    /// Not (yet) audible: still loading, or not confirmed.
    Pending,
    /// PLAYING on our URL with the position advancing. `rel`/`duration` in seconds.
    Playing { rel: u32, duration: u32 },
    /// The speaker is no longer playing our URL.
    Stopped,
}

pub struct Session {
    members: Vec<Member>,
    url: String,
    last_rel: u32,
    pub duration: u32,
}

impl Session {
    /// Take over `speakers` and start `url` at `volume` (0..=100, clamped to the ceiling).
    /// Speakers that cannot be put at exactly that volume are left alone. Fails if none remain
    /// or the play command is rejected; anything already changed is put back first.
    pub fn start(speakers: &[Speaker], url: &str, volume: u8) -> Result<Session> {
        let vol = volume.min(VOLUME_CEILING);
        let mut s = Session { members: Vec::new(), url: url.to_string(), last_rel: 0, duration: 0 };
        for sp in speakers {
            match prepare(sp, vol) {
                Ok(before) => s.members.push(Member { sp: sp.clone(), before }),
                Err(e) => log::warn!("sonos: skipping {}: {e:#}", sp.name),
            }
        }
        if s.members.is_empty() {
            bail!("no speaker could be prepared");
        }
        let r = (|| -> Result<()> {
            let lead = s.members[0].sp.clone();
            for m in &s.members[1..] {
                if let Err(e) = set_uri(m.sp.ip, &format!("x-rincon:{}", lead.id), "") {
                    log::warn!("sonos: {} did not join: {e:#}", m.sp.name);
                }
            }
            set_uri(lead.ip, url, "")?;
            play(lead.ip)
        })();
        if let Err(e) = r {
            s.finish();
            return Err(e);
        }
        Ok(s)
    }

    pub fn names(&self) -> Vec<String> {
        self.members.iter().map(|m| m.sp.name.clone()).collect()
    }

    /// Ask the lead speaker what it is doing. Network errors count as `Pending`, so a speaker
    /// that vanishes never looks like it is playing.
    pub fn poll(&mut self) -> Poll {
        let ip = self.members[0].sp.ip;
        let (Ok(state), Ok(pos)) = (transport_state(ip), position(ip)) else { return Poll::Pending };
        if pos.uri != self.url {
            return Poll::Stopped;
        }
        if pos.duration > 0 {
            self.duration = pos.duration;
        }
        match state.as_str() {
            "PLAYING" if pos.rel >= 1 && pos.rel >= self.last_rel => {
                self.last_rel = pos.rel;
                Poll::Playing { rel: pos.rel, duration: pos.duration }
            }
            "PLAYING" | "TRANSITIONING" => Poll::Pending,
            _ => Poll::Stopped,
        }
    }

    /// Change the volume on every speaker in the session (never above the ceiling).
    pub fn set_volume(&mut self, volume: u8) {
        let v = volume.min(VOLUME_CEILING);
        for m in &self.members {
            if let Err(e) = set_volume(m.sp.ip, v) {
                log::warn!("sonos: {}: volume: {e:#}", m.sp.name);
            }
        }
    }

    /// Seconds of our audio the speaker has reported playing.
    pub fn played(&self) -> u32 {
        self.last_rel
    }

    /// Stop, then put every speaker back: group, source, volume, mute, and resume what was playing.
    /// The old volume is only restored once the speaker confirms it is not playing our audio.
    pub fn finish(self) {
        let lead = self.members[0].sp.ip;
        let _ = stop(lead);
        let mut silent = false;
        for _ in 0..8 {
            match (transport_state(lead), position(lead)) {
                (Ok(st), Ok(p)) if st != "PLAYING" && st != "TRANSITIONING" || p.uri != self.url => {
                    silent = true;
                    break;
                }
                (Ok(_), Ok(_)) => {
                    let _ = stop(lead);
                }
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        if !silent {
            log::warn!("sonos: could not confirm stop; leaving volumes at the quiet level");
        }
        // Followers leave our group first, then everyone goes back to what they had.
        for m in self.members.iter().skip(1) {
            let _ = standalone(m.sp.ip);
        }
        let now = topology(lead).unwrap_or_default();
        let ours: Vec<String> = self.members.iter().map(|m| m.sp.id.clone()).collect();
        // Speakers that led a group (or were alone) get their source back first, so the ones
        // that followed them have something to rejoin.
        for led in [true, false] {
            for m in self.members.iter().filter(|m| (m.sp.coordinator == m.sp.id) == led) {
                restore(m, &now, &ours, silent);
            }
        }
    }
}

/// Snapshot one speaker, detach it from any group, and put it at exactly `vol`, unmuted.
fn prepare(sp: &Speaker, vol: u8) -> Result<Before> {
    if output_fixed(sp.ip) {
        bail!("fixed line-out volume");
    }
    let old_volume = volume(sp.ip)?;
    let muted = mute(sp.ip)?;
    let grouped = sp.buddy.is_some();
    let mut b = Before { volume: old_volume, muted, buddy: sp.buddy.clone(), uri: String::new(), meta: String::new(), track: 0, rel: 0, seekable: false, playing: false };
    if !grouped || sp.coordinator == sp.id {
        let state = transport_state(sp.ip)?;
        let media = soap(sp.ip, Svc::Transport, "GetMediaInfo", "")?;
        let pos = position(sp.ip)?;
        b.uri = tag(&media, "CurrentURI").unwrap_or_default();
        b.meta = tag(&media, "CurrentURIMetaData").unwrap_or_default();
        b.track = pos.track;
        b.rel = pos.rel;
        b.seekable = pos.duration > 0;
        b.playing = state == "PLAYING" || state == "TRANSITIONING";
    }
    // Quiet first, so nothing that is playing now (TV, music) carries on loud.
    set_volume(sp.ip, vol)?;
    let got = volume(sp.ip)?;
    if got != vol {
        let _ = set_volume(sp.ip, old_volume.min(got));
        bail!("volume read back {got}, wanted {vol}");
    }
    let r = (|| -> Result<()> {
        if grouped {
            standalone(sp.ip)?;
        }
        if muted {
            set_mute(sp.ip, false)?;
        }
        // Volume again after any regrouping: this is the value Play will use.
        let got = volume(sp.ip)?;
        if got != vol {
            bail!("volume changed to {got} before play");
        }
        Ok(())
    })();
    if let Err(e) = r {
        let now = topology(sp.ip).unwrap_or_default();
        restore(&Member { sp: sp.clone(), before: b }, &now, &[], true);
        return Err(e);
    }
    Ok(b)
}

/// `ours`: ids of every speaker in the session being undone.
fn restore(m: &Member, now: &[Speaker], ours: &[String], silent: bool) {
    let (ip, b) = (m.sp.ip, &m.before);
    let name = &m.sp.name;
    let step = |what: &str, r: Result<()>| {
        if let Err(e) = r {
            log::warn!("sonos: {name}: restore {what}: {e:#}");
        }
    };
    // Back into its old group: follow the buddy's current coordinator.
    // A speaker that led its group rejoins only if the group carried on without it (under a
    // speaker we did not touch); otherwise it leads again and gets its own source back.
    let led = m.sp.coordinator == m.sp.id;
    let rejoin = b.buddy.as_ref().and_then(|buddy| now.iter().find(|s| s.id == *buddy)).map(|s| s.coordinator.clone());
    if let Some(coord) = rejoin.filter(|c| *c != m.sp.id && !(led && ours.contains(c))) {
        if silent {
            step("volume", set_volume(ip, b.volume));
            step("mute", set_mute(ip, b.muted));
        }
        step("group", set_uri(ip, &format!("x-rincon:{coord}"), ""));
        return;
    }
    if !b.uri.is_empty() {
        step("source", set_uri(ip, &b.uri, &b.meta));
        if b.uri.starts_with("x-rincon-queue:") && b.track > 0 {
            step("track", seek(ip, "TRACK_NR", &b.track.to_string()));
        }
        if b.seekable && b.rel > 0 {
            step("position", seek(ip, "REL_TIME", &format!("{}:{:02}:{:02}", b.rel / 3600, b.rel / 60 % 60, b.rel % 60)));
        }
    }
    if silent {
        step("volume", set_volume(ip, b.volume));
        step("mute", set_mute(ip, b.muted));
        if b.playing && !b.uri.is_empty() {
            step("resume", play(ip));
        }
    }
}

// ───────────────────────────── file server ─────────────────────────────

/// Our address on the network that reaches `peer` (computer only: lwIP reports 0.0.0.0 here,
/// so the firmware asks its network interface instead).
#[allow(dead_code)]
pub fn local_ip(peer: Ipv4Addr) -> Result<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0")?;
    s.connect((peer, 1400))?;
    match s.local_addr()? {
        SocketAddr::V4(a) if !a.ip().is_unspecified() => Ok(*a.ip()),
        a => bail!("no local IPv4 address ({a})"),
    }
}

pub fn adhan_url(local: Ipv4Addr, slot: u8) -> String {
    format!("http://{local}:{SERVE_PORT}/adhan/{slot}.mp3")
}

/// `Range: bytes=a-` / `bytes=a-b` → inclusive (start, end) within `len`.
pub fn parse_range(v: &str, len: usize) -> Option<(usize, usize)> {
    let v = v.trim().strip_prefix("bytes=")?;
    let (a, b) = v.split_once('-')?;
    let start: usize = a.trim().parse().ok()?;
    let end = if b.trim().is_empty() { len.checked_sub(1)? } else { b.trim().parse::<usize>().ok()?.min(len.checked_sub(1)?) };
    (start <= end).then_some((start, end))
}

/// Serves `/adhan/<slot>.mp3` to the speakers for as long as it is alive.
pub struct Server {
    stop: Arc<AtomicBool>,
    pub sent: Arc<AtomicU32>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub type Source = Arc<dyn Fn(u8) -> Option<&'static [u8]> + Send + Sync>;

pub fn serve(source: Source) -> Result<Server> {
    let listener = TcpListener::bind(("0.0.0.0", SERVE_PORT)).context("bind file server")?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let sent = Arc::new(AtomicU32::new(0));
    let (st, se) = (stop.clone(), sent.clone());
    std::thread::Builder::new().name("sonos-http".into()).stack_size(6 * 1024).spawn(move || {
        let live = Arc::new(AtomicU32::new(0));
        while !st.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((conn, _)) if live.load(Ordering::SeqCst) < 4 => {
                    let (source, se, live2) = (source.clone(), se.clone(), live.clone());
                    live.fetch_add(1, Ordering::SeqCst);
                    let spawned = std::thread::Builder::new().name("sonos-conn".into()).stack_size(6 * 1024).spawn(move || {
                        if let Err(e) = handle(conn, &source, &se) {
                            log::debug!("sonos-http: {e:#}");
                        }
                        live2.fetch_sub(1, Ordering::SeqCst);
                    });
                    if spawned.is_err() {
                        live.fetch_sub(1, Ordering::SeqCst);
                    }
                }
                Ok(_) => {} // too many at once: drop it, the speaker retries
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    })?;
    Ok(Server { stop, sent })
}

fn handle(mut c: TcpStream, source: &Source, sent: &AtomicU32) -> Result<()> {
    c.set_nonblocking(false)?;
    c.set_read_timeout(Some(Duration::from_secs(3)))?;
    c.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut head = Vec::with_capacity(512);
    let mut buf = [0u8; 256];
    while find(&head, b"\r\n\r\n").is_none() {
        let n = c.read(&mut buf)?;
        if n == 0 || head.len() > 2048 {
            bail!("bad request");
        }
        head.extend_from_slice(&buf[..n]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut first = head.lines().next().unwrap_or("").split(' ');
    let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let data = path
        .strip_prefix("/adhan/")
        .and_then(|p| p.strip_suffix(".mp3"))
        .and_then(|n| n.parse::<u8>().ok())
        .and_then(|slot| source(slot))
        .filter(|_| method == "GET" || method == "HEAD");
    let Some(data) = data else {
        c.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return Ok(());
    };
    let range = head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("range")).map(|(_, v)| v.to_string()));
    let (status, start, end) = match range.as_deref().map(|r| parse_range(r, data.len())) {
        None => ("200 OK", 0, data.len() - 1),
        Some(Some((a, b))) => ("206 Partial Content", a, b),
        Some(None) => {
            c.write_all(format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", data.len()).as_bytes())?;
            return Ok(());
        }
    };
    let mut h = format!("HTTP/1.1 {status}\r\nContent-Type: audio/mpeg\r\nAccept-Ranges: bytes\r\nConnection: close\r\nContent-Length: {}\r\n", end - start + 1);
    if status.starts_with("206") {
        h.push_str(&format!("Content-Range: bytes {start}-{end}/{}\r\n", data.len()));
    }
    h.push_str("\r\n");
    c.write_all(h.as_bytes())?;
    if method == "GET" {
        for chunk in data[start..=end].chunks(2048) {
            c.write_all(chunk)?;
            sent.fetch_add(chunk.len() as u32, Ordering::Relaxed);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOPO: &str = r#"<s:Body><u:GetZoneGroupStateResponse><ZoneGroupState>&lt;ZoneGroupState&gt;&lt;ZoneGroups&gt;&lt;ZoneGroup Coordinator="RINCON_A" ID="RINCON_A:1"&gt;&lt;ZoneGroupMember UUID="RINCON_A" Location="http://10.0.0.5:1400/xml/device_description.xml" ZoneName="Living Room" Icon=""&gt;&lt;Satellite UUID="RINCON_S" Location="http://10.0.0.9:1400/xml/device_description.xml" ZoneName="Living Room" Invisible="1"/&gt;&lt;/ZoneGroupMember&gt;&lt;ZoneGroupMember UUID="RINCON_B" Location="http://10.0.0.6:1400/xml/device_description.xml" ZoneName="Kitchen &amp;amp; Bar"/&gt;&lt;ZoneGroupMember UUID="RINCON_P" Location="http://10.0.0.8:1400/xml/device_description.xml" ZoneName="Kitchen &amp;amp; Bar" Invisible="1"/&gt;&lt;/ZoneGroup&gt;&lt;ZoneGroup Coordinator="RINCON_C" ID="RINCON_C:2"&gt;&lt;ZoneGroupMember UUID="RINCON_C" Location="http://10.0.0.7:1400/xml/device_description.xml" ZoneName="bedroom"/&gt;&lt;/ZoneGroup&gt;&lt;/ZoneGroups&gt;&lt;/ZoneGroupState&gt;</ZoneGroupState></u:GetZoneGroupStateResponse></s:Body>"#;

    #[test]
    fn topology_groups_and_hidden_players() {
        let v = parse_topology(TOPO);
        let names: Vec<_> = v.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["bedroom", "Kitchen & Bar", "Living Room"]);
        let bed = &v[0];
        assert_eq!((bed.id.as_str(), bed.coordinator.as_str(), bed.buddy.clone()), ("RINCON_C", "RINCON_C", None));
        assert_eq!(bed.ip, Ipv4Addr::new(10, 0, 0, 7));
        // follower rejoins via the coordinator; the coordinator via its follower
        assert_eq!(v[1].buddy.as_deref(), Some("RINCON_A"));
        assert_eq!(v[2].buddy.as_deref(), Some("RINCON_B"));
        assert_eq!(v[1].coordinator, "RINCON_A");
    }

    #[test]
    fn small_parsers() {
        assert_eq!(hms("0:04:39"), Some(279));
        assert_eq!(hms("1:00:01.500"), Some(3601));
        assert_eq!(hms("NOT_IMPLEMENTED"), None);
        assert_eq!(tag("<a><RelTime>0:00:07</RelTime></a>", "RelTime").as_deref(), Some("0:00:07"));
        assert_eq!(tag("<TrackURI>http://x/a?b=1&amp;c=2</TrackURI>", "TrackURI").as_deref(), Some("http://x/a?b=1&c=2"));
        assert_eq!(tag("<a></a>", "b"), None);
        assert_eq!(parse_range("bytes=10-", 100), Some((10, 99)));
        assert_eq!(parse_range("bytes=962014-", 962142), Some((962014, 962141)));
        assert_eq!(parse_range(" bytes=0-49", 100), Some((0, 49)));
        assert_eq!(parse_range("bytes=0-500", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=-5", 100), None);
        assert_eq!(parse_range("bytes=0-", 0), None);
    }

    #[test]
    fn volume_never_exceeds_the_ceiling() {
        for step in 0..=255u8 {
            assert!(step_volume(step) <= VOLUME_CEILING);
        }
        assert_eq!(step_volume(1), 4);
        assert_eq!(step_volume(6), VOLUME_CEILING);
        assert!(VOLUME_STEPS.windows(2).all(|w| w[0] < w[1]));
    }
}
