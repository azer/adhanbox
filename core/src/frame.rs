//! 200×200 1-bit framebuffer in the SSD1681 RAM layout (row-major, MSB = leftmost,
//! bit set = white). Shared by the firmware (sent over SPI) and the simulator (PNG).
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;

pub const WIDTH: u32 = 200;
pub const HEIGHT: u32 = 200;
pub const BYTES: usize = (WIDTH * HEIGHT / 8) as usize;

/// `BinaryColor::On` is ink (black), `Off` is paper (white).
pub struct Frame {
    pub buf: [u8; BYTES],
}

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

impl Frame {
    pub const fn new() -> Self {
        Self { buf: [0xFF; BYTES] }
    }

    pub fn clear_white(&mut self) {
        self.buf.fill(0xFF);
    }

    pub fn is_black(&self, x: u32, y: u32) -> bool {
        let i = (y * (WIDTH / 8) + x / 8) as usize;
        self.buf[i] & (0x80 >> (x % 8)) == 0
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, black: bool) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let i = (y * (WIDTH / 8) + x / 8) as usize;
        let bit = 0x80 >> (x % 8);
        if black {
            self.buf[i] &= !bit;
        } else {
            self.buf[i] |= bit;
        }
    }
}

impl OriginDimensions for Frame {
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}

impl DrawTarget for Frame {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 {
                self.set(p.x as u32, p.y as u32, c.is_on());
            }
        }
        Ok(())
    }
}
