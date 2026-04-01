use std::path::Path;

use tracing::warn;
use zremote_protocol::project::{ArchitecturePattern, Convention, ConventionKind};

/// Maximum bytes to read from any single manifest file.
const MAX_READ_BYTES: usize = 64 * 1024;

/// Result of deep-scanning a project directory for framework/architecture/convention data.
#[derive(Debug, Default)]
pub struct ProjectIntelligence {
    pub frameworks: Vec<String>,
    pub architecture: Option<ArchitecturePattern>,
    pub conventions: Vec<Convention>,
    pub package_manager: Option<String>,
}

/// Analyze a project directory. Reads marker files that are known to exist.
/// Called after basic detection confirms the project type.
pub fn analyze(dir: &Path, project_type: &str) -> ProjectIntelligence {
    ProjectIntelligence {
        frameworks: detect_frameworks(dir, project_type),
        architecture: detect_architecture(dir, project_type),
        conventions: detect_conventions(dir),
        package_manager: detect_package_manager(dir, project_type),
    }
}

/// Read a file's contents, bounded to `MAX_READ_BYTES`. Returns `None` on any error.
fn read_bounded(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_READ_BYTES as u64 {
        warn!(
            "skipping oversized file: {} ({} bytes)",
            path.display(),
            metadata.len()
        );
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Detect frameworks from marker file contents.
fn detect_frameworks(dir: &Path, project_type: &str) -> Vec<String> {
    match project_type {
        "node" => detect_node_frameworks(dir),
        "rust" => detect_rust_frameworks(dir),
        "python" => detect_python_frameworks(dir),
        "go" => detect_go_frameworks(dir),
        "php" => detect_php_frameworks(dir),
        _ => vec![],
    }
}

fn detect_node_frameworks(dir: &Path) -> Vec<String> {
    let path = dir.join("package.json");
    let Some(content) = read_bounded(&path) else {
        return vec![];
    };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            warn!("malformed package.json at {}: {e}", path.display());
            return vec![];
        }
    };

    let mappings: &[(&str, &str)] = &[
        ("next", "Next.js"),
        ("react", "React"),
        ("vue", "Vue"),
        ("svelte", "Svelte"),
        ("@angular/core", "Angular"),
        ("express", "Express"),
        ("@nestjs/core", "NestJS"),
        ("nuxt", "Nuxt"),
    ];

    let mut frameworks = Vec::new();
    for section in ["dependencies", "devDependencies"] {
        if let Some(deps) = parsed.get(section).and_then(|v| v.as_object()) {
            for &(dep, framework) in mappings {
                if deps.contains_key(dep) && !frameworks.contains(&framework.to_string()) {
                    frameworks.push(framework.to_string());
                }
            }
        }
    }
    frameworks
}

fn detect_rust_frameworks(dir: &Path) -> Vec<String> {
    let path = dir.join("Cargo.toml");
    let Some(content) = read_bounded(&path) else {
        return vec![];
    };

    let mappings: &[(&str, &str)] = &[
        ("axum", "Axum"),
        ("actix-web", "Actix"),
        ("gpui", "GPUI"),
        ("rocket", "Rocket"),
        ("warp", "Warp"),
        ("tauri", "Tauri"),
        ("bevy", "Bevy"),
        ("leptos", "Leptos"),
    ];

    // Line-by-line scan: after [dependencies] or [workspace.dependencies], look for dep = or dep.workspace
    let mut in_deps = false;
    let mut frameworks = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = trimmed == "[dependencies]" || trimmed == "[workspace.dependencies]";
            continue;
        }
        if !in_deps {
            continue;
        }
        for &(dep, framework) in mappings {
            // Match patterns: `dep = `, `dep.workspace`, `dep = {`
            if trimmed.starts_with(dep)
                && trimmed
                    .get(dep.len()..dep.len() + 1)
                    .is_some_and(|c| c == " " || c == "=" || c == ".")
                && !frameworks.contains(&framework.to_string())
            {
                frameworks.push(framework.to_string());
            }
        }
    }
    frameworks
}

