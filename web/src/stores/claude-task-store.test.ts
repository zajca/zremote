import { describe, test, expect, beforeEach, vi } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useClaudeTaskStore } from "./claude-task-store";
import type { ClaudeTask } from "../types/claude-session";

const mockTask: ClaudeTask = {
  id: "t1",
  session_id: "s1",
  host_id: "h1",
  project_path: "/app",
  project_id: "p1",
  model: "opus",
  initial_prompt: "fix the bug",
  claude_session_id: null,
  resume_from: null,
  status: "active",
  options_json: null,
  loop_id: null,
  started_at: "2026-01-01T00:00:00Z",
  ended_at: null,
  total_cost_usd: 0.5,
  total_tokens_in: 1000,
  total_tokens_out: 500,
  summary: null,
  created_at: "2026-01-01T00:00:00Z",
};

beforeEach(() => {
  vi.restoreAllMocks();
  useClaudeTaskStore.setState({
    tasks: new Map(),
    sessionTaskIndex: new Map(),
  });
});

describe("initial state", () => {
  test("has empty maps", () => {
    const { result } = renderHook(() => useClaudeTaskStore());
    expect(result.current.tasks.size).toBe(0);
    expect(result.current.sessionTaskIndex.size).toBe(0);
  });
});

describe("updateTask", () => {
  test("adds task and indexes by session_id", () => {
    const { result } = renderHook(() => useClaudeTaskStore());
    act(() => result.current.updateTask(mockTask));
    expect(result.current.tasks.get("t1")).toEqual(mockTask);
    expect(result.current.sessionTaskIndex.get("s1")).toBe("t1");
  });

  test("updates existing task", () => {
    const { result } = renderHook(() => useClaudeTaskStore());
    act(() => result.current.updateTask(mockTask));
    const updated = { ...mockTask, status: "completed" as const };
    act(() => result.current.updateTask(updated));
    expect(result.current.tasks.get("t1")?.status).toBe("completed");
    expect(result.current.tasks.size).toBe(1);
  });
});

describe("removeTask", () => {
  test("removes task and session index", () => {
    const { result } = renderHook(() => useClaudeTaskStore());
    act(() => result.current.updateTask(mockTask));
    act(() => result.current.removeTask("t1"));
    expect(result.current.tasks.size).toBe(0);
    expect(result.current.sessionTaskIndex.size).toBe(0);
  });

  test("no-op for non-existent task (no session index cleanup needed)", () => {
    const { result } = renderHook(() => useClaudeTaskStore());
    act(() => result.current.removeTask("nonexistent"));
    expect(result.current.tasks.size).toBe(0);
  });
});

describe("fetchTask", () => {
  test("fetches single task and updates store", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => JSON.stringify(mockTask),
    });

    const { result } = renderHook(() => useClaudeTaskStore());
    await act(async () => {
      await result.current.fetchTask("t1");
    });
    expect(result.current.tasks.get("t1")).toEqual(mockTask);
  });
});

describe("fetchTasks", () => {
  test("fetches tasks list and merges into store", async () => {
    const task2 = { ...mockTask, id: "t2", session_id: "s2" };
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => JSON.stringify([mockTask, task2]),
    });

    const { result } = renderHook(() => useClaudeTaskStore());
    await act(async () => {
      await result.current.fetchTasks();
    });
    expect(result.current.tasks.size).toBe(2);
    expect(result.current.sessionTaskIndex.get("s1")).toBe("t1");
    expect(result.current.sessionTaskIndex.get("s2")).toBe("t2");
  });

  test("fetches with filters", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => JSON.stringify([]),
    });

    const { result } = renderHook(() => useClaudeTaskStore());
    await act(async () => {
      await result.current.fetchTasks({ host_id: "h1", status: "active" });
    });
    const url = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0][0] as string;
    expect(url).toContain("host_id=h1");
    expect(url).toContain("status=active");
  });
});

