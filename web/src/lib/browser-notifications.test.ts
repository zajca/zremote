import { describe, test, expect, vi, beforeEach } from "vitest";
import {
  isBrowserNotificationSupported,
  getBrowserPermission,
  requestBrowserPermission,
  showBrowserNotification,
} from "./browser-notifications";

let mockNotificationInstance: {
  onclick: ((e: Event) => void) | null;
  close: ReturnType<typeof vi.fn>;
};

const MockNotification = vi.fn().mockImplementation(() => {
  mockNotificationInstance = { onclick: null, close: vi.fn() };
  return mockNotificationInstance;
}) as unknown as typeof Notification;

Object.defineProperty(MockNotification, "permission", {
  value: "granted",
  writable: true,
  configurable: true,
});
MockNotification.requestPermission = vi.fn().mockResolvedValue("granted");

beforeEach(() => {
  vi.clearAllMocks();
  (MockNotification as unknown as ReturnType<typeof vi.fn>).mockImplementation(() => {
    mockNotificationInstance = { onclick: null, close: vi.fn() };
    return mockNotificationInstance;
  });
  vi.stubGlobal("Notification", MockNotification);
  Object.defineProperty(MockNotification, "permission", { value: "granted", writable: true, configurable: true });
  (MockNotification.requestPermission as ReturnType<typeof vi.fn>).mockResolvedValue("granted");
  Object.defineProperty(document, "visibilityState", {
    value: "hidden",
    writable: true,
    configurable: true,
  });
});

describe("isBrowserNotificationSupported", () => {
  test("returns true when Notification exists in window", () => {
    expect(isBrowserNotificationSupported()).toBe(true);
  });

  test("returns false when Notification does not exist", () => {
    // @ts-expect-error - testing missing API
    delete window.Notification;
    expect(isBrowserNotificationSupported()).toBe(false);
    // Restore for other tests
    vi.stubGlobal("Notification", MockNotification);
  });
});

describe("getBrowserPermission", () => {
  test("returns current permission", () => {
    Object.defineProperty(MockNotification, "permission", { value: "denied", writable: true, configurable: true });
    expect(getBrowserPermission()).toBe("denied");
  });

  test("returns 'unsupported' when API missing", () => {
    // @ts-expect-error - testing missing API
    delete window.Notification;
    expect(getBrowserPermission()).toBe("unsupported");
    vi.stubGlobal("Notification", MockNotification);
  });
});

describe("requestBrowserPermission", () => {
  test("calls Notification.requestPermission and returns result", async () => {
    const result = await requestBrowserPermission();
    expect(Notification.requestPermission).toHaveBeenCalled();
    expect(result).toBe("granted");
  });

  test("returns 'denied' when API not supported", async () => {
    // @ts-expect-error - testing missing API
    delete window.Notification;
    const result = await requestBrowserPermission();
    expect(result).toBe("denied");
    vi.stubGlobal("Notification", MockNotification);
  });
});

describe("showBrowserNotification", () => {
  test("creates notification when hidden and granted", () => {
    showBrowserNotification("Test", { body: "body", tag: "t1" });
    expect(MockNotification).toHaveBeenCalledWith("Test", {
      body: "body",
      tag: "t1",
      icon: "/favicon.ico",
    });
  });

  test("does not create notification when tab is visible", () => {
    Object.defineProperty(document, "visibilityState", {
      value: "visible",
      configurable: true,
    });
    showBrowserNotification("Test", { body: "body", tag: "t1" });
    expect(MockNotification).not.toHaveBeenCalled();
  });

  test("does not create notification when permission is not granted", () => {
    Object.defineProperty(MockNotification, "permission", { value: "denied", writable: true, configurable: true });
    showBrowserNotification("Test", { body: "body", tag: "t1" });
    expect(MockNotification).not.toHaveBeenCalled();
  });

  test("onclick focuses window, calls onClick, and closes", () => {
    const onClick = vi.fn();
    const focusSpy = vi.spyOn(window, "focus").mockImplementation(() => {});
    showBrowserNotification("Test", { body: "body", tag: "t1", onClick });

    mockNotificationInstance.onclick?.(new Event("click"));
    expect(focusSpy).toHaveBeenCalled();
    expect(onClick).toHaveBeenCalled();
    expect(mockNotificationInstance.close).toHaveBeenCalled();
  });

  test("onclick works without onClick callback", () => {
    const focusSpy = vi.spyOn(window, "focus").mockImplementation(() => {});
    showBrowserNotification("Test", { body: "body", tag: "t1" });

    mockNotificationInstance.onclick?.(new Event("click"));
    expect(focusSpy).toHaveBeenCalled();
    expect(mockNotificationInstance.close).toHaveBeenCalled();
  });

  test("does nothing when API not supported", () => {
    // @ts-expect-error - testing missing API
    delete window.Notification;
    showBrowserNotification("Test", { body: "body", tag: "t1" });
    // Should not throw
    vi.stubGlobal("Notification", MockNotification);
  });
});
