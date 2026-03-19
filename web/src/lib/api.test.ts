import { describe, test, expect, beforeEach, vi } from "vitest";
import { api } from "./api";

function mockFetch(data: unknown, ok = true, status = 200) {
  const text = data === undefined ? "" : JSON.stringify(data);
  return vi.fn().mockResolvedValueOnce({
    ok,
    status,
    statusText: ok ? "OK" : "Error",
    text: async () => text,
  });
}

function mockFetchError(status: number, body: string) {
  return vi.fn().mockResolvedValueOnce({
    ok: false,
    status,
    statusText: "Error",
    text: async () => body,
  });
}

beforeEach(() => {
  vi.restoreAllMocks();
});

// ---------- request() internals ----------

describe("request() error handling", () => {
  test("throws ApiError on non-ok response", async () => {
    global.fetch = mockFetchError(404, "not found");
    await expect(api.hosts.get("x")).rejects.toThrow("not found");
  });

  test("ApiError has status property", async () => {
    global.fetch = mockFetchError(500, "internal");
    try {
      await api.hosts.get("x");
      expect.unreachable("should have thrown");
    } catch (e: unknown) {
      expect((e as { status: number }).status).toBe(500);
      expect((e as Error).name).toBe("ApiError");
    }
  });

  test("uses statusText when body is empty", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: false,
      status: 403,
      statusText: "Forbidden",
      text: async () => "",
    });
    await expect(api.hosts.list()).rejects.toThrow("Forbidden");
  });

  test("returns undefined for empty response body", async () => {
    global.fetch = mockFetch(undefined);
    const result = await api.sessions.close("s1");
    expect(result).toBeUndefined();
  });

  test("sets Content-Type header when body is present", async () => {
    global.fetch = mockFetch({ id: "s1" });
    await api.sessions.create("h1");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[1].headers["Content-Type"]).toBe("application/json");
  });

  test("does not set Content-Type header when no body", async () => {
    global.fetch = mockFetch([]);
    await api.hosts.list();
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[1]?.headers?.["Content-Type"]).toBeUndefined();
  });
});

// ---------- api.hosts ----------

describe("api.hosts", () => {
  test("list fetches /api/hosts", async () => {
    const hosts = [{ id: "1", hostname: "h1" }];
    global.fetch = mockFetch(hosts);
    const result = await api.hosts.list();
    expect(result).toEqual(hosts);
    expect(fetch).toHaveBeenCalledWith("/api/hosts", expect.any(Object));
  });

  test("get fetches /api/hosts/:id", async () => {
    global.fetch = mockFetch({ id: "1" });
    await api.hosts.get("1");
    expect(fetch).toHaveBeenCalledWith("/api/hosts/1", expect.any(Object));
  });
});

// ---------- api.sessions ----------

describe("api.sessions", () => {
  test("list fetches sessions for host", async () => {
    global.fetch = mockFetch([]);
    await api.sessions.list("h1");
    expect(fetch).toHaveBeenCalledWith("/api/hosts/h1/sessions", expect.any(Object));
  });

  test("get fetches session by id", async () => {
    global.fetch = mockFetch({ id: "s1" });
    await api.sessions.get("s1");
    expect(fetch).toHaveBeenCalledWith("/api/sessions/s1", expect.any(Object));
  });

  test("create sends POST with defaults", async () => {
    global.fetch = mockFetch({ id: "s1" });
    await api.sessions.create("h1");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/hosts/h1/sessions");
    expect(call[1].method).toBe("POST");
    const body = JSON.parse(call[1].body);
    expect(body.cols).toBe(80);
    expect(body.rows).toBe(24);
  });

  test("create sends POST with custom options", async () => {
    global.fetch = mockFetch({ id: "s1" });
    await api.sessions.create("h1", { name: "my-session", shell: "/bin/zsh", cols: 120, rows: 40, workingDir: "/tmp" });
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    const body = JSON.parse(call[1].body);
    expect(body.name).toBe("my-session");
    expect(body.shell).toBe("/bin/zsh");
    expect(body.cols).toBe(120);
    expect(body.rows).toBe(40);
    expect(body.working_dir).toBe("/tmp");
  });

  test("close sends DELETE", async () => {
    global.fetch = mockFetch(undefined);
    await api.sessions.close("s1");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/sessions/s1");
    expect(call[1].method).toBe("DELETE");
  });

  test("purge sends DELETE to /purge", async () => {
    global.fetch = mockFetch(undefined);
    await api.sessions.purge("s1");
    expect(fetch).toHaveBeenCalledWith("/api/sessions/s1/purge", expect.objectContaining({ method: "DELETE" }));
  });

  test("rename sends PATCH with name", async () => {
    global.fetch = mockFetch({ id: "s1", name: "new-name" });
    await api.sessions.rename("s1", "new-name");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[1].method).toBe("PATCH");
    expect(JSON.parse(call[1].body)).toEqual({ name: "new-name" });
  });

  test("rename with null clears name", async () => {
    global.fetch = mockFetch({ id: "s1", name: null });
    await api.sessions.rename("s1", null);
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(JSON.parse(call[1].body)).toEqual({ name: null });
  });
});

