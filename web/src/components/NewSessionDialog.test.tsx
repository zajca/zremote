import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { NewSessionDialog } from "./NewSessionDialog";

describe("NewSessionDialog", () => {
  test("renders nothing when not open", () => {
    const { container } = render(
      <NewSessionDialog
        open={false}
        onClose={vi.fn()}
        onSubmit={vi.fn()}
      />,
    );
    expect(container.children.length).toBe(0);
  });

  test("renders dialog when open", () => {
    render(
      <NewSessionDialog open={true} onClose={vi.fn()} onSubmit={vi.fn()} />,
    );
    expect(screen.getByText("New Session")).toBeInTheDocument();
  });

  test("renders Name input", () => {
    render(
      <NewSessionDialog open={true} onClose={vi.fn()} onSubmit={vi.fn()} />,
    );
    expect(screen.getByPlaceholderText("e.g. deploy-prod")).toBeInTheDocument();
  });

  test("renders Shell selector", () => {
    render(
      <NewSessionDialog open={true} onClose={vi.fn()} onSubmit={vi.fn()} />,
    );
    expect(screen.getByText("Shell")).toBeInTheDocument();
  });

  test("renders Working Directory input", () => {
    render(
      <NewSessionDialog open={true} onClose={vi.fn()} onSubmit={vi.fn()} />,
    );
    expect(screen.getByPlaceholderText("/home/user/project")).toBeInTheDocument();
  });

  test("renders Cancel and Create buttons", () => {
    render(
      <NewSessionDialog open={true} onClose={vi.fn()} onSubmit={vi.fn()} />,
    );
    expect(screen.getByText("Cancel")).toBeInTheDocument();
    expect(screen.getByText("Create")).toBeInTheDocument();
  });

  test("calls onClose when Cancel clicked", async () => {
    const onClose = vi.fn();
    render(
      <NewSessionDialog open={true} onClose={onClose} onSubmit={vi.fn()} />,
    );
    await userEvent.click(screen.getByText("Cancel"));
    expect(onClose).toHaveBeenCalled();
  });

  test("calls onSubmit when Create clicked", async () => {
    const onSubmit = vi.fn();
    render(
      <NewSessionDialog open={true} onClose={vi.fn()} onSubmit={onSubmit} />,
    );
    await userEvent.click(screen.getByText("Create"));
    expect(onSubmit).toHaveBeenCalledWith({
      name: undefined,
      shell: undefined,
      workingDir: undefined,
    });
  });

  test("sets defaultWorkingDir", () => {
    render(
      <NewSessionDialog
        open={true}
        onClose={vi.fn()}
        onSubmit={vi.fn()}
        defaultWorkingDir="/home/user/my-project"
      />,
    );
    expect(screen.getByPlaceholderText("/home/user/project")).toHaveValue(
      "/home/user/my-project",
    );
  });
});
