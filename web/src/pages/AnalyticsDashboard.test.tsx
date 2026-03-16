import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { AnalyticsDashboard } from "./AnalyticsDashboard";

// Mock recharts to avoid SVG rendering issues in jsdom
vi.mock("recharts", () => ({
  ResponsiveContainer: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="responsive-container">{children}</div>
  ),
  AreaChart: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="area-chart">{children}</div>
  ),
  BarChart: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="bar-chart">{children}</div>
  ),
  Area: () => null,
  Bar: () => null,
  CartesianGrid: () => null,
  XAxis: () => null,
  YAxis: () => null,
  Tooltip: () => null,
}));

beforeEach(() => {
  vi.restoreAllMocks();
  global.fetch = vi.fn().mockImplementation((url: string) => {
    if (url.includes("/api/analytics/loops")) {
      return Promise.resolve({
        ok: true,
        json: async () => ({
          total_loops: 42,
          completed: 38,
          errored: 4,
          avg_cost_usd: 0.15,
          total_cost_usd: 6.3,
          total_tokens_in: 500000,
          total_tokens_out: 200000,
        }),
      });
    }
    if (url.includes("/api/analytics/sessions")) {
      return Promise.resolve({
        ok: true,
        json: async () => ({
          total_sessions: 10,
          active_sessions: 3,
          avg_duration_seconds: 3600,
        }),
      });
    }
    if (url.includes("/api/analytics/cost")) {
      return Promise.resolve({
        ok: true,
        json: async () => [
          { period: "2026-03-10", cost: 1.5 },
          { period: "2026-03-11", cost: 2.0 },
        ],
      });
    }
    if (url.includes("/api/analytics/tokens")) {
      return Promise.resolve({
        ok: true,
        json: async () => [
          { label: "sonnet", tokens_in: 300000, tokens_out: 100000 },
        ],
      });
    }
    return Promise.resolve({ ok: true, json: async () => ({}) });
  });
});

describe("AnalyticsDashboard", () => {
  test("renders Analytics heading", () => {
    render(<AnalyticsDashboard />);
    expect(screen.getByText("Analytics")).toBeInTheDocument();
  });

  test("renders time range buttons", () => {
    render(<AnalyticsDashboard />);
    expect(screen.getByText("7 days")).toBeInTheDocument();
    expect(screen.getByText("30 days")).toBeInTheDocument();
    expect(screen.getByText("90 days")).toBeInTheDocument();
    expect(screen.getByText("All time")).toBeInTheDocument();
  });

  test("shows loading state initially", () => {
    render(<AnalyticsDashboard />);
    expect(screen.getByText("Loading analytics...")).toBeInTheDocument();
  });

  test("renders stat cards after loading", async () => {
    render(<AnalyticsDashboard />);
    await waitFor(() => {
      expect(screen.getByText("Total Cost")).toBeInTheDocument();
      expect(screen.getByText("Total Tokens")).toBeInTheDocument();
      expect(screen.getByText("Sessions")).toBeInTheDocument();
      expect(screen.getByText("Loops")).toBeInTheDocument();
    });
  });

  test("renders cost and token values", async () => {
    render(<AnalyticsDashboard />);
    await waitFor(() => {
      expect(screen.getByText("$6.3000")).toBeInTheDocument();
      expect(screen.getByText("42")).toBeInTheDocument();
      expect(screen.getByText("10")).toBeInTheDocument();
    });
  });
});
