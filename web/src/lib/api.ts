export interface Host {
  id: string;
  hostname: string;
  os: string;
  arch: string;
  agent_version: string;
  status: "online" | "offline";
  last_seen: string;
  connected_at: string;
}

export interface Session {
  id: string;
  host_id: string;
  name: string | null;
  shell: string | null;
  status: "creating" | "active" | "closed" | "error";
  cols: number;
  rows: number;
  created_at: string;
  closed_at: string | null;
  exit_code: number | null;
  working_dir: string | null;
  project_id: string | null;
}

export interface Project {
  id: string;
  host_id: string;
  path: string;
  name: string;
  has_claude_config: boolean;
  project_type: string;
  created_at: string;
  parent_project_id: string | null;
  git_branch: string | null;
  git_commit_hash: string | null;
  git_commit_message: string | null;
  git_is_dirty: boolean;
  git_ahead: number;
  git_behind: number;
  git_remotes: string | null;
  git_updated_at: string | null;
}

export interface ConfigValue {
  key: string;
  value: string;
  updated_at: string;
}

class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    ...options,
    headers: {
      "Content-Type": "application/json",
      ...options?.headers,
    },
  });

  if (!response.ok) {
    const text = await response.text();
    throw new ApiError(response.status, text || response.statusText);
  }

  return response.json() as Promise<T>;
}

import type {
  AgenticLoop,
  AgenticMetrics,
  PermissionRule,
  ToolCall,
  TranscriptEntry,
  UserAction,
} from "../types/agentic";
import type {
  ClaudeTask,
  CreateClaudeTaskRequest,
} from "../types/claude-session";
import type {
  KnowledgeBase,
  KnowledgeMemory,
  MemoryCategory,
  SearchResponse,
  SearchTier,
} from "../types/knowledge";

