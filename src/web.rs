//! HARM-stack web UI: HTMX + Axum/Alpine.js + Rust + Maud.
//!
//! Runs an Axum server on a dedicated std thread. It only ever *reads*
//! `WebState` (maintained by the bridge thread) to render HTML, and *writes*
//! `Command`s back to the bridge through a flume channel. Live controller
//! state is streamed to the browser over Server-Sent Events.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    body::Body,
    extract::{Form, Path as AxumPath, State},
    http::{header::CONTENT_TYPE, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Response,
    },
    routing::{get, post},
    Router,
};
use flume::Sender;
use maud::{html, Markup};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::models::{AppState, Command, ControllerState, Stick, WebState, XboxInput};

const WEB_HTMX: &str = include_str!("../web/static/htmx.min.js");
const WEB_SSE: &str = include_str!("../web/static/htmx-ext-sse.min.js");
const WEB_ALPINE: &str = include_str!("../web/static/alpine.min.js");
const WEB_CSS: &str = include_str!("../web/static/style.css");

const SSE_TICK: Duration = Duration::from_millis(33);

/// Shared handle for the web layer.
pub struct WebApp {
    pub state: Arc<Mutex<AppState>>,
    pub web: Arc<Mutex<WebState>>,
    pub command_tx: Sender<Command>,
    pub num_slots: usize,
}

/// Run the Axum server on a new Tokio runtime until the app enters
/// `AppState::Exiting` (Ctrl-C tears the whole process down).
pub fn serve(addr: SocketAddr, app: Arc<WebApp>) -> std::io::Result<()> {
    let router = Router::new()
        .route("/", get(index))
        .route("/fragments/overview", get(overview_fragment))
        .route("/actions/identify", post(action_identify))
        .route("/actions/vibrate", post(action_vibrate))
        .route("/actions/remap", post(action_remap))
        .route("/events", get(sse_handler))
        .route("/static/{file}", get(static_file))
        .with_state(app.clone());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        log::info!("Web UI listening on http://{}", addr);

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let state = app.state.clone();
        tokio::task::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                if state.lock().map(|s| s.is_exiting()).unwrap_or(true) {
                    let _ = shutdown_tx.send(());
                    break;
                }
            }
        });

        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .map_err(std::io::Error::other)
    })
}

// --- Handlers ---------------------------------------------------------------

async fn index(State(app): State<Arc<WebApp>>) -> Html<String> {
    let ws = app.web.lock().unwrap();
    Html(page(&ws, app.num_slots).into_string())
}

async fn overview_fragment(State(app): State<Arc<WebApp>>) -> Html<String> {
    let ws = app.web.lock().unwrap();
    Html(overview_partial(&ws, app.num_slots).into_string())
}

type FormData = Form<HashMap<String, String>>;

async fn action_identify(State(app): State<Arc<WebApp>>, Form(form): FormData) -> Html<String> {
    if let Some(id) = form
        .get("controller_id")
        .and_then(|v| v.parse::<usize>().ok())
    {
        let _ = app
            .command_tx
            .try_send(Command::Identify { controller_id: id });
    }
    let ws = app.web.lock().unwrap();
    Html(overview_partial(&ws, app.num_slots).into_string())
}

async fn action_vibrate(State(app): State<Arc<WebApp>>, Form(form): FormData) -> Html<String> {
    let id = form
        .get("controller_id")
        .and_then(|v| v.parse::<usize>().ok());
    let duration = form
        .get("duration_ms")
        .and_then(|v| v.parse::<u64>().ok())
        .map(|d| d.clamp(1, 5000));
    if let (Some(controller_id), Some(duration_ms)) = (id, duration) {
        let _ = app.command_tx.try_send(Command::Vibrate {
            controller_id,
            duration_ms,
        });
    }
    let ws = app.web.lock().unwrap();
    Html(overview_partial(&ws, app.num_slots).into_string())
}

