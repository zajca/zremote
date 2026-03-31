use crate::views::toast::ToastLevel;

/// Send a native OS notification via tokio's blocking thread pool.
///
/// Uses `spawn_blocking` (not `std::thread::spawn`) to keep thread creation
/// bounded by tokio's blocking pool limit. The notify-rust default "z" feature
/// uses zbus/async-io internally -- do NOT switch to "z-with-tokio" without
/// revisiting this.
pub fn send_native(title: &str, body: &str, level: ToastLevel, handle: &tokio::runtime::Handle) {
    let title = title.to_string();
    let body = body.to_string();
    handle.spawn_blocking(move || {
        let mut notification = notify_rust::Notification::new();
        notification.appname("ZRemote").summary(&title).body(&body);

        #[cfg(target_os = "linux")]
        {
            let urgency = match level {
                ToastLevel::Error => notify_rust::Urgency::Critical,
                ToastLevel::Warning => notify_rust::Urgency::Normal,
                ToastLevel::Info | ToastLevel::Success => notify_rust::Urgency::Low,
            };
            notification.urgency(urgency);
        }

        if let Err(e) = notification.show() {
            tracing::warn!(error = %e, "failed to send native notification");
        }
    });
}