export const api = {
  hosts: {
    list: () => request<Host[]>("/api/hosts"),
    get: (hostId: string) => request<Host>(`/api/hosts/${hostId}`),
  },
  sessions: {
    list: (hostId: string) =>
      request<Session[]>(`/api/hosts/${hostId}/sessions`),
    get: (sessionId: string) =>
      request<Session>(`/api/sessions/${sessionId}`),
    create: (hostId: string, options?: {
      name?: string;
      shell?: string;
      cols?: number;
      rows?: number;
      workingDir?: string;
    }) =>
      request<Session>(`/api/hosts/${hostId}/sessions`, {
        method: "POST",
        body: JSON.stringify({
          name: options?.name,
          shell: options?.shell,
          cols: options?.cols ?? 80,
          rows: options?.rows ?? 24,
          working_dir: options?.workingDir,
        }),
      }),
    close: (sessionId: string) =>
      request<void>(`/api/sessions/${sessionId}`, {
        method: "DELETE",
      }),
    purge: (sessionId: string) =>
      request<void>(`/api/sessions/${sessionId}/purge`, {
        method: "DELETE",
      }),
    rename: (sessionId: string, name: string | null) =>
      request<Session>(`/api/sessions/${sessionId}`, {
        method: "PATCH",
        body: JSON.stringify({ name }),
      }),
  },
  loops: {
    list: (filters?: { session_id?: string; status?: string; project_id?: string }) => {
      const params = new URLSearchParams();
      if (filters?.session_id) params.set("session_id", filters.session_id);
      if (filters?.status) params.set("status", filters.status);
      if (filters?.project_id) params.set("project_id", filters.project_id);
      const qs = params.toString();
      return request<AgenticLoop[]>(`/api/loops${qs ? `?${qs}` : ""}`);
    },
    get: (id: string) => request<AgenticLoop>(`/api/loops/${id}`),
    tools: (id: string) => request<ToolCall[]>(`/api/loops/${id}/tools`),
    transcript: (id: string) =>
      request<TranscriptEntry[]>(`/api/loops/${id}/transcript`),
    action: (id: string, action: UserAction, payload?: string) =>
      request<void>(`/api/loops/${id}/action`, {
        method: "POST",
        body: JSON.stringify({ action, payload }),
      }),
    metrics: (id: string) =>
      request<AgenticMetrics>(`/api/loops/${id}/metrics`),
  },
  permissions: {
    list: () => request<PermissionRule[]>("/api/permissions"),
    upsert: (rule: Omit<PermissionRule, "id"> & { id?: string }) =>
      request<PermissionRule>("/api/permissions", {
        method: "PUT",
        body: JSON.stringify(rule),
      }),
    delete: (id: string) =>
      request<void>(`/api/permissions/${id}`, { method: "DELETE" }),
  },
  projects: {
    list: (hostId: string) =>
      request<Project[]>(`/api/hosts/${hostId}/projects`),
    get: (id: string) => request<Project>(`/api/projects/${id}`),
    scan: (hostId: string) =>
      request<void>(`/api/hosts/${hostId}/projects/scan`, {
        method: "POST",
      }),
    add: (hostId: string, path: string) =>
      request<Project>(`/api/hosts/${hostId}/projects`, {
        method: "POST",
        body: JSON.stringify({ path }),
      }),
    delete: (id: string) =>
      request<void>(`/api/projects/${id}`, { method: "DELETE" }),
    sessions: (projectId: string) =>
      request<Session[]>(`/api/projects/${projectId}/sessions`),
    refreshGit: (id: string) =>
      request<void>(`/api/projects/${id}/git/refresh`, { method: "POST" }),
    worktrees: (id: string) =>
      request<Project[]>(`/api/projects/${id}/worktrees`),
    createWorktree: (
      id: string,
      body: { branch: string; path?: string; new_branch?: boolean },
    ) =>
      request<void>(`/api/projects/${id}/worktrees`, {
        method: "POST",
        body: JSON.stringify(body),
      }),
    deleteWorktree: (projectId: string, worktreeId: string) =>
      request<void>(`/api/projects/${projectId}/worktrees/${worktreeId}`, {
        method: "DELETE",
      }),
  },
  analytics: {
    tokens: (params?: { by?: string; from?: string; to?: string }) => {
      const qs = new URLSearchParams();
      if (params?.by) qs.set("by", params.by);
      if (params?.from) qs.set("from", params.from);
      if (params?.to) qs.set("to", params.to);
      const s = qs.toString();
      return request<
        { label: string; tokens_in: number; tokens_out: number }[]
      >(`/api/analytics/tokens${s ? `?${s}` : ""}`);
    },
    cost: (params?: { granularity?: string; from?: string; to?: string }) => {
      const qs = new URLSearchParams();
      if (params?.granularity) qs.set("granularity", params.granularity);
      if (params?.from) qs.set("from", params.from);
      if (params?.to) qs.set("to", params.to);
      const s = qs.toString();
      return request<{ period: string; cost: number }[]>(
        `/api/analytics/cost${s ? `?${s}` : ""}`,
      );
    },
    sessions: (params?: { from?: string; to?: string }) => {
      const qs = new URLSearchParams();
      if (params?.from) qs.set("from", params.from);
      if (params?.to) qs.set("to", params.to);
      const s = qs.toString();
      return request<{
        total_sessions: number;
        active_sessions: number;
        avg_duration_seconds: number | null;
      }>(`/api/analytics/sessions${s ? `?${s}` : ""}`);
    },
    loops: (params?: { from?: string; to?: string }) => {
      const qs = new URLSearchParams();
      if (params?.from) qs.set("from", params.from);
      if (params?.to) qs.set("to", params.to);
      const s = qs.toString();
      return request<{
        total_loops: number;
        completed: number;
        errored: number;
        avg_cost_usd: number | null;
        total_cost_usd: number;
        total_tokens_in: number;
        total_tokens_out: number;
      }>(`/api/analytics/loops${s ? `?${s}` : ""}`);
    },
  },
  search: {
    transcripts: (params: {
      q?: string;
      host?: string;
      project?: string;
      from?: string;
      to?: string;
      page?: number;
      per_page?: number;
    }) => {
      const qs = new URLSearchParams();
      if (params.q) qs.set("q", params.q);
      if (params.host) qs.set("host", params.host);
      if (params.project) qs.set("project", params.project);
      if (params.from) qs.set("from", params.from);
      if (params.to) qs.set("to", params.to);
      if (params.page) qs.set("page", String(params.page));
      if (params.per_page) qs.set("per_page", String(params.per_page));
      const s = qs.toString();
      return request<{
        results: {
          transcript_id: number;
          loop_id: string;
          role: string;
          content: string;
          timestamp: string;
          tool_name: string;
          project_path: string | null;
          loop_status: string;
          model: string | null;
          estimated_cost_usd: number | null;
        }[];
        total: number;
        page: number;
        per_page: number;
      }>(`/api/search/transcripts${s ? `?${s}` : ""}`);
    },
  },
  config: {
    getGlobal: (key: string) =>
      request<ConfigValue>(`/api/config/${key}`),
    setGlobal: (key: string, value: string) =>
      request<ConfigValue>(`/api/config/${key}`, {
        method: "PUT",
        body: JSON.stringify({ value }),
      }),
    getHost: (hostId: string, key: string) =>
      request<ConfigValue>(`/api/hosts/${hostId}/config/${key}`),
    setHost: (hostId: string, key: string, value: string) =>
      request<ConfigValue>(`/api/hosts/${hostId}/config/${key}`, {
        method: "PUT",
        body: JSON.stringify({ value }),
      }),
  },
  knowledge: {
    getStatus: (projectId: string) =>
      request<KnowledgeBase | null>(
        `/api/projects/${projectId}/knowledge/status`,
      ),
    triggerIndex: (projectId: string, forceReindex = false) =>
      request<void>(`/api/projects/${projectId}/knowledge/index`, {
        method: "POST",
        body: JSON.stringify({ force_reindex: forceReindex }),
      }),
    search: (
      projectId: string,
      query: string,
      tier?: SearchTier,
      maxResults?: number,
    ) =>
      request<SearchResponse>(
        `/api/projects/${projectId}/knowledge/search`,
        {
          method: "POST",
          body: JSON.stringify({ query, tier, max_results: maxResults }),
        },
      ),
    listMemories: (projectId: string, category?: MemoryCategory) => {
      const params = category ? `?category=${category}` : "";
      return request<KnowledgeMemory[]>(
        `/api/projects/${projectId}/knowledge/memories${params}`,
      );
    },
    updateMemory: (
      projectId: string,
      memoryId: string,
      data: { content?: string; category?: string },
    ) =>
      request<KnowledgeMemory>(
        `/api/projects/${projectId}/knowledge/memories/${memoryId}`,
        {
          method: "PUT",
          body: JSON.stringify(data),
        },
      ),
    deleteMemory: (projectId: string, memoryId: string) =>
      request<void>(
        `/api/projects/${projectId}/knowledge/memories/${memoryId}`,
        {
          method: "DELETE",
        },
      ),
    extractMemories: (projectId: string, loopId: string) =>
      request<void>(`/api/projects/${projectId}/knowledge/extract`, {
        method: "POST",
        body: JSON.stringify({ loop_id: loopId }),
      }),
    generateInstructions: (projectId: string) =>
      request<{ content: string; memories_used: number }>(
        `/api/projects/${projectId}/knowledge/generate-instructions`,
        {
          method: "POST",
        },
      ),
    writeClaudeMd: (projectId: string) =>
      request<{ written: boolean; bytes: number }>(
        `/api/projects/${projectId}/knowledge/write-claude-md`,
        {
          method: "POST",
        },
      ),
    bootstrapProject: (projectId: string) =>
      request<void>(`/api/projects/${projectId}/knowledge/bootstrap`, {
        method: "POST",
      }),
    controlService: (hostId: string, action: "start" | "stop" | "restart") =>
      request<void>(`/api/hosts/${hostId}/knowledge/service`, {
        method: "POST",
        body: JSON.stringify({ action }),
      }),
  },
  claudeTasks: {
    create: (body: CreateClaudeTaskRequest) =>
      request<ClaudeTask>("/api/claude-tasks", {
        method: "POST",
        body: JSON.stringify(body),
      }),
    list: (filters?: { host_id?: string; status?: string; project_id?: string }) => {
      const params = new URLSearchParams();
      if (filters?.host_id) params.set("host_id", filters.host_id);
      if (filters?.status) params.set("status", filters.status);
      if (filters?.project_id) params.set("project_id", filters.project_id);
      const qs = params.toString();
      return request<ClaudeTask[]>(`/api/claude-tasks${qs ? `?${qs}` : ""}`);
    },
    get: (id: string) => request<ClaudeTask>(`/api/claude-tasks/${id}`),
  },
};
