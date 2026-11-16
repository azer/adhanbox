use super::*;
use crate::date::LocalTime;
use crate::prayer::{DayTimes, Prayer};
use crate::schedule::{countdown, Countdown, Next};
use crate::strings as s;

macro_rules! hm {
    ($m:expr) => {
        format_args!("{:02}:{:02}", $m / 60, $m % 60)
    };
}

// ───────────────────────────── Home ─────────────────────────────

#[derive(Clone, Copy, Default)]
pub struct Status {
    /// Last daily sync failed for 3+ days.
    pub no_wifi: bool,
    /// Battery percentage when running on battery.
    pub battery: Option<u8>,
    /// Fewer than 3 days of prayer times cached.
    pub times_expiring: bool,
}

pub struct Home<'a> {
    pub city: &'a str,
    pub now: LocalTime,
    pub next: Option<Next>,
    /// The day the next prayer belongs to (today, or tomorrow after Isha).
    pub grid: Option<&'a DayTimes>,
    pub status: Status,
}

pub fn home(f: &mut Frame, h: &Home) {
    f.clear_white();

    // Top bar: city · date
    let mut city = heapless::String::<32>::new();
    for c in h.city.chars() {
        let _ = city.push(c.to_ascii_uppercase());
    }
    text(f, &F_TITLE, city.as_str(), M, 16, Align::Left, INK);
    let d = h.now.date;
    text(
        f,
        &F_BODY,
        format_args!("{} {} {}", s::WEEKDAYS[d.weekday() as usize], d.d, s::MONTHS[(d.m as usize).saturating_sub(1) % 12]),
        W - M,
        16,
        Align::Right,
        INK,
    );

    let (Some(next), Some(grid)) = (h.next, h.grid) else {
        text(f, &F_LABEL, "Waiting for", W / 2, 90, Align::Center, INK);
        text(f, &F_LABEL, "prayer times", W / 2, 112, Align::Center, INK);
        text(f, &F_SMALL, "Will fetch when Wi-Fi is available", W / 2, 140, Align::Center, INK);
        return;
    };

    // Current time, big and centered.
    text(f, &F_BIG, format_args!("{:02}:{:02}", h.now.hour(), h.now.minute()), W / 2, 96, Align::Center, INK);

    // "Asr · 2h 14m left", centered under the clock.
    let mut left = heapless::String::<24>::new();
    let _ = match countdown(next.remaining_s) {
        Countdown::HoursMinutes(hh, mm) => core::fmt::write(&mut left, format_args!(" · {}h {}m {}", hh, mm, s::LEFT)),
        Countdown::Minutes(mm) => core::fmt::write(&mut left, format_args!(" · {}m {}", mm, s::LEFT)),
        Countdown::Now => core::fmt::write(&mut left, format_args!(" · {}", s::NOW_LOWER)),
    };
    let name = s::prayer_name(next.prayer);
    let total = text_width(&F_LABEL, name) + text_width(&F_BODY, left.as_str());
    let x = W / 2 - total / 2;
    let x = text(f, &F_LABEL, name, x, 122, Align::Left, INK);
    text(f, &F_BODY, left.as_str(), x, 122, Align::Left, INK);

    // Times grid: two columns, three rows, column-major (Fajr, Sunrise, Dhuhr | Asr, Maghrib, Isha)
    hline(f, M, W - M, 130, INK);
    let row_h = 17;
    let top = 133;
    for (i, p) in Prayer::ALL.iter().enumerate() {
        let col = (i / 3) as i32;
        let row = (i % 3) as i32;
        let x0 = if col == 0 { 3 } else { W / 2 + 2 };
        let cw = (W / 2 - 5) as u32;
        let y = top + row * row_h;
        let hl = *p == next.prayer && grid.date == next.date;
        let (font, color) = if hl { (&F_GRID, PAPER) } else { (&F_GRID, INK) };
        if hl {
            fill_round(f, x0, y, cw, (row_h - 1) as u32, 3, INK);
        }
        let base = y + 12;
        text(f, font, s::prayer_name(*p), x0 + 3, base, Align::Left, color);
        text(f, font, hm!(grid.time(*p)), x0 + cw as i32 - 3, base, Align::Right, color);
    }

    // Footer: Hijri date · status
    let hj = grid.hijri;
    if hj.m >= 1 && hj.m <= 12 {
        text(f, &F_SMALL, format_args!("{} {} {}", hj.d, s::HIJRI_MONTHS[hj.m as usize - 1], hj.y), M, H - 4, Align::Left, INK);
    }
    let mut x = W - M;
    if let Some(pct) = h.status.battery {
        x = battery_icon(f, x, H - 12, pct) - 5;
    }
    if h.status.no_wifi {
        x = no_wifi_icon(f, x, H - 4) - 5;
    }
    if h.status.times_expiring {
        text(f, &F_SMALL, "!", x, H - 4, Align::Right, INK);
    }
}

