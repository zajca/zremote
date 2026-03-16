import { render, screen, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { ToastContainer, showToast } from "./Toast";

describe("ToastContainer", () => {
  test("renders nothing when no toasts", () => {
    const { container } = render(<ToastContainer />);
    expect(container.children.length).toBe(0);
  });

  test("shows toast when showToast is called", () => {
    render(<ToastContainer />);
    act(() => {
      showToast("Something went wrong", "error");
    });
    expect(screen.getByText("Something went wrong")).toBeInTheDocument();
  });

  test("shows success toast", () => {
    render(<ToastContainer />);
    act(() => {
      showToast("Operation successful", "success");
    });
    expect(screen.getByText("Operation successful")).toBeInTheDocument();
  });

  test("shows info toast", () => {
    render(<ToastContainer />);
    act(() => {
      showToast("FYI", "info");
    });
    expect(screen.getByText("FYI")).toBeInTheDocument();
  });

  test("can dismiss toast by clicking close button", async () => {
    render(<ToastContainer />);
    act(() => {
      showToast("Dismissable", "error");
    });
    expect(screen.getByText("Dismissable")).toBeInTheDocument();

    const closeButton = screen.getByRole("button");
    await userEvent.click(closeButton);
    expect(screen.queryByText("Dismissable")).not.toBeInTheDocument();
  });

  test("auto-dismisses success toasts after delay", () => {
    vi.useFakeTimers();
    render(<ToastContainer />);
    act(() => {
      showToast("Quick toast", "success");
    });
    expect(screen.getByText("Quick toast")).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(4000);
    });
    expect(screen.queryByText("Quick toast")).not.toBeInTheDocument();
    vi.useRealTimers();
  });

  test("shows multiple toasts", () => {
    render(<ToastContainer />);
    act(() => {
      showToast("First", "error");
      showToast("Second", "info");
    });
    expect(screen.getByText("First")).toBeInTheDocument();
    expect(screen.getByText("Second")).toBeInTheDocument();
  });
});
