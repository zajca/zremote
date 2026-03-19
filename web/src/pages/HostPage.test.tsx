import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";
import { HostPage } from "./HostPage";

vi.mock("../hooks/useHosts", () => ({
  useHosts: () => ({
    hosts: [
      {
        id: "host-1",
        hostname: "test-server",
        status: "online",
        agent_version: "0.1.0",
        os: "linux",
        arch: "x86_64",
        last_seen: new Date().toISOString(),
        created_at: new Date().toISOString(),
      },
    ],
    loading: false,
    error: null,
  }),
}));

vi.mock("../hooks/useSessions", () => ({
  useSessions: () => ({
    sessions: [
      {
        id: "sess-1",
        host_id: "host-1",
        name: "dev",
        shell: "/bin/zsh",
        status: "active",
        pid: 1234,
        working_dir: "/home",
        project_id: null,
        created_at: new Date().toISOString(),
        last_activity: new Date().toISOString(),
      },
    ],
    loading: false,
    error: null,
  }),
}));

function renderHostPage() {
  return render(
    <MemoryRouter initialEntries={["/hosts/host-1"]}>
      <Routes>
        <Route path="/hosts/:hostId" element={<HostPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe("HostPage", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  test("renders host hostname", () => {
    renderHostPage();
    expect(screen.getByText("test-server")).toBeInTheDocument();
  });

  test("renders OS and arch info", () => {
    renderHostPage();
    expect(screen.getByText("linux/x86_64")).toBeInTheDocument();
  });

  test("renders New Session button", () => {
    renderHostPage();
    expect(screen.getByText("New Session")).toBeInTheDocument();
  });

  test("renders session list", () => {
    renderHostPage();
    expect(screen.getByText("dev")).toBeInTheDocument();
    expect(screen.getByText("active")).toBeInTheDocument();
  });

  test("shows Host not found for unknown host", () => {
    render(
      <MemoryRouter initialEntries={["/hosts/unknown"]}>
        <Routes>
          <Route path="/hosts/:hostId" element={<HostPage />} />
        </Routes>
      </MemoryRouter>,
    );
    expect(screen.getByText("Host not found")).toBeInTheDocument();
  });
});
