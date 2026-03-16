import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";
import { AgenticLoopPage } from "./AgenticLoopPage";

// Mock AgenticLoopPanel since it's tested separately
vi.mock("../components/agentic/AgenticLoopPanel", () => ({
  AgenticLoopPanel: ({ loopId }: { loopId: string }) => (
    <div data-testid="agentic-panel">Panel: {loopId}</div>
  ),
}));

describe("AgenticLoopPage", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  test("renders with loop ID", () => {
    render(
      <MemoryRouter
        initialEntries={["/hosts/host-1/sessions/sess-1/loops/loop-1"]}
      >
        <Routes>
          <Route
            path="/hosts/:hostId/sessions/:sessionId/loops/:loopId"
            element={<AgenticLoopPage />}
          />
        </Routes>
      </MemoryRouter>,
    );
    expect(screen.getByTestId("agentic-panel")).toBeInTheDocument();
    expect(screen.getByText("Panel: loop-1")).toBeInTheDocument();
  });

  test("renders back to session link", () => {
    render(
      <MemoryRouter
        initialEntries={["/hosts/host-1/sessions/sess-1/loops/loop-1"]}
      >
        <Routes>
          <Route
            path="/hosts/:hostId/sessions/:sessionId/loops/:loopId"
            element={<AgenticLoopPage />}
          />
        </Routes>
      </MemoryRouter>,
    );
    expect(screen.getByText("Back to session")).toBeInTheDocument();
  });

  test("shows Loop not found when no loopId", () => {
    render(
      <MemoryRouter initialEntries={["/hosts/host-1/sessions/sess-1/loops/"]}>
        <Routes>
          <Route
            path="/hosts/:hostId/sessions/:sessionId/loops/"
            element={<AgenticLoopPage />}
          />
        </Routes>
      </MemoryRouter>,
    );
    expect(screen.getByText("Loop not found")).toBeInTheDocument();
  });
});