fn detect_python_frameworks(dir: &Path) -> Vec<String> {
    let mappings: &[(&str, &str)] = &[
        ("django", "Django"),
        ("fastapi", "FastAPI"),
        ("flask", "Flask"),
        ("starlette", "Starlette"),
        ("sqlalchemy", "SQLAlchemy"),
    ];

    // Try pyproject.toml first
    let pyproject_path = dir.join("pyproject.toml");
    if let Some(content) = read_bounded(&pyproject_path) {
        let frameworks = extract_python_frameworks_from_toml(&content, mappings);
        if !frameworks.is_empty() {
            return frameworks;
        }
    }

    // Fallback to requirements.txt
    let req_path = dir.join("requirements.txt");
    if let Some(content) = read_bounded(&req_path) {
        return extract_python_frameworks_from_requirements(&content, mappings);
    }

    vec![]
}

fn extract_python_frameworks_from_toml(content: &str, mappings: &[(&str, &str)]) -> Vec<String> {
    // Scan for dependency names after [project.dependencies] or [tool.poetry.dependencies]
    let mut in_deps = false;
    let mut frameworks = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps =
                trimmed == "[project.dependencies]" || trimmed == "[tool.poetry.dependencies]";
            continue;
        }
        if !in_deps {
            continue;
        }
        // Check for `dependencies = [` style (inline list under [project])
        // or `dep = "version"` style under [tool.poetry.dependencies]
        for &(dep, framework) in mappings {
            let lower = trimmed.to_lowercase();
            if lower.contains(dep) && !frameworks.contains(&framework.to_string()) {
                frameworks.push(framework.to_string());
            }
        }
    }
    frameworks
}

fn extract_python_frameworks_from_requirements(
    content: &str,
    mappings: &[(&str, &str)],
) -> Vec<String> {
    let mut frameworks = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Strip version specifiers and extras
        let pkg_name = trimmed
            .split(&['=', '>', '<', '~', '!', '[', ';'][..])
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();

        for &(dep, framework) in mappings {
            if pkg_name == dep && !frameworks.contains(&framework.to_string()) {
                frameworks.push(framework.to_string());
            }
        }
    }
    frameworks
}

fn detect_go_frameworks(dir: &Path) -> Vec<String> {
    let path = dir.join("go.mod");
    let Some(content) = read_bounded(&path) else {
        return vec![];
    };

    let mappings: &[(&str, &str)] = &[
        ("github.com/gin-gonic/gin", "Gin"),
        ("github.com/gofiber/fiber", "Fiber"),
        ("github.com/labstack/echo", "Echo"),
        ("github.com/gorilla/mux", "Gorilla"),
    ];

    let mut frameworks = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        for &(module, framework) in mappings {
            if trimmed.starts_with(module) && !frameworks.contains(&framework.to_string()) {
                frameworks.push(framework.to_string());
            }
        }
    }
    frameworks
}

fn detect_php_frameworks(dir: &Path) -> Vec<String> {
    let path = dir.join("composer.json");
    let Some(content) = read_bounded(&path) else {
        return vec![];
    };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            warn!("malformed composer.json at {}: {e}", path.display());
            return vec![];
        }
    };

    let mappings: &[(&str, &str)] = &[("laravel/framework", "Laravel"), ("slim/slim", "Slim")];

    let mut frameworks = Vec::new();
    if let Some(deps) = parsed.get("require").and_then(|v| v.as_object()) {
        for &(dep, framework) in mappings {
            if deps.contains_key(dep) && !frameworks.contains(&framework.to_string()) {
                frameworks.push(framework.to_string());
            }
        }
        // Symfony: any key starting with "symfony/"
        for key in deps.keys() {
            if key.starts_with("symfony/") && !frameworks.contains(&"Symfony".to_string()) {
                frameworks.push("Symfony".to_string());
            }
        }
    }
    frameworks
}

