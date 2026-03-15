import { useCallback, useEffect, useState } from "react";
import {
  BarChart3,
  Calendar,
  DollarSign,
  Hash,
  Layers,
} from "lucide-react";
import { format, subDays } from "date-fns";
import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { Button } from "../components/ui/Button";

interface TokenBreakdown {
  label: string;
  tokens_in: number;
  tokens_out: number;
}

interface CostPoint {
  period: string;
  cost: number;
}

interface LoopStats {
  total_loops: number;
  completed: number;
  errored: number;
  avg_cost_usd: number | null;
  total_cost_usd: number;
  total_tokens_in: number;
  total_tokens_out: number;
}

interface SessionStats {
  total_sessions: number;
  active_sessions: number;
  avg_duration_seconds: number | null;
}

type RangeKey = "7d" | "30d" | "90d" | "all";

const RANGES: { key: RangeKey; label: string }[] = [
  { key: "7d", label: "7 days" },
  { key: "30d", label: "30 days" },
  { key: "90d", label: "90 days" },
  { key: "all", label: "All time" },
];

function getDateRange(key: RangeKey): { from?: string; to?: string } {
  if (key === "all") return {};
  const days = key === "7d" ? 7 : key === "30d" ? 30 : 90;
  return { from: format(subDays(new Date(), days), "yyyy-MM-dd") };
}

function formatCost(usd: number): string {
  return `$${usd.toFixed(4)}`;
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return n.toString();
}

interface StatCardProps {
  icon: React.ReactNode;
  label: string;
  value: string;
  sub?: string;
}

function StatCard({ icon, label, value, sub }: StatCardProps) {
  return (
    <div className="rounded-lg border border-border bg-bg-secondary p-4">
      <div className="flex items-center gap-2 text-text-secondary">
        {icon}
        <span className="text-xs font-medium">{label}</span>
      </div>
      <div className="mt-2 text-2xl font-semibold text-text-primary">
        {value}
      </div>
      {sub && (
        <div className="mt-0.5 text-xs text-text-tertiary">{sub}</div>
      )}
    </div>
  );
}

