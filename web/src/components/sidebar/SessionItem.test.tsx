import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { SessionItem } from "./SessionItem";
import type { Session } from "../../lib/api";

vi.mock("../../hooks/useAgenticLoops", () => ({
  useAgenticLoops: () => ({ loops: [], loading: false }),
}));

vi.mock("../../stores/claude-task-store", () => ({
  useClaudeTaskStore: (selector: (s: { sessionTaskIndex: Map<string, string> }) => unknown) =>
    selector({ sessionTaskIndex: new Map() }),
}));

const mockSession: Session = {
  id: "sess-1",
  host_id: "host-1",
  name: "dev-session",
  shell: "/bin/zsh",
  status: "active",
  pid: 1234,
  working_dir: "/home/user",
  project_id: null,
  created_at: new Date().toISOString(),
  last_activity: new Date().toISOString(),
};

describe("SessionItem", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  test("renders session name", () => {
    render(
      <MemoryRouter>
        <SessionItem session={mockSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("dev-session")).toBeInTheDocument();
  });

  test("renders session status badge", () => {
    render(
      <MemoryRouter>
        <SessionItem session={mockSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("active")).toBeInTheDocument();
  });

  test("shows shell name when session has no name", () => {
    const noNameSession = { ...mockSession, name: "" };
    render(
      <MemoryRouter>
        <SessionItem session={noNameSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("/bin/zsh")).toBeInTheDocument();
  });

  test("shows close button for active sessions", () => {
    render(
      <MemoryRouter>
        <SessionItem session={mockSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByLabelText("Close session")).toBeInTheDocument();
  });

  test("does not show close button for closed sessions", () => {
    const closedSession = { ...mockSession, status: "closed" as const };
    render(
      <MemoryRouter>
        <SessionItem session={closedSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.queryByLabelText("Close session")).not.toBeInTheDocument();
  });

  test("renders suspended session with warning badge", () => {
    const suspendedSession = { ...mockSession, status: "suspended" as const };
    render(
      <MemoryRouter>
        <SessionItem session={suspendedSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("suspended")).toBeInTheDocument();
  });
});
