//! Adhan playback: packed MP3s in the `audio` flash partition → nanomp3 → ES8311 over I²S.
use crate::board::AudioPins;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use anyhow::{bail, Context, Result};
use esp_idf_svc::hal::delay::{FreeRtos, BLOCK};
use esp_idf_svc::hal::i2c::I2cDriver;
use esp_idf_svc::hal::i2s::{I2sDriver, I2sTx};
use esp_idf_svc::sys;

const ES8311: u8 = 0x18;
pub const SAMPLE_RATE: u32 = 22_050;
const AUDIO_STACK: usize = 256 * 1024;

/// Software gain per volume step 1..=6.
const GAIN: [f32; 7] = [0.0, 0.05, 0.10, 0.18, 0.30, 0.50, 0.80];

// ───────────── adhan pack (memory-mapped flash) ─────────────

/// The `audio` partition, memory-mapped once. Reading mapped flash needs no flash API calls,
/// which matters because the audio thread's stack lives in PSRAM (flash ops would disable it).
pub struct Partition {
    data: &'static [u8],
}

impl Partition {
    pub fn find() -> Option<Self> {
        unsafe {
            let p = sys::esp_partition_find_first(
                sys::esp_partition_type_t_ESP_PARTITION_TYPE_DATA,
                sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_ANY,
                c"audio".as_ptr(),
            );
            if p.is_null() {
                return None;
            }
            // Map only what the pack uses: read its header first.
            let mut h = [0u8; 12];
            sys::esp_partition_read(p, 0, h.as_mut_ptr() as *mut _, 12);
            if &h[0..4] != b"EZAN" {
                log::warn!("audio: partition has no adhan pack");
                return None;
            }
            let count = u32::from_le_bytes([h[8], h[9], h[10], h[11]]).min(16) as usize;
            let mut idx = vec![0u8; count * 12];
            sys::esp_partition_read(p, 12, idx.as_mut_ptr() as *mut _, idx.len());
            let used = idx
                .chunks_exact(12)
                .map(|e| u32::from_le_bytes([e[4], e[5], e[6], e[7]]) + u32::from_le_bytes([e[8], e[9], e[10], e[11]]))
                .max()
                .unwrap_or(0)
                .min((*p).size);
            let len = ((used as usize + 0xFFFF) & !0xFFFF).min((*p).size as usize); // 64 KB pages
            let mut ptr: *const core::ffi::c_void = core::ptr::null();
            let mut handle: sys::esp_partition_mmap_handle_t = 0;
            let r = sys::esp_partition_mmap(
                p,
                0,
                len,
                sys::esp_partition_mmap_memory_t_ESP_PARTITION_MMAP_DATA,
                &mut ptr,
                &mut handle,
            );
            if r != 0 || ptr.is_null() {
                log::error!("audio: mmap failed: {r}");
                return None;
            }
            log::info!("audio: mapped {} KB adhan pack", len / 1024);
            Some(Self { data: core::slice::from_raw_parts(ptr as *const u8, len) })
        }
    }

    /// The MP3 bytes for `slot` (0 Fajr … 4 Isha); falls back to the first entry.
    pub fn entry(&self, slot: u8) -> Result<&'static [u8]> {
        let d = self.data;
        if d.len() < 12 || &d[0..4] != b"EZAN" {
            bail!("no adhan pack in flash (run `cargo xtask flash-audio`)");
        }
        let count = u32::from_le_bytes([d[8], d[9], d[10], d[11]]).min(16) as usize;
        let mut fallback = None;
        for e in d[12..12 + count * 12].chunks_exact(12) {
            let off = u32::from_le_bytes([e[4], e[5], e[6], e[7]]) as usize;
            let len = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
            if off + len > d.len() {
                continue;
            }
            if e[0] == slot {
                return Ok(&d[off..off + len]);
            }
            fallback.get_or_insert(&d[off..off + len]);
        }
        fallback.context("adhan pack is empty")
    }
}

unsafe impl Send for Partition {}

// ───────────── codec ─────────────

fn wr(i2c: &mut I2cDriver, reg: u8, val: u8) -> Result<()> {
    i2c.write(ES8311, &[reg, val], BLOCK)?;
    Ok(())
}

