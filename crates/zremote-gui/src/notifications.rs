use crate::views::toast::ToastLevel;

/// Optional urgency override for native notifications.
#[derive(Debug, Clone, Copy)]
pub enum NativeUrgency {
    /// Derive urgency from toast level (default behavior).
    Auto,
    /// Force critical urgency regardless of toast level.
    Critical,
}

/// Send a native OS notification via tokio's blocking thread pool.
///
/// Uses `spawn_blocking` (not `std::thread::spawn`) to keep thread creation
/// bounded by tokio's blocking pool limit. The notify-rust default "z" feature
/// uses zbus/async-io internally -- do NOT switch to "z-with-tokio" without
/// revisiting this.
pub fn send_native(title: &str, body: &str, level: ToastLevel, handle: &tokio::runtime::Handle) {
    send_native_with_urgency(title, body, level, NativeUrgency::Auto, handle);
}

/// Send a native OS notification with explicit urgency control.
pub fn send_native_with_urgency(
    title: &str,
    body: &str,
    level: ToastLevel,
    urgency: NativeUrgency,
    handle: &tokio::runtime::Handle,
) {
    let title = title.to_string();
    let body = body.to_string();
    handle.spawn_blocking(move || {
        let mut notification = notify_rust::Notification::new();
        notification.appname("ZRemote").summary(&title).body(&body);

        #[cfg(target_os = "linux")]
        {
            let urg = match urgency {
                NativeUrgency::Critical => notify_rust::Urgency::Critical,
                NativeUrgency::Auto => match level {
                    ToastLevel::Error => notify_rust::Urgency::Critical,
                    ToastLevel::Warning => notify_rust::Urgency::Normal,
                    ToastLevel::Info | ToastLevel::Success => notify_rust::Urgency::Low,
                },
            };
            notification.urgency(urg);
        }

        if let Err(e) = notification.show() {
            tracing::warn!(error = %e, "failed to send native notification");
        }
    });
}
