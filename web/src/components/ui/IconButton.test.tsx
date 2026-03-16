import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { Settings } from "lucide-react";
import { IconButton } from "./IconButton";

describe("IconButton", () => {
  test("renders a button", () => {
    render(<IconButton icon={Settings} />);
    expect(screen.getByRole("button")).toBeInTheDocument();
  });

  test("renders with tooltip as title", () => {
    render(<IconButton icon={Settings} tooltip="Open settings" />);
    expect(screen.getByRole("button")).toHaveAttribute("title", "Open settings");
  });

  test("handles click events", async () => {
    const onClick = vi.fn();
    render(<IconButton icon={Settings} onClick={onClick} />);
    await userEvent.click(screen.getByRole("button"));
    expect(onClick).toHaveBeenCalledOnce();
  });

  test("is disabled when disabled prop is set", () => {
    render(<IconButton icon={Settings} disabled />);
    expect(screen.getByRole("button")).toBeDisabled();
  });

  test("applies custom className", () => {
    render(<IconButton icon={Settings} className="extra" />);
    expect(screen.getByRole("button").className).toContain("extra");
  });
});
