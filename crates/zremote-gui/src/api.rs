use crate::types::{
    CreateSessionRequest, CreateSessionResponse, Host, LoopInfoLite, ModeResponse, Project,
    Session, UpdateProjectRequest,
};

/// HTTP client for the `ZRemote` REST API.
#[derive(Clone)]
pub struct ApiClient {
    base_url: String,
    client: reqwest::Client,
}

impl ApiClient {
    pub fn new(base_url: &str) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            base_url,
            client: reqwest::Client::new(),
        }
    }

    /// Detect server mode ("server" or "local").
    pub async fn get_mode(&self) -> Result<String, ApiError> {
        let resp: ModeResponse = self
            .client
            .get(format!("{}/api/mode", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.mode)
    }

    /// List all hosts.
    pub async fn list_hosts(&self) -> Result<Vec<Host>, ApiError> {
        let hosts: Vec<Host> = self
            .client
            .get(format!("{}/api/hosts", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(hosts)
    }

    /// List sessions for a host.
    pub async fn list_sessions(&self, host_id: &str) -> Result<Vec<Session>, ApiError> {
        let sessions: Vec<Session> = self
            .client
            .get(format!("{}/api/hosts/{host_id}/sessions", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(sessions)
    }

    /// Create a new terminal session.
    pub async fn create_session(
        &self,
        host_id: &str,
        req: &CreateSessionRequest,
    ) -> Result<CreateSessionResponse, ApiError> {
        let resp: CreateSessionResponse = self
            .client
            .post(format!("{}/api/hosts/{host_id}/sessions", self.base_url))
            .json(req)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    /// List projects for a host.
    pub async fn list_projects(&self, host_id: &str) -> Result<Vec<Project>, ApiError> {
        let projects: Vec<Project> = self
            .client
            .get(format!("{}/api/hosts/{host_id}/projects", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(projects)
    }

    /// Update a project (e.g. pin/unpin).
    pub async fn update_project(
        &self,
        project_id: &str,
        req: &UpdateProjectRequest,
    ) -> Result<Project, ApiError> {
        let project: Project = self
            .client
            .patch(format!("{}/api/projects/{project_id}", self.base_url))
            .json(req)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(project)
    }

    /// Fetch currently active (working or waiting_for_input) agentic loops.
    /// Returns an empty vec on any error (best-effort reconciliation).
    pub async fn get_active_loops(&self) -> Result<Vec<LoopInfoLite>, ApiError> {
        let loops: Vec<LoopInfoLite> = self
            .client
            .get(format!("{}/api/loops", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(loops
            .into_iter()
            .filter(|l| l.status == "working" || l.status == "waiting_for_input")
            .collect())
    }

    /// Close (delete) a session.
    pub async fn close_session(&self, session_id: &str) -> Result<(), ApiError> {
        self.client
            .delete(format!("{}/api/sessions/{session_id}", self.base_url))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Get the WebSocket URL for events.
    pub fn events_ws_url(&self) -> String {
        let ws_base = self
            .base_url
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        format!("{ws_base}/ws/events")
    }

    /// Get the WebSocket URL for a terminal session.
    pub fn terminal_ws_url(&self, session_id: &str) -> String {
        let ws_base = self
            .base_url
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        format!("{ws_base}/ws/terminal/{session_id}")
    }
}

#[derive(Debug)]
pub enum ApiError {
    Http(reqwest::Error),
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "{e}"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<reqwest::Error> for ApiError {
    fn from(err: reqwest::Error) -> Self {
        Self::Http(err)
    }
}
