import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { HostItem } from "./HostItem";
import type { Host } from "../../lib/api";

vi.mock("../../hooks/useSessions", () => ({
  useSessions: () => ({ sessions: [], loading: false }),
}));

vi.mock("../../hooks/useProjects", () => ({
  useProjects: () => ({ projects: [], loading: false }),
}));

const mockHost: Host = {
  id: "host-1",
  hostname: "my-server",
  status: "online",
  agent_version: "0.1.0",
  os: "linux",
  arch: "x86_64",
  last_seen: new Date().toISOString(),
  created_at: new Date().toISOString(),
};

describe("HostItem", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    localStorage.clear();
  });

  test("renders hostname", () => {
    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );
    expect(screen.getByText("my-server")).toBeInTheDocument();
  });

  test("renders expand/collapse button", () => {
    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );
    expect(screen.getByLabelText("Expand")).toBeInTheDocument();
  });

  test("toggles expanded state on click", async () => {
    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );
    const toggleBtn = screen.getByLabelText("Expand");
    await userEvent.click(toggleBtn);
    expect(screen.getByLabelText("Collapse")).toBeInTheDocument();
  });

  test("renders offline host", () => {
    const offlineHost = { ...mockHost, status: "offline" as const };
    render(
      <MemoryRouter>
        <HostItem host={offlineHost} />
      </MemoryRouter>,
    );
    expect(screen.getByText("my-server")).toBeInTheDocument();
  });
});
