export function isBrowserNotificationSupported(): boolean {
  return "Notification" in window;
}

export function getBrowserPermission(): NotificationPermission | "unsupported" {
  if (!isBrowserNotificationSupported()) return "unsupported";
  return Notification.permission;
}

export async function requestBrowserPermission(): Promise<NotificationPermission> {
  if (!isBrowserNotificationSupported()) return "denied";
  return Notification.requestPermission();
}

export function showBrowserNotification(
  title: string,
  options: { body: string; tag: string; onClick?: () => void },
): void {
  if (!isBrowserNotificationSupported()) return;
  if (Notification.permission !== "granted") return;
  if (document.visibilityState !== "hidden") return;

  const notification = new Notification(title, {
    body: options.body,
    tag: options.tag,
    icon: "/favicon.ico",
  });

  notification.onclick = () => {
    window.focus();
    options.onClick?.();
    notification.close();
  };
}