/// ES8311 as I²S slave, MCLK = 256·fs from the ESP32, 16-bit, DAC only.
/// Register values follow Espressif's esp_codec_dev es8311 driver.
fn es8311_init(i2c: &mut I2cDriver) -> Result<()> {
    for (r, v) in [
        (0x44, 0x08),
        (0x44, 0x08),
        (0x01, 0x30),
        (0x02, 0x00),
        (0x03, 0x10),
        (0x16, 0x24),
        (0x04, 0x10),
        (0x05, 0x00),
        (0x0B, 0x00),
        (0x0C, 0x00),
        (0x10, 0x1F),
        (0x11, 0x7F),
        (0x00, 0x80), // power on, slave mode
        (0x01, 0x3F), // all clocks on, MCLK from pin
        (0x06, 0x03), // BCLK not inverted
        (0x07, 0x00),
        (0x08, 0xFF),
        (0x09, 0x0C), // DAC serial in: I²S, 16-bit
        (0x0A, 0x4C), // ADC serial out muted
        (0x13, 0x10),
        (0x1B, 0x0A),
        (0x1C, 0x6A),
        (0x0E, 0x02),
        (0x12, 0x00), // DAC power up
        (0x14, 0x1A),
        (0x0D, 0x01), // analog power up
        (0x15, 0x40),
        (0x37, 0x08), // bypass DAC EQ
        (0x45, 0x00),
        (0x31, 0x00), // unmute
        (0x32, 0xBF), // DAC volume 0 dB (gain is applied in software)
    ] {
        wr(i2c, r, v)?;
    }
    Ok(())
}

fn es8311_off(i2c: &mut I2cDriver) {
    let _ = wr(i2c, 0x32, 0x00);
    let _ = wr(i2c, 0x31, 0x60);
    let _ = wr(i2c, 0x12, 0x02);
    let _ = wr(i2c, 0x0D, 0xFC);
}

// ───────────── player ─────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Continue,
    Stop,
}

pub struct PlayCtl {
    pub volume: u8,
    pub elapsed_ms: u32,
}

/// Play the adhan for `slot`. `tick` runs between frames: it can change the volume or stop.
/// `loop_ms` replays the first N ms forever (volume preview) instead of the whole recording.
pub fn play(
    i2s: &mut I2sDriver<'static, I2sTx>,
    i2c: &Mutex<I2cDriver<'static>>,
    power: &mut AudioPins,
    part: &Partition,
    slot: u8,
    volume: u8,
    loop_ms: Option<u32>,
    mut tick: impl FnMut(&mut PlayCtl) -> Control,
) -> Result<()> {
    let mp3 = part.entry(slot)?;
    power.rail(true);
    FreeRtos::delay_ms(20);
    let res = (|| -> Result<()> {
        es8311_init(&mut i2c.lock().unwrap())?;
        i2s.tx_enable()?;
        power.amp(true);
        let mut ctl = PlayCtl { volume, elapsed_ms: 0 };
        let mut pcm = vec![0i16; 2304];
        let mut out = vec![0u8; 2304 * 4];
        'outer: loop {
            unsafe {
                log::info!(
                    "audio: before decoder: internal {} KB (largest {} KB), psram {} KB",
                    sys::heap_caps_get_free_size(sys::MALLOC_CAP_INTERNAL) / 1024,
                    sys::heap_caps_get_largest_free_block(sys::MALLOC_CAP_INTERNAL) / 1024,
                    sys::heap_caps_get_free_size(sys::MALLOC_CAP_SPIRAM) / 1024
                );
            }
            let mut dec = nanomp3::Reader::<_, i16>::new(std::io::Cursor::new(mp3))
                .map_err(|e| anyhow::anyhow!("mp3: {e:?}"))?;
            let ch = dec.channels().map(|c| c.num() as usize).unwrap_or(1).max(1);
            let mut samples: u64 = 0;
            let mut fade_in = 0u32;
            loop {
                if tick(&mut ctl) == Control::Stop {
                    break 'outer;
                }
                let n = dec.read(&mut pcm).map_err(|e| anyhow::anyhow!("mp3: {e:?}"))?;
                if n == 0 {
                    break;
                }
                let gain = GAIN[ctl.volume.min(6) as usize];
                // mono → stereo i16 LE; 50 ms fade-in avoids a pop
                let frames = n / ch;
                for i in 0..frames {
                    let s = pcm[i * ch] as f32;
                    let ramp = (fade_in as f32 / (SAMPLE_RATE as f32 * 0.05)).min(1.0);
                    fade_in += 1;
                    let v = (s * gain * ramp) as i16;
                    let b = v.to_le_bytes();
                    out[i * 4..i * 4 + 4].copy_from_slice(&[b[0], b[1], b[0], b[1]]);
                }
                i2s.write_all(&out[..frames * 4], BLOCK)?;
                samples += frames as u64;
                ctl.elapsed_ms = (samples * 1000 / SAMPLE_RATE as u64) as u32;
                if let Some(l) = loop_ms {
                    if ctl.elapsed_ms >= l {
                        continue 'outer;
                    }
                }
            }
            if loop_ms.is_none() {
                break;
            }
        }
        // flush silence so the DMA buffer drains without a click
        out.fill(0);
        let _ = i2s.write_all(&out[..2048], BLOCK);
        Ok(())
    })();
    power.amp(false);
    let _ = i2s.tx_disable();
    es8311_off(&mut i2c.lock().unwrap());
    power.rail(false);
    res
}

