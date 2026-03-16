import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { SearchResults } from "./SearchResults";
import type { SearchResult } from "../../types/knowledge";

describe("SearchResults", () => {
  test("renders nothing when results are empty", () => {
    const { container } = render(<SearchResults results={[]} />);
    expect(container.children.length).toBe(0);
  });

  test("renders result count", () => {
    const results: SearchResult[] = [
      {
        path: "/src/main.rs",
        score: 0.95,
        snippet: "fn main() { }",
        line_start: 1,
        line_end: 3,
        tier: "l0",
      },
    ];
    render(<SearchResults results={results} />);
    expect(screen.getByText("1 results")).toBeInTheDocument();
  });

  test("renders result path", () => {
    const results: SearchResult[] = [
      {
        path: "/src/lib.rs",
        score: 0.8,
        snippet: "pub mod state;",
        line_start: 5,
        line_end: 5,
        tier: "l1",
      },
    ];
    render(<SearchResults results={results} />);
    expect(screen.getByText("/src/lib.rs")).toBeInTheDocument();
  });

  test("renders score percentage", () => {
    const results: SearchResult[] = [
      {
        path: "/src/test.rs",
        score: 0.75,
        snippet: "test code",
        line_start: null,
        line_end: null,
        tier: "l2",
      },
    ];
    render(<SearchResults results={results} />);
    expect(screen.getByText("75%")).toBeInTheDocument();
  });

  test("renders tier badge", () => {
    const results: SearchResult[] = [
      {
        path: "/src/test.rs",
        score: 0.5,
        snippet: "code",
        line_start: null,
        line_end: null,
        tier: "l0",
      },
    ];
    render(<SearchResults results={results} />);
    expect(screen.getByText("L0")).toBeInTheDocument();
  });

  test("renders line range", () => {
    const results: SearchResult[] = [
      {
        path: "/src/test.rs",
        score: 0.5,
        snippet: "code",
        line_start: 10,
        line_end: 20,
        tier: "l1",
      },
    ];
    render(<SearchResults results={results} />);
    expect(screen.getByText("L10-20")).toBeInTheDocument();
  });

  test("renders snippet", () => {
    const results: SearchResult[] = [
      {
        path: "/src/test.rs",
        score: 0.5,
        snippet: "fn hello_world() {}",
        line_start: null,
        line_end: null,
        tier: "l1",
      },
    ];
    render(<SearchResults results={results} />);
    expect(screen.getByText("fn hello_world() {}")).toBeInTheDocument();
  });
});
