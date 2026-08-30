use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use flume::{Receiver, Sender};
use gilrs::GamepadId;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};

use crate::models::AppState;

/// Commands sent from UI to bridge thread.
#[derive(Debug, Clone)]
pub enum UiCommand {
    /// Remap a controller to a new slot.
    Remap {
        controller_id: GamepadId,
        new_slot: usize,
    },
    /// Send a quick vibration to identify a controller.
    Identify { controller_id: GamepadId },
    /// Send vibration for a specific duration.
    Vibrate {
        controller_id: GamepadId,
        duration_ms: u64,
    },
}

/// Events sent from bridge thread to UI.
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// A controller connected.
    ControllerConnected { id: GamepadId, name: String },
    /// A controller disconnected.
    ControllerDisconnected { id: GamepadId },
    /// A slot assignment changed.
    SlotUpdated {
        slot: usize,
        controller_id: Option<GamepadId>,
        controller_name: Option<String>,
    },
    /// Vibration completed.
    VibrationComplete { id: GamepadId },
}

/// Information about a connected controller.
#[derive(Debug, Clone)]
pub struct ControllerInfo {
    pub id: GamepadId,
    pub name: String,
    pub slot: Option<usize>,
    pub is_vibrating: bool,
}

/// State of the terminal UI.
pub struct UiState {
    pub controllers: Vec<ControllerInfo>,
    pub slot_assignments: Vec<Option<ControllerInfo>>,
    pub selected_index: usize,
    pub show_remap_dialog: bool,
    pub selected_slot_for_remap: usize,
    pub status_message: String,
    pub should_quit: bool,
}

impl UiState {
    pub fn new(num_slots: usize) -> Self {
        Self {
            controllers: Vec::new(),
            slot_assignments: vec![None; num_slots],
            selected_index: 0,
            show_remap_dialog: false,
            selected_slot_for_remap: 0,
            status_message: "Ready".to_string(),
            should_quit: false,
        }
    }

    pub fn handle_controller_connected(&mut self, id: GamepadId, name: String) {
        // Check if already known
        if self.controllers.iter().any(|c| c.id == id) {
            return;
        }
        self.controllers.push(ControllerInfo {
            id,
            name,
            slot: None,
            is_vibrating: false,
        });
        self.status_message = "Controller connected".to_string();
    }

    pub fn handle_controller_disconnected(&mut self, id: GamepadId) {
        if let Some(pos) = self.controllers.iter().position(|c| c.id == id) {
            let controller = self.controllers.remove(pos);
            // Clear slot assignment
            if let Some(slot) = controller.slot {
                self.slot_assignments[slot] = None;
            }
            // Adjust selected index
            if self.selected_index >= self.controllers.len() && self.selected_index > 0 {
                self.selected_index -= 1;
            }
            self.status_message = "Controller disconnected".to_string();
        }
    }

    pub fn handle_slot_updated(
        &mut self,
        slot: usize,
        controller_id: Option<GamepadId>,
        controller_name: Option<String>,
    ) {
        if slot < self.slot_assignments.len() {
            self.slot_assignments[slot] = controller_id.map(|id| ControllerInfo {
                id,
                name: controller_name.unwrap_or_default(),
                slot: Some(slot),
                is_vibrating: false,
            });
        }
    }

    pub fn selected_controller(&self) -> Option<&ControllerInfo> {
        self.controllers.get(self.selected_index)
    }
}

/// Run the terminal UI.
pub fn run_ui(
    num_slots: usize,
    command_tx: Sender<UiCommand>,
    event_rx: Receiver<UiEvent>,
    state: Arc<Mutex<AppState>>,
) {
    // Setup terminal
    enable_raw_mode().expect("Failed to enable raw mode");
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture).expect("Failed to setup terminal");
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).expect("Failed to create terminal");

    let mut ui_state = UiState::new(num_slots);
    let mut list_state = ListState::default();

    loop {
        // Check for exit
        if state.lock().unwrap().is_exiting() || ui_state.should_quit {
            break;
        }

        // Handle events from bridge
        while let Ok(event) = event_rx.try_recv() {
            match event {
                UiEvent::ControllerConnected { id, name } => {
                    ui_state.handle_controller_connected(id, name);
                }
                UiEvent::ControllerDisconnected { id } => {
                    ui_state.handle_controller_disconnected(id);
                }
                UiEvent::SlotUpdated {
                    slot,
                    controller_id,
                    controller_name,
                } => {
                    ui_state.handle_slot_updated(slot, controller_id, controller_name);
                }
                UiEvent::VibrationComplete { id } => {
                    if let Some(controller) = ui_state.controllers.iter_mut().find(|c| c.id == id) {
                        controller.is_vibrating = false;
                    }
                }
            }
        }

        // Handle keyboard input
        if event::poll(Duration::from_millis(100)).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                if key.kind == KeyEventKind::Press {
                    handle_key_event(key.code, &mut ui_state, &command_tx, num_slots);
                }
            }
        }

        // Draw UI
        terminal
            .draw(|f| {
                ui_state.selected_index = ui_state
                    .selected_index
                    .min(ui_state.controllers.len().saturating_sub(1));
                list_state.select(Some(ui_state.selected_index));
                draw_ui(f, &ui_state, &mut list_state);
            })
            .expect("Failed to draw UI");
    }

    // Restore terminal
    disable_raw_mode().expect("Failed to disable raw mode");
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .expect("Failed to restore terminal");
    terminal.show_cursor().expect("Failed to show cursor");
}

