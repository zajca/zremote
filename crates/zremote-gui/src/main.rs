#[allow(dead_code)]
mod api;
#[allow(dead_code)]
mod app_state;
mod events_ws;
mod terminal_ws;
#[allow(dead_code)]
mod theme;
#[allow(dead_code)]
mod types;
mod views;

use std::sync::Arc;

use clap::Parser;
use gpui::*;

use api::ApiClient;
use app_state::AppState;
use types::ServerEvent;
use views::main_view::MainView;

#[derive(Parser)]
#[command(name = "zremote-gui", version, about = "ZRemote native desktop client")]
struct Cli {
    /// Server base URL, e.g. http://localhost:3000 or https://zremote.example.com
    /// (or set ZREMOTE_URL env var)
    #[arg(long, env = "ZREMOTE_URL", default_value = "http://localhost:3000")]
    server: String,
}

fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let server_url = cli.server.trim_end_matches('/').to_string();

    tracing::info!(server = %server_url, "starting ZRemote GUI");

    // Create tokio runtime on background threads (GPUI owns the main thread)
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");

    let tokio_handle = rt.handle().clone();

    // Detect server mode
    let mode = rt
        .block_on(async {
            let api = ApiClient::new(&server_url);
            api.get_mode().await
        })
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to detect server mode, assuming 'server'");
            "server".to_string()
        });

    tracing::info!(mode = %mode, "connected to server");

    // Start events WebSocket on background tokio task
    let (event_tx, event_rx) = flume::bounded::<ServerEvent>(256);
    let api = ApiClient::new(&server_url);
    let events_url = api.events_ws_url();
    rt.spawn(events_ws::run_events_ws(events_url, event_tx));

    let app_state = Arc::new(AppState {
        api,
        tokio_handle,
        event_rx,
        mode,
    });

    // Launch GPUI application on main thread
    Application::new().run(move |cx: &mut App| {
        let app_state = app_state.clone();
        cx.open_window(window_options(), move |window, cx| {
            cx.new(|cx| MainView::new(app_state, window, cx))
        })
        .expect("failed to open window");
    });
}

fn window_options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            Point::default(),
            Size {
                width: px(1200.0),
                height: px(800.0),
            },
        ))),
        app_id: Some("zremote-gui".to_string()),
        ..Default::default()
    }
}
