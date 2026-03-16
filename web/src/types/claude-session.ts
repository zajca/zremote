export type ClaudeTaskStatus = "starting" | "active" | "completed" | "error";

export type ToolPreset = "standard" | "read_only" | "full_access" | "custom";

export interface ClaudeTask {
  id: string;
  session_id: string;
  host_id: string;
  project_path: string;
  project_id: string | null;
  model: string | null;
  initial_prompt: string | null;
  claude_session_id: string | null;
  resume_from: string | null;
  status: ClaudeTaskStatus;
  options_json: string | null;
  loop_id: string | null;
  started_at: string;
  ended_at: string | null;
  total_cost_usd: number;
  total_tokens_in: number;
  total_tokens_out: number;
  summary: string | null;
  created_at: string;
}

export interface CreateClaudeTaskRequest {
  host_id: string;
  project_path: string;
  project_id?: string;
  model?: string;
  initial_prompt?: string;
  allowed_tools?: string[];
  skip_permissions?: boolean;
  output_format?: string;
  custom_flags?: string;
}

export interface DiscoveredClaudeSession {
  session_id: string;
  project_path: string;
  model: string | null;
  last_active: string | null;
  message_count: number | null;
  summary: string | null;
}