/// Detect architecture pattern from directory structure and config files.
fn detect_architecture(dir: &Path, project_type: &str) -> Option<ArchitecturePattern> {
    // Check monorepo patterns first (order matters: first match wins)
    if dir.join("pnpm-workspace.yaml").exists() {
        return Some(ArchitecturePattern::MonorepoPnpm);
    }
    if dir.join("lerna.json").exists() {
        return Some(ArchitecturePattern::MonorepoLerna);
    }
    if dir.join("nx.json").exists() {
        return Some(ArchitecturePattern::MonorepoNx);
    }
    if dir.join("turbo.json").exists() {
        return Some(ArchitecturePattern::MonorepoTurborepo);
    }
    if project_type == "rust"
        && let Some(content) = read_bounded(&dir.join("Cargo.toml"))
        && is_cargo_workspace_with_members(&content, 3)
    {
        return Some(ArchitecturePattern::MonorepoCargo);
    }

    // MVC pattern: at least 2 of controllers/, models/, views/
    let mvc_dirs = ["controllers", "models", "views"];
    let mvc_count = mvc_dirs.iter().filter(|d| dir.join(d).is_dir()).count();
    if mvc_count >= 2 {
        return Some(ArchitecturePattern::Mvc);
    }

    // Microservices: docker-compose with >3 services + subdirs with own manifests
    if is_microservices(dir) {
        return Some(ArchitecturePattern::Microservices);
    }

    None
}

/// Check if Cargo.toml has a [workspace] section with more than `min_members` members.
fn is_cargo_workspace_with_members(content: &str, min_members: usize) -> bool {
    let mut in_workspace = false;
    let mut in_members = false;
    let mut member_count = 0;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if trimmed == "[workspace]" {
                in_workspace = true;
                in_members = false;
            } else if in_workspace && !trimmed.starts_with("[workspace.") {
                in_workspace = false;
                in_members = false;
            }
            continue;
        }
        if !in_workspace {
            continue;
        }
        if trimmed.starts_with("members") && trimmed.contains('=') {
            in_members = true;
            // Count entries on same line: members = ["a", "b"]
            member_count += trimmed.matches('"').count() / 2;
            continue;
        }
        if in_members {
            if trimmed.starts_with(']') {
                in_members = false;
                continue;
            }
            if trimmed.starts_with('"') || trimmed.starts_with('\'') {
                member_count += 1;
            }
        }
    }

    member_count > min_members
}

/// Check for microservices pattern: docker-compose with >3 services AND subdirectories
/// with their own manifests.
fn is_microservices(dir: &Path) -> bool {
    let compose_path = if dir.join("docker-compose.yml").exists() {
        dir.join("docker-compose.yml")
    } else if dir.join("docker-compose.yaml").exists() {
        dir.join("docker-compose.yaml")
    } else {
        return false;
    };

    let Some(content) = read_bounded(&compose_path) else {
        return false;
    };

    // Count top-level services by looking for lines under `services:` that are
    // non-indented service names (2-space indented keys)
    let service_count = count_docker_compose_services(&content);
    if service_count <= 3 {
        return false;
    }

    // Check for subdirectories with their own manifests
    let manifests = [
        "package.json",
        "Cargo.toml",
        "pyproject.toml",
        "go.mod",
        "composer.json",
    ];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let subdir_with_manifest_count = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter(|e| manifests.iter().any(|m| e.path().join(m).exists()))
        .count();

    subdir_with_manifest_count > 1
}

/// Count services in a docker-compose file by looking at the YAML structure.
fn count_docker_compose_services(content: &str) -> usize {
    let mut in_services = false;
    let mut count = 0;
    for line in content.lines() {
        if line.starts_with("services:") {
            in_services = true;
            continue;
        }
        if in_services {
            // A top-level key under services is indented exactly 2 spaces and ends with ':'
            if !line.starts_with(' ') && !line.is_empty() {
                // We've left the services block
                break;
            }
            if line.starts_with("  ") && !line.starts_with("    ") {
                let trimmed = line.trim();
                if trimmed.ends_with(':') && !trimmed.starts_with('#') {
                    count += 1;
                }
            }
        }
    }
    count
}

