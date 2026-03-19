export interface LinearUser {
  id: string;
  name: string;
  email: string;
  displayName: string;
}

export interface LinearIssue {
  id: string;
  identifier: string;
  title: string;
  description: string | null;
  priority: number;
  priorityLabel: string;
  state: LinearState;
  assignee: LinearUser | null;
  labels: { nodes: LinearLabel[] };
  createdAt: string;
  updatedAt: string;
  cycle: LinearCycle | null;
  url: string;
}

export interface LinearState {
  id: string;
  name: string;
  type: string;
  color: string;
}

export interface LinearLabel {
  id: string;
  name: string;
  color: string;
}

export interface LinearCycle {
  id: string;
  name: string | null;
  number: number;
  startsAt: string;
  endsAt: string;
}

export interface LinearTeam {
  id: string;
  name: string;
  key: string;
}

export interface LinearProject {
  id: string;
  name: string;
  state: string;
}

export interface LinearSettings {
  token_env_var: string;
  team_key: string;
  project_id?: string;
  my_email?: string;
  actions: LinearAction[];
}

export interface LinearAction {
  name: string;
  icon?: string;
  prompt: string;
}

export type IssuePreset = "my_issues" | "current_sprint" | "backlog";
