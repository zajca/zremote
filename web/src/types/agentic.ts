export type AgenticStatus =
  | "working"
  | "waiting_for_input"
  | "paused"
  | "error"
  | "completed";

export type ToolCallStatus =
  | "pending"
  | "approved"
  | "rejected"
  | "running"
  | "completed"
  | "failed";

export type UserAction =
  | "approve"
  | "reject"
  | "provide_input"
  | "pause"
  | "resume"
  | "stop";

export type TranscriptRole = "assistant" | "user" | "tool" | "system";

export type PermissionAction = "auto_approve" | "ask" | "deny";

export interface AgenticLoop {
  id: string;
  session_id: string;
  project_path: string | null;
  tool_name: string;
  model: string | null;
  status: AgenticStatus;
  started_at: string;
  ended_at: string | null;
  total_tokens_in: number;
  total_tokens_out: number;
  estimated_cost_usd: number;
  end_reason: string | null;
  summary: string | null;
  context_used: number;
  context_max: number;
  pending_tool_calls: number;
}

export interface ToolCall {
  id: string;
  loop_id: string;
  tool_name: string;
  arguments_json: string | null;
  status: ToolCallStatus;
  result_preview: string | null;
  duration_ms: number | null;
  created_at: string;
  resolved_at: string | null;
}

export interface TranscriptEntry {
  id: number;
  loop_id: string;
  role: TranscriptRole;
  content: string;
  tool_call_id: string | null;
  timestamp: string;
}

export interface AgenticMetrics {
  loop_id: string;
  tokens_in: number;
  tokens_out: number;
  model: string;
  context_used: number;
  context_max: number;
  estimated_cost_usd: number;
}

export interface PermissionRule {
  id: string;
  scope: string;
  tool_pattern: string;
  action: PermissionAction;
}