/// Wi-Fi fan with a slash, right-aligned at `right`, bottom at `base`. Returns the left edge.
fn no_wifi_icon(f: &mut Frame, right: i32, base: i32) -> i32 {
    use embedded_graphics::primitives::Arc;
    let cx = right - 6;
    for (i, d) in [4u32, 8, 12].iter().enumerate() {
        let r = *d as i32 / 2;
        let _ = Arc::new(Point::new(cx - r, base - r - i as i32 * 0), *d, (225.0f32).deg(), (90.0f32).deg())
            .into_styled(PrimitiveStyle::with_stroke(INK, 1))
            .draw(f);
    }
    let _ = Line::new(Point::new(cx - 6, base - 10), Point::new(cx + 6, base + 1))
        .into_styled(PrimitiveStyle::with_stroke(INK, 1))
        .draw(f);
    cx - 7
}

/// Battery outline with fill level, right-aligned at `right`. Returns the left edge.
fn battery_icon(f: &mut Frame, right: i32, y: i32, pct: u8) -> i32 {
    let w = 18;
    let x = right - w - 2;
    outline(f, x, y, w as u32, 9, INK);
    fill(f, x + w, y + 3, 2, 3, INK);
    let lvl = ((w - 4) * pct.min(100) as i32) / 100;
    fill(f, x + 2, y + 2, lvl.max(1) as u32, 5, INK);
    x
}

// ───────────────────────────── Adhan ─────────────────────────────

pub fn adhan(f: &mut Frame, p: Prayer, at: u16) {
    f.clear_white();
    fill(f, 0, 0, W as u32, H as u32, INK);
    text(f, &F_LABEL, s::prayer_upper(p), W / 2, 74, Align::Center, PAPER);
    text(f, &F_HUGE_WORD, hm!(at), W / 2, 128, Align::Center, PAPER);
    text(f, &F_BODY, s::ADHAN_PLAYING, W / 2, 162, Align::Center, PAPER);
    text(f, &F_SMALL, s::ANY_BUTTON_STOP, W / 2, 184, Align::Center, PAPER);
}

// ───────────────────────────── Lists & menus ─────────────────────────────

pub const LIST_ROWS: usize = 6;

/// Scrolling list with a title, `count` rows, selection and an optional ● marker.
pub fn list<'a>(
    f: &mut Frame,
    heading: &str,
    count: usize,
    sel: usize,
    marked: Option<usize>,
    row: impl Fn(usize) -> (&'a str, &'a str),
    select_hint: &str,
) {
    f.clear_white();
    title(f, heading, None);
    let first = sel.saturating_sub(LIST_ROWS - 1).min(count.saturating_sub(LIST_ROWS));
    let row_h = 25;
    let top = 26;
    for i in first..(first + LIST_ROWS).min(count) {
        let y = top + (i - first) as i32 * row_h;
        let (a, b) = row(i);
        let on = i == sel;
        let color = if on { PAPER } else { INK };
        if on {
            fill_round(f, M - 4, y, (W - 2 * M + 8) as u32, (row_h - 3) as u32, 4, INK);
        }
        let base = y + 16;
        let mut x = M;
        if marked.is_some() {
            if marked == Some(i) {
                let _ = embedded_graphics::primitives::Circle::new(Point::new(x, y + 8), 6)
                    .into_styled(PrimitiveStyle::with_fill(color))
                    .draw(f);
            }
            x += 12;
        }
        text(f, if on { &F_BODY_B } else { &F_BODY }, a, x, base, Align::Left, color);
        text(f, &F_BODY, b, W - M, base, Align::Right, color);
    }
    // Scrollbar
    if count > LIST_ROWS {
        let track = (LIST_ROWS as i32) * row_h - 4;
        let th = (track * LIST_ROWS as i32 / count as i32).max(8);
        let ty = top + (track - th) * first as i32 / (count - LIST_ROWS) as i32;
        fill(f, W - 3, ty, 2, th as u32, INK);
    }
    nav_hints(f, select_hint);
}

// ───────────────────────────── Setup & messages ─────────────────────────────

pub fn welcome(f: &mut Frame) {
    f.clear_white();
    text(f, &F_UNIT, "AdhanBox", W / 2, 80, Align::Center, INK);
    text(f, &F_BODY, "Prayer times & adhan", W / 2, 106, Align::Center, INK);
    hints(f, "", "press any button  >");
}

/// Generic message: title, optional step, body lines, hints.
pub fn message(f: &mut Frame, heading: &str, step: Option<&str>, body: &[&str], hint_left: &str, hint_right: &str) {
    f.clear_white();
    title(f, heading, step);
    let mut y = 50;
    for line in body {
        if line.is_empty() {
            y += 8;
            continue;
        }
        text(f, &F_BODY, *line, M, y, Align::Left, INK);
        y += 18;
    }
    hints(f, hint_left, hint_right);
}

