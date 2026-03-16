import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { Badge } from "./Badge";

describe("Badge", () => {
  test("renders children text", () => {
    render(<Badge variant="online">Active</Badge>);
    expect(screen.getByText("Active")).toBeInTheDocument();
  });

  test("renders online variant", () => {
    render(<Badge variant="online">online</Badge>);
    expect(screen.getByText("online").className).toContain("text-status-online");
  });

  test("renders offline variant", () => {
    render(<Badge variant="offline">offline</Badge>);
    expect(screen.getByText("offline").className).toContain("text-status-offline");
  });

  test("renders error variant", () => {
    render(<Badge variant="error">error</Badge>);
    expect(screen.getByText("error").className).toContain("text-status-error");
  });

  test("renders warning variant", () => {
    render(<Badge variant="warning">warning</Badge>);
    expect(screen.getByText("warning").className).toContain("text-status-warning");
  });

  test("renders creating variant", () => {
    render(<Badge variant="creating">creating</Badge>);
    expect(screen.getByText("creating").className).toContain("text-accent");
  });
});