/// Detect conventions (linter, formatter, CI, testing).
fn detect_conventions(dir: &Path) -> Vec<Convention> {
    let mut conventions = Vec::new();

    // ESLint
    for pattern in &[
        ".eslintrc",
        ".eslintrc.json",
        ".eslintrc.js",
        ".eslintrc.yml",
        ".eslintrc.yaml",
        ".eslintrc.cjs",
    ] {
        if dir.join(pattern).exists() {
            conventions.push(Convention {
                kind: ConventionKind::Linter,
                name: "eslint".to_string(),
                config_file: Some((*pattern).to_string()),
            });
            break;
        }
    }
    if conventions.iter().all(|c| c.name != "eslint") {
        for pattern in &[
            "eslint.config.js",
            "eslint.config.mjs",
            "eslint.config.cjs",
            "eslint.config.ts",
        ] {
            if dir.join(pattern).exists() {
                conventions.push(Convention {
                    kind: ConventionKind::Linter,
                    name: "eslint".to_string(),
                    config_file: Some((*pattern).to_string()),
                });
                break;
            }
        }
    }

    // Prettier
    for pattern in &[
        ".prettierrc",
        ".prettierrc.json",
        ".prettierrc.js",
        ".prettierrc.yml",
        ".prettierrc.yaml",
        ".prettierrc.cjs",
    ] {
        if dir.join(pattern).exists() {
            conventions.push(Convention {
                kind: ConventionKind::Formatter,
                name: "prettier".to_string(),
                config_file: Some((*pattern).to_string()),
            });
            break;
        }
    }
    if conventions.iter().all(|c| c.name != "prettier") {
        for pattern in &[
            "prettier.config.js",
            "prettier.config.mjs",
            "prettier.config.cjs",
        ] {
            if dir.join(pattern).exists() {
                conventions.push(Convention {
                    kind: ConventionKind::Formatter,
                    name: "prettier".to_string(),
                    config_file: Some((*pattern).to_string()),
                });
                break;
            }
        }
    }

    // Clippy
    if dir.join("clippy.toml").exists() {
        conventions.push(Convention {
            kind: ConventionKind::Linter,
            name: "clippy".to_string(),
            config_file: Some("clippy.toml".to_string()),
        });
    } else if dir.join(".clippy.toml").exists() {
        conventions.push(Convention {
            kind: ConventionKind::Linter,
            name: "clippy".to_string(),
            config_file: Some(".clippy.toml".to_string()),
        });
    } else if let Some(content) = read_bounded(&dir.join("Cargo.toml"))
        && content.contains("[lints.clippy]")
    {
        conventions.push(Convention {
            kind: ConventionKind::Linter,
            name: "clippy".to_string(),
            config_file: Some("Cargo.toml".to_string()),
        });
    }

    // Rustfmt
    if dir.join("rustfmt.toml").exists() {
        conventions.push(Convention {
            kind: ConventionKind::Formatter,
            name: "rustfmt".to_string(),
            config_file: Some("rustfmt.toml".to_string()),
        });
    } else if dir.join(".rustfmt.toml").exists() {
        conventions.push(Convention {
            kind: ConventionKind::Formatter,
            name: "rustfmt".to_string(),
            config_file: Some(".rustfmt.toml".to_string()),
        });
    }

    // Ruff
    if dir.join("ruff.toml").exists() {
        conventions.push(Convention {
            kind: ConventionKind::Linter,
            name: "ruff".to_string(),
            config_file: Some("ruff.toml".to_string()),
        });
    } else if let Some(content) = read_bounded(&dir.join("pyproject.toml"))
        && content.contains("[tool.ruff]")
    {
        conventions.push(Convention {
            kind: ConventionKind::Linter,
            name: "ruff".to_string(),
            config_file: Some("pyproject.toml".to_string()),
        });
    }

    // Black
    if let Some(content) = read_bounded(&dir.join("pyproject.toml"))
        && content.contains("[tool.black]")
    {
        conventions.push(Convention {
            kind: ConventionKind::Formatter,
            name: "black".to_string(),
            config_file: Some("pyproject.toml".to_string()),
        });
    }

    // Biome
    if dir.join("biome.json").exists() {
        conventions.push(Convention {
            kind: ConventionKind::Linter,
            name: "biome".to_string(),
            config_file: Some("biome.json".to_string()),
        });
    } else if dir.join("biome.jsonc").exists() {
        conventions.push(Convention {
            kind: ConventionKind::Linter,
            name: "biome".to_string(),
            config_file: Some("biome.jsonc".to_string()),
        });
    }

    // GitHub Actions
    if dir.join(".github").join("workflows").is_dir() {
        conventions.push(Convention {
            kind: ConventionKind::BuildTool,
            name: "github_actions".to_string(),
            config_file: None,
        });
    }

    // Docker
    if dir.join("Dockerfile").exists() {
        conventions.push(Convention {
            kind: ConventionKind::BuildTool,
            name: "docker".to_string(),
            config_file: Some("Dockerfile".to_string()),
        });
    } else if dir.join("docker-compose.yml").exists() {
        conventions.push(Convention {
            kind: ConventionKind::BuildTool,
            name: "docker".to_string(),
            config_file: Some("docker-compose.yml".to_string()),
        });
    } else if dir.join("docker-compose.yaml").exists() {
        conventions.push(Convention {
            kind: ConventionKind::BuildTool,
            name: "docker".to_string(),
            config_file: Some("docker-compose.yaml".to_string()),
        });
    }

    // GitLab CI
    if dir.join(".gitlab-ci.yml").exists() {
        conventions.push(Convention {
            kind: ConventionKind::BuildTool,
            name: "ci_gitlab".to_string(),
            config_file: Some(".gitlab-ci.yml".to_string()),
        });
    }

    // Editorconfig
    if dir.join(".editorconfig").exists() {
        conventions.push(Convention {
            kind: ConventionKind::Formatter,
            name: "editorconfig".to_string(),
            config_file: Some(".editorconfig".to_string()),
        });
    }

    // Pre-commit
    if dir.join(".pre-commit-config.yaml").exists() {
        conventions.push(Convention {
            kind: ConventionKind::Linter,
            name: "pre_commit".to_string(),
            config_file: Some(".pre-commit-config.yaml".to_string()),
        });
    }

    // Husky
    if dir.join(".husky").is_dir() {
        conventions.push(Convention {
            kind: ConventionKind::Linter,
            name: "husky".to_string(),
            config_file: None,
        });
    }

    // TypeScript
    if dir.join("tsconfig.json").exists() {
        conventions.push(Convention {
            kind: ConventionKind::BuildTool,
            name: "typescript".to_string(),
            config_file: Some("tsconfig.json".to_string()),
        });
    }

    // Sort by name for deterministic output
    conventions.sort_by(|a, b| a.name.cmp(&b.name));
    conventions.dedup_by(|a, b| a.name == b.name);
    conventions
}

