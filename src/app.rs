use crate::{AppWindow, DeviceRow, platform};
use chrono::{DateTime, Local};
use periph_in_one::{
    battery::Batteries,
    config::Config,
    devices,
    hardware::{self, ScanReport},
    model::{self, Device},
    presentation,
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tray_icon::{
    Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

pub enum Action {
    InitialShow,
    Refresh,
    ReadingStarted,
    Readings(Result<ScanReport, String>),
    Show,
    Expand,
    Hide,
    SaveSize,
    Edit(String),
    Remark(String, String),
    Rebind(String, usize),
    Copy(String),
    CopyName(String),
    CopyIds(String),
    Delete(String),
    Saved(u64, Result<(), String>),
    Autostart(bool),
    Quit,
}

enum WorkerCommand {
    Refresh,
    Stop,
}

struct SaveRequest {
    revision: u64,
    config: Config,
}

thread_local! { static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) }; }

pub fn post(action: Action) {
    let _ = slint::invoke_from_event_loop(move || {
        let app = APP.with(|slot| slot.borrow().clone());
        if let Some(app) = app {
            app.borrow_mut().handle(action);
        }
    });
}

struct App {
    ui: AppWindow,
    tray: TrayIcon,
    config: Config,
    config_error: Option<String>,
    devices: Vec<Device>,
    batteries: Batteries,
    worker: Sender<WorkerCommand>,
    cancel: std::sync::Arc<AtomicBool>,
    saver: Sender<SaveRequest>,
    save_revision: u64,
    save_notice: String,
    busy: bool,
    rebind_ids: Vec<String>,
    presenter: platform::Presenter,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn timestamp(time: u64) -> String {
    i64::try_from(time)
        .ok()
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
        .map(|date| {
            date.with_timezone(&Local)
                .format("%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "时间未知".into())
}

fn fallback_name(device: &Device) -> &str {
    presentation::title(device)
}

fn identity_note(device: &Device) -> &'static str {
    if device.saved.identity_conflict {
        "此身份信息不能唯一识别设备"
    } else {
        ""
    }
}

impl App {
    fn notice(&self, message: impl Into<slint::SharedString>) {
        let message = message.into();
        self.ui.set_notice(message.clone());
        let transient = matches!(
            &*message,
            "备注已保存"
                | "备注已重新绑定"
                | "序列号已复制"
                | "硬件名称已复制"
                | "硬件标识已复制"
                | "离线记录已删除"
                | "已启用开机自启"
                | "已关闭开机自启"
        );
        if transient {
            let weak = self.ui.as_weak();
            slint::Timer::single_shot(Duration::from_secs(3), move || {
                if let Some(ui) = weak.upgrade().filter(|ui| ui.get_notice() == message) {
                    ui.set_notice("".into());
                }
            });
        }
    }

    fn persist(&mut self, success: &str) -> bool {
        if self.config_error.is_some() {
            self.notice("无法保存修改，请检查 config/settings.json 后重启");
            return false;
        }
        self.config.devices = self.devices.iter().map(|d| d.saved.clone()).collect();
        self.config.batteries = self.batteries.saved.clone();
        self.save_revision += 1;
        if !success.is_empty() {
            self.save_notice = success.into();
        }
        if self
            .saver
            .send(SaveRequest {
                revision: self.save_revision,
                config: self.config.clone(),
            })
            .is_err()
        {
            self.notice("保存失败，请退出后重试");
            return false;
        }
        true
    }

    fn save_size(&mut self) {
        if self.ui.get_mini_mode() || !self.ui.window().is_visible() {
            return;
        }
        let size = self
            .ui
            .window()
            .size()
            .to_logical(self.ui.window().scale_factor());
        if self.config.width != size.width || self.config.height != size.height {
            self.config.width = size.width;
            self.config.height = size.height;
            self.persist("");
        }
    }

    fn update(&self) {
        let mut ordered: Vec<_> = self.devices.iter().collect();
        ordered.sort_by_key(|device| !device.present);
        let rows: Vec<DeviceRow> = ordered
            .into_iter()
            .map(|device| {
                let saved = &device.saved;
                let battery = self.batteries.view(&saved.id, "main");
                DeviceRow {
                    id: saved.id.clone().into(),
                    title: fallback_name(device).into(),
                    hardware_name: saved.descriptor.name.clone().into(),
                    serial: presentation::serial_label(&saved.descriptor).into(),
                    hardware_id: presentation::identifiers(&saved.descriptor).into(),
                    kind: presentation::icon(&saved.descriptor).into(),
                    category: presentation::category(&saved.descriptor).into(),
                    percentage: battery
                        .level
                        .map(|b| format!("{b}%"))
                        .unwrap_or("—".into())
                        .into(),
                    level: battery.level.unwrap_or(0) as f32 / 100.0,
                    fresh: device.connection == periph_hal::Connection::Connected,
                    has_battery: battery.current && battery.level.is_some(),
                    present: device.present,
                    status: presentation::status(device).into(),
                    updated: match battery.observed_at {
                        Some(_) if battery.current && battery.system_cached => "系统读数".into(),
                        Some(time) => format!(
                            "{} {}",
                            if battery.current {
                                "更新于"
                            } else {
                                "上次读数"
                            },
                            timestamp(time)
                        ),
                        None => presentation::battery_message(&battery).into(),
                    }
                    .into(),
                    alias: saved.alias.clone().into(),
                    identity_note: identity_note(device).into(),
                }
            })
            .collect();
        self.ui.set_devices(ModelRc::new(VecModel::from(rows)));
        let connected = self
            .devices
            .iter()
            .filter(|d| d.connection == periph_hal::Connection::Connected)
            .count();
        let present = self.devices.iter().filter(|d| d.present).count();
        let offline = self
            .devices
            .iter()
            .filter(|d| d.connection == periph_hal::Connection::Disconnected)
            .count();
        let mut summary = vec![format!("{connected} 台已连接")];
        if present > connected {
            summary.push(format!("{} 台未响应", present - connected));
        }
        if offline > 0 {
            summary.push(format!("{offline} 条离线记录"));
        }
        self.ui.set_summary(summary.join(" · ").into());
        let tooltip = if self.devices.is_empty() {
            "Periph In One · 等待设备".to_string()
        } else {
            let readings = self
                .devices
                .iter()
                .map(|d| {
                    let battery = self.batteries.view(&d.saved.id, "main");
                    let level = battery.level.map(|b| format!("{b}%")).unwrap_or("—".into());
                    let note = if !battery.current && battery.level.is_some() {
                        "（上次读数）"
                    } else if battery.unsupported {
                        "（不提供电量）"
                    } else if battery.available && battery.level.is_none() {
                        "（电量暂不可用）"
                    } else {
                        ""
                    };
                    format!("{}：{}{}", fallback_name(d), level, note)
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("Periph In One\n{readings}")
        };
        let _ = self
            .tray
            .set_tooltip(Some(tooltip.chars().take(120).collect::<String>()));
    }

    fn handle(&mut self, action: Action) {
        match action {
            Action::InitialShow => {
                if let Err(error) = self.presenter.show(
                    &self.ui,
                    slint::LogicalSize::new(self.config.width, self.config.height),
                    None,
                ) {
                    platform::error_dialog(&error);
                    let _ = slint::quit_event_loop();
                }
            }
            Action::Refresh => {
                if !self.busy {
                    self.busy = true;
                    self.ui.set_busy(true);
                    if self.worker.send(WorkerCommand::Refresh).is_err() {
                        self.busy = false;
                        self.ui.set_busy(false);
                        self.notice("读取线程已退出，请从托盘退出后重新启动");
                    }
                }
            }
            Action::ReadingStarted => {
                self.busy = true;
                self.ui.set_busy(true);
            }
            Action::Readings(result) => {
                self.busy = false;
                self.ui.set_busy(false);
                match result {
                    Ok(report) => {
                        let partial = !report.snapshot.failures.is_empty();
                        self.devices = hardware::merge_report(
                            &self.devices,
                            &mut self.batteries,
                            report,
                            now(),
                        );
                        self.ui.set_updated(
                            format!("上次刷新 {}", Local::now().format("%H:%M:%S")).into(),
                        );
                        self.persist("");
                        if partial {
                            self.notice("部分设备读取失败，请稍后刷新");
                        }
                    }
                    Err(error) => {
                        for device in &mut self.devices {
                            device.connection = periph_hal::Connection::Unknown;
                            device.issue = Some(periph_hal::HardwareError::new(
                                periph_hal::ErrorKind::Unavailable,
                                error.clone(),
                            ));
                        }
                        self.notice(error);
                    }
                }
                self.update();
            }
            Action::Show => {
                self.save_size();
                self.ui.set_mini_mode(true);
                self.ui.set_settings_open(false);
                self.ui.set_editing(false);
                self.ui.set_deleting(false);
                let size = slint::LogicalSize::new(280.0, self.ui.get_mini_height());
                if let Err(error) =
                    self.presenter
                        .show(&self.ui, size, Some(platform::tray_anchor(&self.tray)))
                {
                    self.notice(error);
                }
            }
            Action::Expand => {
                let anchor = platform::window_rect(self.ui.window());
                self.ui.set_mini_mode(false);
                self.ui.set_autostart(platform::autostart_enabled());
                if let Err(error) = self.presenter.show(
                    &self.ui,
                    slint::LogicalSize::new(self.config.width, self.config.height),
                    Some(anchor),
                ) {
                    self.notice(error);
                }
            }
            Action::Hide => {
                self.save_size();
                self.ui.set_settings_open(false);
                self.ui.set_editing(false);
                self.ui.set_deleting(false);
                let _ = self.presenter.hide(&self.ui);
            }
            Action::SaveSize => self.save_size(),
            Action::Edit(id) => {
                if let Some(device) = self.devices.iter().find(|d| d.saved.id == id) {
                    self.ui.set_editing_id(id.clone().into());
                    self.ui.set_editing_hardware_id(
                        presentation::identifiers(&device.saved.descriptor).into(),
                    );
                    self.ui.set_editing_name(
                        format!("硬件名称：{}", device.saved.descriptor.name).into(),
                    );
                    self.ui
                        .set_editing_has_name(!device.saved.descriptor.name.is_empty());
                    self.ui.set_editing_has_serial(
                        presentation::serial(&device.saved.descriptor).is_some(),
                    );
                    self.ui.set_editing_alias(device.saved.alias.clone().into());
                    self.ui.set_editing_serial(
                        presentation::serial_label(&device.saved.descriptor).into(),
                    );
                    self.ui.set_editing_identity(identity_note(device).into());
                    self.rebind_ids.clear();
                    let mut names = Vec::<slint::SharedString>::new();
                    if device.present {
                        for other in self
                            .devices
                            .iter()
                            .filter(|d| !d.present && !d.saved.alias.is_empty() && d.saved.id != id)
                        {
                            self.rebind_ids.push(other.saved.id.clone());
                            names.push(
                                format!("{} · {}", other.saved.alias, other.saved.descriptor.name)
                                    .into(),
                            );
                        }
                    }
                    self.ui
                        .set_rebind_options(ModelRc::new(VecModel::from(names)));
                    self.ui.set_settings_open(false);
                    self.ui.set_editing(true);
                }
            }
            Action::Remark(id, alias) => {
                let alias = alias.trim();
                if alias.chars().count() > 64 {
                    self.notice("备注最多 64 个字符");
                    return;
                }
                if let Some(device) = self.devices.iter_mut().find(|d| d.saved.id == id) {
                    device.saved.alias = alias.into();
                }
                if self.persist("备注已保存") {
                    self.ui.set_editing(false);
                }
                self.update();
            }
            Action::Rebind(target, index) => {
                let Some(source) = self.rebind_ids.get(index).cloned() else {
                    self.notice("请先选择离线记录");
                    return;
                };
                match model::rebind(&mut self.devices, &source, &target) {
                    Ok(()) => {
                        if self.persist("备注已重新绑定") {
                            self.ui.set_editing(false);
                        }
                        self.update();
                    }
                    Err(error) => self.notice(error),
                }
            }
            Action::Copy(id) => {
                if let Some(device) = self.devices.iter().find(|d| d.saved.id == id) {
                    let Some(serial) = presentation::serial(&device.saved.descriptor) else {
                        self.notice("设备未提供序列号");
                        return;
                    };
                    match platform::copy_text(&serial.value) {
                        Ok(()) => self.notice("序列号已复制"),
                        Err(error) => self.notice(error),
                    }
                }
            }
            Action::CopyName(id) => {
                if let Some(device) = self.devices.iter().find(|d| d.saved.id == id) {
                    if device.saved.descriptor.name.is_empty() {
                        self.notice("设备未提供硬件名称");
                        return;
                    }
                    match platform::copy_text(&device.saved.descriptor.name) {
                        Ok(()) => self.notice("硬件名称已复制"),
                        Err(error) => self.notice(error),
                    }
                }
            }
            Action::CopyIds(id) => {
                if let Some(device) = self.devices.iter().find(|d| d.saved.id == id) {
                    match platform::copy_text(&presentation::identifiers(&device.saved.descriptor))
                    {
                        Ok(()) => self.notice("硬件标识已复制"),
                        Err(error) => self.notice(error),
                    }
                }
            }
            Action::Delete(id) => {
                let previous = self.devices.clone();
                let previous_batteries = self.batteries.clone();
                match model::remove_offline(&mut self.devices, &id) {
                    Ok(()) => {
                        self.batteries.remove(&id);
                        if self.persist("离线记录已删除") {
                            self.ui.set_deleting(false);
                        } else {
                            self.devices = previous;
                            self.batteries = previous_batteries;
                        }
                        self.update();
                    }
                    Err(error) => {
                        self.ui.set_deleting(false);
                        self.notice(error);
                    }
                }
            }
            Action::Saved(revision, result) => {
                // Older completions must not clear a newer error or confirm a
                // modification that has not reached disk yet.
                if revision == self.save_revision {
                    match result {
                        Ok(()) => {
                            if !self.save_notice.is_empty() {
                                let message = std::mem::take(&mut self.save_notice);
                                self.notice(message);
                            }
                        }
                        Err(error) => {
                            self.save_notice.clear();
                            self.notice(error);
                        }
                    }
                }
            }
            Action::Autostart(enabled) => {
                match platform::set_autostart(enabled) {
                    Ok(()) => self.notice(if enabled {
                        "已启用开机自启"
                    } else {
                        "已关闭开机自启"
                    }),
                    Err(_) => self.notice("无法修改开机启动，请稍后重试"),
                }
                self.ui.set_autostart(platform::autostart_enabled());
            }
            Action::Quit => {
                self.save_size();
                self.cancel.store(true, Ordering::Release);
                let _ = self.worker.send(WorkerCommand::Stop);
                let _ = slint::quit_event_loop();
            }
        }
    }
}

fn tray_icon() -> Result<TrayIcon, String> {
    let menu = Menu::new();
    let show = MenuItem::new("显示面板", true, None);
    let refresh = MenuItem::new("立即刷新", true, None);
    let quit = MenuItem::new("退出", true, None);
    menu.append_items(&[&show, &refresh, &PredefinedMenuItem::separator(), &quit])
        .map_err(|e| e.to_string())?;
    let show_id = show.id().clone();
    let refresh_id = refresh.id().clone();
    let quit_id = quit.id().clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == show_id {
            post(Action::Show);
        } else if event.id == refresh_id {
            post(Action::Refresh);
        } else if event.id == quit_id {
            post(Action::Quit);
        }
    }));
    TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
        if matches!(
            event,
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        ) {
            post(Action::Show);
        }
    }));
    let icon = Icon::from_rgba(include_bytes!("../assets/tray-icon.rgba").to_vec(), 32, 32)
        .map_err(|e| e.to_string())?;
    TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Periph In One")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .map_err(|e| e.to_string())
}

