use chrono::{DateTime, Utc};
use crossbeam_channel::{unbounded, Receiver, Sender};
use iced::{alignment, time};
use iced::widget::{button, column, container, row, svg, text_input, toggler, Svg};
use iced::{application, Color, Element, Length, Point, Theme, Renderer, Subscription};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

// ===== Figure coordinate system (must match assets/nkisi.svg viewBox) =====
const FIGURE_W: f32 = 100.0;
const FIGURE_H: f32 = 150.0;

// On-screen size for the figure
const SCREEN_W: f32 = 360.0;
const SCREEN_H: f32 = SCREEN_W * (FIGURE_H / FIGURE_W);

// FIX constants
const SOH: u8 = 0x01;
const FIX_ADDR: &str = "0.0.0.0:9898";

// -------------------- Domain --------------------
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NkisiNkondi {
    pub id: Uuid,
    pub culture: String,
    pub events: Vec<ActivationEvent>,
    pub pins: Vec<(f32, f32)>, // SVG-space coords (0..FIGURE_W/H)
}

impl NkisiNkondi {
    fn new(culture: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            culture: culture.into(),
            events: vec![],
            pins: vec![],
        }
    }
    fn intensity(&self) -> u32 {
        self.events.len() as u32 + 3
    }
}
impl Default for NkisiNkondi {
    fn default() -> Self {
        Self::new("Kongo peoples")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationEvent {
    pub id: Uuid,
    pub date: DateTime<Utc>,
    pub performed_by: String,          // who added the spike
    pub purpose: ActivationPurpose,
    pub outcome: Outcome,
    pub notes: Option<String>,         // message
    pub pos: (f32, f32),               // SVG coords
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActivationPurpose {
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Outcome {
    Pending,
    Resolved,
    Failed,
}

// -------------------- Program state --------------------
struct State {
    nkisi: NkisiNkondi,
    status: String,

    // Global toggles/paths
    show_grid: bool,
    save_path: String,
    svg_path: String,

    // Mouse tracking (local to mouse_area)
    last_cursor: Option<Point>,

    // Confirmation UI state (for local clicks)
    pending_pos: Option<(f32, f32)>,
    striker_input: String,
    message_input: String,

    // FIX: channel to receive spikes from acceptor thread
    fix_rx: Receiver<ExternalSpike>,
}

impl State {
    fn new(fix_rx: Receiver<ExternalSpike>) -> Self {
        Self {
            nkisi: NkisiNkondi::new("Kongo peoples"),
            status: format!("Ready. FIX acceptor on {}", FIX_ADDR),
            show_grid: false,
            save_path: "nkisi_state.json".into(),
            svg_path: "assets/nkisi.svg".into(),
            last_cursor: None,
            pending_pos: None,
            striker_input: String::new(),
            message_input: String::new(),
            fix_rx,
        }
    }
}

// -------------------- Messages --------------------
#[derive(Debug, Clone)]
enum Message {
    // Local UI
    CursorMoved(Point),
    ProposeSpike,
    ConfirmSpike,
    CancelSpike,
    Save,
    Load,
    ClearAll,
    ToggleGrid(bool),
    SvgPathChanged(String),
    SavePathChanged(String),
    StrikerChanged(String),
    SpikeMessageChanged(String),

    // External (FIX)
    PollExternal, // tick to drain channel
    ExternalArrived(ExternalSpike), // (used if we switch to direct subscription)
}

// -------------------- External spike envelope --------------------
#[derive(Debug, Clone)]
struct ExternalSpike {
    pos: (f32, f32),
    who: String,
    message: Option<String>,
    when: Option<DateTime<Utc>>,
}

// -------------------- Update --------------------
fn update(state: &mut State, message: Message) {
    match message {
        Message::CursorMoved(p) => {
            state.last_cursor = Some(p);
        }
        Message::ProposeSpike => {
            if let Some(p) = state.last_cursor {
                let nx = (p.x / SCREEN_W) * FIGURE_W;
                let ny = (p.y / SCREEN_H) * FIGURE_H;
                let nx = nx.clamp(0.0, FIGURE_W);
                let ny = ny.clamp(0.0, FIGURE_H);
                state.pending_pos = Some((nx, ny));
                state.status = format!("Pending spike at ({:.1}, {:.1}). Confirm or cancel.", nx, ny);
            } else {
                state.status = "Click ignored (no cursor yet)".into();
            }
        }
        Message::ConfirmSpike => {
            if let Some((nx, ny)) = state.pending_pos.take() {
                let who = state.striker_input.trim();
                if who.is_empty() {
                    state.status = "Please enter a Striker name before confirming.".into();
                    state.pending_pos = Some((nx, ny));
                    return;
                }
                state.nkisi.pins.push((nx, ny));
                state.nkisi.events.push(ActivationEvent {
                    id: Uuid::new_v4(),
                    date: Utc::now(),
                    performed_by: who.to_string(),
                    purpose: ActivationPurpose::Other("Manual spike".into()),
                    outcome: Outcome::Pending,
                    notes: if state.message_input.trim().is_empty() {
                        None
                    } else {
                        Some(state.message_input.clone())
                    },
                    pos: (nx, ny),
                });
                state.status = format!(
                    "Spike confirmed at ({:.1}, {:.1}) by {} • total events: {}",
                    nx, ny, who, state.nkisi.events.len()
                );
                state.message_input.clear();
            } else {
                state.status = "No pending spike to confirm.".into();
            }
        }
        Message::CancelSpike => {
            state.pending_pos = None;
            state.status = "Pending spike canceled.".into();
        }
        Message::Save => match save_json(&state.save_path, &state.nkisi) {
            Ok(_) => state.status = format!("Saved to {}", state.save_path),
            Err(e) => state.status = format!("Save failed: {e}"),
        },
        Message::Load => match load_json(&state.save_path) {
            Ok(n) => {
                state.nkisi = n;
                state.status = format!(
                    "Loaded {} events / {} pins from {}",
                    state.nkisi.events.len(),
                    state.nkisi.pins.len(),
                    state.save_path
                );
            }
            Err(e) => state.status = format!("Load failed: {e}"),
        },
        Message::ClearAll => {
            state.nkisi.pins.clear();
            state.nkisi.events.clear();
            state.pending_pos = None;
            state.status = "Cleared all pins & events.".into();
        }
        Message::ToggleGrid(v) => state.show_grid = v,
        Message::SvgPathChanged(p) => state.svg_path = p,
        Message::SavePathChanged(p) => state.save_path = p,
        Message::StrikerChanged(s) => state.striker_input = s,
        Message::SpikeMessageChanged(s) => state.message_input = s,

        // Poll the FIX channel on a timer
        Message::PollExternal => {
            let mut count = 0usize;
            while let Ok(spike) = state.fix_rx.try_recv() {
                count += 1;
                let when = spike.when.unwrap_or_else(Utc::now);
                let who = spike.who;
                let (nx, ny) = spike.pos;

                state.nkisi.pins.push((nx, ny));
                state.nkisi.events.push(ActivationEvent {
                    id: Uuid::new_v4(),
                    date: when,
                    performed_by: who.clone(),
                    purpose: ActivationPurpose::Other("External FIX spike".into()),
                    outcome: Outcome::Pending,
                    notes: spike.message.clone(),
                    pos: (nx, ny),
                });
            }
            if count > 0 {
                state.status = format!("Accepted {count} FIX spike(s). Total events: {}", state.nkisi.events.len());
            }
        }

        // Not used in the timer-based approach
        Message::ExternalArrived(_s) => {}
    }
}

// -------------------- View --------------------
fn view(state: &State) -> Element<Message> {
    // Base SVG (type-annotated to pin Theme generic)
    let handle = svg::Handle::from_path(&state.svg_path);
    let base: Svg<'_, Theme> = svg(handle)
        .width(Length::Fixed(SCREEN_W))
        .height(Length::Fixed(SCREEN_H));

    // Mouse area over the base: track cursor & emit "ProposeSpike" on click
    let clickable: Element<Message> =
        iced::widget::mouse_area::<Message, Theme, Renderer>(base)
            .on_move(|p| Message::CursorMoved(p))
            .on_press(Message::ProposeSpike)
            .into();

    // Overlay pins/grid as another SVG on top
    let overlay_handle =
        svg::Handle::from_memory(render_overlay_svg(&state.nkisi, state.show_grid).into_bytes());
    let overlay_svg: Svg<'_, Theme> = svg(overlay_handle)
        .width(Length::Fixed(SCREEN_W))
        .height(Length::Fixed(SCREEN_H));
    let overlay: Element<Message> = overlay_svg.into();

    let figure: Element<Message> = column![clickable, overlay].spacing(0).into();

    use iced::widget::text; // for text::Style
    let mut controls_col = column![
        iced::widget::text("Rustic Nkisi • Spike Ledger (FIX-enabled)").size(22),
        row![
            button("Save").on_press(Message::Save),
            button("Load").on_press(Message::Load),
            button("Clear All").on_press(Message::ClearAll),
        ]
        .spacing(10),
        row![
            toggler(state.show_grid)
                .label("Show grid")
                .on_toggle(Message::ToggleGrid),
            iced::widget::text(format!("Intensity: {}", state.nkisi.intensity()))
        ]
        .spacing(16),
        row![
            iced::widget::text("SVG path:"),
            text_input("assets/nkisi.svg", &state.svg_path)
                .on_input(Message::SvgPathChanged)
                .padding(6),
        ]
        .spacing(8),
        row![
            iced::widget::text("Save path:"),
            text_input("nkisi_state.json", &state.save_path)
                .on_input(Message::SavePathChanged)
                .padding(6),
        ]
        .spacing(8),
    ]
        .spacing(8)
        .align_x(alignment::Horizontal::Left);

    // Pending Spike confirmation panel (for local clicks)
    if let Some((nx, ny)) = state.pending_pos {
        let pending = container(
            column![
                iced::widget::text("Pending Spike").size(18),
                iced::widget::text(format!("Position (SVG): x={:.1}, y={:.1}", nx, ny)),
                row![
                    iced::widget::text("Striker:"),
                    text_input("who is adding the spike", &state.striker_input)
                        .on_input(Message::StrikerChanged)
                        .padding(6)
                        .width(Length::Fill),
                ]
                .spacing(8),
                row![
                    iced::widget::text("Message:"),
                    text_input("context / reason (optional)", &state.message_input)
                        .on_input(Message::SpikeMessageChanged)
                        .padding(6)
                        .width(Length::Fill),
                ]
                .spacing(8),
                row![
                    button("Confirm").on_press(Message::ConfirmSpike),
                    button("Cancel").on_press(Message::CancelSpike),
                ]
                .spacing(12),
            ]
                .spacing(8),
        )
            .padding(12)
            .style(|_theme: &Theme| {
                use iced::Border;
                container::Style {
                    background: Some(Color::from_rgba(0.15, 0.15, 0.18, 0.9).into()),
                    border: Border { radius: 12.0.into(), ..Default::default() },
                    ..Default::default()
                }
            });

        controls_col = controls_col.push(pending);
    }

    // Status line
    controls_col = controls_col.push(
        iced::widget::text(&state.status).style(|_| text::Style {
            color: Some(Color::from_rgb(0.85, 0.85, 0.95)),
            ..Default::default()
        }),
    );

    row![figure, container(controls_col).padding(16)]
        .spacing(24)
        .padding(16)
        .into()
}

// -------------------- Overlay SVG (pins + grid) --------------------
fn render_overlay_svg(nkisi: &NkisiNkondi, show_grid: bool) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {} {}">"##,
        FIGURE_W, FIGURE_H
    ));

    if show_grid {
        s.push_str(r##"<g stroke="#ffffff22" stroke-width="0.3">"##);
        for x in (0..=100).step_by(10) {
            let x = (x as f32) * (FIGURE_W / 100.0);
            s.push_str(&format!(r#"<line x1="{x}" y1="0" x2="{x}" y2="{FIGURE_H}"/>"#));
        }
        for y in (0..=150).step_by(10) {
            let y = (y as f32) * (FIGURE_H / 150.0);
            s.push_str(&format!(r#"<line x1="0" y1="{y}" x2="{FIGURE_W}" y2="{y}"/>"#));
        }
        s.push_str("</g>");
    }

    // Pins
    s.push_str(r##"<g fill="#ff4d4d" stroke="#00000099" stroke-width="0.4">"##);
    for &(x, y) in &nkisi.pins {
        s.push_str(&format!(r#"<circle cx="{x:.2}" cy="{y:.2}" r="1.8"/>"#));
    }
    s.push_str("</g></svg>");
    s
}

// -------------------- Persistence --------------------
#[derive(Debug, Error)]
pub enum IoError {
    #[error("read error: {0}")]
    Read(String),
    #[error("write error: {0}")]
    Write(String),
    #[error("parse error: {0}")]
    Parse(String),
}
fn save_json(path: &str, state: &NkisiNkondi) -> Result<(), IoError> {
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|e| IoError::Write(e.to_string()))?;
    std::fs::write(path, bytes).map_err(|e| IoError::Write(e.to_string()))
}
fn load_json(path: &str) -> Result<NkisiNkondi, IoError> {
    let bytes = std::fs::read(path).map_err(|e| IoError::Read(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| IoError::Parse(e.to_string()))
}

// -------------------- FIX acceptor --------------------
// Minimal FIX “U1 Spike” parser/acceptor.
// 35=U1 (custom); 55=NKISI; 448=PartyID (who); 58=Text (message);
// 60=TransactTime (optional ISO); 6010=PosX; 6011=PosY
fn start_fix_acceptor(addr: &str, tx: Sender<ExternalSpike>) {
    let addr = addr.to_string();
    thread::spawn(move || {
        let listener = TcpListener::bind(&addr).expect("bind FIX acceptor");
        eprintln!("[FIX] listening on {}", addr);

        for stream in listener.incoming() {
            match stream {
                Ok(mut s) => {
                    let txc = tx.clone();
                    thread::spawn(move || handle_fix_connection(&mut s, txc));
                }
                Err(e) => eprintln!("[FIX] accept error: {e:?}"),
            }
        }
    });
}

fn handle_fix_connection(stream: &mut TcpStream, tx: Sender<ExternalSpike>) {
    let mut buf = vec![0u8; 8192];
    let mut acc: Vec<u8> = Vec::new();

    loop {
        match stream.read(&mut buf) {
            Ok(0) => break, // closed
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);

                // Try to frame whole messages: search for "10=" and trailing SOH as end marker.
                // This is simplistic but works for many test feeds.
                while let Some(end_idx) = find_fix_end(&acc) {
                    let msg = acc.drain(..=end_idx).collect::<Vec<u8>>();
                    if let Some(spike) = parse_fix_spike(&msg) {
                        let _ = tx.send(spike);
                    }
                }
            }
            Err(e) => {
                eprintln!("[FIX] read error: {e:?}");
                break;
            }
        }
    }
}

fn find_fix_end(buf: &[u8]) -> Option<usize> {
    // Look for "10=" then the next SOH; return that SOH index as end of message
    let needle = b"10=";
    let mut i = 0;
    while i + 3 < buf.len() {
        if &buf[i..i + 3] == needle {
            // find next SOH
            let mut j = i + 3;
            while j < buf.len() && buf[j] != SOH { j += 1; }
            if j < buf.len() && buf[j] == SOH {
                return Some(j);
            }
        }
        i += 1;
    }
    None
}

fn parse_fix_spike(raw: &[u8]) -> Option<ExternalSpike> {
    // Split by SOH into key=val pairs
    let mut map: HashMap<i32, String> = HashMap::new();
    for field in raw.split(|b| *b == SOH) {
        if field.is_empty() { continue; }
        if let Some(eq) = field.iter().position(|b| *b == b'=') {
            let (k, v) = field.split_at(eq);
            let key = std::str::from_utf8(k).ok()?.parse::<i32>().ok()?;
            let val = std::str::from_utf8(&v[1..]).ok()?.to_string();
            map.insert(key, val);
        }
    }

    // Check it’s our message
    let msg_type = map.get(&35)?; // 35=U1
    if msg_type != "U1" { return None; }
    if map.get(&55).map(|s| s.as_str()) != Some("NKISI") { return None; }

    // Required: who (448), pos (6010, 6011)
    let who = map.get(&448)?.clone();
    let x: f32 = map.get(&6010)?.parse().ok()?;
    let y: f32 = map.get(&6011)?.parse().ok()?;

    // Optional message, timestamp
    let message = map.get(&58).cloned();
    let when = map.get(&60)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    Some(ExternalSpike {
        pos: (x.clamp(0.0, FIGURE_W), y.clamp(0.0, FIGURE_H)),
        who,
        message,
        when,
    })
}



// -------------------- Subscriptions --------------------
fn subscriptions(_state: &State) -> Subscription<Message> {
    // Simple timer to poll FIX channel regularly
    time::every(Duration::from_millis(200))
        .map(|_| Message::PollExternal)
}

// -------------------- Boot --------------------
pub fn main() -> iced::Result {
    // Start FIX acceptor thread
    let (fix_tx, fix_rx) = unbounded::<ExternalSpike>();
    start_fix_acceptor(FIX_ADDR, fix_tx);

    let init = State::new(fix_rx);
    let title = "Rustic Nkisi — Iced 0.13 (FIX-enabled)";

    application(title, update, view)
        .subscription(subscriptions)
        .theme(|_| Theme::Dark)
        .centered()
        .run_with(move || (init, iced::Task::none()))
}