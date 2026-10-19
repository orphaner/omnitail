//! Sources backed by external CLIs (docker, kubectl, journalctl). Each
//! builder returns the command line for a run: the first one shows the last
//! `lines` lines, later ones (restarts) resume from when the previous ended.
//! Native clients can replace these later behind the same `Source` trait.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, SecondsFormat, Utc};

use super::command::Argv;
use crate::spec::K8sPod;

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

/// Runs a command to completion, for its standard output.
async fn output(argv: &[String]) -> Result<String> {
    let output = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .with_context(|| format!("cannot run `{}`", argv[0]))?;
    if !output.status.success() {
        bail!("`{}` failed: {}", argv.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Lists the services of a compose project, to give each its own pane.
pub async fn compose_services(file: Option<&Path>) -> Result<Vec<String>> {
    let mut argv = compose_base(file);
    argv.extend(strings(["config", "--services"]));
    let services: Vec<String> =
        output(&argv).await?.lines().map(str::trim).filter(|s| !s.is_empty()).map(Into::into).collect();
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
    /// Label selector of a workload's pods, to follow them all.
    pub pods: Option<String>,
}

impl K8sTarget {
    /// A label selector (`app=api`) rather than a named object.
    fn is_selector(&self) -> bool {
        self.target.contains('=')
    }

    /// A deployment, statefulset…: not a pod, not a selector.
    pub fn is_workload(&self) -> bool {
        !self.is_selector()
            && self.target.split_once('/').is_some_and(|(kind, _)| !matches!(kind, "pod" | "pods" | "po"))
    }

    fn kubectl(&self, command: &str) -> Vec<String> {
        let mut argv = strings(["kubectl"]);
        if let Some(ctx) = &self.context {
            argv.extend(["--context".into(), ctx.clone()]);
        }
        argv.push(command.into());
        if let Some(ns) = &self.namespace {
            argv.extend(["--namespace".into(), ns.clone()]);
        }
        argv
    }
}

pub fn k8s(t: K8sTarget, lines: usize) -> Argv {
    Box::new(move |since| {
        let mut argv = t.kubectl("logs");
        argv.push("--follow".into());
        // Every line says its pod and container, for badges and splitting.
        argv.push("--prefix".into());
        match &t.container {
            Some(c) => argv.extend(["--container".into(), c.clone()]),
            None if !t.args.iter().any(|a| a.starts_with("--all-containers")) => argv.push("--all-containers".into()),
            None => {}
        }
        match since {
            None => argv.push(format!("--tail={lines}")),
            Some(t) => argv.push(format!("--since-time={}", rfc3339(t))),
        }
        match t.pods.as_ref().or(t.is_selector().then_some(&t.target)) {
            // One request per pod and container.
            Some(selector) => argv.extend(["--selector".into(), selector.clone(), "--max-log-requests=100".into()]),
            None => argv.push(t.target.clone()),
        }
        argv.extend(t.args.iter().cloned());
        argv
    })
}

/// What a k8s target runs: for a workload, the label selector of its
/// pods; and those pods with their containers.
#[derive(Debug, PartialEq, Eq)]
pub struct K8sPods {
    pub selector: Option<String>,
    pub pods: Vec<K8sPod>,
}

/// Asks the cluster what `t` runs (`kubectl get … -o json`): a workload's
/// selector first, then its pods.
pub async fn k8s_pods(t: &K8sTarget) -> Result<K8sPods> {
    let get = |what: Vec<String>| {
        let mut argv = t.kubectl("get");
        argv.extend(what);
        argv.extend(strings(["--output", "json"]));
        async move {
            let json: serde_json::Value =
                serde_json::from_str(&output(&argv).await?).context("unexpected kubectl output")?;
            Ok::<_, anyhow::Error>(json)
        }
    };
    let by_selector = |selector: &str| vec!["pods".into(), "--selector".into(), selector.to_owned()];
    if t.is_selector() {
        return Ok(K8sPods { selector: None, pods: pods_of(&get(by_selector(&t.target)).await?) });
    }
    if !t.is_workload() {
        // `kubectl logs x` means the pod x.
        let name = t.target.split_once('/').map_or(t.target.as_str(), |(_, name)| name);
        return Ok(K8sPods { selector: None, pods: pods_of(&get(vec![format!("pod/{name}")]).await?) });
    }
    let workload = get(vec![t.target.clone()]).await?;
    let Some(selector) = label_selector(&workload["spec"]["selector"]) else {
        // Not something with pods of its own (a service…): kubectl decides.
        return Ok(K8sPods { selector: None, pods: vec![] });
    };
    let pods = pods_of(&get(by_selector(&selector)).await?);
    Ok(K8sPods { selector: Some(selector), pods })
}

/// The pods of a `kubectl get` answer: one pod, or a list of them.
fn pods_of(object: &serde_json::Value) -> Vec<K8sPod> {
    let pods: Vec<&serde_json::Value> = match object["kind"].as_str() {
        Some("List") => object["items"].as_array().into_iter().flatten().collect(),
        _ => vec![object],
    };
    pods.into_iter()
        .map(|pod| {
            let spec = &pod["spec"];
            // Init containers run before the others and end, except native
            // sidecars, which keep running next to them.
            let sidecars =
                spec["initContainers"].as_array().into_iter().flatten().filter(|c| c["restartPolicy"] == "Always");
            let containers = spec["containers"].as_array().into_iter().flatten().chain(sidecars);
            K8sPod {
                name: pod["metadata"]["name"].as_str().unwrap_or_default().to_owned(),
                containers: containers.filter_map(|c| c["name"].as_str()).map(Into::into).collect(),
            }
        })
        .collect()
}

/// A workload's `spec.selector` as a `kubectl --selector` argument.
fn label_selector(selector: &serde_json::Value) -> Option<String> {
    let labels = selector["matchLabels"].as_object().into_iter().flatten().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")));
    let expressions = selector["matchExpressions"].as_array().into_iter().flatten().filter_map(|e| {
        let key = e["key"].as_str()?;
        let values = || e["values"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(",");
        Some(match e["operator"].as_str()? {
            "In" => format!("{key} in ({})", values()),
            "NotIn" => format!("{key} notin ({})", values()),
            "Exists" => key.to_owned(),
            "DoesNotExist" => format!("!{key}"),
            _ => return None,
        })
    });
    let all: Vec<String> = labels.chain(expressions).collect();
    (!all.is_empty()).then(|| all.join(","))
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
    fn k8s_follows_all_containers_with_prefixes() {
        let t = |target: &str| K8sTarget {
            target: target.into(),
            namespace: Some("prod".into()),
            context: Some("orbstack".into()),
            container: None,
            args: vec!["--timestamps".into()],
            pods: None,
        };
        assert_eq!(
            k8s(t("pod/x"), 10)(None).join(" "),
            "kubectl --context orbstack logs --namespace prod --follow --prefix --all-containers --tail=10 pod/x --timestamps"
        );
        assert_eq!(
            k8s(t("app=api"), 10)(since()).join(" "),
            "kubectl --context orbstack logs --namespace prod --follow --prefix --all-containers \
             --since-time=2026-10-17T12:00:00.000Z --selector app=api --max-log-requests=100 --timestamps"
        );
        let workload = K8sTarget { pods: Some("app=api".into()), container: Some("app".into()), ..t("deploy/api") };
        assert_eq!(
            k8s(workload, 10)(None).join(" "),
            "kubectl --context orbstack logs --namespace prod --follow --prefix --container app --tail=10 \
             --selector app=api --max-log-requests=100 --timestamps"
        );
        let all = K8sTarget { args: vec!["--all-containers=true".into()], ..t("pod/x") };
        assert_eq!(k8s(all, 1)(None).iter().filter(|a| a.starts_with("--all-containers")).count(), 1);
    }

    #[test]
    fn k8s_targets() {
        let t = |target: &str| K8sTarget { target: target.into(), namespace: None, context: None, container: None, args: vec![], pods: None };
        assert!(t("deploy/api").is_workload() && t("sts/db").is_workload());
        assert!(!t("pod/x").is_workload() && !t("x").is_workload() && !t("app=api").is_workload());
    }

    #[test]
    fn k8s_selectors_and_pods() {
        let selector = serde_json::json!({
            "matchLabels": {"app": "shop"},
            "matchExpressions": [{"key": "tier", "operator": "In", "values": ["web", "api"]}, {"key": "canary", "operator": "DoesNotExist"}]
        });
        assert_eq!(label_selector(&selector).as_deref(), Some("app=shop,tier in (web,api),!canary"));
        assert_eq!(label_selector(&serde_json::json!(null)), None);

        let pod = |name: &str| serde_json::json!({
            "kind": "Pod",
            "metadata": {"name": name},
            "spec": {
                "initContainers": [{"name": "migrate"}, {"name": "shipper", "restartPolicy": "Always"}],
                "containers": [{"name": "app"}, {"name": "proxy"}]
            }
        });
        let shop = |name: &str| K8sPod { name: name.into(), containers: vec!["app".into(), "proxy".into(), "shipper".into()] };
        assert_eq!(pods_of(&pod("shop-1")), [shop("shop-1")]);
        let list = serde_json::json!({"kind": "List", "items": [pod("shop-1"), pod("shop-2")]});
        assert_eq!(pods_of(&list), [shop("shop-1"), shop("shop-2")]);
    }

    #[test]
    fn journal_unit() {
        assert_eq!(journal(Some("nginx".into()), 7)(None).join(" "), "journalctl --follow --no-pager --lines=7 --unit nginx");
        assert!(journal(None, 7)(since())[3].starts_with("--since=2026-10-17 "));
    }
}