// ---------- api.loops ----------

describe("api.loops", () => {
  test("list with no filters", async () => {
    global.fetch = mockFetch([]);
    await api.loops.list();
    expect(fetch).toHaveBeenCalledWith("/api/loops", expect.any(Object));
  });

  test("list with session_id filter", async () => {
    global.fetch = mockFetch([]);
    await api.loops.list({ session_id: "s1" });
    expect(fetch).toHaveBeenCalledWith("/api/loops?session_id=s1", expect.any(Object));
  });

  test("list with multiple filters", async () => {
    global.fetch = mockFetch([]);
    await api.loops.list({ session_id: "s1", status: "working", project_id: "p1" });
    const url = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0][0] as string;
    expect(url).toContain("session_id=s1");
    expect(url).toContain("status=working");
    expect(url).toContain("project_id=p1");
  });

  test("get fetches loop by id", async () => {
    global.fetch = mockFetch({ id: "l1" });
    await api.loops.get("l1");
    expect(fetch).toHaveBeenCalledWith("/api/loops/l1", expect.any(Object));
  });

  test("tools fetches tool calls for loop", async () => {
    global.fetch = mockFetch([]);
    await api.loops.tools("l1");
    expect(fetch).toHaveBeenCalledWith("/api/loops/l1/tools", expect.any(Object));
  });

  test("transcript fetches transcript entries", async () => {
    global.fetch = mockFetch([]);
    await api.loops.transcript("l1");
    expect(fetch).toHaveBeenCalledWith("/api/loops/l1/transcript", expect.any(Object));
  });

  test("action sends POST with action and payload", async () => {
    global.fetch = mockFetch(undefined);
    await api.loops.action("l1", "approve", "yes");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/loops/l1/action");
    expect(call[1].method).toBe("POST");
    expect(JSON.parse(call[1].body)).toEqual({ action: "approve", payload: "yes" });
  });

  test("metrics fetches loop metrics", async () => {
    global.fetch = mockFetch({ loop_id: "l1" });
    await api.loops.metrics("l1");
    expect(fetch).toHaveBeenCalledWith("/api/loops/l1/metrics", expect.any(Object));
  });
});

// ---------- api.permissions ----------

describe("api.permissions", () => {
  test("list fetches permissions", async () => {
    global.fetch = mockFetch([]);
    await api.permissions.list();
    expect(fetch).toHaveBeenCalledWith("/api/permissions", expect.any(Object));
  });

  test("upsert sends PUT", async () => {
    global.fetch = mockFetch({ id: "p1" });
    await api.permissions.upsert({ scope: "global", tool_pattern: "*", action: "ask" });
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[1].method).toBe("PUT");
  });

  test("delete sends DELETE", async () => {
    global.fetch = mockFetch(undefined);
    await api.permissions.delete("p1");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/permissions/p1");
    expect(call[1].method).toBe("DELETE");
  });
});

// ---------- api.projects ----------

