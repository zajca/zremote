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
};
