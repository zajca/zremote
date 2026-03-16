import { render, screen, act } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { CommandPalette } from "./CommandPalette";

// cmdk uses ResizeObserver and scrollIntoView
vi.stubGlobal(
  "ResizeObserver",
  class {
    observe() {}
    unobserve() {}
    disconnect() {}
  },
);

// jsdom doesn't implement scrollIntoView
Element.prototype.scrollIntoView = vi.fn();

// Mock hooks that fetch data
vi.mock("../../hooks/useHosts", () => ({
  useHosts: () => ({ hosts: [], loading: false, error: null }),
}));

vi.mock("../../hooks/useMode", () => ({
  useMode: () => ({ mode: "server", isLocal: false }),
}));

vi.mock("../../hooks/useProjects", () => ({
  useProjects: () => ({ projects: [], loading: false }),
}));

describe("CommandPalette", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  test("renders nothing when closed", () => {
    const { container } = render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );
    expect(container.children.length).toBe(0);
  });

  test("opens on Ctrl+K", () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    act(() => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "k", ctrlKey: true }),
      );
    });

    expect(screen.getByPlaceholderText("Search commands...")).toBeInTheDocument();
  });

  test("shows navigation commands when open", () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    act(() => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "k", ctrlKey: true }),
      );
    });

    expect(screen.getByText("Open Analytics")).toBeInTheDocument();
    expect(screen.getByText("Open History")).toBeInTheDocument();
    expect(screen.getByText("Open Settings")).toBeInTheDocument();
  });
});
