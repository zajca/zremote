import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { ToolCallQueue } from "./ToolCallQueue";
import type { ToolCall } from "../../types/agentic";

function makeToolCall(overrides: Partial<ToolCall> = {}): ToolCall {
  return {
    id: "tc-1",
    loop_id: "loop-1",
    tool_name: "Read",
    arguments_json: '{"path": "/src/main.rs"}',
    status: "pending",
    result_preview: null,
    duration_ms: null,
    created_at: new Date().toISOString(),
    resolved_at: null,
    ...overrides,
  };
}

describe("ToolCallQueue", () => {
  test("shows empty state when no tool calls", () => {
    render(
      <ToolCallQueue toolCalls={[]} onApprove={vi.fn()} onReject={vi.fn()} />,
    );
    expect(screen.getByText("No tool calls yet")).toBeInTheDocument();
  });

  test("renders pending tool calls with Approve/Reject icon buttons", () => {
    const toolCalls = [makeToolCall({ status: "pending", tool_name: "Edit" })];
    render(
      <ToolCallQueue
        toolCalls={toolCalls}
        onApprove={vi.fn()}
        onReject={vi.fn()}
      />,
    );
    expect(screen.getByText("Pending (1)")).toBeInTheDocument();
    expect(screen.getByText("Edit")).toBeInTheDocument();
    expect(screen.getByTitle("Approve")).toBeInTheDocument();
    expect(screen.getByTitle("Reject")).toBeInTheDocument();
  });

  test("calls onApprove with tool call id", async () => {
    const onApprove = vi.fn();
    const toolCalls = [makeToolCall({ id: "tc-42", status: "pending" })];
    render(
      <ToolCallQueue
        toolCalls={toolCalls}
        onApprove={onApprove}
        onReject={vi.fn()}
      />,
    );
    await userEvent.click(screen.getByTitle("Approve"));
    expect(onApprove).toHaveBeenCalledWith("tc-42");
  });

  test("calls onReject with tool call id", async () => {
    const onReject = vi.fn();
    const toolCalls = [makeToolCall({ id: "tc-42", status: "pending" })];
    render(
      <ToolCallQueue
        toolCalls={toolCalls}
        onApprove={vi.fn()}
        onReject={onReject}
      />,
    );
    await userEvent.click(screen.getByTitle("Reject"));
    expect(onReject).toHaveBeenCalledWith("tc-42");
  });

  test("renders running tool calls", () => {
    const toolCalls = [makeToolCall({ status: "running", tool_name: "Bash" })];
    render(
      <ToolCallQueue
        toolCalls={toolCalls}
        onApprove={vi.fn()}
        onReject={vi.fn()}
      />,
    );
    expect(screen.getByText("Running (1)")).toBeInTheDocument();
    expect(screen.getByText("Bash")).toBeInTheDocument();
  });

  test("renders completed tool calls in history", () => {
    const toolCalls = [
      makeToolCall({
        status: "completed",
        tool_name: "Grep",
        duration_ms: 1500,
      }),
    ];
    render(
      <ToolCallQueue
        toolCalls={toolCalls}
        onApprove={vi.fn()}
        onReject={vi.fn()}
      />,
    );
    expect(screen.getByText("History (1)")).toBeInTheDocument();
    expect(screen.getByText("Grep")).toBeInTheDocument();
    expect(screen.getByText("1.5s")).toBeInTheDocument();
  });

  test("truncates long arguments", () => {
    const longArgs = JSON.stringify({ path: "a".repeat(200) });
    const toolCalls = [
      makeToolCall({ status: "pending", arguments_json: longArgs }),
    ];
    render(
      <ToolCallQueue
        toolCalls={toolCalls}
        onApprove={vi.fn()}
        onReject={vi.fn()}
      />,
    );
    // Should contain "..." for truncation
    const argsEl = screen.getByText(/\.\.\./);
    expect(argsEl).toBeInTheDocument();
  });
});