describe("api.projects", () => {
  test("list fetches projects for host", async () => {
    global.fetch = mockFetch([]);
    await api.projects.list("h1");
    expect(fetch).toHaveBeenCalledWith("/api/hosts/h1/projects", expect.any(Object));
  });

  test("get fetches project by id", async () => {
    global.fetch = mockFetch({ id: "p1" });
    await api.projects.get("p1");
    expect(fetch).toHaveBeenCalledWith("/api/projects/p1", expect.any(Object));
  });

  test("scan triggers project scan", async () => {
    global.fetch = mockFetch(undefined);
    await api.projects.scan("h1");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/hosts/h1/projects/scan");
    expect(call[1].method).toBe("POST");
  });

  test("add sends POST with path", async () => {
    global.fetch = mockFetch({ id: "p1" });
    await api.projects.add("h1", "/projects/my-app");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(JSON.parse(call[1].body)).toEqual({ path: "/projects/my-app" });
  });

  test("delete sends DELETE", async () => {
    global.fetch = mockFetch(undefined);
    await api.projects.delete("p1");
    expect(fetch).toHaveBeenCalledWith("/api/projects/p1", expect.objectContaining({ method: "DELETE" }));
  });

  test("sessions fetches project sessions", async () => {
    global.fetch = mockFetch([]);
    await api.projects.sessions("p1");
    expect(fetch).toHaveBeenCalledWith("/api/projects/p1/sessions", expect.any(Object));
  });

  test("refreshGit sends POST", async () => {
    global.fetch = mockFetch(undefined);
    await api.projects.refreshGit("p1");
    expect(fetch).toHaveBeenCalledWith("/api/projects/p1/git/refresh", expect.objectContaining({ method: "POST" }));
  });

  test("worktrees fetches worktrees", async () => {
    global.fetch = mockFetch([]);
    await api.projects.worktrees("p1");
    expect(fetch).toHaveBeenCalledWith("/api/projects/p1/worktrees", expect.any(Object));
  });

  test("createWorktree sends POST with body", async () => {
    global.fetch = mockFetch(undefined);
    await api.projects.createWorktree("p1", { branch: "feat", new_branch: true });
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/projects/p1/worktrees");
    expect(call[1].method).toBe("POST");
    expect(JSON.parse(call[1].body)).toEqual({ branch: "feat", new_branch: true });
  });

  test("deleteWorktree sends DELETE", async () => {
    global.fetch = mockFetch(undefined);
    await api.projects.deleteWorktree("p1", "w1");
    expect(fetch).toHaveBeenCalledWith("/api/projects/p1/worktrees/w1", expect.objectContaining({ method: "DELETE" }));
  });
});

// ---------- api.analytics ----------

describe("api.analytics", () => {
  test("tokens with no params", async () => {
    global.fetch = mockFetch([]);
    await api.analytics.tokens();
    expect(fetch).toHaveBeenCalledWith("/api/analytics/tokens", expect.any(Object));
  });

  test("tokens with params", async () => {
    global.fetch = mockFetch([]);
    await api.analytics.tokens({ by: "day", from: "2026-01-01", to: "2026-01-31" });
    const url = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0][0] as string;
    expect(url).toContain("by=day");
    expect(url).toContain("from=2026-01-01");
    expect(url).toContain("to=2026-01-31");
  });

  test("cost with no params", async () => {
    global.fetch = mockFetch([]);
    await api.analytics.cost();
    expect(fetch).toHaveBeenCalledWith("/api/analytics/cost", expect.any(Object));
  });

  test("cost with params", async () => {
    global.fetch = mockFetch([]);
    await api.analytics.cost({ granularity: "hour" });
    const url = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0][0] as string;
    expect(url).toContain("granularity=hour");
  });

  test("sessions analytics", async () => {
    global.fetch = mockFetch({ total_sessions: 5 });
    await api.analytics.sessions();
    expect(fetch).toHaveBeenCalledWith("/api/analytics/sessions", expect.any(Object));
  });

  test("sessions analytics with date range", async () => {
    global.fetch = mockFetch({ total_sessions: 5 });
    await api.analytics.sessions({ from: "2026-01-01", to: "2026-03-01" });
    const url = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0][0] as string;
    expect(url).toContain("from=2026-01-01");
  });

  test("loops analytics", async () => {
    global.fetch = mockFetch({ total_loops: 10 });
    await api.analytics.loops();
    expect(fetch).toHaveBeenCalledWith("/api/analytics/loops", expect.any(Object));
  });
});