async fn action_remap(State(app): State<Arc<WebApp>>, Form(form): FormData) -> Html<String> {
    let id = form
        .get("controller_id")
        .and_then(|v| v.parse::<usize>().ok());
    let new_slot = form.get("new_slot").and_then(|v| v.parse::<usize>().ok());
    if let (Some(controller_id), Some(new_slot)) = (id, new_slot) {
        let _ = app.command_tx.try_send(Command::Remap {
            controller_id,
            new_slot,
        });
    }
    let ws = app.web.lock().unwrap();
    Html(overview_partial(&ws, app.num_slots).into_string())
}

async fn static_file(AxumPath(file): AxumPath<String>) -> Response {
    let (body, mime) = match file.as_str() {
        "htmx.min.js" => (WEB_HTMX, "text/javascript"),
        "htmx-ext-sse.min.js" => (WEB_SSE, "text/javascript"),
        "alpine.min.js" => (WEB_ALPINE, "text/javascript"),
        "style.css" => (WEB_CSS, "text/css"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    Response::builder()
        .header(CONTENT_TYPE, mime)
        .body(Body::from(body))
        .unwrap()
}

/// SSE stream: pushes full pad-card sets on connect/disconnect (`pads`) and
/// individual card readouts (`pad-<id>`) when they change (~30Hz).
async fn sse_handler(State(app): State<Arc<WebApp>>) -> impl IntoResponse {
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);
    let web = app.web.clone();
    let state = app.state.clone();
    let num_slots = app.num_slots;

    tokio::spawn(async move {
        let mut last_ids: Option<Vec<usize>> = None;
        let mut last_readouts: HashMap<usize, String> = HashMap::new();
        'sse: loop {
            // Terminate on shutdown so graceful shutdown doesn't wait forever
            // for this open SSE stream. Also catch a client that disconnected
            // while the server was idle.
            if tx.is_closed()
                || state.lock().map(|s| s.is_exiting()).unwrap_or(true)
            {
                break 'sse;
            }
            let (ids, readouts) = {
                let ws = match web.lock() {
                    Ok(guard) => guard,
                    Err(_) => break 'sse,
                };
                let ids: Vec<usize> = ws.controllers.iter().map(|c| usize::from(c.id)).collect();
                let readouts: HashMap<usize, String> = ws
                    .controllers
                    .iter()
                    .map(|c| {
                        (
                            usize::from(c.id),
                            pad_readout(c, c.input.as_ref()).into_string(),
                        )
                    })
                    .collect();
                (ids, readouts)
            };

            let changed_ids = last_ids.as_ref() != Some(&ids);
            if changed_ids {
                let pads = {
                    let ws = match web.lock() {
                        Ok(guard) => guard,
                        Err(_) => break 'sse,
                    };
                    pads_full(&ws, num_slots).into_string()
                };
                if tx
                    .send(Ok(Event::default().event("pads").data(pads)))
                    .await
                    .is_err()
                {
                    break 'sse;
                }
                last_ids = Some(ids.clone());
                last_readouts = readouts.clone();
            } else {
                for id in &ids {
                    if let Some(readout) = readouts.get(id) {
                        if last_readouts.get(id) != Some(readout) {
                            if tx
                                .send(Ok(Event::default()
                                    .event(format!("pad-{}", id))
                                    .data(readout.clone())))
                                .await
                                .is_err()
                            {
                                break 'sse;
                            }
                            last_readouts.insert(*id, readout.clone());
                        }
                    }
                }
            }

            tokio::time::sleep(SSE_TICK).await;
        }
    });

    Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}

// --- Templates --------------------------------------------------------------

