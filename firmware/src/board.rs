//! Power rails and the battery latch. Pin map: SPEC.md §2.
use esp_idf_svc::hal::gpio::{Output, PinDriver};

/// Display power + battery latch (owned by the main loop).
pub struct Power {
    epd: PinDriver<'static, Output>,  // GPIO6, active low
    vbat: PinDriver<'static, Output>, // GPIO17, high = stay on (battery)
}

impl Power {
    pub fn new(epd: PinDriver<'static, Output>, vbat: PinDriver<'static, Output>) -> anyhow::Result<Self> {
        let mut p = Self { epd, vbat };
        p.vbat.set_high()?; // latch on first, so a short PWR press on battery keeps us alive
        p.epd.set_low()?;
        Ok(p)
    }

    /// Release the battery latch. On USB power the board stays on.
    pub fn power_off(&mut self) {
        let _ = self.vbat.set_low();
    }
}

/// Codec power + speaker amp (owned by the audio thread).
pub struct AudioPins {
    rail: PinDriver<'static, Output>, // GPIO42, active low
    pa: PinDriver<'static, Output>,   // GPIO46, high = amp on
}

impl AudioPins {
    pub fn new(rail: PinDriver<'static, Output>, pa: PinDriver<'static, Output>) -> anyhow::Result<Self> {
        let mut p = Self { rail, pa };
        p.rail.set_high()?;
        p.pa.set_low()?;
        Ok(p)
    }
    pub fn rail(&mut self, on: bool) {
        let _ = if on { self.rail.set_low() } else { self.rail.set_high() };
    }
    pub fn amp(&mut self, on: bool) {
        let _ = if on { self.pa.set_high() } else { self.pa.set_low() };
    }
}
