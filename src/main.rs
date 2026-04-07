mod ble;
mod pinecil;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::sync::mpsc;
use std::time::Duration;

use ble::{BleCommand, BleEvent, FoundDevice, start_ble_thread};
use pinecil::{LiveData, OperatingMode};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Application, AsyncApp, Bounds, Context, Decorations, FontWeight,
    MouseButton, SharedString, WeakEntity, Window, WindowBounds,
    WindowDecorations, WindowOptions, div, px, rgb, size,
};

// ── IBM Carbon Gray 100 + extended palette ────────────────────────────────────
const C_BG: u32 = 0x161616;
const C_LAYER1: u32 = 0x262626;
const C_LAYER2: u32 = 0x393939;
const C_LAYER3: u32 = 0x4c4c4c;
const C_TEXT: u32 = 0xf4f4f4;
const C_TEXT2: u32 = 0xc6c6c6;
const C_TEXT3: u32 = 0x6f6f6f;
const C_BLUE: u32 = 0x78a9ff;
const C_BLUE_BTN: u32 = 0x0f62fe;
const C_BLUE_HOV: u32 = 0x0353e9;
const C_BLUE_TEN: u32 = 0x0043ce;
const C_GREEN: u32 = 0x42be65;
const C_GREEN_DARK: u32 = 0x24a148;
const C_YELLOW: u32 = 0xf1c21b;
const C_ORANGE: u32 = 0xff832b;
const C_RED: u32 = 0xda1e28;
const C_BORDER: u32 = 0x393939;

// ── Temperature → color mapping ───────────────────────────────────────────────
fn tip_color(t: f32) -> u32 {
    if t >= 400.0 { C_RED }
    else if t >= 300.0 { C_ORANGE }
    else if t >= 200.0 { C_YELLOW }
    else if t >= 100.0 { C_BLUE }
    else { C_TEXT2 }
}

// ── Operating mode badge colors ───────────────────────────────────────────────
fn mode_colors(mode: &OperatingMode) -> (u32 /* bg */, u32 /* fg */) {
    match mode {
        OperatingMode::Soldering => (0x044317, C_GREEN),
        OperatingMode::Boost     => (0x3d1a00, C_ORANGE),
        OperatingMode::Sleeping  => (0x001d6c, C_BLUE),
        OperatingMode::Standby   => (C_LAYER2, C_TEXT2),
        OperatingMode::Debug     => (C_LAYER2, C_YELLOW),
        _                        => (C_LAYER1, C_TEXT3),
    }
}

// ── Multi-step LIVE dot animation (6-phase, ~900 ms cycle) ────────────────────
fn live_dot_color(tick: u32) -> u32 {
    match tick % 6 {
        0 => C_GREEN,
        1 => C_GREEN_DARK,
        2 => 0x198038,
        3 => 0x0e6027,
        4 => 0x198038,
        _ => C_GREEN_DARK,
    }
}

// ── Per-device state ──────────────────────────────────────────────────────────
#[derive(Clone)]
struct DeviceState {
    name: String,
    build_info: Option<String>,
    data: Option<LiveData>,
    local_setpoint: u16,
    tick: u32,
}

// ── Root view ─────────────────────────────────────────────────────────────────
struct PinemonitorView {
    scan_list: Vec<FoundDevice>,
    connecting: HashSet<String>,
    devices: HashMap<String, DeviceState>,
    show_scan: bool,
    events: Arc<Mutex<Vec<BleEvent>>>,
    cmd_tx: mpsc::Sender<BleCommand>,
    global_tick: u32,
    error: Option<String>,
}

