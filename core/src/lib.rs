//! EzanBox core: pure logic and UI, shared by the firmware, the simulator and host tools.
//! `no_std` by default; the `std` feature adds the Diyanet JSON parser.
#![cfg_attr(not(feature = "std"), no_std)]

pub mod cities;
pub mod date;
pub mod frame;
pub mod prayer;
pub mod schedule;
pub mod strings;
pub mod ui;

#[cfg(feature = "std")]
pub mod parse;

pub use date::{Date, LocalTime};
pub use prayer::{DayTimes, Hijri, Prayer};
