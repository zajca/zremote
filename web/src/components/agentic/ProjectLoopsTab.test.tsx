import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { ProjectLoopsTab } from "./ProjectLoopsTab";
import type { AgenticLoop } from "../../types/agentic";
import type { ClaudeTask } from "../../types/claude-session";

const mockLoop = (overrides: Partial<AgenticLoop> = {}): AgenticLoop => ({
  id: "loop-1",
  session_id: "sess-1",
  project_path: "/home/user/project",
  tool_name: "claude_code",
  model: "sonnet",
  status: "completed",
  started_at: new Date(Date.now() - 60_000).toISOString(),
  ended_at: new Date().toISOString(),
  total_tokens_in: 150_000,
  total_tokens_out: 50_000,
  estimated_cost_usd: 1.25,
  end_reason: null,
  summary: null,
  context_used: 0,
  context_max: 200_000,
  pending_tool_calls: 0,
  ...overrides,
});

const mockTask = (overrides: Partial<ClaudeTask> = {}): ClaudeTask => ({
  id: "task-1",
  session_id: "sess-1",
  host_id: "host-1",
  project_path: "/home/user/project",
  project_id: "proj-1",
  model: "sonnet",
  initial_prompt: "Fix the tests",
  claude_session_id: null,
  resume_from: null,
  status: "completed",
  options_json: null,
  loop_id: null,
  started_at: new Date(Date.now() - 120_000).toISOString(),
  ended_at: new Date().toISOString(),
  total_cost_usd: 0.75,
  total_tokens_in: 100_000,
  total_tokens_out: 30_000,
  summary: "Fixed all tests successfully",
  created_at: new Date().toISOString(),
  ...overrides,
});

function mockFetch(loops: AgenticLoop[] = [], tasks: ClaudeTask[] = []) {
  global.fetch = vi.fn().mockImplementation((url: string) => {
    if (url.includes("/api/loops")) {
      return Promise.resolve({ ok: true, json: async () => loops, text: async () => JSON.stringify(loops) });
    }
    if (url.includes("/api/claude-task") || url.includes("/api/claude-sessions")) {
      return Promise.resolve({ ok: true, json: async () => tasks, text: async () => JSON.stringify(tasks) });
    }
    return Promise.resolve({ ok: true, json: async () => [], text: async () => "[]" });
  });
}

beforeEach(() => {
  vi.restoreAllMocks();
  mockFetch();
});