impl PinemonitorView {
    fn new(cx: &mut Context<Self>) -> Self {
        let events: Arc<Mutex<Vec<BleEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let (cmd_tx, cmd_rx) = mpsc::channel::<BleCommand>();
        start_ble_thread(events.clone(), cmd_rx);

        cx.spawn(async |entity: WeakEntity<PinemonitorView>, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                entity
                    .update(cx, |view, cx| {
                        view.drain_events(cx);
                        view.global_tick = view.global_tick.wrapping_add(1);
                        cx.notify();
                    })
                    .ok();
            }
        })
        .detach();

        Self {
            scan_list: vec![],
            connecting: HashSet::new(),
            devices: HashMap::new(),
            show_scan: true,
            events,
            cmd_tx,
            global_tick: 0,
            error: None,
        }
    }

    fn drain_events(&mut self, _cx: &mut Context<Self>) {
        let batch: Vec<BleEvent> = self
            .events
            .lock()
            .map(|mut g| g.drain(..).collect())
            .unwrap_or_default();

        for event in batch {
            match event {
                BleEvent::DeviceFound(dev) => {
                    if !self.scan_list.iter().any(|d| d.id == dev.id) {
                        self.scan_list.push(dev);
                    }
                }
                BleEvent::DeviceLost(id) => {
                    self.scan_list.retain(|d| d.id != id);
                    // If we were mid-connect when it vanished, unblock the UI
                    self.connecting.remove(&id);
                }
                BleEvent::ConnectFailed(id) => {
                    self.connecting.remove(&id);
                }
                BleEvent::ScanTick => {}
                BleEvent::Connected(id, name) => {
                    self.connecting.remove(&id);
                    self.devices.insert(
                        id,
                        DeviceState {
                            name,
                            build_info: None,
                            data: None,
                            local_setpoint: 350,
                            tick: 0,
                        },
                    );
                    self.show_scan = false;
                    self.error = None;
                }
                BleEvent::Disconnected(id) => {
                    self.devices.remove(&id);
                    self.connecting.remove(&id);
                    if self.devices.is_empty() && self.connecting.is_empty() {
                        self.show_scan = true;
                    }
                }
                BleEvent::LiveData(id, incoming) => {
                    if let Some(dev) = self.devices.get_mut(&id) {
                        // On first valid frame, sync setpoint from device
                        // but only if it looks sane (device actually has one set)
                        if dev.data.is_none() && incoming.setpoint >= 50.0 {
                            dev.local_setpoint = incoming.setpoint as u16;
                        }
                        dev.tick = dev.tick.wrapping_add(1);
                        dev.data = Some(*incoming);
                    }
                }
                BleEvent::BuildInfo(id, info) => {
                    if let Some(dev) = self.devices.get_mut(&id) {
                        dev.build_info = Some(info);
                    }
                }
                BleEvent::Error(msg) => {
                    self.error = Some(msg);
                }
            }
        }
    }

    fn adjust_setpoint(&mut self, id: &str, delta: i16) {
        if let Some(dev) = self.devices.get_mut(id) {
            let next = (dev.local_setpoint as i16 + delta).clamp(100, 450) as u16;
            dev.local_setpoint = next;
            let _ = self
                .cmd_tx
                .send(BleCommand::SetTemperature(id.to_string(), next));
        }
    }

    // ── Shared draggable titlebar ─────────────────────────────────────────────
    fn titlebar(&self, window: &Window, subtitle: &str) -> impl IntoElement + use<> {
        let decorations = window.window_decorations();
        let subtitle = subtitle.to_string();
        div()
            .w_full()
            .h(px(48.))
            .bg(rgb(C_BG))
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(C_BORDER))
            .map(|d| match decorations {
                Decorations::Client { .. } => d.on_mouse_down(
                    MouseButton::Left,
                    |_ev, window, _cx| window.start_window_move(),
                ),
                Decorations::Server => d,
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .child(
                        div()
                            .text_color(rgb(C_TEXT))
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("PINEMONITOR"),
                    )
                    .child(
                        div()
                            .text_color(rgb(C_TEXT3))
                            .text_size(px(12.))
                            .child("·"),
                    )
                    .child(
                        div()
                            .text_color(rgb(C_TEXT3))
                            .text_size(px(12.))
                            .child(SharedString::from(subtitle)),
                    ),
            )
            .child(match decorations {
                Decorations::Client { .. } => div()
                    .id("titlebar-close")
                    .w(px(48.))
                    .h(px(48.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(C_TEXT3))
                    .text_size(px(14.))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(0x520408)).text_color(rgb(C_RED)))
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(|_ev, _window, cx| cx.quit())
                    .child("✕")
                    .into_any_element(),
                Decorations::Server => div().w(px(0.)).into_any_element(),
            })
    }

    // ── Scan / device picker ──────────────────────────────────────────────────
    fn view_scan(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let devices = self.scan_list.clone();
        let connecting = self.connecting.clone();
        let connected_ids: HashSet<String> = self.devices.keys().cloned().collect();
        let has_connected = !self.devices.is_empty();
        let tick = self.global_tick;
        let total = self.devices.len() + self.connecting.len();

        // Scan pulse: 8-step cycle
        let scan_phase = tick % 16;
        let pulse_color = if scan_phase < 4 { C_GREEN }
            else if scan_phase < 8 { C_GREEN_DARK }
            else if scan_phase < 12 { 0x198038 }
            else { C_LAYER2 };
        // Animated ellipsis on the scanning label
        let dot_count = ((tick / 3) % 4) as usize;
        let dots: &str = ["", ".", "..", "..."][dot_count];

        let header = self.titlebar(window, "BLE Scanner");

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(C_BG))
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .px_6()
                    .pt_6()
                    .pb_4()
                    .gap_4()
                    // ── Hero ──────────────────────────────────────────────────
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(div().w(px(10.)).h(px(10.)).bg(rgb(pulse_color)))
                                            .child(
                                                div()
                                                    .text_color(rgb(C_TEXT))
                                                    .text_size(px(22.))
                                                    .font_weight(FontWeight::LIGHT)
                                                    .child("Scanning"),
                                            )
                                            .child(
                                                div()
                                                    .text_color(rgb(C_GREEN))
                                                    .text_size(px(22.))
                                                    .font_weight(FontWeight::LIGHT)
                                                    .child(SharedString::from(dots.to_string())),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_color(rgb(C_TEXT3))
                                            .text_size(px(12.))
                                            .child(if devices.is_empty() {
                                                "Looking for nearby Pinecil devices"
                                            } else {
                                                "Tap a device to connect  ·  max 2"
                                            }),
                                    ),
                            )
                            .child(if has_connected {
                                div()
                                    .id("back-btn")
                                    .h(px(32.))
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .bg(rgb(C_LAYER1))
                                    .border_1()
                                    .border_color(rgb(C_BORDER))
                                    .text_color(rgb(C_TEXT2))
                                    .text_size(px(12.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(C_LAYER2)))
                                    .on_click(cx.listener(|view, _ev, _win, _cx| {
                                        view.show_scan = false;
                                    }))
                                    .child("← Dashboard")
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            }),
                    )
                    // ── Device list ───────────────────────────────────────────
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_px()
                            .children(devices.into_iter().enumerate().map(|(i, dev)| {
                                let cid = dev.id.clone();
                                let name = dev.name.clone();
                                let is_conn = connected_ids.contains(&dev.id);
                                let is_connecting = connecting.contains(&dev.id);

                                // Connecting blink
                                let dot_c = if is_conn { C_GREEN }
                                    else if is_connecting {
                                        if (tick / 3) % 2 == 0 { C_BLUE } else { C_LAYER3 }
                                    }
                                    else { C_TEXT3 };

                                let btn_bg = if is_conn { 0x044317 }
                                    else if is_connecting { C_LAYER2 }
                                    else { C_BLUE_BTN };
                                let btn_fg = if is_conn { C_GREEN }
                                    else { 0xffffff };
                                let btn_label: &'static str = if is_conn { "● Connected" }
                                    else if is_connecting { "Connecting…" }
                                    else if total >= 2 { "Max reached" }
                                    else { "Connect →" };
                                let can_connect = !is_conn && !is_connecting && total < 2;

                                div()
                                    .id(i)
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .w_full()
                                    .h(px(60.))
                                    .px_4()
                                    .bg(rgb(C_LAYER1))
                                    .border_b_1()
                                    .border_color(rgb(C_BORDER))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(C_LAYER2)))
                                    .active(|s| s.bg(rgb(C_LAYER3)))
                                    .on_click(cx.listener(move |view, _ev, _win, _cx| {
                                        if can_connect {
                                            view.connecting.insert(cid.clone());
                                            let _ = view.cmd_tx.send(BleCommand::Connect(cid.clone()));
                                        }
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .child(div().w(px(8.)).h(px(8.)).bg(rgb(dot_c)))
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .child(
                                                        div()
                                                            .text_color(rgb(C_TEXT))
                                                            .text_size(px(14.))
                                                            .child(SharedString::from(name)),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_color(rgb(C_TEXT3))
                                                            .text_size(px(11.))
                                                            .child("Pinecil  ·  IronOS"),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .h(px(32.))
                                            .px_3()
                                            .flex()
                                            .items_center()
                                            .bg(rgb(btn_bg))
                                            .border_1()
                                            .border_color(rgb(if is_conn { C_GREEN_DARK } else { C_LAYER3 }))
                                            .text_color(rgb(btn_fg))
                                            .text_size(px(12.))
                                            .child(btn_label),
                                    )
                            })),
                    )
                    // ── Footer ────────────────────────────────────────────────
                    .child(
                        div()
                            .mt_auto()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_color(rgb(C_TEXT3))
                                    .text_size(px(11.))
                                    .child(SharedString::from(format!(
                                        "{}/2 connected",
                                        total
                                    ))),
                            )
                            .child(
                                div()
                                    .text_color(rgb(C_TEXT3))
                                    .text_size(px(11.))
                                    .child("Refreshes every 5 s"),
                            ),
                    ),
            )
            .into_any_element()
    }

    // ── Dashboard wrapper (1 or 2 devices stacked) ────────────────────────────
    fn view_dashboard(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let device_ids: Vec<String> = self.devices.keys().cloned().collect();
        let subtitle = if device_ids.len() == 1 {
            self.devices[&device_ids[0]].name.clone()
        } else {
            format!("{} devices", device_ids.len())
        };
        let header = self.titlebar(window, &subtitle);

        let mut panels: Vec<AnyElement> = Vec::new();
        for id in &device_ids {
            panels.push(self.render_device_panel(id, cx));
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(C_BG))
            .child(header)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .h(px(32.))
                    .bg(rgb(C_LAYER1))
                    .border_b_1()
                    .border_color(rgb(C_BORDER))
                    .child(
                        div()
                            .text_color(rgb(C_TEXT3))
                            .text_size(px(11.))
                            .child(SharedString::from(format!(
                                "{} device{} connected",
                                device_ids.len(),
                                if device_ids.len() == 1 { "" } else { "s" }
                            ))),
                    )
                    .child(
                        div()
                            .id("scan-toggle")
                            .h(px(20.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .bg(rgb(C_LAYER2))
                            .text_color(rgb(C_BLUE))
                            .text_size(px(11.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(C_LAYER3)))
                            .on_click(cx.listener(|view, _ev, _win, _cx| {
                                view.show_scan = true;
                            }))
                            .child("+ Add"),
                    ),
            )
            .child(div().flex().flex_col().flex_1().children(panels))
            .into_any_element()
    }

    // ── Single device panel ───────────────────────────────────────────────────
    fn render_device_panel(&mut self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let dev = match self.devices.get(id) {
            Some(d) => d.clone(),
            None => return div().into_any_element(),
        };

        let tick = dev.tick;
        let local_sp = dev.local_setpoint;
        let id_str = id.to_string();

        let panel: AnyElement = match dev.data {
            // ── No data yet ──────────────────────────────────────────────────
            None => {
                let dc_id = id_str.clone();
                let disconnect = cx.listener(move |view, _ev, _win, _cx| {
                    let _ = view.cmd_tx.send(BleCommand::Disconnect(dc_id.clone()));
                    view.devices.remove(&dc_id);
                    if view.devices.is_empty() { view.show_scan = true; }
                });
                let su_id = id_str.clone();
                let sp_up = cx.listener(move |view, _ev, _win, _cx| { view.adjust_setpoint(&su_id, 5); });
                let sd_id = id_str.clone();
                let sp_dn = cx.listener(move |view, _ev, _win, _cx| { view.adjust_setpoint(&sd_id, -5); });

                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .px_6()
                    .py_6()
                    .gap_4()
                    .child(
                        div()
                            .text_color(rgb(C_TEXT2))
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(dev.name.clone())),
                    )
                    .child(setpoint_row(local_sp, sp_up, sp_dn, id_str.as_str()))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(rgb(C_TEXT3))
                                    .text_size(px(13.))
                                    .child("Waiting for live data…"),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("dc-wait-{}", id_str)))
                            .h(px(32.))
                            .px_3()
                            .flex()
                            .items_center()
                            .text_color(rgb(C_TEXT3))
                            .text_size(px(12.))
                            .border_1()
                            .border_color(rgb(C_BORDER))
                            .cursor_pointer()
                            .hover(|s| s.border_color(rgb(C_RED)).text_color(rgb(C_RED)))
                            .on_click(disconnect)
                            .child("Disconnect"),
                    )
                    .into_any_element()
            }

            // ── Live data ─────────────────────────────────────────────────────
            Some(live) => {
                let dc_id = id_str.clone();
                let disconnect = cx.listener(move |view, _ev, _win, _cx| {
                    let _ = view.cmd_tx.send(BleCommand::Disconnect(dc_id.clone()));
                    view.devices.remove(&dc_id);
                    if view.devices.is_empty() { view.show_scan = true; }
                });
                let su_id = id_str.clone();
                let sp_up = cx.listener(move |view, _ev, _win, _cx| { view.adjust_setpoint(&su_id, 5); });
                let sd_id = id_str.clone();
                let sp_dn = cx.listener(move |view, _ev, _win, _cx| { view.adjust_setpoint(&sd_id, -5); });

                let tc = tip_color(live.tip_temp);
                let live_c = live_dot_color(tick);
                let (mode_bg, mode_fg) = mode_colors(&live.operating_mode);
                let idle_warn = live.last_move_secs > 60;
                let bar_pct = (live.power_pwm as f32 / 100.0).clamp(0.0, 1.0);
                // Bar width scales to available space (280px inner)
                let bar_w = bar_pct * 280.0;
                // Heating indicator: tip is meaningfully below setpoint
                let is_heating = live.setpoint > 50.0
                    && live.tip_temp < live.setpoint - 5.0
                    && live.power_pwm > 0;

                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    // ── Temperature hero ──────────────────────────────────────
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .justify_between()
                            .px_6()
                            .pt_5()
                            .pb_4()
                            .border_b_1()
                            .border_color(rgb(C_BORDER))
                            .child(
                                // Left: actual tip temp (big)
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_color(rgb(C_TEXT3))
                                            .text_size(px(10.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("TIP TEMP"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_baseline()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_color(rgb(tc))
                                                    .text_size(px(60.))
                                                    .font_weight(FontWeight::LIGHT)
                                                    .child(format!("{:.0}", live.tip_temp)),
                                            )
                                            .child(
                                                div()
                                                    .text_color(rgb(tc))
                                                    .text_size(px(24.))
                                                    .child("°C"),
                                            ),
                                    ),
                            )
                            .child(
                                // Right: setpoint + status cluster
                                div()
                                    .flex()
                                    .flex_col()
                                    .items_end()
                                    .gap_2()
                                    // LIVE badge
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .child(div().w(px(6.)).h(px(6.)).bg(rgb(live_c)))
                                            .child(
                                                div()
                                                    .text_color(rgb(live_c))
                                                    .text_size(px(9.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child("LIVE"),
                                            ),
                                    )
                                    // Mode badge
                                    .child(
                                        div()
                                            .px_2()
                                            .py(px(3.))
                                            .bg(rgb(mode_bg))
                                            .text_color(rgb(mode_fg))
                                            .text_size(px(10.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(live.operating_mode.to_string()),
                                    )
                                    // Setpoint reference
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .items_end()
                                            .child(
                                                div()
                                                    .text_color(rgb(C_TEXT3))
                                                    .text_size(px(10.))
                                                    .child(if is_heating { "HEATING TO" } else { "SETPOINT" }),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_baseline()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_color(rgb(C_TEXT2))
                                                            .text_size(px(28.))
                                                            .font_weight(FontWeight::LIGHT)
                                                            .child(format!("{:.0}", live.setpoint)),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_color(rgb(C_TEXT3))
                                                            .text_size(px(14.))
                                                            .child("°C"),
                                                    ),
                                            ),
                                    )
                                    // Watts
                                    .child(
                                        div()
                                            .text_color(rgb(C_TEXT2))
                                            .text_size(px(13.))
                                            .child(format!("{:.1} W", live.estimated_watts)),
                                    ),
                            ),
                    )
                    // ── Setpoint control ──────────────────────────────────────
                    .child(
                        div()
                            .px_6()
                            .py_3()
                            .border_b_1()
                            .border_color(rgb(C_BORDER))
                            .child(setpoint_row(local_sp, sp_up, sp_dn, id_str.as_str())),
                    )
                    // ── Power bar ─────────────────────────────────────────────
                    .child(
                        div()
                            .px_6()
                            .pt_3()
                            .pb_2()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .border_b_1()
                            .border_color(rgb(C_BORDER))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_color(rgb(C_TEXT3))
                                            .text_size(px(10.))
                                            .child("HEATER  PWM"),
                                    )
                                    .child(
                                        div()
                                            .text_color(rgb(if bar_pct > 0.7 { C_ORANGE } else { C_TEXT2 }))
                                            .text_size(px(12.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(SharedString::from(format!("{}%", live.power_pwm))),
                                    ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(4.))
                                    .bg(rgb(C_LAYER2))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(px(bar_w))
                                            .bg(rgb(if bar_pct > 0.85 { C_RED }
                                                else if bar_pct > 0.6 { C_ORANGE }
                                                else { C_BLUE_BTN })),
                                    ),
                            ),
                    )
                    // ── Stats grid ────────────────────────────────────────────
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(stat_row("VOLTAGE",       format!("{:.2} V",  live.voltage),          C_TEXT,  false))
                            .child(stat_row("HANDLE TEMP",   format!("{:.0} °C", live.handle_temp),      C_TEXT,  true))
                            .child(stat_row("SOURCE",        live.power_source.to_string(),               C_TEXT,  false))
                            .child(stat_row("TIP RESISTANCE",format!("{:.1} Ω",  live.tip_resistance),   C_TEXT,  true))
                            .child(stat_row(
                                "IDLE FOR",
                                fmt_duration(live.last_move_secs),
                                if idle_warn { C_YELLOW } else { C_TEXT },
                                false,
                            ))
                            .child(stat_row("UPTIME", fmt_duration(live.uptime_secs), C_TEXT, true)),
                    )
                    // ── Footer ────────────────────────────────────────────────
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_6()
                            .py_2()
                            .mt_auto()
                            .border_t_1()
                            .border_color(rgb(C_BORDER))
                            .child(
                                div()
                                    .text_color(rgb(C_TEXT3))
                                    .text_size(px(10.))
                                    .child(SharedString::from(
                                        dev.build_info.as_deref().unwrap_or("IronOS").to_string(),
                                    )),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("dc-{}", id_str)))
                                    .h(px(24.))
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .text_color(rgb(C_TEXT3))
                                    .text_size(px(11.))
                                    .border_1()
                                    .border_color(rgb(C_BORDER))
                                    .cursor_pointer()
                                    .hover(|s| s.border_color(rgb(C_RED)).text_color(rgb(C_RED)))
                                    .on_click(disconnect)
                                    .child("Disconnect"),
                            ),
                    )
                    .into_any_element()
            }
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .border_b_1()
            .border_color(rgb(C_BORDER))
            .child(panel)
            .into_any_element()
    }

    // ── Error screen ──────────────────────────────────────────────────────────
    fn view_error(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let header = self.titlebar(window, "Error");
        let msg = self.error.clone().unwrap_or_default();
        let retry = cx.listener(|view, _ev, _win, _cx| {
            view.error = None;
            view.show_scan = true;
        });
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(C_BG))
            .child(header)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .p_8()
                    .child(
                        div()
                            .text_color(rgb(C_TEXT3))
                            .text_size(px(11.))
                            .child("BLE ERROR"),
                    )
                    .child(
                        div()
                            .text_color(rgb(C_RED))
                            .text_size(px(14.))
                            .child(SharedString::from(msg)),
                    )
                    .child(
                        div()
                            .id("retry-btn")
                            .h(px(40.))
                            .px_6()
                            .flex()
                            .items_center()
                            .bg(rgb(C_BLUE_BTN))
                            .text_color(rgb(0xffffff))
                            .text_size(px(13.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(C_BLUE_HOV)))
                            .on_click(retry)
                            .child("Try again"),
                    ),
            )
            .into_any_element()
    }
}

