//! K75 lighting diagnostics; read-only unless --on or --off is specified.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use periph_hal::{diagnostics::KzziLink, protocol};
    use std::time::{Duration, Instant};

    let api = hidapi::HidApi::new()?;
    let info = api
        .device_list()
        .find(|info| {
            info.vendor_id() == 0x3554
                && info.product_id() == 0xfa09
                && info.usage_page() == 0xff02
                && info.usage() == 2
        })
        .ok_or("KZZI receiver not found")?;
    let link = KzziLink::open(&info.path().to_string_lossy())?;
    let mut buffer = [0u8; 64];
    let mut profile = None;
    for _ in 0..3 {
        let mut pages = protocol::KzziLightingPages::default();
        link.drain();
        link.send(&protocol::kzzi_lighting_read_request())?;
        let deadline = Instant::now() + Duration::from_millis(1200);
        while Instant::now() < deadline {
            if let Some(n) = link.read(&mut buffer, Duration::from_millis(100))
                && let Some(result) = pages.push(&buffer[..n])
            {
                profile = Some(result);
                break;
            }
        }
        if profile.is_some() {
            break;
        }
    }
    let profile = profile.ok_or("lighting profile read timed out")?;
    drop(link);
    let effect = usize::from(profile[10]);
    println!(
        "effect={}, lit={}",
        effect,
        protocol::kzzi_profile_is_lit(&profile)
    );
    if effect < 35 {
        println!(
            "parameters={:02x?}",
            &profile[56 + effect * 2..58 + effect * 2]
        );
    }
    let target = match std::env::args().nth(1).as_deref() {
        Some("--on") => Some(true),
        Some("--off") => Some(false),
        None => None,
        _ => return Err("usage: probe_kzzi [--on|--off]".into()),
    };
    if let Some(on) = target {
        let mut hardware = periph_hal::Hardware::system()?;
        let snapshot = hardware.discover();
        let lighting = snapshot
            .devices
            .iter()
            .find_map(|device| {
                device
                    .capabilities
                    .get::<periph_hal::lighting::Lighting>("main")
            })
            .ok_or("lighting capability not found")?;
        lighting.set_state(on)?;
        println!("lighting={}", lighting.read_state()?);
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
}
