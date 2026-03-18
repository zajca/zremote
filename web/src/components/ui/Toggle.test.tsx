import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { Toggle } from "./Toggle";

describe("Toggle", () => {
  test("renders in unchecked state", () => {
    render(<Toggle checked={false} onChange={() => {}} aria-label="Test" />);
    const toggle = screen.getByRole("switch");
    expect(toggle).toHaveAttribute("aria-checked", "false");
    expect(toggle.className).toContain("bg-bg-tertiary");
  });

  test("renders in checked state", () => {
    const { container } = render(
      <Toggle checked={true} onChange={() => {}} aria-label="Test" />,
    );
    const toggle = screen.getByRole("switch");
    expect(toggle).toHaveAttribute("aria-checked", "true");
    expect(toggle.className).toContain("bg-accent");
    const knob = container.querySelector("span");
    expect(knob?.className).toContain("translate-x-3.5");
  });

  test("calls onChange with toggled value on click", async () => {
    const onChange = vi.fn();
    render(<Toggle checked={false} onChange={onChange} aria-label="Test" />);
    await userEvent.click(screen.getByRole("switch"));
    expect(onChange).toHaveBeenCalledWith(true);
  });

  test("calls onChange with false when checked and clicked", async () => {
    const onChange = vi.fn();
    render(<Toggle checked={true} onChange={onChange} aria-label="Test" />);
    await userEvent.click(screen.getByRole("switch"));
    expect(onChange).toHaveBeenCalledWith(false);
  });

  test("respects disabled prop", async () => {
    const onChange = vi.fn();
    render(
      <Toggle checked={false} onChange={onChange} disabled aria-label="Test" />,
    );
    const toggle = screen.getByRole("switch");
    expect(toggle).toBeDisabled();
    expect(toggle.className).toContain("disabled:opacity-40");
  });

  test("has focus-visible ring classes", () => {
    render(<Toggle checked={false} onChange={() => {}} aria-label="Test" />);
    const toggle = screen.getByRole("switch");
    expect(toggle.className).toContain("focus-visible:ring-2");
    expect(toggle.className).toContain("focus-visible:ring-accent/50");
  });

  test("has proper aria-label", () => {
    render(
      <Toggle checked={false} onChange={() => {}} aria-label="Enable feature" />,
    );
    expect(screen.getByLabelText("Enable feature")).toBeInTheDocument();
  });
});