// ---------- api.search ----------

describe("api.search", () => {
  test("transcripts with query", async () => {
    global.fetch = mockFetch({ results: [], total: 0 });
    await api.search.transcripts({ q: "hello" });
    const url = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0][0] as string;
    expect(url).toContain("q=hello");
  });

  test("transcripts with all params", async () => {
    global.fetch = mockFetch({ results: [], total: 0 });
    await api.search.transcripts({
      q: "test",
      host: "h1",
      project: "p1",
      from: "2026-01-01",
      to: "2026-01-31",
      page: 2,
      per_page: 50,
    });
    const url = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0][0] as string;
    expect(url).toContain("q=test");
    expect(url).toContain("host=h1");
    expect(url).toContain("project=p1");
    expect(url).toContain("page=2");
    expect(url).toContain("per_page=50");
  });

  test("transcripts with no params", async () => {
    global.fetch = mockFetch({ results: [], total: 0 });
    await api.search.transcripts({});
    expect(fetch).toHaveBeenCalledWith("/api/search/transcripts", expect.any(Object));
  });
});

// ---------- api.config ----------

describe("api.config", () => {
  test("getGlobal fetches config key", async () => {
    global.fetch = mockFetch({ key: "theme", value: "dark" });
    await api.config.getGlobal("theme");
    expect(fetch).toHaveBeenCalledWith("/api/config/theme", expect.any(Object));
  });

  test("setGlobal sends PUT", async () => {
    global.fetch = mockFetch({ key: "theme", value: "light" });
    await api.config.setGlobal("theme", "light");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[1].method).toBe("PUT");
    expect(JSON.parse(call[1].body)).toEqual({ value: "light" });
  });

  test("getHost fetches host config", async () => {
    global.fetch = mockFetch({ key: "shell", value: "zsh" });
    await api.config.getHost("h1", "shell");
    expect(fetch).toHaveBeenCalledWith("/api/hosts/h1/config/shell", expect.any(Object));
  });

  test("setHost sends PUT", async () => {
    global.fetch = mockFetch({ key: "shell", value: "bash" });
    await api.config.setHost("h1", "shell", "bash");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/hosts/h1/config/shell");
    expect(call[1].method).toBe("PUT");
  });
});

// ---------- api.knowledge ----------

