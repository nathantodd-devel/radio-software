//! Open each HackRF attached and say what it is: a check that the library
//! loads and the device answers.

fn main() {
    match hackrf::HackRf::open_all() {
        Ok(devices) => {
            for device in devices {
                println!("{}: {:?} samples per second", device.name(), device.sample_rates());
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
