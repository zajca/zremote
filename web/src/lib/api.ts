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
  shell: string | null;
  status: "creating" | "active" | "closed" | "error";
  cols: number;
  rows: number;
  created_at: string;
  closed_at: string | null;
  exit_code: number | null;
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

export const api = {
  hosts: {
    list: () => request<Host[]>("/api/hosts"),
    get: (hostId: string) => request<Host>(`/api/hosts/${hostId}`),
  },
  sessions: {
    list: (hostId: string) =>
      request<Session[]>(`/api/hosts/${hostId}/sessions`),
    get: (hostId: string, sessionId: string) =>
      request<Session>(`/api/hosts/${hostId}/sessions/${sessionId}`),
    create: (hostId: string, cols = 80, rows = 24) =>
      request<Session>(`/api/hosts/${hostId}/sessions`, {
        method: "POST",
        body: JSON.stringify({ cols, rows }),
      }),
    close: (hostId: string, sessionId: string) =>
      request<void>(`/api/hosts/${hostId}/sessions/${sessionId}`, {
        method: "DELETE",
      }),
  },
  loops: {
    list: (filters?: { session_id?: string; status?: string }) => {
      const params = new URLSearchParams();
      if (filters?.session_id) params.set("session_id", filters.session_id);
      if (filters?.status) params.set("status", filters.status);
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
};