describe("ProjectLoopsTab", () => {
  test("shows loading state initially", () => {
    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("Loading...")).toBeInTheDocument();
  });

  test("shows empty state when no loops or tasks", async () => {
    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(
        screen.getByText("No Claude tasks or agentic loops for this project yet."),
      ).toBeInTheDocument();
    });
  });

  test("renders active loops section when there are working loops", async () => {
    const workingLoop = mockLoop({ id: "loop-active", status: "working", tool_name: "edit_file" });
    mockFetch([workingLoop]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText(/Active Loops/)).toBeInTheDocument();
      expect(screen.getByText("edit_file")).toBeInTheDocument();
      expect(screen.getByText("working")).toBeInTheDocument();
    });
  });

  test("renders history loops section for completed loops", async () => {
    const completedLoop = mockLoop({ status: "completed", tool_name: "read_file" });
    mockFetch([completedLoop]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText(/Loop History/)).toBeInTheDocument();
      expect(screen.getByText("read_file")).toBeInTheDocument();
      expect(screen.getByText("completed")).toBeInTheDocument();
    });
  });

  test("renders active tasks section for starting/active tasks", async () => {
    const activeTask = mockTask({ id: "task-active", status: "active", initial_prompt: "Implement feature X" });
    mockFetch([], [activeTask]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText(/Active Tasks/)).toBeInTheDocument();
      expect(screen.getByText("Implement feature X")).toBeInTheDocument();
      expect(screen.getByText("active")).toBeInTheDocument();
    });
  });

  test("renders completed tasks section with resume button", async () => {
    const completedTask = mockTask({ status: "completed", initial_prompt: "Fix bug" });
    mockFetch([], [completedTask]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText(/Completed Tasks/)).toBeInTheDocument();
      expect(screen.getByText("Fix bug")).toBeInTheDocument();
      expect(screen.getByText("Resume")).toBeInTheDocument();
    });
  });

  test("displays cost and token information for loops", async () => {
    const loop = mockLoop({ estimated_cost_usd: 2.5, total_tokens_in: 1_500_000, total_tokens_out: 500_000 });
    mockFetch([loop]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("$2.50")).toBeInTheDocument();
      expect(screen.getByText(/1\.5M/)).toBeInTheDocument();
      expect(screen.getByText(/500\.0k/)).toBeInTheDocument();
    });
  });

  test("displays cost and token information for tasks", async () => {
    const task = mockTask({ total_cost_usd: 3.14, total_tokens_in: 200_000, total_tokens_out: 80_000 });
    mockFetch([], [task]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("$3.14")).toBeInTheDocument();
      expect(screen.getByText(/200\.0k/)).toBeInTheDocument();
      expect(screen.getByText(/80\.0k/)).toBeInTheDocument();
    });
  });

  test("shows pending tool call count for waiting loops", async () => {
    const waitingLoop = mockLoop({
      status: "waiting_for_input",
      pending_tool_calls: 3,
    });
    mockFetch([waitingLoop]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("3 pending")).toBeInTheDocument();
    });
  });

  test("shows project path in loop card", async () => {
    const loop = mockLoop({ project_path: "/home/user/my-app" });
    mockFetch([loop]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("/home/user/my-app")).toBeInTheDocument();
    });
  });

  test("shows task summary when available", async () => {
    const task = mockTask({ status: "completed", summary: "Refactored the auth module" });
    mockFetch([], [task]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("Refactored the auth module")).toBeInTheDocument();
    });
  });

  test("truncates long initial prompt in task card", async () => {
    const longPrompt = "A".repeat(100);
    const task = mockTask({ status: "active", initial_prompt: longPrompt });
    mockFetch([], [task]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      // Should show first 80 chars + "..."
      expect(screen.getByText("A".repeat(80) + "...")).toBeInTheDocument();
    });
  });

  test("shows model name as fallback when task has no prompt", async () => {
    const task = mockTask({ status: "active", initial_prompt: null, model: "opus" });
    mockFetch([], [task]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      // "opus" appears as the title (model fallback) and in the detail row
      const opusTexts = screen.getAllByText("opus");
      expect(opusTexts.length).toBeGreaterThanOrEqual(1);
    });
  });

  test("shows Refresh button and handles click", async () => {
    const activeTask = mockTask({ status: "active" });
    mockFetch([], [activeTask]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("Refresh")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("Refresh"));
    // Should have called fetch again for both loops and tasks
    expect(global.fetch).toHaveBeenCalled();
  });

  test("shows both active tasks and active loops simultaneously", async () => {
    const activeTask = mockTask({ status: "starting", initial_prompt: "Task prompt" });
    const activeLoop = mockLoop({ status: "working", tool_name: "grep_search" });
    mockFetch([activeLoop], [activeTask]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText(/Active Tasks/)).toBeInTheDocument();
      expect(screen.getByText(/Active Loops/)).toBeInTheDocument();
      expect(screen.getByText("Task prompt")).toBeInTheDocument();
      expect(screen.getByText("grep_search")).toBeInTheDocument();
    });
  });

  test("renders error status loop in history section", async () => {
    const errorLoop = mockLoop({ status: "error", tool_name: "bash" });
    mockFetch([errorLoop]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText(/Loop History/)).toBeInTheDocument();
      expect(screen.getByText("error")).toBeInTheDocument();
    });
  });

  test("renders task with error status in completed section", async () => {
    const errorTask = mockTask({ status: "error", initial_prompt: "Broken task" });
    mockFetch([], [errorTask]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText(/Completed Tasks/)).toBeInTheDocument();
      expect(screen.getByText("Broken task")).toBeInTheDocument();
    });
  });

  test("displays duration for completed task with ended_at", async () => {
    const task = mockTask({
      status: "completed",
      started_at: "2026-03-16T10:00:00Z",
      ended_at: "2026-03-16T10:05:30Z",
    });
    mockFetch([], [task]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("5m 30s")).toBeInTheDocument();
    });
  });

  test("displays hours in duration for long loops", async () => {
    const loop = mockLoop({
      status: "completed",
      started_at: "2026-03-16T10:00:00Z",
      ended_at: "2026-03-16T12:15:00Z",
    });
    mockFetch([loop]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("2h 15m")).toBeInTheDocument();
    });
  });

  test("formats small token counts without suffix", async () => {
    const loop = mockLoop({
      total_tokens_in: 500,
      total_tokens_out: 200,
    });
    mockFetch([loop]);

    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText(/500/)).toBeInTheDocument();
      expect(screen.getByText(/200/)).toBeInTheDocument();
    });
  });
});