/// Handle a key event.
fn handle_key_event(
    key: KeyCode,
    state: &mut UiState,
    command_tx: &Sender<UiCommand>,
    num_slots: usize,
) {
    if state.show_remap_dialog {
        // Handle remap dialog
        match key {
            KeyCode::Esc => {
                state.show_remap_dialog = false;
                state.status_message = "Remap cancelled".to_string();
            }
            KeyCode::Up => {
                state.selected_slot_for_remap = state.selected_slot_for_remap.saturating_add(1);
            }
            KeyCode::Down => {
                state.selected_slot_for_remap = state
                    .selected_slot_for_remap
                    .saturating_sub(1)
                    .min(num_slots - 1);
            }
            KeyCode::Enter => {
                if let Some(controller) = state.selected_controller() {
                    let _ = command_tx.send(UiCommand::Remap {
                        controller_id: controller.id,
                        new_slot: state.selected_slot_for_remap,
                    });
                    state.status_message = format!(
                        "Remapping {} to slot {}",
                        controller.name,
                        state.selected_slot_for_remap + 1
                    );
                }
                state.show_remap_dialog = false;
            }
            _ => {}
        }
    } else {
        // Handle main UI
        match key {
            KeyCode::Char('q') | KeyCode::Esc => {
                state.should_quit = true;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if state.selected_index > 0 {
                    state.selected_index -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if state.selected_index < state.controllers.len().saturating_sub(1) {
                    state.selected_index += 1;
                }
            }
            KeyCode::Char('v') | KeyCode::Char('V') => {
                if let Some(controller) = state.selected_controller() {
                    let _ = command_tx.send(UiCommand::Identify {
                        controller_id: controller.id,
                    });
                    state.status_message = format!("Vibrating {}", controller.name);
                    // Mark as vibrating
                    if let Some(ctrl) = state.controllers.get_mut(state.selected_index) {
                        ctrl.is_vibrating = true;
                    }
                }
            }
            KeyCode::Char('r') | KeyCode::Char('R') if state.selected_controller().is_some() => {
                state.show_remap_dialog = true;
                state.selected_slot_for_remap = 0;
                state.status_message =
                    "Select slot (↑/↓, Enter to confirm, Esc to cancel)".to_string();
            }
            _ => {}
        }
    }
}

/// Draw the UI.
fn draw_ui(f: &mut Frame, state: &UiState, list_state: &mut ListState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3), // Title
            Constraint::Min(0),    // Main content
            Constraint::Length(3), // Status bar
        ])
        .split(f.area());

    // Title
    let title = Paragraph::new("Raspberry Switch Controller - Controller Mapping UI")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    // Main content
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    // Left panel: Connected controllers
    let controller_items: Vec<ListItem> = state
        .controllers
        .iter()
        .enumerate()
        .map(|(i, controller)| {
            let style = if i == state.selected_index {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let slot_text = match controller.slot {
                Some(slot) => format!("→ Slot {}", slot + 1),
                None => "→ (unassigned)".to_string(),
            };
            let vibration_text = if controller.is_vibrating {
                " [VIBRATING]"
            } else {
                ""
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{}. {}", i + 1, controller.name), style),
                Span::styled(slot_text, Style::default().fg(Color::Green)),
                Span::styled(vibration_text, Style::default().fg(Color::Magenta)),
            ]))
        })
        .collect();

    let controller_list = List::new(controller_items)
        .block(
            Block::default()
                .title("Connected Xbox Controllers")
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    f.render_stateful_widget(controller_list, main_chunks[0], list_state);

    // Right panel: Slot assignments
    let slot_items: Vec<ListItem> = state
        .slot_assignments
        .iter()
        .enumerate()
        .map(|(i, assignment)| {
            let (text, style) = match assignment {
                Some(controller) => (
                    format!(
                        "Slot {}: {} (ID: {:?})",
                        i + 1,
                        controller.name,
                        controller.id
                    ),
                    Style::default().fg(Color::Green),
                ),
                None => (
                    format!("Slot {}: (Empty - idle)", i + 1),
                    Style::default().fg(Color::DarkGray),
                ),
            };
            ListItem::new(Line::from(Span::styled(text, style)))
        })
        .collect();

    let slot_list = List::new(slot_items).block(
        Block::default()
            .title("Switch Pro Controller Slots")
            .borders(Borders::ALL),
    );
    f.render_widget(slot_list, main_chunks[1]);

    // Status bar
    let status_text = if state.show_remap_dialog {
        format!(
            "Remap Mode: Use ↑/↓ to select slot, Enter to confirm, Esc to cancel | {}",
            state.status_message
        )
    } else {
        format!(
            "[V] Vibrate | [R] Remap | [↑/↓] Navigate | [Q] Quit | {}",
            state.status_message
        )
    };
    let status = Paragraph::new(status_text)
        .style(Style::default().fg(Color::White))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(status, chunks[2]);
}
