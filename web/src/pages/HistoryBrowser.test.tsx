import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { HistoryBrowser } from "./HistoryBrowser";

beforeEach(() => {
  vi.restoreAllMocks();
  global.fetch = vi.fn().mockResolvedValue({
    ok: true,
    json: async () => ({ results: [], total: 0, page: 1, per_page: 20 }),
  });
});

describe("HistoryBrowser", () => {
  test("renders History heading", () => {
    render(<HistoryBrowser />);
    expect(screen.getByText("History")).toBeInTheDocument();
  });

  test("renders search input", () => {
    render(<HistoryBrowser />);
    expect(
      screen.getByPlaceholderText("Search transcripts..."),
    ).toBeInTheDocument();
  });

  test("renders filter inputs", () => {
    render(<HistoryBrowser />);
    expect(screen.getByPlaceholderText("Host")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Project")).toBeInTheDocument();
  });

  test("shows empty state when no results", async () => {
    render(<HistoryBrowser />);
    await waitFor(() => {
      expect(screen.getByText("No results found")).toBeInTheDocument();
    });
  });

  test("shows detail panel placeholder", async () => {
    render(<HistoryBrowser />);
    await waitFor(() => {
      expect(
        screen.getByText("Select a result to view details"),
      ).toBeInTheDocument();
    });
  });
});
