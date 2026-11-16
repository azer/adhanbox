//! Screens, drawn into a [`Frame`]. Same code runs on the panel and in the simulator.
//!
//! Layout grid: 200×200, 8 px side margin, hint bar on the bottom 16 px.
mod screens;
pub use screens::*;

use crate::frame::Frame;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, PrimitiveStyle, Rectangle, RoundedRectangle};
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use u8g2_fonts::{fonts, FontRenderer};

pub const W: i32 = 200;
pub const H: i32 = 200;
pub const M: i32 = 8;

pub const INK: BinaryColor = BinaryColor::On;
pub const PAPER: BinaryColor = BinaryColor::Off;

pub static F_BIG: FontRenderer = FontRenderer::new::<fonts::u8g2_font_logisoso62_tn>();
pub static F_HUGE_WORD: FontRenderer = FontRenderer::new::<fonts::u8g2_font_fub42_tf>();
pub static F_UNIT: FontRenderer = FontRenderer::new::<fonts::u8g2_font_fub17_tf>();
pub static F_LABEL: FontRenderer = FontRenderer::new::<fonts::u8g2_font_fub14_tf>();
pub static F_TITLE: FontRenderer = FontRenderer::new::<fonts::u8g2_font_fub11_tf>();
pub static F_BODY: FontRenderer = FontRenderer::new::<fonts::u8g2_font_fur11_tf>();
pub static F_BODY_B: FontRenderer = FontRenderer::new::<fonts::u8g2_font_fub11_tf>();
pub static F_GRID: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvR10_tf>();
pub static F_GRID_B: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvB10_tf>();
pub static F_SMALL: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_helvR08_tf>().with_ignore_unknown_chars(true);

#[derive(Clone, Copy)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Draw text with its baseline at `y`. Returns the right edge (x) of the text.
pub fn text(
    f: &mut Frame,
    font: &FontRenderer,
    s: impl u8g2_fonts::Content,
    x: i32,
    y: i32,
    align: Align,
    color: BinaryColor,
) -> i32 {
    let h = match align {
        Align::Left => HorizontalAlignment::Left,
        Align::Center => HorizontalAlignment::Center,
        Align::Right => HorizontalAlignment::Right,
    };
    match font.render_aligned(s, Point::new(x, y), VerticalPosition::Baseline, h, FontColor::Transparent(color), f) {
        Ok(Some(r)) => r.top_left.x + r.size.width as i32,
        _ => x,
    }
}

pub fn text_width(font: &FontRenderer, s: impl u8g2_fonts::Content) -> i32 {
    font.get_rendered_dimensions(s, Point::zero(), VerticalPosition::Baseline)
        .ok()
        .and_then(|d| d.bounding_box)
        .map(|b| b.size.width as i32)
        .unwrap_or(0)
}

pub fn fill(f: &mut Frame, x: i32, y: i32, w: u32, h: u32, color: BinaryColor) {
    let _ = Rectangle::new(Point::new(x, y), Size::new(w, h))
        .into_styled(PrimitiveStyle::with_fill(color))
        .draw(f);
}

pub fn fill_round(f: &mut Frame, x: i32, y: i32, w: u32, h: u32, r: u32, color: BinaryColor) {
    let _ = RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(x, y), Size::new(w, h)),
        Size::new(r, r),
    )
    .into_styled(PrimitiveStyle::with_fill(color))
    .draw(f);
}

pub fn outline(f: &mut Frame, x: i32, y: i32, w: u32, h: u32, color: BinaryColor) {
    let _ = Rectangle::new(Point::new(x, y), Size::new(w, h))
        .into_styled(PrimitiveStyle::with_stroke(color, 1))
        .draw(f);
}

pub fn hline(f: &mut Frame, x0: i32, x1: i32, y: i32, color: BinaryColor) {
    let _ = Line::new(Point::new(x0, y), Point::new(x1, y))
        .into_styled(PrimitiveStyle::with_stroke(color, 1))
        .draw(f);
}

/// Bottom hint bar: "BOOT: back" left, "PWR: next" right.
pub fn hints(f: &mut Frame, left: &str, right: &str) {
    hline(f, M, W - M, H - 18, INK);
    if !left.is_empty() {
        text(f, &F_SMALL, left, M, H - 5, Align::Left, INK);
    }
    if !right.is_empty() {
        text(f, &F_SMALL, right, W - M, H - 5, Align::Right, INK);
    }
}

fn triangle(f: &mut Frame, x: i32, y: i32, up: bool) {
    use embedded_graphics::primitives::Triangle;
    let (a, b, c) = if up {
        (Point::new(x, y + 5), Point::new(x + 6, y + 5), Point::new(x + 3, y))
    } else {
        (Point::new(x, y), Point::new(x + 6, y), Point::new(x + 3, y + 5))
    };
    let _ = Triangle::new(a, b, c).into_styled(PrimitiveStyle::with_fill(INK)).draw(f);
}

/// Hint bar for up/down screens: "▲ top  ▼ bottom" left, `right` (e.g. "hold top: select").
pub fn nav_hints(f: &mut Frame, right: &str) {
    hline(f, M, W - M, H - 18, INK);
    let y = H - 5;
    triangle(f, M, y - 6, true);
    let x = text(f, &F_SMALL, "top", M + 9, y, Align::Left, INK);
    triangle(f, x + 8, y - 6, false);
    text(f, &F_SMALL, "bottom", x + 17, y, Align::Left, INK);
    text(f, &F_SMALL, right, W - M, y, Align::Right, INK);
}

/// Screen title with optional step counter ("2/5") on the right.
pub fn title(f: &mut Frame, t: &str, step: Option<&str>) {
    text(f, &F_TITLE, t, M, 18, Align::Left, INK);
    if let Some(s) = step {
        text(f, &F_SMALL, s, W - M, 16, Align::Right, INK);
    }
}