describe("handleTaskStarted", () => {
  test("dispatches event and triggers fetch", () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => JSON.stringify(mockTask),
    });

    const eventSpy = vi.fn();
    window.addEventListener("myremote:claude-task-update", eventSpy);

    const { result } = renderHook(() => useClaudeTaskStore());
    act(() => {
      result.current.handleTaskStarted({
        task_id: "t1",
        session_id: "s1",
        host_id: "h1",
        project_path: "/app",
      });
    });

    expect(eventSpy).toHaveBeenCalledTimes(1);
    expect(fetch).toHaveBeenCalled();

    window.removeEventListener("myremote:claude-task-update", eventSpy);
  });
});

describe("handleTaskUpdated", () => {
  test("updates existing task status and loop_id", () => {
    const eventSpy = vi.fn();
    window.addEventListener("myremote:claude-task-update", eventSpy);

    const { result } = renderHook(() => useClaudeTaskStore());
    act(() => result.current.updateTask(mockTask));

    act(() => {
      result.current.handleTaskUpdated({
        task_id: "t1",
        status: "completed",
        loop_id: "l1",
      });
    });

    expect(result.current.tasks.get("t1")?.status).toBe("completed");
    expect(result.current.tasks.get("t1")?.loop_id).toBe("l1");
    expect(eventSpy).toHaveBeenCalled();

    window.removeEventListener("myremote:claude-task-update", eventSpy);
  });

  test("fetches unknown task from server", () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => JSON.stringify(mockTask),
    });

    const { result } = renderHook(() => useClaudeTaskStore());
    act(() => {
      result.current.handleTaskUpdated({
        task_id: "unknown",
        status: "active",
        loop_id: null,
      });
    });
    expect(fetch).toHaveBeenCalled();
  });

  test("preserves existing loop_id when update has null", () => {
    const { result } = renderHook(() => useClaudeTaskStore());
    const taskWithLoop = { ...mockTask, loop_id: "existing-loop" };
    act(() => result.current.updateTask(taskWithLoop));

    act(() => {
      result.current.handleTaskUpdated({
        task_id: "t1",
        status: "active",
        loop_id: null,
      });
    });

    expect(result.current.tasks.get("t1")?.loop_id).toBe("existing-loop");
  });
});

describe("handleTaskEnded", () => {
  test("updates existing task with end data", () => {
    const eventSpy = vi.fn();
    window.addEventListener("myremote:claude-task-update", eventSpy);

    const { result } = renderHook(() => useClaudeTaskStore());
    act(() => result.current.updateTask(mockTask));

    act(() => {
      result.current.handleTaskEnded({
        task_id: "t1",
        status: "completed",
        summary: "Done!",
        total_cost_usd: 1.5,
      });
    });

    const task = result.current.tasks.get("t1")!;
    expect(task.status).toBe("completed");
    expect(task.summary).toBe("Done!");
    expect(task.total_cost_usd).toBe(1.5);
    expect(task.ended_at).toBeTruthy();
    expect(eventSpy).toHaveBeenCalled();

    window.removeEventListener("myremote:claude-task-update", eventSpy);
  });

  test("fetches unknown task on end", () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => JSON.stringify(mockTask),
    });

    const { result } = renderHook(() => useClaudeTaskStore());
    act(() => {
      result.current.handleTaskEnded({
        task_id: "unknown",
        status: "completed",
        summary: null,
        total_cost_usd: 0,
      });
    });
    expect(fetch).toHaveBeenCalled();
  });

  test("preserves existing summary when update has null", () => {
    const { result } = renderHook(() => useClaudeTaskStore());
    const taskWithSummary = { ...mockTask, summary: "existing summary" };
    act(() => result.current.updateTask(taskWithSummary));

    act(() => {
      result.current.handleTaskEnded({
        task_id: "t1",
        status: "completed",
        summary: null,
        total_cost_usd: 2.0,
      });
    });

    expect(result.current.tasks.get("t1")?.summary).toBe("existing summary");
  });
});
