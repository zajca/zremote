#[allow(dead_code)]
mod api;
#[allow(dead_code)]
mod app_state;
mod assets;
mod events_ws;
mod icons;
mod terminal_ws;
#[allow(dead_code)]
mod theme;
#[allow(dead_code)]
mod types;
mod views;

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use gpui::*;

use api::ApiClient;
use app_state::AppState;
use assets::Assets;
use types::ServerEvent;
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

    let exit_after = cli.exit_after;

    // Launch GPUI application on main thread
    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        let app_state = app_state.clone();
        cx.open_window(window_options(), move |window, cx| {
            cx.new(|cx| MainView::new(app_state, window, cx))
        })
        .expect("failed to open window");

        if let Some(seconds) = exit_after {
            cx.spawn(async move |cx: &mut AsyncApp| {
                Timer::after(Duration::from_secs(seconds)).await;
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
        }
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
