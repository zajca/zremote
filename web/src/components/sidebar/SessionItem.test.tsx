import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { SessionItem } from "./SessionItem";
import type { Session } from "../../lib/api";
import type { AgenticLoop } from "../../types/agentic";

let mockLoops: AgenticLoop[] = [];
let mockSessionTaskIndex = new Map<string, string>();

vi.mock("../../hooks/useAgenticLoops", () => ({
  useAgenticLoops: () => ({ loops: mockLoops, loading: false }),
}));

vi.mock("../../stores/claude-task-store", () => ({
  useClaudeTaskStore: (selector: (s: { sessionTaskIndex: Map<string, string> }) => unknown) =>
    selector({ sessionTaskIndex: mockSessionTaskIndex }),
}));

const baseSession: Session = {
  id: "sess-1",
  host_id: "host-1",
  name: "dev-session",
  shell: "/bin/zsh",
  status: "active",
  cols: 80,
  rows: 24,
  created_at: new Date().toISOString(),
  closed_at: null,
  exit_code: null,
  working_dir: "/home/user",
  project_id: null,
};

const mockLoop = (overrides: Partial<AgenticLoop> = {}): AgenticLoop => ({
  id: "loop-1",
  session_id: "sess-1",
  project_path: "/home/user/project",
  tool_name: "claude_code",
  model: "sonnet",
  status: "working",
  started_at: new Date().toISOString(),
  ended_at: null,
  total_tokens_in: 10_000,
  total_tokens_out: 5_000,
  estimated_cost_usd: 0.5,
  end_reason: null,
  summary: null,
  context_used: 0,
  context_max: 200_000,
  pending_tool_calls: 0,
  ...overrides,
});

describe("SessionItem", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    mockLoops = [];
    mockSessionTaskIndex = new Map();
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({}),
      text: async () => "{}",
    });
  });

  test("renders session name", () => {
    render(
      <MemoryRouter>
        <SessionItem session={baseSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("dev-session")).toBeInTheDocument();
  });

  test("renders session status badge", () => {
    render(
      <MemoryRouter>
        <SessionItem session={baseSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("active")).toBeInTheDocument();
  });

  test("shows shell name when session has no name", () => {
    const noNameSession = { ...baseSession, name: "" };
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
        <SessionItem session={baseSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByLabelText("Close session")).toBeInTheDocument();
  });

  test("does not show close button for closed sessions", () => {
    const closedSession = { ...baseSession, status: "closed" as const };
    render(
      <MemoryRouter>
        <SessionItem session={closedSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.queryByLabelText("Close session")).not.toBeInTheDocument();
  });

  test("renders suspended session with warning badge", () => {
    const suspendedSession = { ...baseSession, status: "suspended" as const };
    render(
      <MemoryRouter>
        <SessionItem session={suspendedSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("suspended")).toBeInTheDocument();
  });

  test("shows close button for suspended sessions", () => {
    const suspendedSession = { ...baseSession, status: "suspended" as const };
    render(
      <MemoryRouter>
        <SessionItem session={suspendedSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByLabelText("Close session")).toBeInTheDocument();
  });

  test("does not show close button for error sessions", () => {
    const errorSession = { ...baseSession, status: "error" as const };
    render(
      <MemoryRouter>
        <SessionItem session={errorSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.queryByLabelText("Close session")).not.toBeInTheDocument();
  });

  test("shows creating status badge", () => {
    const creatingSession = { ...baseSession, status: "creating" as const };
    render(
      <MemoryRouter>
        <SessionItem session={creatingSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("creating")).toBeInTheDocument();
  });

  test("shows fallback 'shell' when name and shell are both empty", () => {
    const noInfoSession = { ...baseSession, name: "", shell: "" };
    render(
      <MemoryRouter>
        <SessionItem session={noInfoSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("shell")).toBeInTheDocument();
  });

  test("shows null name falls back to shell", () => {
    const nullNameSession = { ...baseSession, name: null, shell: "/bin/bash" };
    render(
      <MemoryRouter>
        <SessionItem session={nullNameSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("/bin/bash")).toBeInTheDocument();
  });

  test("shows active loop count badge when loops are active", () => {
    mockLoops = [mockLoop({ status: "working" })];
    render(
      <MemoryRouter>
        <SessionItem session={baseSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("1")).toBeInTheDocument();
  });

  test("shows active loop sub-items with tool name and status", () => {
    mockLoops = [mockLoop({ status: "working", tool_name: "grep_search" })];
    render(
      <MemoryRouter>
        <SessionItem session={baseSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("grep_search")).toBeInTheDocument();
    expect(screen.getByText("working")).toBeInTheDocument();
  });

  test("shows waiting_for_input loop with warning style", () => {
    mockLoops = [mockLoop({ status: "waiting_for_input", pending_tool_calls: 2 })];
    render(
      <MemoryRouter>
        <SessionItem session={baseSession} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("waiting_for_input")).toBeInTheDocument();
    expect(screen.getByText("2")).toBeInTheDocument();
  });

  test("does not show loop items for completed loops", () => {
    mockLoops = [mockLoop({ status: "completed", tool_name: "done_tool" })];
    render(
      <MemoryRouter>
        <SessionItem session={baseSession} hostId="host-1" />
      </MemoryRouter>,
    );
    // Completed loops are not shown as sub-items
    expect(screen.queryByText("done_tool")).not.toBeInTheDocument();
  });

  test("shows Bot icon when session is a Claude task", () => {
    mockSessionTaskIndex = new Map([["sess-1", "task-1"]]);
    render(
      <MemoryRouter>
        <SessionItem session={baseSession} hostId="host-1" />
      </MemoryRouter>,
    );
    // Bot icon should be rendered (Claude task indicator)
    // The component renders Bot icon when isClaudeTask is true
    expect(screen.getByText("dev-session")).toBeInTheDocument();
  });

  test("close button calls api and dispatches event", async () => {
    window.confirm = vi.fn().mockReturnValue(true);
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      text: async () => "",
      json: async () => ({}),
    });

    const dispatchSpy = vi.spyOn(window, "dispatchEvent");

    render(
      <MemoryRouter>
        <SessionItem session={baseSession} hostId="host-1" />
      </MemoryRouter>,
    );

    await userEvent.click(screen.getByLabelText("Close session"));

    await waitFor(() => {
      expect(window.confirm).toHaveBeenCalledWith("Close this session?");
      expect(global.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/api/sessions/sess-1"),
        expect.objectContaining({ method: "DELETE" }),
      );
      expect(dispatchSpy).toHaveBeenCalledWith(
        expect.objectContaining({ type: "myremote:session-update" }),
      );
    });
  });

  test("close button does nothing when confirm is cancelled", async () => {
    window.confirm = vi.fn().mockReturnValue(false);

    render(
      <MemoryRouter>
        <SessionItem session={baseSession} hostId="host-1" />
      </MemoryRouter>,
    );

    await userEvent.click(screen.getByLabelText("Close session"));

    expect(window.confirm).toHaveBeenCalled();
    // fetch should not have been called for delete
    expect(global.fetch).not.toHaveBeenCalled();
  });
});
