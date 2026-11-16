//! The two side buttons, named by position. Top = GPIO0, bottom = GPIO18 (the board's power key;
//! holding it can trigger the power circuit, so the UI never uses bottom-button holds).
//! Both active low. Poll every ~20 ms.
use esp_idf_svc::hal::gpio::{Input, PinDriver};
use std::time::Instant;

pub const LONG_MS: u32 = 800;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Btn {
    Top,
    Bottom,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// Released before LONG_MS.
    Short(Btn),
    /// Held for LONG_MS (fires once).
    Long(Btn),
    /// Still held, every ~100 ms after LONG_MS: total ms held.
    Held(Btn, u32),
    /// Released after a long press.
    Released(Btn),
}

struct State {
    down_at: Option<Instant>,
    long: bool,
    last_held_report: u32,
    stable: bool,
    raw_since: Option<Instant>,
}

pub struct Buttons {
    top: PinDriver<'static, Input>,
    bottom: PinDriver<'static, Input>,
    s: [State; 2],
    /// Ignore a button until it is released once (e.g. PWR held to power on).
    ignore_until_release: [bool; 2],
}

impl Buttons {
    pub fn new(top: PinDriver<'static, Input>, bottom: PinDriver<'static, Input>) -> Self {
        let mk = || State { down_at: None, long: false, last_held_report: 0, stable: false, raw_since: None };
        let ignore = [top.is_low(), bottom.is_low()];
        Self { top, bottom, s: [mk(), mk()], ignore_until_release: ignore }
    }

    pub fn is_down(&self, b: Btn) -> bool {
        match b {
            Btn::Top => self.top.is_low(),
            Btn::Bottom => self.bottom.is_low(),
        }
    }

    pub fn poll(&mut self) -> Option<Event> {
        for (i, b) in [Btn::Top, Btn::Bottom].into_iter().enumerate() {
            let raw = self.is_down(b);
            let now = Instant::now();
            let st = &mut self.s[i];
            // 30 ms debounce
            if raw != st.stable {
                let since = *st.raw_since.get_or_insert(now);
                if now.duration_since(since).as_millis() < 30 {
                    continue;
                }
                st.stable = raw;
                st.raw_since = None;
                if raw {
                    st.down_at = Some(now);
                    st.long = false;
                    st.last_held_report = 0;
                } else {
                    let was_long = st.long;
                    st.down_at = None;
                    if self.ignore_until_release[i] {
                        self.ignore_until_release[i] = false;
                        continue;
                    }
                    return Some(if was_long { Event::Released(b) } else { Event::Short(b) });
                }
            } else {
                st.raw_since = None;
            }
            if self.ignore_until_release[i] {
                continue;
            }
            if let Some(t) = st.down_at {
                let ms = now.duration_since(t).as_millis() as u32;
                if ms >= LONG_MS && !st.long {
                    st.long = true;
                    st.last_held_report = ms;
                    return Some(Event::Long(b));
                }
                if st.long && ms >= st.last_held_report + 100 {
                    st.last_held_report = ms;
                    return Some(Event::Held(b, ms));
                }
            }
        }
        None
    }

    /// Wait until both buttons are up (used after blocking actions).
    pub fn ignore_held(&mut self) {
        for (i, b) in [Btn::Top, Btn::Bottom].into_iter().enumerate() {
            if self.is_down(b) {
                self.ignore_until_release[i] = true;
            }
        }
    }
}
