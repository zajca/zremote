import { render } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { StatusDot } from "./StatusDot";

describe("StatusDot", () => {
  test("renders online status", () => {
    const { container } = render(<StatusDot status="online" />);
    const dot = container.querySelector(".bg-status-online");
    expect(dot).toBeInTheDocument();
  });

  test("renders offline status", () => {
    const { container } = render(<StatusDot status="offline" />);
    const dot = container.querySelector(".bg-status-offline");
    expect(dot).toBeInTheDocument();
  });

  test("renders error status", () => {
    const { container } = render(<StatusDot status="error" />);
    const dot = container.querySelector(".bg-status-error");
    expect(dot).toBeInTheDocument();
  });

  test("does not show ping animation by default", () => {
    const { container } = render(<StatusDot status="online" />);
    const pings = container.querySelectorAll(".animate-ping");
    expect(pings.length).toBe(0);
  });

  test("shows ping animation when pulse is true", () => {
    const { container } = render(<StatusDot status="online" pulse />);
    const ping = container.querySelector(".animate-ping");
    expect(ping).toBeInTheDocument();
  });
});
