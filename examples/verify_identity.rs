//! Identity-chain verification harness. Runs one scan/merge/persist cycle
//! against a scratch config (target/verify/config/settings.json), never the
//! installed app's config. Run it once per gamepad state change:
//! wireless -> docked -> wireless, then compare the printed records.
//!
//! `--alias <name>` sets a remark on the gamepad record first, mimicking a
//! user rename, so remark survival is part of the check.
use periph_in_one::{battery::Batteries, config::Config, devices, hardware, model};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let mut alias_request = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--alias" {
            alias_request = args.next().unwrap_or_default();
        }
    }

    // cargo run starts in the package root, so a cwd-relative scratch path
    // keeps the verification config out of the installed app's directory.
    let path = std::path::PathBuf::from("target/verify/config/settings.json");
    let config = Config::load(&path).unwrap_or_default();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let report = devices::scan().expect("扫描失败");
    let mut batteries = Batteries::restore(config.batteries.clone());
    let mut devices = hardware::merge_report(
        &model::restore(&config.devices),
        &mut batteries,
        report,
        now,
    );

    if !alias_request.is_empty() {
        for device in &mut devices {
            if device
                .saved
                .descriptor
                .roles
                .iter()
                .any(|role| role == "input.gamepad")
            {
                device.saved.alias = alias_request.clone();
            }
        }
        println!("(已把备注 {alias_request:?} 写到手柄记录上)");
    }

    Config {
        devices: devices.iter().map(|d| d.saved.clone()).collect(),
        batteries: batteries.saved.clone(),
        ..config
    }
    .save(&path)
    .expect("保存失败");

    println!("== 记录 {} 条 ==", devices.len());
    for device in &devices {
        println!(
            "name={:?} alias={:?} route={:?} connection={:?} battery={:?}",
            device.saved.descriptor.name,
            device.saved.alias,
            device.saved.descriptor.key,
            device.connection,
            batteries.view(&device.saved.id, "main").level
        );
        println!("        id={}", device.saved.id);
    }
}
