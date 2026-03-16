import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { AgenticActionBar } from "./AgenticActionBar";

describe("AgenticActionBar", () => {
  test("renders action buttons", () => {
    render(<AgenticActionBar status="waiting_for_input" onAction={vi.fn()} />);
    expect(screen.getByText("Approve")).toBeInTheDocument();
    expect(screen.getByText("Reject")).toBeInTheDocument();
    expect(screen.getByText("Input")).toBeInTheDocument();
    expect(screen.getByText("Pause")).toBeInTheDocument();
    expect(screen.getByText("Stop")).toBeInTheDocument();
  });

  test("enables Approve and Reject when waiting_for_input", () => {
    render(<AgenticActionBar status="waiting_for_input" onAction={vi.fn()} />);
    const approveBtn = screen.getByText("Approve").closest("button");
    const rejectBtn = screen.getByText("Reject").closest("button");
    expect(approveBtn).not.toBeDisabled();
    expect(rejectBtn).not.toBeDisabled();
  });

  test("disables Approve and Reject when working", () => {
    render(<AgenticActionBar status="working" onAction={vi.fn()} />);
    const approveBtn = screen.getByText("Approve").closest("button");
    const rejectBtn = screen.getByText("Reject").closest("button");
    expect(approveBtn).toBeDisabled();
    expect(rejectBtn).toBeDisabled();
  });

  test("calls onAction with approve when Approve clicked", async () => {
    const onAction = vi.fn();
    render(<AgenticActionBar status="waiting_for_input" onAction={onAction} />);
    await userEvent.click(screen.getByText("Approve").closest("button")!);
    expect(onAction).toHaveBeenCalledWith("approve", undefined);
  });

  test("calls onAction with reject when Reject clicked", async () => {
    const onAction = vi.fn();
    render(<AgenticActionBar status="waiting_for_input" onAction={onAction} />);
    await userEvent.click(screen.getByText("Reject").closest("button")!);
    expect(onAction).toHaveBeenCalledWith("reject", undefined);
  });

  test("enables Pause when working", () => {
    render(<AgenticActionBar status="working" onAction={vi.fn()} />);
    const pauseBtn = screen.getByText("Pause").closest("button");
    expect(pauseBtn).not.toBeDisabled();
  });

  test("shows Resume when paused", () => {
    render(<AgenticActionBar status="paused" onAction={vi.fn()} />);
    expect(screen.getByText("Resume")).toBeInTheDocument();
  });

  test("disables all action buttons when completed", () => {
    render(<AgenticActionBar status="completed" onAction={vi.fn()} />);
    const approveBtn = screen.getByText("Approve").closest("button");
    const rejectBtn = screen.getByText("Reject").closest("button");
    const stopBtn = screen.getByText("Stop").closest("button");
    expect(approveBtn).toBeDisabled();
    expect(rejectBtn).toBeDisabled();
    expect(stopBtn).toBeDisabled();
  });
});
