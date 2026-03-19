import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { AgenticLoopPanel } from "./AgenticLoopPanel";

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
  context_used: 30000,
  context_max: 200000,
  pending_tool_calls: 2,
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

describe("AgenticLoopPanel", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  test("renders loop tool name", () => {
    render(<AgenticLoopPanel loopId="loop-1" />);
    expect(screen.getByText("claude-code")).toBeInTheDocument();
  });

  test("renders status badge", () => {
    render(<AgenticLoopPanel loopId="loop-1" />);
    expect(screen.getByText("working")).toBeInTheDocument();
  });

  test("renders tab buttons", () => {
    render(<AgenticLoopPanel loopId="loop-1" />);
    expect(screen.getByText("Tool Queue")).toBeInTheDocument();
    expect(screen.getByText("Transcript")).toBeInTheDocument();
  });

  test("shows loading when loop not found", () => {
    vi.doMock("../../stores/agentic-store", () => ({
      useAgenticStore: Object.assign(
        (selector: (s: unknown) => unknown) =>
          selector({
            activeLoops: new Map(),
            toolCalls: new Map(),
            transcripts: new Map(),
          }),
        {
          getState: () => ({
            fetchLoop: vi.fn(),
            fetchToolCalls: vi.fn(),
            fetchTranscript: vi.fn(),
          }),
        },
      ),
    }));
    // Loading state for unknown loop IDs
    render(<AgenticLoopPanel loopId="nonexistent" />);
    expect(screen.getByText("Loading loop...")).toBeInTheDocument();
  });
});