export function AnalyticsDashboard() {
  const [range, setRange] = useState<RangeKey>("30d");
  const [loopStats, setLoopStats] = useState<LoopStats | null>(null);
  const [sessionStats, setSessionStats] = useState<SessionStats | null>(null);
  const [costData, setCostData] = useState<CostPoint[]>([]);
  const [tokensByModel, setTokensByModel] = useState<TokenBreakdown[]>([]);
  const [loading, setLoading] = useState(true);

  const fetchData = useCallback(async () => {
    setLoading(true);
    const { from, to } = getDateRange(range);
    const params = new URLSearchParams();
    if (from) params.set("from", from);
    if (to) params.set("to", to);
    const qs = params.toString();
    const suffix = qs ? `?${qs}` : "";

    try {
      const [loops, sessions, cost, tokens] = await Promise.all([
        fetch(`/api/analytics/loops${suffix}`).then((r) => r.json()) as Promise<LoopStats>,
        fetch(`/api/analytics/sessions${suffix}`).then((r) => r.json()) as Promise<SessionStats>,
        fetch(`/api/analytics/cost${suffix}`).then((r) => r.json()) as Promise<CostPoint[]>,
        fetch(`/api/analytics/tokens?by=model${suffix ? `&${qs}` : ""}`).then((r) => r.json()) as Promise<TokenBreakdown[]>,
      ]);
      setLoopStats(loops);
      setSessionStats(sessions);
      setCostData(cost);
      setTokensByModel(tokens);
    } catch (e) {
      console.error("Failed to fetch analytics", e);
    } finally {
      setLoading(false);
    }
  }, [range]);

  useEffect(() => {
    void fetchData();
  }, [fetchData]);

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between border-b border-border px-6 py-4">
        <div className="flex items-center gap-3">
          <BarChart3 size={20} className="text-accent" />
          <h1 className="text-lg font-semibold text-text-primary">
            Analytics
          </h1>
        </div>
        <div className="flex items-center gap-1">
          <Calendar size={14} className="text-text-tertiary" />
          {RANGES.map((r) => (
            <Button
              key={r.key}
              variant={range === r.key ? "primary" : "ghost"}
              size="sm"
              onClick={() => setRange(r.key)}
            >
              {r.label}
            </Button>
          ))}
        </div>
      </div>

      <div className="flex-1 overflow-auto p-6">
        {loading ? (
          <div className="text-sm text-text-tertiary">Loading analytics...</div>
        ) : (
          <div className="space-y-6">
            {/* Stat cards */}
            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
              <StatCard
                icon={<DollarSign size={14} />}
                label="Total Cost"
                value={formatCost(loopStats?.total_cost_usd ?? 0)}
                sub={
                  loopStats?.avg_cost_usd != null
                    ? `avg ${formatCost(loopStats.avg_cost_usd)} / loop`
                    : undefined
                }
              />
              <StatCard
                icon={<Hash size={14} />}
                label="Total Tokens"
                value={formatTokens(
                  (loopStats?.total_tokens_in ?? 0) +
                    (loopStats?.total_tokens_out ?? 0),
                )}
                sub={`${formatTokens(loopStats?.total_tokens_in ?? 0)} in / ${formatTokens(loopStats?.total_tokens_out ?? 0)} out`}
              />
              <StatCard
                icon={<Layers size={14} />}
                label="Sessions"
                value={String(sessionStats?.total_sessions ?? 0)}
                sub={`${sessionStats?.active_sessions ?? 0} active`}
              />
              <StatCard
                icon={<BarChart3 size={14} />}
                label="Loops"
                value={String(loopStats?.total_loops ?? 0)}
                sub={`${loopStats?.completed ?? 0} completed, ${loopStats?.errored ?? 0} errors`}
              />
            </div>

            {/* Cost over time chart */}
            {costData.length > 0 && (
              <div className="rounded-lg border border-border bg-bg-secondary p-4">
                <h2 className="mb-4 text-sm font-medium text-text-secondary">
                  Cost Over Time
                </h2>
                <ResponsiveContainer width="100%" height={240}>
                  <AreaChart data={costData}>
                    <defs>
                      <linearGradient
                        id="costGrad"
                        x1="0"
                        y1="0"
                        x2="0"
                        y2="1"
                      >
                        <stop
                          offset="0%"
                          stopColor="#5e6ad2"
                          stopOpacity={0.3}
                        />
                        <stop
                          offset="100%"
                          stopColor="#5e6ad2"
                          stopOpacity={0}
                        />
                      </linearGradient>
                    </defs>
                    <CartesianGrid
                      strokeDasharray="3 3"
                      stroke="#222228"
                      vertical={false}
                    />
                    <XAxis
                      dataKey="period"
                      stroke="#5c5c66"
                      fontSize={11}
                      tickLine={false}
                    />
                    <YAxis
                      stroke="#5c5c66"
                      fontSize={11}
                      tickLine={false}
                      tickFormatter={(v: number) => `$${v.toFixed(2)}`}
                    />
                    <Tooltip
                      contentStyle={{
                        background: "#1a1a1e",
                        border: "1px solid #222228",
                        borderRadius: 6,
                        fontSize: 12,
                      }}
                      formatter={(v) => [`$${Number(v).toFixed(4)}`, "Cost"]}
                    />
                    <Area
                      type="monotone"
                      dataKey="cost"
                      stroke="#5e6ad2"
                      fill="url(#costGrad)"
                      strokeWidth={2}
                    />
                  </AreaChart>
                </ResponsiveContainer>
              </div>
            )}

            {/* Token usage by model */}
            {tokensByModel.length > 0 && (
              <div className="rounded-lg border border-border bg-bg-secondary p-4">
                <h2 className="mb-4 text-sm font-medium text-text-secondary">
                  Tokens by Model
                </h2>
                <ResponsiveContainer width="100%" height={240}>
                  <BarChart data={tokensByModel} layout="vertical">
                    <CartesianGrid
                      strokeDasharray="3 3"
                      stroke="#222228"
                      horizontal={false}
                    />
                    <XAxis
                      type="number"
                      stroke="#5c5c66"
                      fontSize={11}
                      tickLine={false}
                      tickFormatter={(v: number) => formatTokens(v)}
                    />
                    <YAxis
                      type="category"
                      dataKey="label"
                      stroke="#5c5c66"
                      fontSize={11}
                      tickLine={false}
                      width={100}
                    />
                    <Tooltip
                      contentStyle={{
                        background: "#1a1a1e",
                        border: "1px solid #222228",
                        borderRadius: 6,
                        fontSize: 12,
                      }}
                      formatter={(v) => [formatTokens(Number(v))]}
                    />
                    <Bar
                      dataKey="tokens_in"
                      fill="#5e6ad2"
                      name="Tokens In"
                      radius={[0, 4, 4, 0]}
                    />
                    <Bar
                      dataKey="tokens_out"
                      fill="#4ade80"
                      name="Tokens Out"
                      radius={[0, 4, 4, 0]}
                    />
                  </BarChart>
                </ResponsiveContainer>
              </div>
            )}

            {costData.length === 0 && tokensByModel.length === 0 && (
              <div className="flex flex-col items-center gap-2 pt-12 text-center">
                <BarChart3
                  size={32}
                  className="text-text-tertiary"
                />
                <p className="text-sm text-text-secondary">
                  No analytics data yet
                </p>
                <p className="text-xs text-text-tertiary">
                  Data will appear after agentic loops run on your hosts.
                </p>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