fn page(ws: &WebState, num_slots: usize) -> Markup {
    html! {
        (maud::DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "Raspberry Switch Controller" }
                link rel="stylesheet" href="/static/style.css";
                script src="/static/htmx.min.js" {}
                script src="/static/htmx-ext-sse.min.js" {}
                script defer src="/static/alpine.min.js" {}
            }
            body hx-ext="sse" {
                header {
                    h1 { "Raspberry Switch Controller" }
                    div #theme-toggle x-data="{ dark: localStorage.getItem('rsc-dark') === '1' }" x-init="document.documentElement.classList.toggle('dark', dark); $watch('dark', v => { document.documentElement.classList.toggle('dark', v); localStorage.setItem('rsc-dark', v ? '1' : '0'); })" {
                        label {
                            input type="checkbox" x-model="dark";
                            span { "Dark mode" }
                        }
                    }
                }
                main {
                    section #overview hx-get="/fragments/overview" hx-trigger="every 1s" hx-swap="innerHTML" {
                        (overview_partial(ws, num_slots))
                    }
                    section #tester {
                        h2 { "Controller Tester" }
                        p.hint { "Press a button on a connected Xbox controller and it lights up below." }
                        div #pads-wrap sse-connect="/events" {
                            div #pads sse-swap="pads" {
                                (pads_full(ws, num_slots))
                            }
                        }
                    }
                }
                footer {
                    p { "Raspberry Switch Controller — web UI" }
                }
            }
        }
    }
}

fn overview_partial(ws: &WebState, num_slots: usize) -> Markup {
    html! {
        h2 { "Overview" }
        div.overview-grid {
            div.panel {
                h3 { "Connected Controllers" }
                @if ws.controllers.is_empty() {
                    p.empty { "No Xbox controllers connected" }
                } @else {
                    ul.controller-list {
                        @for c in &ws.controllers {
                            li {
                                span.name { (c.name) }
                                span.badge { (battery_label(c.battery)) }
                                @if let Some(slot) = c.slot {
                                    span.badge.slot { "Slot " (slot + 1) }
                                }
                            }
                        }
                    }
                }
            }
            div.panel {
                h3 { "Switch Pro Controller Slots" }
                ol.slot-list {
                    @for slot in 0..num_slots {
                        li {
                            "Slot " (slot + 1) ": "
                            @if let Some(c) = ws.controllers.iter().find(|c| c.slot == Some(slot)) {
                                strong { (c.name) }
                            } @else {
                                span.idle { "(idle)" }
                            }
                        }
                    }
                }
            }
        }
        p.status { (ws.status) }
    }
}

fn pads_full(ws: &WebState, num_slots: usize) -> Markup {
    html! {
        @if ws.controllers.is_empty() {
            div.empty-pad { "No Xbox controllers connected." }
        } @else {
            @for c in ws.controllers.iter() {
                (pad_card(c, c.input.as_ref(), num_slots))
            }
        }
    }
}

fn pad_card(c: &ControllerState, input: Option<&XboxInput>, num_slots: usize) -> Markup {
    html! {
        div.pad-card id=(format!("card-{}", usize::from(c.id))) {
            div.pad-readout sse-swap=(format!("pad-{}", usize::from(c.id))) {
                (pad_readout(c, input))
            }
            div.pad-controls {
                form hx-post="/actions/identify" hx-target="#overview" hx-swap="innerHTML" {
                    input type="hidden" name="controller_id" value=(usize::from(c.id));
                    button type="submit" { "Identify" }
                }
                form hx-post="/actions/vibrate" hx-target="#overview" hx-swap="innerHTML" {
                    input type="hidden" name="controller_id" value=(usize::from(c.id));
                    select name="duration_ms" {
                        option value="200" { "200ms" }
                        option value="500" selected { "500ms" }
                        option value="1000" { "1s" }
                        option value="2000" { "2s" }
                    }
                    button type="submit" { "Vibrate" }
                }
                form hx-post="/actions/remap" hx-target="#overview" hx-swap="innerHTML" {
                    input type="hidden" name="controller_id" value=(usize::from(c.id));
                    select name="new_slot" {
                        @for slot in 0..num_slots {
                            @if Some(slot) == c.slot {
                                option value=(slot) selected { "Slot " (slot + 1) }
                            } @else {
                                option value=(slot) { "Slot " (slot + 1) }
                            }
                        }
                    }
                    button type="submit" { "Move" }
                }
            }
        }
    }
}

