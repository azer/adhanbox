//! Renders every screen to PNG (target/screens/*.png) at 3× scale, using the same
//! drawing code as the firmware. `cargo run -p simulator`.
use ezan::cities::{self, CITIES};
use ezan::frame::{Frame, HEIGHT, WIDTH};
use ezan::schedule;
use ezan::ui::{self, Home, Mark, Status};
use ezan::{Date, LocalTime, Prayer};
use std::path::PathBuf;

const SCALE: u32 = 3;

fn save(f: &Frame, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/screens");
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (WIDTH * SCALE, HEIGHT * SCALE);
    let mut px = vec![0u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            // e-paper-ish paper tone
            px[(y * w + x) as usize] = if f.is_black(x / SCALE, y / SCALE) { 0x1a } else { 0xee };
        }
    }
    let file = std::fs::File::create(dir.join(format!("{name}.png"))).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&px).unwrap();
    println!("target/screens/{name}.png");
}

fn main() {
    let json = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../core/tests/fixtures/berlin.json")).unwrap();
    let days = ezan::parse::parse_vakitler(&json).unwrap();
    let today = Date::new(2026, 11, 16);
    let mut f = Frame::new();
    let berlin = cities::get(cities::DEFAULT_CITY);

    let home_at = |f: &mut Frame, h: u32, m: u32, status: Status| {
        let now = LocalTime::new(today, h, m, 0);
        let next = schedule::next(now, &days);
        let grid = next.map(|n| &days[n.day_index]);
        ui::home(f, &Home { city: berlin.name, now, next, grid, status });
    };

    home_at(&mut f, 11, 31, Status::default());
    save(&f, "01_home");
    home_at(&mut f, 14, 1, Status::default());
    save(&f, "02_home_minutes");
    home_at(&mut f, 21, 40, Status { battery: Some(64), no_wifi: true, ..Default::default() });
    save(&f, "03_home_after_isha_status");
    ui::home(&mut f, &Home { city: berlin.name, now: LocalTime::new(today, 9, 0, 0), next: None, grid: None, status: Status::default() });
    save(&f, "04_home_no_times");

    let d = schedule::today(LocalTime::new(today, 0, 0, 0), &days).unwrap();
    ui::adhan(&mut f, Prayer::Asr, d.time(Prayer::Asr));
    save(&f, "05_adhan");

    let menu = [("City", berlin.name), ("Adhan", "On"), ("Volume", "4/6"), ("Fajr volume", "2/6"), ("Sonos", "2 rooms"), ("Test adhan", ""), ("Wi-Fi", "HomeNet"), ("About", ""), ("Reset", ""), ("Back", "")];
    ui::list(&mut f, "Menu", menu.len(), 0, None, |i| menu[i], ezan::strings::SELECT_HINT);
    save(&f, "06_menu");
    ui::list(&mut f, "City", CITIES.len(), 1, Some(1), |i| (CITIES[i].name, CITIES[i].utc_label), ezan::strings::SELECT_HINT);
    save(&f, "07_city_picker");
    ui::list(&mut f, "City", CITIES.len(), 14, Some(1), |i| (CITIES[i].name, CITIES[i].utc_label), ezan::strings::SELECT_HINT);
    save(&f, "08_city_picker_scrolled");

    ui::welcome(&mut f);
    save(&f, "10_setup_welcome");
    ui::setup_qr(&mut f, "AdhanBox-3C1A", "192.168.4.1", "Phone joined");
    save(&f, "11_setup_qr");
    ui::message(&mut f, "Setup", Some("2/5"), &["Phone connected.", "", "Pick your Wi-Fi network", "on the page that opened", "on your phone."], "hold top: cancel", "");
    save(&f, "12_setup_phone");
    ui::checklist(&mut f, "Setting up", Some("4/5"), &[(Mark::Done, "Wi-Fi", "HomeNet"), (Mark::Done, "Clock", "13:24 UTC+1"), (Mark::Working, "Prayer times", "Berlin")], "", "");
    save(&f, "13_setup_sync");
    ui::checklist(&mut f, "Setting up", Some("4/5"), &[(Mark::Done, "Wi-Fi", "HomeNet"), (Mark::Done, "Clock", "13:24 UTC+1"), (Mark::Failed, "Prayer times", "failed")], "bottom: skip", "top: retry");
    save(&f, "14_setup_sync_failed");
    ui::volume(&mut f, "Speaker check", Some("5/5"), 4, 6, true, "hold top: done");
    save(&f, "15_setup_volume");
    ui::message(&mut f, "You're set", None, &["Next: Asr at 14:02", "", "Press any button", "for the menu."], "", "");
    save(&f, "16_setup_done");

    let sonos = [("Sonos", "On"), ("Volume", "3/6"), ("Fajr volume", "Same"), ("Speakers", "2 on"), ("Back", "")];
    ui::list(&mut f, "Sonos", sonos.len(), 3, None, |i| sonos[i], ezan::strings::SELECT_HINT);
    save(&f, "30_sonos");
    let rooms = [("Bedroom", "On"), ("Kitchen", "Off"), ("Living Room", "On"), ("Search again", ""), ("Back", "")];
    ui::list(&mut f, "Speakers", rooms.len(), 0, None, |i| rooms[i], "hold top: on/off");
    save(&f, "31_sonos_speakers");
    ui::level_bars(&mut f, "Sonos volume", None, "Playing on Sonos...", "Sonos volume", 3, 6, ["Top louder, bottom quieter", "Applies to every ticked speaker."], "hold top: done");
    save(&f, "32_sonos_volume");
    ui::level_bars(&mut f, "Fajr volume", None, "Playing Fajr adhan sample...", "Own volume for Fajr", 2, 6, ["Top louder, bottom quieter", "Other prayers: 4/6"], "hold top: done");
    save(&f, "33_fajr_volume");
    ui::level_bars(&mut f, "Fajr volume", None, "Playing Fajr adhan sample...", "Same as other prayers", 4, 6, ["Top louder, bottom quieter", "Other prayers: 4/6"], "hold top: done");
    save(&f, "34_fajr_volume_same");

    ui::message(&mut f, "Reset settings?", None, &["Wi-Fi, city, volume and", "saved prayer times will", "be erased.", "", "Adhan recordings stay."], "press: cancel", "hold top: continue");
    save(&f, "20_reset_confirm");
    ui::hold(&mut f, "Are you sure?", &["Hold the top button", "for 2 seconds to reset."], 55, "Release to cancel");
    save(&f, "21_reset_hold");
    ui::hold(&mut f, "Emergency reset", &["Keep holding the top", "button to open reset."], 30, "Release to cancel");
    save(&f, "22_emergency_hold");
}
