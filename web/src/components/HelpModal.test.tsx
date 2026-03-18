import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { HelpModal } from "./HelpModal";

describe("HelpModal", () => {
  test("renders nothing when not open", () => {
    const { container } = render(
      <HelpModal open={false} onClose={vi.fn()} />,
    );
    expect(container.children.length).toBe(0);
  });

  test("renders help heading when open", () => {
    render(<HelpModal open={true} onClose={vi.fn()} />);
    expect(
      screen.getByRole("heading", { name: "Help" }),
    ).toBeInTheDocument();
  });

  test("shows keyboard shortcuts section", () => {
    render(<HelpModal open={true} onClose={vi.fn()} />);
    expect(screen.getByText("Keyboard Shortcuts")).toBeInTheDocument();
    expect(screen.getByText("Global Shortcuts")).toBeInTheDocument();
    expect(screen.getByText("Command Palette Navigation")).toBeInTheDocument();
  });

  test("shows command palette guide section", () => {
    render(<HelpModal open={true} onClose={vi.fn()} />);
    expect(screen.getByText("Command Palette Guide")).toBeInTheDocument();
    expect(screen.getByText("Context Levels")).toBeInTheDocument();
  });

  test("shows all six context cards", () => {
    render(<HelpModal open={true} onClose={vi.fn()} />);
    expect(screen.getByText("Global")).toBeInTheDocument();
    expect(screen.getByText("Host")).toBeInTheDocument();
    expect(screen.getByText("Project")).toBeInTheDocument();
    expect(screen.getByText("Worktree")).toBeInTheDocument();
    expect(screen.getByText("Session")).toBeInTheDocument();
    expect(screen.getByText("Loop")).toBeInTheDocument();
  });

  test("shows local mode note", () => {
    render(<HelpModal open={true} onClose={vi.fn()} />);
    expect(
      screen.getByText(/local mode/i),
    ).toBeInTheDocument();
  });

  test("calls onClose when X button clicked", async () => {
    const onClose = vi.fn();
    render(<HelpModal open={true} onClose={onClose} />);

    await userEvent.click(screen.getByLabelText("Close help"));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test("calls onClose when backdrop clicked", async () => {
    const onClose = vi.fn();
    const { container } = render(
      <HelpModal open={true} onClose={onClose} />,
    );

    // Click on the backdrop (outermost fixed overlay)
    const backdrop = container.firstChild as HTMLElement;
    await userEvent.click(backdrop);
    expect(onClose).toHaveBeenCalled();
  });

  test("does not close when modal content clicked", async () => {
    const onClose = vi.fn();
    render(<HelpModal open={true} onClose={onClose} />);

    // Click on the heading inside the modal content
    await userEvent.click(screen.getByText("Keyboard Shortcuts"));
    expect(onClose).not.toHaveBeenCalled();
  });

  test("displays shortcut descriptions", () => {
    render(<HelpModal open={true} onClose={vi.fn()} />);
    expect(screen.getByText("Toggle command palette")).toBeInTheDocument();
    expect(screen.getByText("Open settings")).toBeInTheDocument();
    expect(screen.getByText("New terminal session")).toBeInTheDocument();
    expect(screen.getByText("Toggle sidebar")).toBeInTheDocument();
    expect(screen.getByText("Show this help")).toBeInTheDocument();
    expect(screen.getByText("Navigate items")).toBeInTheDocument();
    expect(screen.getByText("Drill down into item")).toBeInTheDocument();
    expect(screen.getByText("Go back one level")).toBeInTheDocument();
    expect(screen.getByText("Close palette")).toBeInTheDocument();
  });

  test("shows drill-down navigation explanation", () => {
    render(<HelpModal open={true} onClose={vi.fn()} />);
    expect(
      screen.getAllByText(/drill-down navigation/i).length,
    ).toBeGreaterThanOrEqual(1);
  });
});
