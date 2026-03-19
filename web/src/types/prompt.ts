export type PromptInputType = "text" | "multiline" | "select";
export type PromptExecMode = "paste_to_terminal" | "claude_session";

export interface PromptInput {
  name: string;
  label?: string;
  input_type?: PromptInputType;
  placeholder?: string;
  default?: string;
  required?: boolean;
  options?: string[];
}

export type PromptBody = string | { file: string };

export interface PromptTemplate {
  name: string;
  description?: string;
  icon?: string;
  body: PromptBody;
  inputs: PromptInput[];
  default_mode?: PromptExecMode;
  model?: string;
  allowed_tools?: string[];
  skip_permissions?: boolean;
}

export interface ResolvePromptRequest {
  inputs: Record<string, string>;
  worktree_path?: string;
  branch?: string;
}

export interface ResolvePromptResponse {
  prompt: string;
}