// ── Shared widget: setpoint +/- row ──────────────────────────────────────────

fn setpoint_row(
    local_sp: u16,
    sp_up: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    sp_dn: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    id_suffix: &str,
) -> impl IntoElement {
    let sp_dn_id = SharedString::from(format!("sp-dn-{}", id_suffix));
    let sp_up_id = SharedString::from(format!("sp-up-{}", id_suffix));
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_color(rgb(C_TEXT3))
                        .text_size(px(10.))
                        .child("SETPOINT"),
                )
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_1()
                        .child(
                            div()
                                .text_color(rgb(C_TEXT))
                                .text_size(px(24.))
                                .font_weight(FontWeight::LIGHT)
                                .child(format!("{}", local_sp)),
                        )
                        .child(
                            div()
                                .text_color(rgb(C_TEXT3))
                                .text_size(px(14.))
                                .child("°C"),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .id(sp_dn_id)
                        .w(px(44.))
                        .h(px(44.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(rgb(C_LAYER1))
                        .border_1()
                        .border_color(rgb(C_BORDER))
                        .text_color(rgb(C_TEXT2))
                        .text_size(px(20.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(C_LAYER2)))
                        .active(|s| s.bg(rgb(C_BLUE_TEN)))
                        .on_click(sp_dn)
                        .child("−"),
                )
                .child(
                    div()
                        .id(sp_up_id)
                        .w(px(44.))
                        .h(px(44.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(rgb(C_BLUE_BTN))
                        .text_color(rgb(0xffffff))
                        .text_size(px(20.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(C_BLUE_HOV)))
                        .active(|s| s.bg(rgb(C_BLUE_TEN)))
                        .on_click(sp_up)
                        .child("+"),
                ),
        )
}

// ── Shared widget: stat row ───────────────────────────────────────────────────

fn stat_row(
    label: &str,
    value: impl Into<String>,
    value_color: u32,
    alt: bool,
) -> impl IntoElement {
    div()
        .flex()
        .justify_between()
        .items_center()
        .w_full()
        .h(px(32.))
        .px_6()
        .bg(rgb(if alt { C_LAYER1 } else { C_BG }))
        .child(
            div()
                .text_color(rgb(C_TEXT3))
                .text_size(px(11.))
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .text_color(rgb(value_color))
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(SharedString::from(value.into())),
        )
}

fn fmt_duration(secs: u32) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

// ── Render ────────────────────────────────────────────────────────────────────

impl Render for PinemonitorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.error.is_some() {
            return self.view_error(window, cx);
        }
        if self.show_scan || (self.devices.is_empty() && self.connecting.is_empty()) {
            self.view_scan(window, cx)
        } else {
            self.view_dashboard(window, cx)
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(460.), px(740.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_decorations: Some(WindowDecorations::Client),
                app_id: Some("pinemonitor".into()),
                ..Default::default()
            },
            |_window, cx| cx.new(PinemonitorView::new),
        )
        .unwrap();
        cx.activate(true);
    });
}
