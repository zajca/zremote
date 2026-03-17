import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { AgenticOverlay } from "./AgenticOverlay";

const mockLoop = {
  id: "loop-1",
  session_id: "sess-1",
  project_path: "/home/user/project",
  tool_name: "claude-code",
  model: "sonnet",
  status: "working" as const,
  started_at: new Date().toISOString(),
  ended_at: null,
  total_tokens_in: 15000,
  total_tokens_out: 5000,
  estimated_cost_usd: 0.25,
  end_reason: null,
  summary: null,
  context_used: 90000,
  context_max: 200000,
  pending_tool_calls: 0,
};

vi.mock("../../stores/agentic-store", () => ({
  useAgenticStore: Object.assign(
    (selector: (s: unknown) => unknown) =>
      selector({
        activeLoops: new Map([["loop-1", mockLoop]]),
        toolCalls: new Map(),
        transcripts: new Map(),
      }),
    {
      getState: () => ({
        fetchLoop: vi.fn(),
        fetchToolCalls: vi.fn(),
        fetchTranscript: vi.fn(),
        sendAction: vi.fn().mockResolvedValue(undefined),
      }),
    },
  ),
}));

describe("AgenticOverlay", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  test("renders header bar with tool name and status", () => {
    render(<AgenticOverlay loopId="loop-1" />);
    expect(screen.getByText("claude-code")).toBeInTheDocument();
    expect(screen.getByText("working")).toBeInTheDocument();
  });

  test("renders compact cost in header", () => {
    render(<AgenticOverlay loopId="loop-1" />);
    const costEl = screen.getByText("$0.25");
    expect(costEl).toBeInTheDocument();
    expect(costEl.getAttribute("title")).toBe("15.0k in / 5.0k out");
  });

  test("renders compact context bar with percentage", () => {
    render(<AgenticOverlay loopId="loop-1" />);
    expect(screen.getByText("45%")).toBeInTheDocument();
  });

  test("renders expand/collapse toggle", () => {
    render(<AgenticOverlay loopId="loop-1" />);
    expect(screen.getByTitle(/overlay/i)).toBeInTheDocument();
  });

  test("does not show overlay panel when collapsed", () => {
    render(<AgenticOverlay loopId="loop-1" />);
    expect(screen.queryByText("Tool Queue")).not.toBeInTheDocument();
  });

  test("shows overlay panel with tabs when expanded", async () => {
    render(<AgenticOverlay loopId="loop-1" />);
    await userEvent.click(screen.getByTitle(/expand overlay/i));
    expect(screen.getByText("Tool Queue")).toBeInTheDocument();
    expect(screen.getByText("Transcript")).toBeInTheDocument();
  });

  test("returns null when loop not found", () => {
    const { container } = render(<AgenticOverlay loopId="nonexistent" />);
    expect(container.innerHTML).toBe("");
  });
});
