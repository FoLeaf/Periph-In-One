//! Render the real Slint UI with synthetic, non-identifying showcase data.
//! Run on Windows: cargo run --locked --example readme_screenshots

#[cfg(windows)]
slint::include_modules!();

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use slint::{ComponentHandle, ModelRc, VecModel};
    use std::{fs::File, io::BufWriter, time::Duration};

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()?;
    let ui = AppWindow::new()?;
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    let rows: Vec<_> = [
        (
            "办公鼠标",
            "mouse",
            "NK mouse NANO dongle",
            "373B:1216",
            "72%",
            0.72,
            true,
            "更新于 14:32",
        ),
        (
            "游戏手柄",
            "gamepad",
            "XBOX 360 For Windows",
            "413D:2104",
            "85%",
            0.85,
            true,
            "更新于 14:32",
        ),
        (
            "无线键盘",
            "keyboard",
            "Kzzi-K75 RGB key",
            "3554:FA09",
            "—",
            0.0,
            true,
            "该型号不提供电量",
        ),
    ]
    .into_iter()
    .map(
        |(title, kind, hardware, usb, percentage, level, fresh, updated)| DeviceRow {
            title: title.into(),
            kind: kind.into(),
            category: match kind {
                "mouse" => "鼠标 · USB",
                "keyboard" => "键盘 · USB",
                _ => "手柄 · USB",
            }
            .into(),
            hardware_name: hardware.into(),
            hardware_id: format!("VID: 0x{} · PID: 0x{}", &usb[..4], &usb[5..]).into(),
            percentage: percentage.into(),
            level,
            has_battery: fresh && level > 0.0,
            fresh,
            present: true,
            status: if fresh { "已连接" } else { "未响应" }.into(),
            updated: updated.into(),
            alias: title.into(),
            ..Default::default()
        },
    )
    .collect();
    ui.set_devices(ModelRc::new(VecModel::from(rows)));
    ui.set_summary("3 个设备 · 布局示例".into());
    ui.set_updated("14:32 更新 · 示例数据".into());
    ui.set_editing_name("NK mouse NANO dongle".into());
    ui.set_editing_serial("接收器序列号：未提供".into());
    ui.set_editing_hardware_id("VID: 0x373B · PID: 0x1216".into());
    ui.set_editing_alias("办公鼠标".into());
    ui.set_editing_has_name(true);
    ui.set_update_available(true);
    ui.set_update_version("0.24.0".into());
    ui.set_update_ready(true);
    ui.set_update_status("更新已就绪".into());
    ui.set_update_notes(ModelRc::new(VecModel::from(vec![
        ChangelogRow {
            text: "更新说明 · 布局示例".into(),
            heading: true,
            code: false,
        },
        ChangelogRow {
            text: "• 更新窗口使用对应 Release tag 的说明。".into(),
            heading: false,
            code: false,
        },
        ChangelogRow {
            text: "• 就绪后可重启并更新。".into(),
            heading: false,
            code: false,
        },
    ])));
    ui.window().set_size(slint::LogicalSize::new(740.0, 620.0));
    ui.show()?;
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    let mut step = 0;
    std::fs::create_dir_all("assets/readme")?;
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(600),
        move || {
            let ui = weak.unwrap();
            let names = ["overview", "remark", "settings", "update", "mini"];
            if step < names.len() {
                let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                    let pixels = ui.window().take_snapshot()?;
                    let file = File::create(format!("assets/readme/{}.png", names[step]))?;
                    let mut encoder =
                        png::Encoder::new(BufWriter::new(file), pixels.width(), pixels.height());
                    encoder.set_color(png::ColorType::Rgba);
                    encoder.set_depth(png::BitDepth::Eight);
                    encoder
                        .write_header()?
                        .write_image_data(pixels.as_bytes())?;
                    Ok(())
                })();
                if let Err(error) = result {
                    eprintln!("Snapshot failed: {error}");
                    std::process::exit(1);
                }
            }
            step += 1;
            ui.set_editing(step == 1);
            ui.set_settings_open(step == 2);
            ui.set_updates_open(step == 3);
            if step == 4 {
                ui.set_mini_mode(true);
                ui.window().set_size(slint::LogicalSize::new(280.0, 224.0));
            }
            if step == 5 {
                slint::quit_event_loop().unwrap();
            }
        },
    );
    slint::run_event_loop()?;
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("README screenshots require Windows and its system fonts.");
}
