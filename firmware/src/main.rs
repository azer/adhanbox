//! EzanBox firmware: e-ink prayer timer with adhan for the Waveshare ESP32-S3-ePaper-1.54 (V2).
mod app;
mod audio;
mod board;
mod buttons;
mod clock;
mod console;
mod epd;
mod net;
mod portal;
mod rtc;
mod sonos;
mod sonos_task;
mod store;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::{PinDriver, Pull};
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_svc::hal::i2s::config::{DataBitWidth, StdConfig};
use esp_idf_svc::hal::i2s::I2sDriver;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::spi::{config as spicfg, SpiDeviceDriver, SpiDriver, SpiDriverConfig};
use esp_idf_svc::hal::units::FromValueType;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use std::sync::{Arc, Mutex};

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    let reason = unsafe { esp_idf_svc::sys::esp_reset_reason() };
    log::info!("AdhanBox {} starting (reset reason {reason})", env!("CARGO_PKG_VERSION"));

    let p = Peripherals::take()?;
    let pins = p.pins;

    // Power first: latch on battery, display on.
    let power = board::Power::new(PinDriver::output(pins.gpio6)?, PinDriver::output(pins.gpio17)?)?;
    let audio_pins = board::AudioPins::new(PinDriver::output(pins.gpio42)?, PinDriver::output(pins.gpio46)?)?;

    // Display: SPI2, SCK 12, MOSI 13, CS 11, DC 10, RST 9, BUSY 8
    let spi = SpiDriver::new(p.spi2, pins.gpio12, pins.gpio13, None::<esp_idf_svc::hal::gpio::AnyIOPin>, &SpiDriverConfig::new())?;
    let spi = SpiDeviceDriver::new(spi, Some(pins.gpio11), &spicfg::Config::new().baudrate(20.MHz().into()))?;
    let epd = epd::Epd::new(
        spi,
        PinDriver::output(pins.gpio10)?,
        PinDriver::output(pins.gpio9)?,
        PinDriver::input(pins.gpio8, Pull::Floating)?,
    );

    // Top button = GPIO0, bottom button = GPIO18 (power key), by physical position.
    let buttons = buttons::Buttons::new(PinDriver::input(pins.gpio0, Pull::Up)?, PinDriver::input(pins.gpio18, Pull::Up)?);

    // I²C bus: RTC (0x51), codec (0x18), SHTC3 (0x70)
    let i2c = I2cDriver::new(p.i2c0, pins.gpio47, pins.gpio48, &I2cConfig::new().baudrate(100.kHz().into()))?;
    let i2c = Arc::new(Mutex::new(i2c));

    // I²S to the ES8311: MCLK 14, BCLK 15, WS 38, DOUT 45
    let i2s = I2sDriver::new_std_tx(
        p.i2s0,
        &StdConfig::philips(audio::SAMPLE_RATE, DataBitWidth::Bits16),
        pins.gpio15,
        pins.gpio45,
        Some(pins.gpio14),
        pins.gpio38,
    )?;

    let nvs = EspDefaultNvsPartition::take()?;
    let store = store::Store::new(nvs.clone())?;
    let volume = store.load().volume;
    let audio = audio::Audio::start(i2s, i2c.clone(), audio_pins, volume);
    // Network speakers fetch the recordings straight from the memory-mapped pack.
    let pack = audio.pack;
    let player = sonos_task::Player::start(Arc::new(move |slot| pack.and_then(|p| p.entry(slot).ok())));
    let sysloop = EspSystemEventLoop::take()?;
    let net = net::Net::new(p.modem, sysloop, nvs)?;
    let console = console::start();

    app::App::new(app::Hw { epd, buttons, power, i2c, net, store, audio, player, console }).run()
}