/// Detect package manager from lock files and config.
fn detect_package_manager(dir: &Path, project_type: &str) -> Option<String> {
    match project_type {
        "node" => {
            if dir.join("pnpm-lock.yaml").exists() {
                Some("pnpm".to_string())
            } else if dir.join("yarn.lock").exists() {
                Some("yarn".to_string())
            } else if dir.join("bun.lockb").exists() {
                Some("bun".to_string())
            } else if dir.join("package-lock.json").exists() {
                Some("npm".to_string())
            } else {
                None
            }
        }
        "python" => {
            if dir.join("uv.lock").exists() {
                Some("uv".to_string())
            } else if dir.join("poetry.lock").exists() {
                Some("poetry".to_string())
            } else if dir.join("Pipfile.lock").exists() {
                Some("pipenv".to_string())
            } else if dir.join("requirements.txt").exists() {
                Some("pip".to_string())
            } else {
                None
            }
        }
        "rust" => Some("cargo".to_string()),
        "go" => Some("go".to_string()),
        "php" => Some("composer".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // === Framework Detection ===

    #[test]
    fn detect_nextjs_from_package_json() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"dependencies": {"next": "14.0.0", "react": "18.0.0"}}"#,
        )
        .unwrap();
        let frameworks = detect_frameworks(tmp.path(), "node");
        assert!(frameworks.contains(&"Next.js".to_string()));
    }

    #[test]
    fn detect_react_from_package_json() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"dependencies": {"react": "18.0.0"}}"#,
        )
        .unwrap();
        let frameworks = detect_frameworks(tmp.path(), "node");
        assert!(frameworks.contains(&"React".to_string()));
    }

    #[test]
    fn detect_vue_from_package_json() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"dependencies": {"vue": "3.0.0"}}"#,
        )
        .unwrap();
        let frameworks = detect_frameworks(tmp.path(), "node");
        assert!(frameworks.contains(&"Vue".to_string()));
    }

    #[test]
    fn detect_angular_from_package_json() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"dependencies": {"@angular/core": "17.0.0"}}"#,
        )
        .unwrap();
        let frameworks = detect_frameworks(tmp.path(), "node");
        assert!(frameworks.contains(&"Angular".to_string()));
    }

    #[test]
    fn detect_express_from_package_json() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"dependencies": {"express": "4.18.0"}}"#,
        )
        .unwrap();
        let frameworks = detect_frameworks(tmp.path(), "node");
        assert!(frameworks.contains(&"Express".to_string()));
    }

    #[test]
    fn detect_axum_from_cargo_toml() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"myapp\"\n\n[dependencies]\naxum = \"0.8\"\ntokio = \"1\"\n",
        )
        .unwrap();
        let frameworks = detect_frameworks(tmp.path(), "rust");
        assert!(frameworks.contains(&"Axum".to_string()));
    }

    #[test]
    fn detect_gpui_from_cargo_toml() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"myapp\"\n\n[dependencies]\ngpui = { path = \"../gpui\" }\n",
        )
        .unwrap();
        let frameworks = detect_frameworks(tmp.path(), "rust");
        assert!(frameworks.contains(&"GPUI".to_string()));
    }

    #[test]
    fn detect_multiple_rust_frameworks() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"myapp\"\n\n[dependencies]\naxum = \"0.8\"\ngpui = { path = \"../gpui\" }\n",
        ).unwrap();
        let frameworks = detect_frameworks(tmp.path(), "rust");
        assert!(frameworks.contains(&"Axum".to_string()));
        assert!(frameworks.contains(&"GPUI".to_string()));
    }

    #[test]
    fn detect_django_from_pyproject() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            "[project]\nname = \"myapp\"\n\n[project.dependencies]\ndjango = \">=4.0\"\n",
        )
        .unwrap();
        let frameworks = detect_frameworks(tmp.path(), "python");
        assert!(frameworks.contains(&"Django".to_string()));
    }

    #[test]
    fn detect_fastapi_from_pyproject() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            "[project]\nname = \"api\"\n\n[project.dependencies]\nfastapi = \">=0.100\"\n",
        )
        .unwrap();
        let frameworks = detect_frameworks(tmp.path(), "python");
        assert!(frameworks.contains(&"FastAPI".to_string()));
    }

    #[test]
    fn detect_gin_from_go_mod() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("go.mod"),
            "module example.com/myapp\n\ngo 1.21\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.9.0\n)\n",
        ).unwrap();
        let frameworks = detect_frameworks(tmp.path(), "go");
        assert!(frameworks.contains(&"Gin".to_string()));
    }

    #[test]
    fn detect_laravel_from_composer() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("composer.json"),
            r#"{"require": {"php": "^8.1", "laravel/framework": "^10.0"}}"#,
        )
        .unwrap();
        let frameworks = detect_frameworks(tmp.path(), "php");
        assert!(frameworks.contains(&"Laravel".to_string()));
    }

    #[test]
    fn detect_symfony_from_composer() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("composer.json"),
            r#"{"require": {"php": "^8.1", "symfony/framework-bundle": "^6.0"}}"#,
        )
        .unwrap();
        let frameworks = detect_frameworks(tmp.path(), "php");
        assert!(frameworks.contains(&"Symfony".to_string()));
    }

    #[test]
    fn no_frameworks_for_empty_deps() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("package.json"), r#"{"dependencies": {}}"#).unwrap();
        let frameworks = detect_frameworks(tmp.path(), "node");
        assert!(frameworks.is_empty());
    }

    #[test]
    fn malformed_json_returns_empty() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("package.json"), "not valid json{{{").unwrap();
        let frameworks = detect_frameworks(tmp.path(), "node");
        assert!(frameworks.is_empty());
    }

    // === Architecture Detection ===

    #[test]
    fn detect_monorepo_pnpm() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("pnpm-workspace.yaml"),
            "packages:\n  - 'packages/*'\n",
        )
        .unwrap();
        let arch = detect_architecture(tmp.path(), "node");
        assert_eq!(arch, Some(ArchitecturePattern::MonorepoPnpm));
    }

    #[test]
    fn detect_monorepo_cargo() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\n  \"crate-a\",\n  \"crate-b\",\n  \"crate-c\",\n  \"crate-d\",\n]\n",
        ).unwrap();
        let arch = detect_architecture(tmp.path(), "rust");
        assert_eq!(arch, Some(ArchitecturePattern::MonorepoCargo));
    }

    #[test]
    fn detect_mvc_pattern() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("controllers")).unwrap();
        fs::create_dir_all(tmp.path().join("models")).unwrap();
        fs::create_dir_all(tmp.path().join("views")).unwrap();
        let arch = detect_architecture(tmp.path(), "node");
        assert_eq!(arch, Some(ArchitecturePattern::Mvc));
    }

    #[test]
    fn detect_microservices() {
        let tmp = TempDir::new().unwrap();
        // docker-compose with >3 services
        fs::write(
            tmp.path().join("docker-compose.yml"),
            "services:\n  api:\n    image: api\n  web:\n    image: web\n  db:\n    image: postgres\n  cache:\n    image: redis\n",
        ).unwrap();
        // Subdirectories with manifests
        fs::create_dir_all(tmp.path().join("api")).unwrap();
        fs::write(tmp.path().join("api").join("package.json"), "{}").unwrap();
        fs::create_dir_all(tmp.path().join("web")).unwrap();
        fs::write(tmp.path().join("web").join("package.json"), "{}").unwrap();
        let arch = detect_architecture(tmp.path(), "node");
        assert_eq!(arch, Some(ArchitecturePattern::Microservices));
    }

    #[test]
    fn detect_no_architecture() {
        let tmp = TempDir::new().unwrap();
        let arch = detect_architecture(tmp.path(), "node");
        assert!(arch.is_none());
    }

    // === Convention Detection ===

    #[test]
    fn detect_eslint_convention() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".eslintrc.json"), "{}").unwrap();
        let conventions = detect_conventions(tmp.path());
        assert!(conventions.iter().any(|c| c.name == "eslint"));
    }

    #[test]
    fn detect_prettier_convention() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".prettierrc"), "{}").unwrap();
        let conventions = detect_conventions(tmp.path());
        assert!(conventions.iter().any(|c| c.name == "prettier"));
    }

    #[test]
    fn detect_clippy_convention() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("clippy.toml"), "").unwrap();
        let conventions = detect_conventions(tmp.path());
        assert!(conventions.iter().any(|c| c.name == "clippy"));
    }

    #[test]
    fn detect_github_actions() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".github").join("workflows")).unwrap();
        let conventions = detect_conventions(tmp.path());
        assert!(conventions.iter().any(|c| c.name == "github_actions"));
    }

    #[test]
    fn detect_typescript() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("tsconfig.json"), "{}").unwrap();
        let conventions = detect_conventions(tmp.path());
        assert!(conventions.iter().any(|c| c.name == "typescript"));
    }

    #[test]
    fn detect_biome_convention() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("biome.json"), "{}").unwrap();
        let conventions = detect_conventions(tmp.path());
        assert!(
            conventions
                .iter()
                .any(|c| c.name == "biome" && c.kind == ConventionKind::Linter)
        );
        assert_eq!(
            conventions
                .iter()
                .find(|c| c.name == "biome")
                .unwrap()
                .config_file,
            Some("biome.json".to_string())
        );
    }

    #[test]
    fn detect_biome_jsonc_convention() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("biome.jsonc"), "{}").unwrap();
        let conventions = detect_conventions(tmp.path());
        assert!(
            conventions
                .iter()
                .any(|c| c.name == "biome" && c.kind == ConventionKind::Linter)
        );
        assert_eq!(
            conventions
                .iter()
                .find(|c| c.name == "biome")
                .unwrap()
                .config_file,
            Some("biome.jsonc".to_string())
        );
    }

    #[test]
    fn detect_multiple_conventions() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".eslintrc.json"), "{}").unwrap();
        fs::write(tmp.path().join(".prettierrc"), "{}").unwrap();
        fs::write(tmp.path().join("tsconfig.json"), "{}").unwrap();
        let conventions = detect_conventions(tmp.path());
        assert!(conventions.len() >= 3);
        // Verify sorted by name
        for i in 1..conventions.len() {
            assert!(conventions[i - 1].name <= conventions[i].name);
        }
    }

    // === Package Manager Detection ===

    #[test]
    fn detect_pnpm_package_manager() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(
            detect_package_manager(tmp.path(), "node"),
            Some("pnpm".to_string())
        );
    }

    #[test]
    fn detect_yarn_package_manager() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("yarn.lock"), "").unwrap();
        assert_eq!(
            detect_package_manager(tmp.path(), "node"),
            Some("yarn".to_string())
        );
    }

    #[test]
    fn detect_npm_package_manager() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("package-lock.json"), "{}").unwrap();
        assert_eq!(
            detect_package_manager(tmp.path(), "node"),
            Some("npm".to_string())
        );
    }

    #[test]
    fn detect_uv_package_manager() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("uv.lock"), "").unwrap();
        assert_eq!(
            detect_package_manager(tmp.path(), "python"),
            Some("uv".to_string())
        );
    }

    #[test]
    fn detect_poetry_package_manager() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("poetry.lock"), "").unwrap();
        assert_eq!(
            detect_package_manager(tmp.path(), "python"),
            Some("poetry".to_string())
        );
    }

    #[test]
    fn detect_pip_package_manager() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("requirements.txt"), "flask==2.0").unwrap();
        assert_eq!(
            detect_package_manager(tmp.path(), "python"),
            Some("pip".to_string())
        );
    }

    #[test]
    fn detect_cargo_package_manager() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            detect_package_manager(tmp.path(), "rust"),
            Some("cargo".to_string())
        );
    }

    #[test]
    fn detect_go_package_manager() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            detect_package_manager(tmp.path(), "go"),
            Some("go".to_string())
        );
    }

    // === Full Analyze Integration ===

    #[test]
    fn analyze_rust_project() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"myapp\"\n\n[dependencies]\naxum = \"0.8\"\n\n[lints.clippy]\nall = \"deny\"\n",
        ).unwrap();
        fs::write(tmp.path().join("rustfmt.toml"), "").unwrap();

        let intel = analyze(tmp.path(), "rust");
        assert!(intel.frameworks.contains(&"Axum".to_string()));
        assert_eq!(intel.package_manager, Some("cargo".to_string()));
        assert!(intel.conventions.iter().any(|c| c.name == "clippy"));
        assert!(intel.conventions.iter().any(|c| c.name == "rustfmt"));
    }

    #[test]
    fn analyze_node_project() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"dependencies": {"next": "14.0.0", "react": "18.0.0"}}"#,
        )
        .unwrap();
        fs::write(tmp.path().join("pnpm-lock.yaml"), "").unwrap();
        fs::write(tmp.path().join("tsconfig.json"), "{}").unwrap();

        let intel = analyze(tmp.path(), "node");
        assert!(intel.frameworks.contains(&"Next.js".to_string()));
        assert!(intel.frameworks.contains(&"React".to_string()));
        assert_eq!(intel.package_manager, Some("pnpm".to_string()));
        assert!(intel.conventions.iter().any(|c| c.name == "typescript"));
    }
}