describe("api.knowledge", () => {
  test("getStatus", async () => {
    global.fetch = mockFetch({ id: "kb1" });
    await api.knowledge.getStatus("p1");
    expect(fetch).toHaveBeenCalledWith("/api/projects/p1/knowledge/status", expect.any(Object));
  });

  test("triggerIndex without force", async () => {
    global.fetch = mockFetch(undefined);
    await api.knowledge.triggerIndex("p1");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(JSON.parse(call[1].body)).toEqual({ force_reindex: false });
  });

  test("triggerIndex with force", async () => {
    global.fetch = mockFetch(undefined);
    await api.knowledge.triggerIndex("p1", true);
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(JSON.parse(call[1].body)).toEqual({ force_reindex: true });
  });

  test("search", async () => {
    global.fetch = mockFetch({ results: [], duration_ms: 10 });
    await api.knowledge.search("p1", "query", "l0", 5);
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/projects/p1/knowledge/search");
    expect(JSON.parse(call[1].body)).toEqual({ query: "query", tier: "l0", max_results: 5 });
  });

  test("listMemories without category", async () => {
    global.fetch = mockFetch([]);
    await api.knowledge.listMemories("p1");
    expect(fetch).toHaveBeenCalledWith("/api/projects/p1/knowledge/memories", expect.any(Object));
  });

  test("listMemories with category", async () => {
    global.fetch = mockFetch([]);
    await api.knowledge.listMemories("p1", "pattern");
    expect(fetch).toHaveBeenCalledWith("/api/projects/p1/knowledge/memories?category=pattern", expect.any(Object));
  });

  test("updateMemory", async () => {
    global.fetch = mockFetch({ id: "m1" });
    await api.knowledge.updateMemory("p1", "m1", { content: "updated" });
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/projects/p1/knowledge/memories/m1");
    expect(call[1].method).toBe("PUT");
  });

  test("deleteMemory", async () => {
    global.fetch = mockFetch(undefined);
    await api.knowledge.deleteMemory("p1", "m1");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/projects/p1/knowledge/memories/m1");
    expect(call[1].method).toBe("DELETE");
  });

  test("extractMemories", async () => {
    global.fetch = mockFetch(undefined);
    await api.knowledge.extractMemories("p1", "l1");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(JSON.parse(call[1].body)).toEqual({ loop_id: "l1" });
  });

  test("generateInstructions", async () => {
    global.fetch = mockFetch({ content: "# Instructions", memories_used: 5 });
    await api.knowledge.generateInstructions("p1");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/projects/p1/knowledge/generate-instructions");
    expect(call[1].method).toBe("POST");
  });

  test("writeClaudeMd", async () => {
    global.fetch = mockFetch({ written: true, bytes: 100 });
    await api.knowledge.writeClaudeMd("p1");
    expect(fetch).toHaveBeenCalledWith(
      "/api/projects/p1/knowledge/write-claude-md",
      expect.objectContaining({ method: "POST" }),
    );
  });

  test("bootstrapProject", async () => {
    global.fetch = mockFetch(undefined);
    await api.knowledge.bootstrapProject("p1");
    expect(fetch).toHaveBeenCalledWith(
      "/api/projects/p1/knowledge/bootstrap",
      expect.objectContaining({ method: "POST" }),
    );
  });

  test("controlService", async () => {
    global.fetch = mockFetch(undefined);
    await api.knowledge.controlService("h1", "restart");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/hosts/h1/knowledge/service");
    expect(JSON.parse(call[1].body)).toEqual({ action: "restart" });
  });
});

// ---------- api.claudeTasks ----------

describe("api.claudeTasks", () => {
  test("create sends POST", async () => {
    global.fetch = mockFetch({ id: "t1" });
    await api.claudeTasks.create({ host_id: "h1", project_path: "/app" });
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/claude-tasks");
    expect(call[1].method).toBe("POST");
  });

  test("list with no filters", async () => {
    global.fetch = mockFetch([]);
    await api.claudeTasks.list();
    expect(fetch).toHaveBeenCalledWith("/api/claude-tasks", expect.any(Object));
  });

  test("list with filters", async () => {
    global.fetch = mockFetch([]);
    await api.claudeTasks.list({ host_id: "h1", status: "active" });
    const url = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0][0] as string;
    expect(url).toContain("host_id=h1");
    expect(url).toContain("status=active");
  });

  test("get fetches task by id", async () => {
    global.fetch = mockFetch({ id: "t1" });
    await api.claudeTasks.get("t1");
    expect(fetch).toHaveBeenCalledWith("/api/claude-tasks/t1", expect.any(Object));
  });

  test("resume sends POST with prompt", async () => {
    global.fetch = mockFetch({ id: "t1" });
    await api.claudeTasks.resume("t1", "continue");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/claude-tasks/t1/resume");
    expect(call[1].method).toBe("POST");
    expect(JSON.parse(call[1].body)).toEqual({ initial_prompt: "continue" });
  });

  test("resume sends POST without prompt", async () => {
    global.fetch = mockFetch({ id: "t1" });
    await api.claudeTasks.resume("t1");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/claude-tasks/t1/resume");
    expect(call[1].body).toBeUndefined();
  });

  test("discover sends GET with project_path param", async () => {
    global.fetch = mockFetch([]);
    await api.claudeTasks.discover("h1", "/my/project");
    const url = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0][0] as string;
    expect(url).toContain("/api/hosts/h1/claude-tasks/discover");
    expect(url).toContain("project_path=%2Fmy%2Fproject");
  });
});
