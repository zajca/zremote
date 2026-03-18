import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { SettingsPage } from "./SettingsPage";
import type { Host } from "../../lib/api";

let mockHosts: Host[] = [];
let mockIsLocal = false;

vi.mock("../../hooks/useHosts", () => ({
  useHosts: () => ({ hosts: mockHosts, loading: false, error: null }),
}));

vi.mock("../../hooks/useMode", () => ({
  useMode: () => ({ mode: mockIsLocal ? "local" : "server", isLocal: mockIsLocal }),
}));

vi.mock("../../lib/browser-notifications", () => ({
  requestBrowserPermission: vi.fn().mockResolvedValue("granted"),
}));

vi.mock("../../stores/notification-store", () => ({
  useNotificationStore: {
    getState: vi.fn(() => ({
      setBrowserPermission: vi.fn(),
      setBrowserEnabled: vi.fn(),
    })),
  },
}));

beforeEach(() => {
  vi.clearAllMocks();
  mockHosts = [];
  mockIsLocal = false;
  global.fetch = vi.fn().mockImplementation((url: string) => {
    // GET global config
    if (url.includes("/api/config/") && !url.includes("/hosts/")) {
      return Promise.resolve({
        ok: true,
        text: async () => JSON.stringify({ key: "test", value: "false", updated_at: "2026-01-01T00:00:00Z" }),
        json: async () => ({ key: "test", value: "false", updated_at: "2026-01-01T00:00:00Z" }),
      });
    }
    // GET host config
    if (url.includes("/hosts/") && url.includes("/config/")) {
      return Promise.resolve({
        ok: true,
        text: async () => JSON.stringify({ key: "test", value: "false", updated_at: "2026-01-01T00:00:00Z" }),
        json: async () => ({ key: "test", value: "false", updated_at: "2026-01-01T00:00:00Z" }),
      });
    }
    return Promise.resolve({
      ok: true,
      text: async () => "{}",
      json: async () => ({}),
    });
  });
});

describe("SettingsPage", () => {
  test("renders Settings heading", () => {
    render(<SettingsPage />);
    expect(screen.getByText("Settings")).toBeInTheDocument();
  });

  test("renders Global Settings section", () => {
    render(<SettingsPage />);
    expect(screen.getByText("Global Settings")).toBeInTheDocument();
  });

  test("renders Notifications setting", () => {
    render(<SettingsPage />);
    expect(screen.getByText("Notifications")).toBeInTheDocument();
    expect(screen.getByText("Enable browser notifications when Claude needs input")).toBeInTheDocument();
  });

  test("renders Auto-approve tools setting", () => {
    render(<SettingsPage />);
    expect(screen.getByText("Auto-approve tools")).toBeInTheDocument();
    expect(screen.getByText("Automatically approve safe tool calls")).toBeInTheDocument();
  });

  test("shows Per-Host Overrides section in server mode", () => {
    mockIsLocal = false;
    render(<SettingsPage />);
    expect(screen.getByText("Per-Host Overrides")).toBeInTheDocument();
  });

  test("hides Per-Host Overrides section in local mode", () => {
    mockIsLocal = true;
    render(<SettingsPage />);
    expect(screen.queryByText("Per-Host Overrides")).not.toBeInTheDocument();
  });

  test("shows host dropdown with hosts in server mode", () => {
    mockHosts = [
      {
        id: "host-1",
        hostname: "server-1",
        status: "online",
        agent_version: "0.1.0",
        os: "linux",
        arch: "x86_64",
        last_seen: new Date().toISOString(),
        connected_at: new Date().toISOString(),
      },
    ];

    render(<SettingsPage />);
    expect(screen.getByText("Select a host...")).toBeInTheDocument();
    expect(screen.getByText("server-1")).toBeInTheDocument();
  });

  test("shows host override settings when a host is selected", async () => {
    mockHosts = [
      {
        id: "host-1",
        hostname: "server-1",
        status: "online",
        agent_version: "0.1.0",
        os: "linux",
        arch: "x86_64",
        last_seen: new Date().toISOString(),
        connected_at: new Date().toISOString(),
      },
    ];

    render(<SettingsPage />);

    const select = screen.getByDisplayValue("Select a host...");
    await userEvent.selectOptions(select, "host-1");

    await waitFor(() => {
      // Override labels appear
      const overrideTexts = screen.getAllByText("Override for this host");
      expect(overrideTexts.length).toBe(2); // One per config key
    });
  });

  test("shows Saved indicator after toggling a global setting", async () => {
    // Make the PUT succeed
    global.fetch = vi.fn().mockImplementation((url: string, options?: RequestInit) => {
      if (options?.method === "PUT") {
        return Promise.resolve({
          ok: true,
          text: async () => JSON.stringify({ key: "test", value: "true", updated_at: "2026-01-01T00:00:00Z" }),
          json: async () => ({ key: "test", value: "true", updated_at: "2026-01-01T00:00:00Z" }),
        });
      }
      return Promise.resolve({
        ok: true,
        text: async () => JSON.stringify({ key: "test", value: "false", updated_at: "2026-01-01T00:00:00Z" }),
        json: async () => ({ key: "test", value: "false", updated_at: "2026-01-01T00:00:00Z" }),
      });
    });

    render(<SettingsPage />);

    // Wait for config to load
    await waitFor(() => {
      expect(screen.getByText("Notifications")).toBeInTheDocument();
    });

    // Find all toggle buttons - there are 2 global settings
    // Click the second toggle (auto_approve) to avoid browser permission flow
    const toggleButtons = screen.getAllByRole("button").filter(
      (btn) => btn.className.includes("rounded-full"),
    );
    expect(toggleButtons.length).toBeGreaterThan(1);

    await userEvent.click(toggleButtons[1]);

    await waitFor(() => {
      expect(screen.getByText("Saved")).toBeInTheDocument();
    });
  });

  test("loads global config values on mount", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/api/config/notifications.enabled")) {
        return Promise.resolve({
          ok: true,
          text: async () => JSON.stringify({ key: "notifications.enabled", value: "true", updated_at: "2026-01-01T00:00:00Z" }),
          json: async () => ({ key: "notifications.enabled", value: "true", updated_at: "2026-01-01T00:00:00Z" }),
        });
      }
      if (url.includes("/api/config/auto_approve.enabled")) {
        return Promise.resolve({
          ok: true,
          text: async () => JSON.stringify({ key: "auto_approve.enabled", value: "false", updated_at: "2026-01-01T00:00:00Z" }),
          json: async () => ({ key: "auto_approve.enabled", value: "false", updated_at: "2026-01-01T00:00:00Z" }),
        });
      }
      return Promise.resolve({
        ok: true,
        text: async () => "{}",
        json: async () => ({}),
      });
    });

    render(<SettingsPage />);

    await waitFor(() => {
      // Fetch should have been called for each config key
      expect(global.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/api/config/notifications.enabled"),
        expect.anything(),
      );
      expect(global.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/api/config/auto_approve.enabled"),
        expect.anything(),
      );
    });
  });

  test("handles config load failure gracefully", async () => {
    global.fetch = vi.fn().mockImplementation(() =>
      Promise.resolve({
        ok: false,
        status: 404,
        text: async () => "Not found",
        statusText: "Not Found",
      }),
    );

    render(<SettingsPage />);

    // Should still render the page without errors
    expect(screen.getByText("Settings")).toBeInTheDocument();
    expect(screen.getByText("Global Settings")).toBeInTheDocument();
  });
});
