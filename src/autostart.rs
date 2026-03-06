use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use muninn::AppConfig;

const LAUNCH_AGENT_LABEL: &str = "com.bnomei.muninn";
const LAUNCH_AGENT_FILE_NAME: &str = "com.bnomei.muninn.plist";
const DEFAULT_LAUNCH_AGENT_PATH: &str =
    "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutostartSyncStatus {
    Enabled {
        plist_path: PathBuf,
        launch_path: PathBuf,
        changed: bool,
    },
    Disabled {
        plist_path: PathBuf,
        removed: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchAgentSpec {
    launch_path: PathBuf,
    config_path: PathBuf,
    working_directory: PathBuf,
    load_dotenv: bool,
}

pub fn sync_autostart(config_path: &Path, config: &AppConfig) -> Result<AutostartSyncStatus> {
    let home_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("resolving HOME for macOS autostart")?;
    let plist_path = launch_agent_path(&home_dir);

    if !config.app.autostart {
        let removed = remove_launch_agent_file(&plist_path)?;
        return Ok(AutostartSyncStatus::Disabled {
            plist_path,
            removed,
        });
    }

    let launch_path = resolve_launch_path()?;

    let canonical_config_path = fs::canonicalize(config_path).with_context(|| {
        format!(
            "canonicalizing config path for autostart: {}",
            config_path.display()
        )
    })?;
    let working_directory = canonical_config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"));
    let spec = LaunchAgentSpec {
        launch_path: launch_path.clone(),
        config_path: canonical_config_path,
        working_directory,
        load_dotenv: should_load_dotenv(),
    };
    let changed = write_launch_agent_file(&plist_path, &spec)?;

    Ok(AutostartSyncStatus::Enabled {
        plist_path,
        launch_path,
        changed,
    })
}

fn resolve_launch_path() -> Result<PathBuf> {
    let current_exe = std::env::current_exe().context("resolving current executable path")?;
    resolve_launch_path_from(&current_exe)
}

fn resolve_launch_path_from(current_exe: &Path) -> Result<PathBuf> {
    fs::canonicalize(current_exe).with_context(|| {
        format!(
            "canonicalizing current executable for autostart: {}",
            current_exe.display()
        )
    })
}

fn launch_agent_path(home_dir: &Path) -> PathBuf {
    home_dir
        .join("Library")
        .join("LaunchAgents")
        .join(LAUNCH_AGENT_FILE_NAME)
}

fn remove_launch_agent_file(plist_path: &Path) -> Result<bool> {
    match fs::remove_file(plist_path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("removing macOS autostart agent at {}", plist_path.display())),
    }
}

fn write_launch_agent_file(plist_path: &Path, spec: &LaunchAgentSpec) -> Result<bool> {
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("creating macOS autostart directory at {}", parent.display())
        })?;
    }

    let rendered = render_launch_agent_plist(spec);
    match fs::read_to_string(plist_path) {
        Ok(existing) if existing == rendered => return Ok(false),
        Ok(_) | Err(_) => {}
    }

    fs::write(plist_path, rendered)
        .with_context(|| format!("writing macOS autostart agent at {}", plist_path.display()))?;
    Ok(true)
}

fn render_launch_agent_plist(spec: &LaunchAgentSpec) -> String {
    let mut environment_variables = format!(
        "  <key>EnvironmentVariables</key>\n  <dict>\n    <key>MUNINN_CONFIG</key>\n    <string>{}</string>\n    <key>PATH</key>\n    <string>{}</string>\n",
        escape_plist_xml(&spec.config_path.display().to_string()),
        DEFAULT_LAUNCH_AGENT_PATH,
    );
    if spec.load_dotenv {
        environment_variables
            .push_str("    <key>MUNINN_LOAD_DOTENV</key>\n    <string>1</string>\n");
    }
    environment_variables.push_str("  </dict>");

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{launch_path}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <false/>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
  <key>WorkingDirectory</key>
  <string>{working_directory}</string>
{environment_variables}
</dict>
</plist>
"#,
        label = LAUNCH_AGENT_LABEL,
        launch_path = escape_plist_xml(&spec.launch_path.display().to_string()),
        working_directory = escape_plist_xml(&spec.working_directory.display().to_string()),
        environment_variables = environment_variables,
    )
}

fn escape_plist_xml(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn should_load_dotenv() -> bool {
    std::env::var("MUNINN_LOAD_DOTENV")
        .ok()
        .as_deref()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

#[cfg(test)]
mod tests {
    use super::{
        launch_agent_path, remove_launch_agent_file, render_launch_agent_plist,
        resolve_launch_path_from, write_launch_agent_file, LaunchAgentSpec,
    };
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn accepts_canonical_current_executable_path_without_allowlist() {
        let temp_dir = unique_temp_dir("muninn-autostart-exe");
        let exe_path = temp_dir.join("muninn");
        std::fs::write(&exe_path, "#!/bin/sh\n").expect("write temp executable");

        let resolved = resolve_launch_path_from(&exe_path).expect("resolve current executable");

        assert_eq!(
            resolved,
            std::fs::canonicalize(&exe_path).expect("canonicalize temp executable")
        );
    }

    #[test]
    fn rendered_plist_contains_expected_launch_agent_fields() {
        let spec = LaunchAgentSpec {
            launch_path: PathBuf::from("/opt/homebrew/bin/muninn"),
            config_path: PathBuf::from("/Users/example/.config/muninn/config.toml"),
            working_directory: PathBuf::from("/Users/example/.config/muninn"),
            load_dotenv: true,
        };

        let rendered = render_launch_agent_plist(&spec);

        assert!(rendered.contains("<string>com.bnomei.muninn</string>"));
        assert!(rendered.contains("<string>/opt/homebrew/bin/muninn</string>"));
        assert!(rendered.contains("<string>/Users/example/.config/muninn/config.toml</string>"));
        assert!(rendered.contains("<string>/Users/example/.config/muninn</string>"));
        assert!(rendered.contains("<key>MUNINN_LOAD_DOTENV</key>"));
        assert!(rendered.contains("<string>Aqua</string>"));
    }

    #[test]
    fn writing_same_plist_twice_reports_second_write_as_unchanged() {
        let home_dir = unique_temp_dir("muninn-autostart-home");
        let plist_path = launch_agent_path(&home_dir);
        let spec = LaunchAgentSpec {
            launch_path: PathBuf::from("/opt/homebrew/bin/muninn"),
            config_path: PathBuf::from("/Users/example/.config/muninn/config.toml"),
            working_directory: PathBuf::from("/Users/example/.config/muninn"),
            load_dotenv: false,
        };

        let first = write_launch_agent_file(&plist_path, &spec).expect("first write should work");
        let second = write_launch_agent_file(&plist_path, &spec).expect("second write should work");

        assert!(first);
        assert!(!second);
        assert!(plist_path.exists());
    }

    #[test]
    fn removing_launch_agent_file_is_idempotent() {
        let home_dir = unique_temp_dir("muninn-autostart-remove");
        let plist_path = launch_agent_path(&home_dir);
        if let Some(parent) = plist_path.parent() {
            std::fs::create_dir_all(parent).expect("create launch agents dir");
        }
        std::fs::write(&plist_path, "<plist/>").expect("write plist");

        let removed = remove_launch_agent_file(&plist_path).expect("remove existing plist");
        let removed_again =
            remove_launch_agent_file(&plist_path).expect("removing missing plist should succeed");

        assert!(removed);
        assert!(!removed_again);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before UNIX_EPOCH")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("{prefix}-{}-{unique_suffix}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create temp dir");
        root
    }
}
