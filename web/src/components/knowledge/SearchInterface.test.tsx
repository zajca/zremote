import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { SearchInterface } from "./SearchInterface";

vi.mock("../../stores/knowledge-store", () => ({
  useKnowledgeStore: () => ({
    search: vi.fn(),
    searchResults: [],
    searchLoading: false,
  }),
}));

describe("SearchInterface", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  test("renders search input", () => {
    render(<SearchInterface projectId="proj-1" />);
    expect(
      screen.getByPlaceholderText("Search project knowledge..."),
    ).toBeInTheDocument();
  });

  test("renders tier selector", () => {
    render(<SearchInterface projectId="proj-1" />);
    expect(screen.getByText("L0 - Exact")).toBeInTheDocument();
    expect(screen.getByText("L1 - Semantic")).toBeInTheDocument();
    expect(screen.getByText("L2 - Exploratory")).toBeInTheDocument();
  });

  test("renders search button", () => {
    render(<SearchInterface projectId="proj-1" />);
    expect(screen.getByText("Search")).toBeInTheDocument();
  });

  test("disables search button when query is empty", () => {
    render(<SearchInterface projectId="proj-1" />);
    const btn = screen.getByText("Search").closest("button");
    expect(btn).toBeDisabled();
  });
});