/// Inner content of a pad card readout. This is the part swapped by SSE at
/// ~30Hz; the controls (vibrate/remap) live outside it so client state
/// (Alpine, form values) survives the updates.
fn pad_readout(c: &ControllerState, input: Option<&XboxInput>) -> Markup {
    let inp = input.copied().unwrap_or_default();
    html! {
        div.pad-head {
            span.pad-name { (c.name) }
            span.pad-badge { (battery_label(c.battery)) }
            @if let Some(slot) = c.slot {
                span.pad-badge { "Slot " (slot + 1) }
            }
            @if c.is_vibrating {
                span.pad-badge.vibrating { "VIBRATING" }
            }
        }
        div.pad-body {
            div.sticks {
                (stick("L", inp.left_stick, inp.left_stick_press))
                (stick("R", inp.right_stick, inp.right_stick_press))
            }
            div.face {
                (pad_button("x", "X", inp.x))
                (pad_button("y", "Y", inp.y))
                (pad_button("b", "B", inp.b))
                (pad_button("a", "A", inp.a))
            }
            div.bumpers {
                (bumper("LB", inp.lb))
                (bumper("RB", inp.rb))
            }
            div.center {
                (center_button("View", inp.view))
                (xbox_button(inp.xbox))
                (center_button("Menu", inp.menu))
            }
            div.dpad {
                (dpad_button("dpad-up", "▲", inp.dpad_up))
                (dpad_button("dpad-left", "◀", inp.dpad_left))
                (dpad_button("dpad-right", "▶", inp.dpad_right))
                (dpad_button("dpad-down", "▼", inp.dpad_down))
            }
            div.triggers {
                (trigger("LT", inp.lt, inp.lt_value))
                (trigger("RT", inp.rt, inp.rt_value))
            }
        }
    }
}

fn pad_button(class: &str, label: &str, pressed: bool) -> Markup {
    html! {
        div class=(format!("pad-btn {} {}", class, if pressed { "lit" } else { "" })) { (label) }
    }
}

fn bumper(label: &str, pressed: bool) -> Markup {
    html! {
        div class=(format!("bumper {}", if pressed { "lit" } else { "" })) { (label) }
    }
}

fn center_button(label: &str, pressed: bool) -> Markup {
    html! {
        div class=(format!("center-btn {}", if pressed { "lit" } else { "" })) { (label) }
    }
}

fn xbox_button(pressed: bool) -> Markup {
    html! {
        div class=(format!("xbox-btn {}", if pressed { "lit" } else { "" })) { "X" }
    }
}

fn dpad_button(class: &str, glyph: &str, pressed: bool) -> Markup {
    html! {
        div class=(format!("dpad-btn {} {}", class, if pressed { "lit" } else { "" })) { (glyph) }
    }
}

fn stick(label: &str, s: Stick, pressed: bool) -> Markup {
    let x = ((s.x.clamp(-1.0, 1.0) + 1.0) / 2.0 * 100.0) as u32;
    let y = ((s.y.clamp(-1.0, 1.0) + 1.0) / 2.0 * 100.0) as u32;
    html! {
        div.stick {
            div.stick-label { (label) (if pressed { " (pressed)" } else { "" }) }
            div.crosshair {
                div.dot class=(if pressed { "dot lit" } else { "dot" }) style=(format!("left:{}%;top:{}%", x, 100 - y));
            }
        }
    }
}

fn trigger(label: &str, pressed: bool, value: f32) -> Markup {
    let pct = (value.clamp(0.0, 1.0) * 100.0) as u32;
    html! {
        div.trigger {
            div.trigger-label { (label) }
            div.trigger-bar {
                div.trigger-fill class=(if pressed { "lit" } else { "" }) style=(format!("height:{}%", pct));
            }
        }
    }
}

fn battery_label(battery: u8) -> String {
    let level = battery >> 5;
    let charging = battery & 0x10 != 0;
    let wired = battery & 0x01 != 0;
    if wired {
        "wired".to_string()
    } else {
        format!("{}/4{}", level, if charging { " charging" } else { "" })
    }
}
