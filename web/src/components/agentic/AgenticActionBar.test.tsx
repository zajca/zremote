import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { AgenticActionBar } from "./AgenticActionBar";

describe("AgenticActionBar", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

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

  test("disables all when error status", () => {
    render(<AgenticActionBar status="error" onAction={vi.fn()} />);
    const approveBtn = screen.getByText("Approve").closest("button");
    const rejectBtn = screen.getByText("Reject").closest("button");
    const stopBtn = screen.getByText("Stop").closest("button");
    expect(approveBtn).toBeDisabled();
    expect(rejectBtn).toBeDisabled();
    expect(stopBtn).toBeDisabled();
  });

  test("Input button is disabled when not waiting_for_input", () => {
    render(<AgenticActionBar status="working" onAction={vi.fn()} />);
    const inputBtn = screen.getByText("Input").closest("button");
    expect(inputBtn).toBeDisabled();
  });

  test("Input button is enabled when waiting_for_input", () => {
    render(<AgenticActionBar status="waiting_for_input" onAction={vi.fn()} />);
    const inputBtn = screen.getByText("Input").closest("button");
    expect(inputBtn).not.toBeDisabled();
  });

  test("clicking Input shows text input field", async () => {
    render(<AgenticActionBar status="waiting_for_input" onAction={vi.fn()} />);
    await userEvent.click(screen.getByText("Input").closest("button")!);
    expect(screen.getByPlaceholderText("Type your input...")).toBeInTheDocument();
    expect(screen.getByText("Send")).toBeInTheDocument();
  });

  test("submitting input calls onAction with provide_input", async () => {
    const onAction = vi.fn();
    render(<AgenticActionBar status="waiting_for_input" onAction={onAction} />);

    await userEvent.click(screen.getByText("Input").closest("button")!);
    const input = screen.getByPlaceholderText("Type your input...");
    await userEvent.type(input, "my input text");
    await userEvent.click(screen.getByText("Send"));

    expect(onAction).toHaveBeenCalledWith("provide_input", "my input text");
  });

  test("submitting empty input does not call onAction", async () => {
    const onAction = vi.fn();
    render(<AgenticActionBar status="waiting_for_input" onAction={onAction} />);

    await userEvent.click(screen.getByText("Input").closest("button")!);
    await userEvent.click(screen.getByText("Send"));

    expect(onAction).not.toHaveBeenCalled();
  });

  test("pressing Enter in input field submits", async () => {
    const onAction = vi.fn();
    render(<AgenticActionBar status="waiting_for_input" onAction={onAction} />);

    await userEvent.click(screen.getByText("Input").closest("button")!);
    const input = screen.getByPlaceholderText("Type your input...");
    await userEvent.type(input, "hello{Enter}");

    expect(onAction).toHaveBeenCalledWith("provide_input", "hello");
  });

  test("pressing Escape in input field closes input", async () => {
    render(<AgenticActionBar status="waiting_for_input" onAction={vi.fn()} />);

    await userEvent.click(screen.getByText("Input").closest("button")!);
    expect(screen.getByPlaceholderText("Type your input...")).toBeInTheDocument();

    const input = screen.getByPlaceholderText("Type your input...");
    await userEvent.type(input, "{Escape}");

    expect(screen.queryByPlaceholderText("Type your input...")).not.toBeInTheDocument();
  });

  test("calls onAction with pause when Pause clicked while working", async () => {
    const onAction = vi.fn();
    render(<AgenticActionBar status="working" onAction={onAction} />);
    await userEvent.click(screen.getByText("Pause").closest("button")!);
    expect(onAction).toHaveBeenCalledWith("pause", undefined);
  });

  test("calls onAction with resume when Resume clicked while paused", async () => {
    const onAction = vi.fn();
    render(<AgenticActionBar status="paused" onAction={onAction} />);
    await userEvent.click(screen.getByText("Resume").closest("button")!);
    expect(onAction).toHaveBeenCalledWith("resume", undefined);
  });

  test("Stop button requires confirm and calls onAction with stop", async () => {
    window.confirm = vi.fn().mockReturnValue(true);
    const onAction = vi.fn();
    render(<AgenticActionBar status="working" onAction={onAction} />);
    await userEvent.click(screen.getByText("Stop").closest("button")!);
    expect(window.confirm).toHaveBeenCalledWith("Stop this agentic loop?");
    expect(onAction).toHaveBeenCalledWith("stop", undefined);
  });

  test("Stop button does nothing when confirm is cancelled", async () => {
    window.confirm = vi.fn().mockReturnValue(false);
    const onAction = vi.fn();
    render(<AgenticActionBar status="working" onAction={onAction} />);
    await userEvent.click(screen.getByText("Stop").closest("button")!);
    expect(window.confirm).toHaveBeenCalled();
    expect(onAction).not.toHaveBeenCalled();
  });

  test("Pause/Resume is disabled when completed", () => {
    render(<AgenticActionBar status="completed" onAction={vi.fn()} />);
    const pauseBtn = screen.getByText("Pause").closest("button");
    expect(pauseBtn).toBeDisabled();
  });

  test("Stop is enabled when waiting_for_input", () => {
    render(<AgenticActionBar status="waiting_for_input" onAction={vi.fn()} />);
    const stopBtn = screen.getByText("Stop").closest("button");
    expect(stopBtn).not.toBeDisabled();
  });

  test("Stop is enabled when paused", () => {
    render(<AgenticActionBar status="paused" onAction={vi.fn()} />);
    const stopBtn = screen.getByText("Stop").closest("button");
    expect(stopBtn).not.toBeDisabled();
  });

  test("shows Approving... optimistically after clicking Approve", async () => {
    const onAction = vi.fn();
    render(<AgenticActionBar status="waiting_for_input" onAction={onAction} />);
    await userEvent.click(screen.getByText("Approve").closest("button")!);
    expect(screen.getByText("Approving...")).toBeInTheDocument();
    // Clears after timeout
    await waitFor(() => {
      expect(screen.getByText("Approve")).toBeInTheDocument();
    }, { timeout: 1000 });
  });

  test("shows Rejecting... optimistically after clicking Reject", async () => {
    const onAction = vi.fn();
    render(<AgenticActionBar status="waiting_for_input" onAction={onAction} />);
    await userEvent.click(screen.getByText("Reject").closest("button")!);
    expect(screen.getByText("Rejecting...")).toBeInTheDocument();
  });
});
