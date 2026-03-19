import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";
import { SessionPage } from "./SessionPage";

// Mock Terminal (xterm.js hard to test in jsdom)
vi.mock("../components/Terminal", () => ({
  Terminal: ({ sessionId }: { sessionId: string }) => (
    <div data-testid="terminal">Terminal: {sessionId}</div>
  ),
}));

vi.mock("../hooks/useHosts", () => ({
  useHosts: () => ({
    hosts: [
      {
        id: "host-1",
        hostname: "test-server",
        status: "online",
        agent_version: "0.1.0",
        os: "linux",
        arch: "x86_64",
        last_seen: new Date().toISOString(),
        created_at: new Date().toISOString(),
      },
    ],
    loading: false,
  }),
}));

vi.mock("../hooks/useSessions", () => ({
  useSessions: () => ({
    sessions: [
      {
        id: "sess-1",
        host_id: "host-1",
        name: "dev-session",
        shell: "/bin/zsh",
        status: "active",
        pid: 1234,
        working_dir: "/home",
        project_id: null,
        created_at: new Date().toISOString(),
        last_activity: new Date().toISOString(),
      },
    ],
    loading: false,
    refetch: vi.fn(),
  }),
}));

vi.mock("../hooks/useAgenticLoops", () => ({
  useAgenticLoops: () => ({ loops: [], loading: false }),
}));

vi.mock("../stores/claude-task-store", () => ({
  useClaudeTaskStore: Object.assign(
    (selector: (s: { sessionTaskIndex: Map<string, string>; tasks: Map<string, unknown> }) => unknown) =>
      selector({ sessionTaskIndex: new Map(), tasks: new Map() }),
    {
      getState: () => ({ fetchTasks: vi.fn() }),
    },
  ),
}));

function renderSessionPage() {
  return render(
    <MemoryRouter initialEntries={["/hosts/host-1/sessions/sess-1"]}>
      <Routes>
        <Route
          path="/hosts/:hostId/sessions/:sessionId"
          element={<SessionPage />}
        />
      </Routes>
    </MemoryRouter>,
  );
}

describe("SessionPage", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  test("renders session name", () => {
    renderSessionPage();
    expect(screen.getByText("dev-session")).toBeInTheDocument();
  });

  test("renders session status badge", () => {
    renderSessionPage();
    expect(screen.getByText("active")).toBeInTheDocument();
  });

  test("renders Terminal component", () => {
    renderSessionPage();
    expect(screen.getByTestId("terminal")).toBeInTheDocument();
  });

  test("renders host link", () => {
    renderSessionPage();
    expect(screen.getByText("test-server")).toBeInTheDocument();
  });

  test("shows Session not found for unknown session", () => {
    render(
      <MemoryRouter initialEntries={["/hosts/host-1/sessions/unknown"]}>
        <Routes>
          <Route
            path="/hosts/:hostId/sessions/:sessionId"
            element={<SessionPage />}
          />
        </Routes>
      </MemoryRouter>,
    );
    expect(screen.getByText("Session not found")).toBeInTheDocument();
  });
});
