import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { HistoryBrowser } from "./HistoryBrowser";

const emptyResponse = { results: [], total: 0, page: 1, per_page: 20 };

const mockResult = {
  transcript_id: 1,
  loop_id: "loop-1",
  role: "assistant",
  content: "Here is the fix for the authentication bug",
  timestamp: "2026-03-15T10:00:00Z",
  tool_name: "claude_code",
  project_path: "/home/user/project",
  loop_status: "completed",
  model: "sonnet",
  estimated_cost_usd: 0.0523,
};

const mockResult2 = {
  transcript_id: 2,
  loop_id: "loop-2",
  role: "user",
  content: "Please fix the tests",
  timestamp: "2026-03-15T11:00:00Z",
  tool_name: "bash",
  project_path: null,
  loop_status: "working",
  model: null,
  estimated_cost_usd: null,
};

beforeEach(() => {
  vi.restoreAllMocks();
  global.fetch = vi.fn().mockResolvedValue({
    ok: true,
    json: async () => emptyResponse,
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

  test("renders search results when data is returned", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        results: [mockResult],
        total: 1,
        page: 1,
        per_page: 20,
      }),
    });

    render(<HistoryBrowser />);

    await waitFor(() => {
      expect(screen.getByText(/authentication bug/)).toBeInTheDocument();
      expect(screen.getByText("assistant")).toBeInTheDocument();
      expect(screen.getByText("completed")).toBeInTheDocument();
      expect(screen.getByText("sonnet")).toBeInTheDocument();
      expect(screen.getByText("claude_code")).toBeInTheDocument();
      expect(screen.getByText("/home/user/project")).toBeInTheDocument();
    });
  });

  test("renders cost information for results that have it", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        results: [mockResult],
        total: 1,
        page: 1,
        per_page: 20,
      }),
    });

    render(<HistoryBrowser />);

    await waitFor(() => {
      expect(screen.getByText("$0.0523")).toBeInTheDocument();
    });
  });

  test("shows Clear button when search query is present", async () => {
    render(<HistoryBrowser />);

    const searchInput = screen.getByPlaceholderText("Search transcripts...");
    await userEvent.type(searchInput, "test query");

    await waitFor(() => {
      expect(screen.getByText("Clear")).toBeInTheDocument();
    });
  });

  test("clears all filters when Clear button clicked", async () => {
    render(<HistoryBrowser />);

    const searchInput = screen.getByPlaceholderText("Search transcripts...");
    await userEvent.type(searchInput, "test query");

    await waitFor(() => {
      expect(screen.getByText("Clear")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("Clear"));

    expect(searchInput).toHaveValue("");
  });

  test("shows Reset filters button in empty state when filters active", async () => {
    render(<HistoryBrowser />);

    const hostInput = screen.getByPlaceholderText("Host");
    await userEvent.type(hostInput, "my-host");

    await waitFor(() => {
      expect(screen.getByText("No results found")).toBeInTheDocument();
      expect(screen.getByText("Reset filters")).toBeInTheDocument();
    });
  });

  test("renders pagination when total exceeds per_page", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        results: [mockResult],
        total: 40,
        page: 1,
        per_page: 20,
      }),
    });

    render(<HistoryBrowser />);

    await waitFor(() => {
      expect(screen.getByText("40 results")).toBeInTheDocument();
      expect(screen.getByText("1 / 2")).toBeInTheDocument();
      expect(screen.getByText("Prev")).toBeInTheDocument();
      expect(screen.getByText("Next")).toBeInTheDocument();
    });
  });

  test("disables Prev button on first page", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        results: [mockResult],
        total: 40,
        page: 1,
        per_page: 20,
      }),
    });

    render(<HistoryBrowser />);

    await waitFor(() => {
      const prevBtn = screen.getByText("Prev").closest("button");
      expect(prevBtn).toBeDisabled();
    });
  });

  test("clicking Next fetches next page", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        results: [mockResult],
        total: 40,
        page: 1,
        per_page: 20,
      }),
    });

    render(<HistoryBrowser />);

    await waitFor(() => {
      expect(screen.getByText("Next")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("Next"));

    // Fetch should have been called with page=2
    await waitFor(() => {
      const fetchCalls = (global.fetch as ReturnType<typeof vi.fn>).mock.calls;
      const lastCall = fetchCalls[fetchCalls.length - 1][0] as string;
      expect(lastCall).toContain("page=2");
    });
  });

  test("clicking a result selects it and shows detail panel", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        results: [mockResult],
        total: 1,
        page: 1,
        per_page: 20,
      }),
    });

    render(<HistoryBrowser />);

    await waitFor(() => {
      expect(screen.getByText(/authentication bug/)).toBeInTheDocument();
    });

    // Click the result entry (it's a button)
    const resultButton = screen.getByText(/authentication bug/).closest("button");
    await userEvent.click(resultButton!);

    // Detail panel should now show structured info
    await waitFor(() => {
      expect(screen.getByText(/Role: assistant/)).toBeInTheDocument();
      expect(screen.getByText(/Tool: claude_code/)).toBeInTheDocument();
    });
  });

  test("shows result without model when model is null", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        results: [mockResult2],
        total: 1,
        page: 1,
        per_page: 20,
      }),
    });

    render(<HistoryBrowser />);

    await waitFor(() => {
      expect(screen.getByText("user")).toBeInTheDocument();
      expect(screen.getByText("working")).toBeInTheDocument();
      expect(screen.getByText("bash")).toBeInTheDocument();
    });
  });

  test("shows searching state while loading", async () => {
    // Make fetch hang so we see the loading state
    global.fetch = vi.fn().mockImplementation(
      () => new Promise(() => {}), // never resolves
    );

    render(<HistoryBrowser />);

    // Type to trigger a search
    const searchInput = screen.getByPlaceholderText("Search transcripts...");
    await userEvent.type(searchInput, "x");

    // First it needs to wait for debounce, then show Searching
    await waitFor(
      () => {
        expect(screen.getByText("Searching...")).toBeInTheDocument();
      },
      { timeout: 2000 },
    );
  });

  test("debounced search triggers fetch with query param", async () => {
    render(<HistoryBrowser />);

    const searchInput = screen.getByPlaceholderText("Search transcripts...");
    await userEvent.type(searchInput, "auth bug");

    await waitFor(() => {
      const fetchCalls = (global.fetch as ReturnType<typeof vi.fn>).mock.calls;
      const urlsWithQ = fetchCalls.filter((c) =>
        (c[0] as string).includes("q=auth"),
      );
      expect(urlsWithQ.length).toBeGreaterThan(0);
    });
  });

  test("typing in Host filter triggers fetch with host param", async () => {
    render(<HistoryBrowser />);

    const hostInput = screen.getByPlaceholderText("Host");
    await userEvent.type(hostInput, "server-1");

    await waitFor(() => {
      const fetchCalls = (global.fetch as ReturnType<typeof vi.fn>).mock.calls;
      const urlsWithHost = fetchCalls.filter((c) =>
        (c[0] as string).includes("host=server-1"),
      );
      expect(urlsWithHost.length).toBeGreaterThan(0);
    });
  });

  test("does not show pagination when total is less than per_page", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        results: [mockResult],
        total: 5,
        page: 1,
        per_page: 20,
      }),
    });

    render(<HistoryBrowser />);

    await waitFor(() => {
      expect(screen.getByText(/authentication bug/)).toBeInTheDocument();
    });

    expect(screen.queryByText("Prev")).not.toBeInTheDocument();
    expect(screen.queryByText("Next")).not.toBeInTheDocument();
  });

  test("shows status badge variants correctly", async () => {
    const errorResult = { ...mockResult, transcript_id: 3, loop_status: "error" };
    const workingResult = { ...mockResult, transcript_id: 4, loop_status: "working" };

    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        results: [errorResult, workingResult],
        total: 2,
        page: 1,
        per_page: 20,
      }),
    });

    render(<HistoryBrowser />);

    await waitFor(() => {
      expect(screen.getByText("error")).toBeInTheDocument();
      expect(screen.getByText("working")).toBeInTheDocument();
    });
  });
});
