import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { ProjectLoopsTab } from "./ProjectLoopsTab";

beforeEach(() => {
  vi.restoreAllMocks();
  global.fetch = vi.fn().mockImplementation((url: string) => {
    if (url.includes("/api/loops")) {
      return Promise.resolve({
        ok: true,
        json: async () => [],
      });
    }
    if (url.includes("/api/claude-sessions")) {
      return Promise.resolve({
        ok: true,
        json: async () => [],
      });
    }
    return Promise.resolve({ ok: true, json: async () => [] });
  });
});

describe("ProjectLoopsTab", () => {
  test("shows loading state initially", () => {
    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("Loading...")).toBeInTheDocument();
  });

  test("shows empty state when no loops or tasks", async () => {
    render(
      <MemoryRouter>
        <ProjectLoopsTab projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(
        screen.getByText("No Claude tasks or agentic loops for this project yet."),
      ).toBeInTheDocument();
    });
  });
});