/// Setup step 1: QR code to join the setup network.
pub fn setup_qr(f: &mut Frame, ssid: &str, ip: &str, status: &str) {
    use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};
    f.clear_white();
    title(f, "Setup", Some("1/5"));

    let mut payload = heapless::String::<64>::new();
    let _ = core::fmt::write(&mut payload, format_args!("WIFI:T:nopass;S:{};;", ssid));
    const V: Version = Version::new(4);
    let mut tmp = [0u8; V.buffer_len()];
    let mut out = [0u8; V.buffer_len()];
    if let Ok(qr) = QrCode::encode_text(&payload, &mut tmp, &mut out, QrCodeEcc::Medium, Version::MIN, V, None, true) {
        let n = qr.size();
        let scale = (96 / (n + 2)).max(1);
        let ox = M - 2;
        let oy = 30;
        for y in 0..n {
            for x in 0..n {
                if qr.get_module(x, y) {
                    fill(f, ox + (x + 1) * scale, oy + (y + 1) * scale, scale as u32, scale as u32, INK);
                }
            }
        }
    }
    let x = 114;
    text(f, &F_BODY_B, "Scan to", x, 44, Align::Left, INK);
    text(f, &F_BODY_B, "join:", x, 60, Align::Left, INK);
    text(f, &F_SMALL, ssid, x, 76, Align::Left, INK);
    text(f, &F_BODY, "Then pick", x, 102, Align::Left, INK);
    text(f, &F_BODY, "your Wi-Fi", x, 118, Align::Left, INK);
    text(f, &F_BODY_B, status, M, 150, Align::Left, INK);
    text(f, &F_SMALL, format_args!("No page? Open {} in Safari", ip), M, 170, Align::Left, INK);
    hints(f, "hold top: cancel", "");
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    Done,
    Working,
    Failed,
    Todo,
}

/// Setup checklist ("✓ Wi-Fi  HomeNet").
pub fn checklist(f: &mut Frame, heading: &str, step: Option<&str>, rows: &[(Mark, &str, &str)], hint_left: &str, hint_right: &str) {
    f.clear_white();
    title(f, heading, step);
    let mut y = 54;
    for (mark, a, b) in rows {
        match mark {
            Mark::Done => check(f, M, y - 9),
            Mark::Failed => cross(f, M, y - 9),
            Mark::Working => {
                for i in 0..3 {
                    fill(f, M + i * 4, y - 2, 2, 2, INK);
                }
            }
            Mark::Todo => {}
        }
        text(f, &F_BODY_B, *a, M + 18, y, Align::Left, INK);
        text(f, &F_BODY, *b, W - M, y, Align::Right, INK);
        y += 24;
    }
    hints(f, hint_left, hint_right);
}

fn check(f: &mut Frame, x: i32, y: i32) {
    use embedded_graphics::primitives::Polyline;
    let pts = [Point::new(x, y + 5), Point::new(x + 3, y + 9), Point::new(x + 10, y)];
    let _ = Polyline::new(&pts).into_styled(PrimitiveStyle::with_stroke(INK, 2)).draw(f);
}

fn cross(f: &mut Frame, x: i32, y: i32) {
    let s = PrimitiveStyle::with_stroke(INK, 2);
    let _ = Line::new(Point::new(x, y), Point::new(x + 9, y + 9)).into_styled(s).draw(f);
    let _ = Line::new(Point::new(x + 9, y), Point::new(x, y + 9)).into_styled(s).draw(f);
}

/// Volume screen (setup step 5 and Menu → Volume).
pub fn volume(f: &mut Frame, heading: &str, step: Option<&str>, level: u8, max: u8, playing: bool, hint_right: &str) {
    f.clear_white();
    title(f, heading, step);
    if playing {
        text(f, &F_BODY, "Playing adhan sample...", M, 56, Align::Left, INK);
    }
    text(f, &F_BODY_B, "Volume", M, 88, Align::Left, INK);
    let bw = ((W - 2 * M) - (max as i32 - 1) * 4) / max as i32;
    for i in 0..max {
        let x = M + i as i32 * (bw + 4);
        let hgt = 8 + i as i32 * 3;
        let y = 116 - hgt;
        if i < level {
            fill(f, x, y, bw as u32, hgt as u32, INK);
        } else {
            outline(f, x, y, bw as u32, hgt as u32, INK);
        }
    }
    text(f, &F_BODY, "Top louder, bottom quieter", M, 140, Align::Left, INK);
    text(f, &F_SMALL, "No sound? Check the speaker plug.", M, 160, Align::Left, INK);
    nav_hints(f, hint_right);
}

/// "Hold to confirm" bar (reset confirmation, emergency reset).
pub fn hold(f: &mut Frame, heading: &str, body: &[&str], progress_pct: u8, hint: &str) {
    f.clear_white();
    title(f, heading, None);
    let mut y = 50;
    for line in body {
        text(f, &F_BODY, *line, M, y, Align::Left, INK);
        y += 18;
    }
    let w = (W - 2 * M) as u32;
    outline(f, M, 130, w, 14, INK);
    fill(f, M + 2, 132, (w - 4) * progress_pct.min(100) as u32 / 100, 10, INK);
    hints(f, hint, "");
}