pub fn run() -> Result<(), String> {
    // Windows may discard a hidden software surface without resetting its age.
    // Configure Slint before any threads start; each show gets a fresh surface.
    unsafe {
        std::env::set_var("SLINT_DESTROY_WINDOW_ON_HIDE", "1");
    }

    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.iter().any(|a| a == "--list-devices") {
        let identifiers = devices::enumerate_identifiers()?;
        println!(
            "{}",
            serde_json::to_string_pretty(&identifiers).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if arguments.iter().any(|a| a == "--once") {
        let report = devices::scan()?;
        let mut batteries = Batteries::default();
        let devices = hardware::merge_report(&[], &mut batteries, report, now());
        for device in devices {
            let battery = batteries.view(&device.saved.id, "main");
            let level = battery
                .level
                .map(|level| format!("{level}%"))
                .unwrap_or("—".into());
            let note = if battery.unsupported {
                presentation::battery_message(&battery)
            } else {
                presentation::status(&device)
            };
            println!("{}: {} ({})", fallback_name(&device), level, note);
        }
        return Ok(());
    }
    let autostart = arguments.iter().any(|a| a == "--autostart");
    platform::initialize_taskbar()?;
    let Some(_instance) = platform::single_instance(autostart)? else {
        return Ok(());
    };
    let _ipc = platform::ipc_window()?;
    unsafe {
        windows_sys::Win32::System::Com::CoInitializeEx(
            std::ptr::null(),
            windows_sys::Win32::System::Com::COINIT_APARTMENTTHREADED as u32,
        );
    }
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()
        .map_err(|e| e.to_string())?;
    let ui = AppWindow::new().map_err(|e| e.to_string())?;
    let path = Config::path().map_err(|e| e.to_string())?;
    let (config, config_error) = match Config::load(&path) {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(e.to_string())),
    };
    ui.window()
        .set_size(slint::LogicalSize::new(config.width, config.height));
    let initial = model::restore(&config.devices);
    let batteries = Batteries::restore(config.batteries.clone());
    let tray = tray_icon()?;
    let (sender, receiver) = mpsc::channel();
    let (saver, saves) = mpsc::channel::<SaveRequest>();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    // Keep durable atomic replacement, including sync_all, off the UI thread
    // and independent of slow HID queries. FIFO snapshots preserve edit order.
    let save_worker = thread::Builder::new()
        .name("settings-writer".into())
        .spawn(move || {
            for request in saves {
                let result = request
                    .config
                    .save(&path)
                    .map_err(|_| "保存失败，请检查目录权限和可用空间".to_string());
                post(Action::Saved(request.revision, result));
            }
        })
        .map_err(|e| e.to_string())?;
    let app = Rc::new(RefCell::new(App {
        ui: ui.clone_strong(),
        tray,
        config,
        config_error,
        devices: initial,
        batteries,
        worker: sender.clone(),
        cancel: cancel.clone(),
        saver,
        save_revision: 0,
        save_notice: String::new(),
        busy: true,
        rebind_ids: vec![],
        presenter: platform::Presenter::default(),
    }));
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
    ui.on_refresh(|| post(Action::Refresh));
    ui.on_hide_window(|| post(Action::Hide));
    ui.on_expand(|| post(Action::Expand));
    ui.on_edit_device(|id| post(Action::Edit(id.into())));
    ui.on_save_remark(|id, alias| post(Action::Remark(id.into(), alias.into())));
    ui.on_rebind_remark(|id, index| {
        if index >= 0 {
            post(Action::Rebind(id.into(), index as usize));
        }
    });
    ui.on_copy_serial(|id| post(Action::Copy(id.into())));
    ui.on_copy_name(|id| post(Action::CopyName(id.into())));
    ui.on_copy_hardware_id(|id| post(Action::CopyIds(id.into())));
    ui.on_delete_device(|id| post(Action::Delete(id.into())));
    ui.on_toggle_autostart(|enabled| post(Action::Autostart(enabled)));
    ui.window().on_close_requested(|| {
        post(Action::Hide);
        slint::CloseRequestResponse::KeepWindowShown
    });
    ui.set_autostart(platform::autostart_enabled());
    ui.set_busy(true);
    app.borrow().update();
    if app.borrow().config_error.is_some() {
        app.borrow()
            .notice("配置读取失败，请检查 config/settings.json 后重启");
    }
    // Winit creates its HWND only while the event loop is active.
    // Startup stays completely hidden; normal launches show on the first tick.
    if !autostart {
        post(Action::InitialShow);
    }
    let worker_cancel = cancel.clone();
    let worker = thread::Builder::new()
        .name("battery-reader".into())
        .spawn(move || {
            let mut hardware = periph_hal::Hardware::system()
                .map_err(|_| "当前系统没有可用的设备后端".to_string());
            loop {
                post(Action::ReadingStarted);
                let report = match hardware.as_mut() {
                    Ok(hardware) => Ok(devices::refresh_cancellable(hardware, &worker_cancel)),
                    Err(error) => Err(error.clone()),
                };
                post(Action::Readings(report));
                match receiver.recv_timeout(Duration::from_secs(60)) {
                    Ok(WorkerCommand::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                    Ok(WorkerCommand::Refresh) | Err(RecvTimeoutError::Timeout) => {}
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let result = slint::run_event_loop_until_quit().map_err(|e| e.to_string());
    cancel.store(true, Ordering::Release);
    let _ = sender.send(WorkerCommand::Stop);
    APP.with(|slot| *slot.borrow_mut() = None);
    drop(app);
    ui.hide().ok();
    let _ = worker.join();
    let _ = save_worker.join();
    unsafe {
        windows_sys::Win32::System::Com::CoUninitialize();
    }
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn malformed_saved_timestamp_cannot_crash_the_panel() {
        assert_eq!(super::timestamp(u64::MAX), "时间未知");
    }
}
