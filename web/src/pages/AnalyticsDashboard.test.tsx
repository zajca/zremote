import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
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

  test("renders loop detail stats (completed/errors)", async () => {
    render(<AnalyticsDashboard />);
    await waitFor(() => {
      expect(screen.getByText("38 completed, 4 errors")).toBeInTheDocument();
    });
  });

  test("renders active sessions count", async () => {
    render(<AnalyticsDashboard />);
    await waitFor(() => {
      expect(screen.getByText("3 active")).toBeInTheDocument();
    });
  });

  test("renders avg cost per loop", async () => {
    render(<AnalyticsDashboard />);
    await waitFor(() => {
      expect(screen.getByText("avg $0.1500 / loop")).toBeInTheDocument();
    });
  });

  test("renders token breakdown (in/out)", async () => {
    render(<AnalyticsDashboard />);
    await waitFor(() => {
      expect(screen.getByText("500.0K in / 200.0K out")).toBeInTheDocument();
    });
  });

  test("renders Cost Over Time chart section", async () => {
    render(<AnalyticsDashboard />);
    await waitFor(() => {
      expect(screen.getByText("Cost Over Time")).toBeInTheDocument();
    });
  });

  test("renders Tokens by Model chart section", async () => {
    render(<AnalyticsDashboard />);
    await waitFor(() => {
      expect(screen.getByText("Tokens by Model")).toBeInTheDocument();
    });
  });

  test("switching time range refetches data", async () => {
    render(<AnalyticsDashboard />);
    await waitFor(() => {
      expect(screen.getByText("Total Cost")).toBeInTheDocument();
    });

    const callsBefore = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.length;
    await userEvent.click(screen.getByText("7 days"));

    await waitFor(() => {
      const callsAfter = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.length;
      expect(callsAfter).toBeGreaterThan(callsBefore);
    });
  });

  test("shows empty analytics message when no chart data", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/api/analytics/loops")) {
        return Promise.resolve({
          ok: true,
          json: async () => ({
            total_loops: 0,
            completed: 0,
            errored: 0,
            avg_cost_usd: null,
            total_cost_usd: 0,
            total_tokens_in: 0,
            total_tokens_out: 0,
          }),
        });
      }
      if (url.includes("/api/analytics/sessions")) {
        return Promise.resolve({
          ok: true,
          json: async () => ({
            total_sessions: 0,
            active_sessions: 0,
            avg_duration_seconds: null,
          }),
        });
      }
      if (url.includes("/api/analytics/cost")) {
        return Promise.resolve({ ok: true, json: async () => [] });
      }
      if (url.includes("/api/analytics/tokens")) {
        return Promise.resolve({ ok: true, json: async () => [] });
      }
      return Promise.resolve({ ok: true, json: async () => ({}) });
    });

    render(<AnalyticsDashboard />);
    await waitFor(() => {
      expect(screen.getByText("No analytics data yet")).toBeInTheDocument();
      expect(screen.getByText("Data will appear after agentic loops run on your hosts.")).toBeInTheDocument();
    });
  });
});
