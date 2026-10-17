//! Sources backed by external CLIs (docker, kubectl, journalctl). Each
//! builder returns the command line for a run: the first one shows the last
//! `lines` lines, later ones (restarts) resume from when the previous ended.
//! Native clients can replace these later behind the same `Source` trait.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, SecondsFormat, Utc};

use super::command::Argv;

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn strings<const N: usize>(args: [&str; N]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

pub fn docker(container: String, lines: usize) -> Argv {
    Box::new(move |since| {
        let mut argv = strings(["docker", "logs", "--follow"]);
        match since {
            None => argv.extend(["--tail".into(), lines.to_string()]),
            Some(t) => argv.extend(["--since".into(), rfc3339(t)]),
        }
        argv.push(container.clone());
        argv
    })
}

fn compose_base(file: Option<&Path>) -> Vec<String> {
    let mut argv = strings(["docker", "compose"]);
    if let Some(file) = file {
        argv.extend(["-f".into(), file.display().to_string()]);
    }
    argv
}

pub fn compose(file: Option<PathBuf>, services: Vec<String>, lines: usize) -> Argv {
    Box::new(move |since| {
        let mut argv = compose_base(file.as_deref());
        argv.extend(strings(["logs", "--follow"]));
        // One service per pane: the "service | " prefix would only be noise.
        if services.len() == 1 {
            argv.push("--no-log-prefix".into());
        }
        match since {
            None => argv.extend(["--tail".into(), lines.to_string()]),
            Some(t) => argv.extend(["--since".into(), rfc3339(t)]),
        }
        argv.extend(services.iter().cloned());
        argv
    })
}

/// Lists the services of a compose project, to give each its own pane.
pub async fn compose_services(file: Option<&Path>) -> Result<Vec<String>> {
    let mut argv = compose_base(file);
    argv.extend(strings(["config", "--services"]));
    let output = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .context("cannot run `docker compose`")?;
    if !output.status.success() {
        bail!("`{}` failed: {}", argv.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    let services: Vec<String> =
        String::from_utf8_lossy(&output.stdout).lines().map(str::trim).filter(|s| !s.is_empty()).map(Into::into).collect();
    if services.is_empty() {
        bail!("`{}` lists no services", argv.join(" "));
    }
    Ok(services)
}

pub struct K8sTarget {
    pub target: String,
    pub namespace: Option<String>,
    pub context: Option<String>,
    pub container: Option<String>,
    pub args: Vec<String>,
}

pub fn k8s(t: K8sTarget, lines: usize) -> Argv {
    Box::new(move |since| {
        let mut argv = strings(["kubectl"]);
        if let Some(ctx) = &t.context {
            argv.extend(["--context".into(), ctx.clone()]);
        }
        argv.extend(strings(["logs", "--follow"]));
        if let Some(ns) = &t.namespace {
            argv.extend(["--namespace".into(), ns.clone()]);
        }
        if let Some(c) = &t.container {
            argv.extend(["--container".into(), c.clone()]);
        }
        match since {
            None => argv.push(format!("--tail={lines}")),
            Some(t) => argv.push(format!("--since-time={}", rfc3339(t))),
        }
        if t.target.contains('=') {
            // A label selector: several pods merged, prefixed with their name.
            argv.extend(["--selector".into(), t.target.clone(), "--prefix".into(), "--max-log-requests=20".into()]);
        } else {
            argv.push(t.target.clone());
        }
        argv.extend(t.args.iter().cloned());
        argv
    })
}

pub fn journal(unit: Option<String>, lines: usize) -> Argv {
    Box::new(move |since| {
        let mut argv = strings(["journalctl", "--follow", "--no-pager"]);
        match since {
            None => argv.push(format!("--lines={lines}")),
            // journalctl wants local time, not RFC 3339.
            Some(t) => argv.push(format!("--since={}", t.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S"))),
        }
        if let Some(unit) = &unit {
            argv.extend(["--unit".into(), unit.clone()]);
        }
        argv
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn since() -> Option<DateTime<Utc>> {
        Some(Utc.with_ymd_and_hms(2026, 10, 17, 12, 0, 0).unwrap())
    }

    #[test]
    fn docker_tail_then_since() {
        let argv = docker("db".into(), 20);
        assert_eq!(argv(None).join(" "), "docker logs --follow --tail 20 db");
        assert_eq!(argv(since()).join(" "), "docker logs --follow --since 2026-10-17T12:00:00.000Z db");
    }

    #[test]
    fn compose_single_service_has_no_prefix() {
        let argv = compose(Some("/p/compose.yml".into()), vec!["web".into()], 5);
        assert_eq!(argv(None).join(" "), "docker compose -f /p/compose.yml logs --follow --no-log-prefix --tail 5 web");
        let argv = compose(None, vec!["web".into(), "db".into()], 5);
        assert_eq!(argv(since()).join(" "), "docker compose logs --follow --since 2026-10-17T12:00:00.000Z web db");
    }

    #[test]
    fn k8s_target_and_selector() {
        let t = |target: &str| K8sTarget {
            target: target.into(),
            namespace: Some("prod".into()),
            context: Some("orbstack".into()),
            container: None,
            args: vec!["--timestamps".into()],
        };
        assert_eq!(
            k8s(t("deploy/api"), 10)(None).join(" "),
            "kubectl --context orbstack logs --follow --namespace prod --tail=10 deploy/api --timestamps"
        );
        assert_eq!(
            k8s(t("app=api"), 10)(since()).join(" "),
            "kubectl --context orbstack logs --follow --namespace prod --since-time=2026-10-17T12:00:00.000Z \
             --selector app=api --prefix --max-log-requests=20 --timestamps"
        );
    }

    #[test]
    fn journal_unit() {
        assert_eq!(journal(Some("nginx".into()), 7)(None).join(" "), "journalctl --follow --no-pager --lines=7 --unit nginx");
        assert!(journal(None, 7)(since())[3].starts_with("--since=2026-10-17 "));
    }
}