// ───────────── audio thread ─────────────

pub struct Request {
    pub slot: u8,
    /// Loop the first N ms (volume preview) instead of playing once.
    pub loop_ms: Option<u32>,
}

/// Handle to the audio thread. Playback never blocks the UI.
#[derive(Clone)]
pub struct Audio {
    tx: Sender<Request>,
    pub playing: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    volume: Arc<AtomicU8>,
    pub available: bool,
}

impl Audio {
    pub fn start(
        mut i2s: I2sDriver<'static, I2sTx>,
        i2c: Arc<Mutex<I2cDriver<'static>>>,
        mut pins: AudioPins,
        volume: u8,
    ) -> Self {
        let (tx, rx) = channel::<Request>();
        let playing = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let vol = Arc::new(AtomicU8::new(volume));
        let part = Partition::find();
        let available = part.as_ref().map(|p| p.entry(0).is_ok()).unwrap_or(false);
        if !available {
            log::warn!("audio: no adhan pack in flash; run `cargo xtask flash-audio`");
        }
        let (pl, st, vo) = (playing.clone(), stop.clone(), vol.clone());
        unsafe {
            log::info!(
                "audio: free internal {} KB, largest block {} KB",
                sys::heap_caps_get_free_size(sys::MALLOC_CAP_INTERNAL) / 1024,
                sys::heap_caps_get_largest_free_block(sys::MALLOC_CAP_INTERNAL) / 1024
            );
        }
        // nanomp3 runs at opt-level=1 (Xtensa LLVM bug), so its frames are large: give it a big
        // stack, allocated in PSRAM (8 MB) instead of scarce internal RAM.
        unsafe {
            let mut cfg = sys::esp_pthread_get_default_config();
            cfg.stack_size = AUDIO_STACK;
            cfg.stack_alloc_caps = sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_8BIT;
            sys::esp_pthread_set_cfg(&cfg);
        }
        let handle = std::thread::Builder::new()
            .name("audio".into())
            .stack_size(AUDIO_STACK)
            .spawn(move || {
                let Some(part) = part else { return };
                for req in rx {
                    st.store(false, Ordering::SeqCst);
                    pl.store(true, Ordering::SeqCst);
                    let r = play(&mut i2s, &i2c, &mut pins, &part, req.slot, vo.load(Ordering::SeqCst), req.loop_ms, |ctl| {
                        ctl.volume = vo.load(Ordering::SeqCst);
                        if st.load(Ordering::SeqCst) { Control::Stop } else { Control::Continue }
                    });
                    if let Err(e) = r {
                        log::error!("audio: {e:?}");
                    }
                    pl.store(false, Ordering::SeqCst);
                }
            })
            .expect("audio thread");
        drop(handle);
        unsafe {
            let def = sys::esp_pthread_get_default_config();
            sys::esp_pthread_set_cfg(&def);
        }
        Self { tx, playing, stop, volume: vol, available }
    }

    pub fn play(&self, slot: u8, loop_ms: Option<u32>) {
        self.stop.store(true, Ordering::SeqCst);
        // wait for a running playback to end
        for _ in 0..50 {
            if !self.is_playing() {
                break;
            }
            FreeRtos::delay_ms(10);
        }
        self.playing.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Request { slot, loop_ms });
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::SeqCst)
    }

    pub fn set_volume(&self, v: u8) {
        self.volume.store(v, Ordering::SeqCst);
    }
}
