// Pre-existing pedantic clippy lints — suppress at crate level for now
#![allow(
    clippy::unreadable_literal,       // hex color codes in theme.rs
    clippy::cast_possible_truncation, // terminal rendering casts (f32/usize/u16)
    clippy::cast_sign_loss,           // terminal rendering casts
    clippy::cast_precision_loss,      // terminal rendering casts
    clippy::cast_possible_wrap,       // terminal rendering casts
    clippy::too_many_lines,           // complex view/element functions
    clippy::wildcard_imports,         // gpui::* pattern
    clippy::similar_names,            // view field names
    clippy::match_same_arms,          // exhaustive match patterns
    clippy::match_wildcard_for_single_variants,
    clippy::redundant_closure_for_method_calls,
    clippy::manual_let_else,
    clippy::single_match_else,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    clippy::doc_markdown,
    clippy::assigning_clones,
    clippy::fn_params_excessive_bools,
    clippy::struct_excessive_bools,
    clippy::map_unwrap_or,
    dead_code,
)]

#[allow(dead_code)]
mod app_state;
mod assets;
mod icons;
mod persistence;
mod terminal_direct;
mod terminal_handle;
#[allow(dead_code)]
mod theme;
mod views;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Parser;
use gpui::*;
use zremote_client::ApiClient;

use app_state::AppState;
use assets::Assets;
use persistence::Persistence;
use views::main_view::MainView;

#[derive(Parser)]
#[command(name = "zremote-gui", version, about = "ZRemote native desktop client")]
struct Cli {
    /// Server URL (same ZREMOTE_SERVER_URL as agent uses, e.g. ws://host:3000/ws/agent
    /// or just http://host:3000). Path is stripped automatically.
    #[arg(
        long,
        env = "ZREMOTE_SERVER_URL",
        default_value = "http://localhost:3000"
    )]
    server: String,

    /// Auto-exit after N seconds (for headless screenshot capture).
    #[arg(long)]
    exit_after: Option<u64>,
}

/// Extract base HTTP URL from a server URL that may include a WS path.
/// e.g. "ws://host:3000/ws/agent" -> "http://host:3000"
///      "wss://host.com/ws/agent" -> "https://host.com"
///      "http://localhost:3000"    -> "http://localhost:3000"
fn extract_base_url(raw: &str) -> String {
    let url = raw.trim_end_matches('/');
    // Parse with url crate to extract scheme + host + port
    if let Ok(parsed) = url::Url::parse(url) {
        let scheme = match parsed.scheme() {
            "ws" => "http",
            "wss" => "https",
            other => other,
        };
        let host = parsed.host_str().unwrap_or("localhost");
        if let Some(port) = parsed.port() {
            format!("{scheme}://{host}:{port}")
        } else {
            format!("{scheme}://{host}")
        }
    } else {
        url.to_string()
    }
}

fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let server_url = extract_base_url(&cli.server);

    tracing::info!(server = %server_url, "starting ZRemote GUI");

    // Create tokio runtime on background threads (GPUI owns the main thread)
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");

    let tokio_handle = rt.handle().clone();

    // Detect server mode
    let api = ApiClient::new(&server_url).expect("invalid server URL");
    let mode = rt
        .block_on(async { api.get_mode().await })
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to detect server mode, assuming 'server'");
            "server".to_string()
        });

    tracing::info!(mode = %mode, "connected to server");

    // Load persistent GUI state.
    let mut persistence = Persistence::load();
    persistence.update(|s| s.server_url = Some(server_url.clone()));

    // Start events WebSocket on background tokio task
    let events_url = api.events_ws_url();
    let event_stream = zremote_client::EventStream::connect(events_url, &tokio_handle);

    let restored_width = persistence.state().window_width;
    let restored_height = persistence.state().window_height;

    let app_state = Arc::new(AppState {
        api,
        tokio_handle,
        event_rx: event_stream.rx.clone(),
        _event_stream: event_stream,
        mode,
        persistence: Mutex::new(persistence),
    });

    let exit_after = cli.exit_after;

    // Launch GPUI application on main thread
    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            let app_state_for_quit = app_state.clone();
            let app_state_clone = app_state.clone();
            cx.open_window(
                window_options(restored_width, restored_height),
                move |window, cx| cx.new(|cx| MainView::new(app_state_clone, window, cx)),
            )
            .expect("failed to open window");

            // Save state on quit.
            let _quit_sub = cx.on_app_quit({
                move |cx: &mut App| {
                    // Try to read window bounds for persistence.
                    if let Some(win) = cx.windows().first().copied()
                        && let Ok(bounds) = win
                            .update(cx, |_root: AnyView, window: &mut Window, _cx: &mut App| {
                                window.bounds()
                            })
                        && let Ok(mut p) = app_state_for_quit.persistence.lock()
                    {
                        p.update(|s| {
                            s.window_width = Some(f32::from(bounds.size.width));
                            s.window_height = Some(f32::from(bounds.size.height));
                        });
                        if let Err(e) = p.save_if_changed() {
                            tracing::warn!(error = %e, "failed to save GUI state on quit");
                        }
                    }
                    async {}
                }
            });

            if let Some(seconds) = exit_after {
                cx.spawn(async move |cx: &mut AsyncApp| {
                    Timer::after(Duration::from_secs(seconds)).await;
                    let _ = cx.update(|cx| cx.quit());
                })
                .detach();
            }
        });
}

fn window_options(restored_width: Option<f32>, restored_height: Option<f32>) -> WindowOptions {
    let width = restored_width.unwrap_or(1200.0);
    let height = restored_height.unwrap_or(800.0);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            Point::default(),
            Size {
                width: px(width),
                height: px(height),
            },
        ))),
        app_id: Some("zremote-gui".to_string()),
        ..Default::default()
    }
}
