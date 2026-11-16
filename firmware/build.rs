fn main() {
    embuild::espidf::sysenv::output();
    // A clock earlier than the build can't be right (stale RTC): treat it as unset.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    println!("cargo:rustc-env=EZAN_BUILD_UNIX={now}");
}
