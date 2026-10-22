//! Kubernetes logs through the API server (kube-rs) instead of `kubectl`.
//! One client per context is shared by every source, so the kubeconfig's
//! credentials (an `exec` plugin can take seconds) are obtained once, then
//! refreshed by the client when they expire. Pods are watched: those a
//! workload starts later are followed too, and a container that restarts
//! is followed again from where its previous run ended.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures::{AsyncBufReadExt, StreamExt};
use jiff::Timestamp;
use k8s_openapi::api::core::v1::{ContainerStatus, Pod};
use kube::api::{Api, DynamicObject, ListParams, LogParams};
use kube::config::KubeConfigOptions;
use kube::core::{ApiResource, GroupVersionKind};
use kube::runtime::WatchStreamExt;
use kube::runtime::watcher::{self, Event, watcher};
use kube::{Client, Config};
use tokio::sync::Mutex;
use tokio::task::{JoinHandle, JoinSet};

use super::tools::{K8sPods, K8sTarget, label_selector, pods_of};
use super::{Emitter, Source, SourceEvent, SourceStatus};
use crate::spec::K8sPod;

/// Clients by context (`None`: the kubeconfig's current one).
static CLIENTS: Mutex<BTreeMap<Option<String>, Client>> = Mutex::const_new(BTreeMap::new());

async fn client(context: Option<&String>) -> Result<Client> {
    let mut clients = CLIENTS.lock().await;
    if let Some(client) = clients.get(&context.cloned()) {
        return Ok(client.clone());
    }
    let options = KubeConfigOptions { context: context.cloned(), ..Default::default() };
    let config = Config::from_kubeconfig(&options).await.context("cannot load the kubeconfig")?;
    let client = Client::try_from(config).context("cannot create a kubernetes client")?;
    clients.insert(context.cloned(), client.clone());
    Ok(client)
}

/// The API server's own message, without the rest of its answer.
fn api_error(e: kube::Error) -> anyhow::Error {
    match e {
        kube::Error::Api(status) => anyhow::anyhow!(status.message),
        e => e.into(),
    }
}

fn namespace(t: &K8sTarget, client: &Client) -> String {
    t.namespace.clone().unwrap_or_else(|| client.default_namespace().to_owned())
}

/// What a target names.
enum Target {
    Pod(String),
    Selector(String),
    Workload(ApiResource, String),
}

/// `None` for kinds only `kubectl` knows how to follow.
fn target(t: &K8sTarget) -> Option<Target> {
    if t.is_selector() {
        return Some(Target::Selector(t.target.clone()));
    }
    let Some((kind, name)) = t.target.split_once('/') else { return Some(Target::Pod(t.target.clone())) };
    // `deployment.apps/x` names the group too.
    let kind = kind.split('.').next().unwrap_or_default().to_ascii_lowercase();
    let (group, version, kind) = match kind.as_str() {
        "po" | "pod" | "pods" => return Some(Target::Pod(name.into())),
        "deploy" | "deployment" | "deployments" => ("apps", "v1", "Deployment"),
        "sts" | "statefulset" | "statefulsets" => ("apps", "v1", "StatefulSet"),
        "ds" | "daemonset" | "daemonsets" => ("apps", "v1", "DaemonSet"),
        "rs" | "replicaset" | "replicasets" => ("apps", "v1", "ReplicaSet"),
        "job" | "jobs" => ("batch", "v1", "Job"),
        "svc" | "service" | "services" => ("", "v1", "Service"),
        _ => return None,
    };
    Some(Target::Workload(ApiResource::from_gvk(&GroupVersionKind::gvk(group, version, kind)), name.into()))
}

/// Whether the native client can follow `t`: `kubectl` takes the others
/// (other kinds, `kubectl logs` options with no equivalent here).
pub fn supports(t: &K8sTarget) -> bool {
    target(t).is_some() && LogOptions::parse(&t.args).is_some()
}

/// What `t` runs, as [`super::tools::k8s_pods`] finds it with kubectl.
pub async fn k8s_pods(t: &K8sTarget) -> Result<K8sPods> {
    let client = client(t.context.as_ref()).await?;
    let ns = namespace(t, &client);
    let api: Api<Pod> = Api::namespaced(client.clone(), &ns);
    let by_selector = async |selector: &str| -> Result<Vec<K8sPod>> {
        let list = api.list(&ListParams::default().labels(selector)).await.map_err(api_error);
        let list = list.with_context(|| format!("cannot list the pods of {selector} in {ns}"))?;
        Ok(list.items.iter().map(k8s_pod).collect())
    };
    match target(t).context("not a kind omnitail knows")? {
        Target::Pod(name) => {
            let pod = api.get(&name).await.map_err(api_error).with_context(|| format!("cannot get pod {name} in {ns}"))?;
            Ok(K8sPods { selector: None, pods: vec![k8s_pod(&pod)] })
        }
        Target::Selector(selector) => Ok(K8sPods { selector: None, pods: by_selector(&selector).await? }),
        Target::Workload(resource, name) => {
            let workloads: Api<DynamicObject> = Api::namespaced_with(client.clone(), &ns, &resource);
            let workload = workloads.get(&name).await.map_err(api_error).with_context(|| format!("cannot get {} in {ns}", t.target))?;
            let Some(selector) = label_selector(&serde_json::to_value(&workload)?["spec"]["selector"]) else {
                return Ok(K8sPods { selector: None, pods: vec![] });
            };
            let pods = by_selector(&selector).await?;
            Ok(K8sPods { selector: Some(selector), pods })
        }
    }
}

fn k8s_pod(pod: &Pod) -> K8sPod {
    serde_json::to_value(pod).ok().and_then(|v| pods_of(&v).pop()).unwrap_or_default()
}

/// The `kubectl logs` options the native client has an equivalent for.
#[derive(Debug, Default, Clone, PartialEq)]
struct LogOptions {
    timestamps: bool,
    since_seconds: Option<i64>,
    since_time: Option<Timestamp>,
}

impl LogOptions {
    /// `None` when an option needs kubectl.
    fn parse(args: &[String]) -> Option<Self> {
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let (flag, value) = match arg.split_once('=') {
                Some((flag, value)) => (flag, Some(value.to_owned())),
                None => (arg.as_str(), None),
            };
            let mut value = || value.clone().or_else(|| args.next().cloned());
            match flag {
                "--timestamps" => options.timestamps = value_is_true(arg),
                // What every native stream does anyway.
                "-f" | "--follow" | "--prefix" | "--all-containers" if value_is_true(arg) => {}
                "--since" => options.since_seconds = Some(go_seconds(&value()?)?),
                "--since-time" => options.since_time = Some(value()?.parse().ok()?),
                _ => return None,
            }
        }
        Some(options)
    }
}

/// `--flag` or `--flag=true`.
fn value_is_true(arg: &str) -> bool {
    arg.split_once('=').is_none_or(|(_, v)| v == "true")
}

/// A Go duration as `kubectl --since` takes it (`90s`, `1h30m`), in whole seconds.
fn go_seconds(duration: &str) -> Option<i64> {
    let mut rest = duration;
    let mut total = 0.0;
    while !rest.is_empty() {
        let number = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
        let value: f64 = rest[..number].parse().ok()?;
        rest = &rest[number..];
        let unit = rest.find(|c: char| c.is_ascii_digit() || c == '.').unwrap_or(rest.len());
        total += value
            * match &rest[..unit] {
                "h" => 3600.0,
                "m" => 60.0,
                "s" => 1.0,
                "ms" => 0.001,
                _ => return None,
            };
        rest = &rest[unit..];
    }
    (!duration.is_empty()).then(|| total.ceil() as i64)
}

/// The pods a source watches.
#[derive(Debug, Clone, PartialEq)]
enum Watch {
    Pod(String),
    Labels(String),
}

pub struct K8sSource {
    context: Option<String>,
    namespace: Option<String>,
    watch: Watch,
    container: Option<String>,
    options: LogOptions,
    /// Lines read from the containers running at start.
    tail: usize,
    /// The pods the app was told about at start.
    running: Vec<K8sPod>,
}

impl K8sSource {
    /// `None` when `t` needs kubectl, or when a workload has no selector.
    pub fn new(t: &K8sTarget, running: Vec<K8sPod>, tail: usize) -> Option<Self> {
        let watch = match target(t)? {
            Target::Pod(name) => Watch::Pod(name),
            Target::Selector(selector) => Watch::Labels(selector),
            Target::Workload(..) => Watch::Labels(t.pods.clone()?),
        };
        Some(Self {
            context: t.context.clone(),
            namespace: t.namespace.clone(),
            watch,
            container: t.container.clone(),
            options: LogOptions::parse(&t.args)?,
            tail,
            running,
        })
    }
}

impl Source for K8sSource {
    fn spawn(self: Box<Self>, out: Emitter) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut delay = MIN_DELAY;
            loop {
                match client(self.context.as_ref()).await {
                    Ok(client) => return Follower::new(*self, client, out).run().await,
                    Err(e) => {
                        if !out.status(SourceStatus::Error(format!("{e:#}"))).await {
                            return;
                        }
                    }
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(MAX_DELAY);
            }
        })
    }
}

const MIN_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(30);
/// A stream that lasted this long resets the delay before the next one.
const HEALTHY: Duration = Duration::from_secs(10);

/// A pod name and one of its containers.
type Key = (String, String);

/// The log stream of one container, over its runs.
struct Stream {
    active: bool,
    /// The container (by id) the last stream read: a new id is a new run.
    read: Option<Option<String>>,
    /// When the last stream ended: the next one starts there.
    since: Option<Timestamp>,
    /// Earliest new stream for the same run (it may have been cut).
    not_before: Instant,
    delay: Duration,
}

impl Stream {
    fn new() -> Self {
        Self { active: false, read: None, since: None, not_before: Instant::now(), delay: MIN_DELAY }
    }

    /// Whether to start reading the container `status` describes: its
    /// first run, a new run, or the same run after its stream was cut.
    fn wanted(&self, status: &ContainerStatus, now: Instant) -> bool {
        let state = status.state.as_ref();
        let running = state.is_some_and(|s| s.running.is_some());
        // Waiting to restart after a crash: the last run has logs.
        let started = running
            || state.is_some_and(|s| s.terminated.is_some())
            || status.last_state.as_ref().is_some_and(|s| s.terminated.is_some());
        !self.active
            && started
            && match &self.read {
                None => true,
                Some(read) => *read != status.container_id || (running && now >= self.not_before),
            }
    }
}

/// What the follower waits for.
enum Wake {
    Watch(Option<Result<Box<Event<Pod>>, watcher::Error>>),
    Ended(Key, Instant, Result<(), String>),
    Retry(Key),
}

struct Follower {
    source: K8sSource,
    api: Api<Pod>,
    out: Emitter,
    pods: BTreeMap<String, Pod>,
    streams: HashMap<Key, Stream>,
    tasks: JoinSet<(Key, Instant, Result<(), String>)>,
    retries: JoinSet<Key>,
    /// Set once the first listing is done: containers found later are read
    /// from their start, not from the tail.
    listed: bool,
    /// Pods seen since the watch (re)started, to drop those gone meanwhile.
    relisted: Option<HashSet<String>>,
    /// The pods the app knows about.
    published: Vec<K8sPod>,
    /// Something failed since the status was last `Running`.
    degraded: Arc<AtomicBool>,
}

impl Follower {
    fn new(source: K8sSource, client: Client, out: Emitter) -> Self {
        let ns = source.namespace.clone().unwrap_or_else(|| client.default_namespace().to_owned());
        Self {
            api: Api::namespaced(client, &ns),
            out,
            pods: BTreeMap::new(),
            streams: HashMap::new(),
            tasks: JoinSet::new(),
            retries: JoinSet::new(),
            listed: false,
            relisted: None,
            published: source.running.clone(),
            degraded: Arc::new(AtomicBool::new(true)),
            source,
        }
    }

    async fn run(mut self) {
        let config = match &self.source.watch {
            Watch::Pod(name) => watcher::Config::default().fields(&format!("metadata.name={name}")),
            Watch::Labels(selector) => watcher::Config::default().labels(selector),
        };
        let mut events = watcher(self.api.clone(), config).default_backoff().boxed();
        loop {
            let wake = tokio::select! {
                event = events.next() => Wake::Watch(event.map(|e| e.map(Box::new))),
                Some(Ok((key, started, result))) = self.tasks.join_next(), if !self.tasks.is_empty() => Wake::Ended(key, started, result),
                Some(Ok(key)) = self.retries.join_next(), if !self.retries.is_empty() => Wake::Retry(key),
            };
            let alive = match wake {
                Wake::Watch(None) => return,
                Wake::Watch(Some(Ok(event))) => self.on_event(*event).await,
                Wake::Watch(Some(Err(e))) => self.failed(format!("watching pods: {e}")).await,
                Wake::Ended(key, started, result) => self.ended(key, started, result).await,
                Wake::Retry(key) => {
                    self.reconcile(&key);
                    true
                }
            };
            if !alive {
                return;
            }
        }
    }

    /// Returns `false` once the app has gone away.
    async fn on_event(&mut self, event: Event<Pod>) -> bool {
        if self.degraded.swap(false, Ordering::Relaxed) && !self.out.status(SourceStatus::Running).await {
            return false;
        }
        match event {
            Event::Init => self.relisted = Some(HashSet::new()),
            Event::InitApply(pod) | Event::Apply(pod) => {
                let name = pod.metadata.name.clone().unwrap_or_default();
                if let Some(relisted) = &mut self.relisted {
                    relisted.insert(name.clone());
                }
                self.pods.insert(name.clone(), pod);
                // The app learns of a new pod before its lines.
                if !self.publish().await {
                    return false;
                }
                for container in self.containers(&name) {
                    self.reconcile(&(name.clone(), container));
                }
            }
            Event::Delete(pod) => {
                // Its streams end with its containers.
                self.pods.remove(pod.metadata.name.as_deref().unwrap_or_default());
            }
            Event::InitDone => {
                if let Some(relisted) = self.relisted.take() {
                    self.pods.retain(|name, _| relisted.contains(name));
                }
                self.listed = true;
            }
        }
        self.publish().await
    }

    async fn failed(&mut self, why: String) -> bool {
        tracing::warn!(%why);
        self.degraded.store(true, Ordering::Relaxed);
        self.out.status(SourceStatus::Waiting(why)).await
    }

    /// Tells the app when pods come and go, for badges and splits.
    async fn publish(&mut self) -> bool {
        if !self.listed {
            return true;
        }
        let pods: Vec<K8sPod> = self.pods.keys().map(|name| K8sPod { name: name.clone(), containers: self.containers(name) }).collect();
        if pods == self.published {
            return true;
        }
        self.published = pods.clone();
        self.out.emit(SourceEvent::Pods(pods)).await
    }

    /// The containers followed in pod `name`.
    fn containers(&self, name: &str) -> Vec<String> {
        if let Some(container) = &self.source.container {
            return vec![container.clone()];
        }
        self.pods.get(name).map(|pod| k8s_pod(pod).containers).unwrap_or_default()
    }

    /// Starts reading a container if it has logs nobody reads yet: its
    /// first run, a new run, or the same run after its stream was cut.
    fn reconcile(&mut self, key: &Key) {
        let Some(status) = self.pods.get(&key.0).and_then(|pod| container_status(pod, &key.1)) else { return };
        let stream = self.streams.entry(key.clone()).or_insert_with(Stream::new);
        let start = stream.wanted(status, Instant::now());
        if !start {
            return;
        }
        let mut params = LogParams { container: Some(key.1.clone()), follow: true, timestamps: self.source.options.timestamps, ..Default::default() };
        match stream.since {
            Some(since) => params.since_time = Some(since),
            None => {
                params.since_seconds = self.source.options.since_seconds;
                params.since_time = self.source.options.since_time;
                // A pod started after omnitail: all its lines are new.
                if !self.listed {
                    params.tail_lines = Some(self.source.tail as i64);
                }
            }
        }
        stream.active = true;
        stream.read = Some(status.container_id.clone());
        let (api, out, key, degraded) = (self.api.clone(), self.out.clone(), key.clone(), self.degraded.clone());
        self.tasks.spawn(async move {
            let started = Instant::now();
            let result = follow(&api, &key, &params, &out, &degraded).await;
            (key, started, result)
        });
    }

    async fn ended(&mut self, key: Key, started: Instant, result: Result<(), String>) -> bool {
        if let Err(e) = &result
            && !self.failed(format!("{}/{}: {e}", key.0, key.1)).await
        {
            return false;
        }
        if !self.pods.contains_key(&key.0) {
            self.streams.remove(&key);
            return true;
        }
        let Some(stream) = self.streams.get_mut(&key) else { return true };
        stream.active = false;
        stream.since = Some(Timestamp::now());
        // Streams that keep ending early wait longer and longer.
        let delay = if started.elapsed() >= HEALTHY { MIN_DELAY } else { stream.delay };
        stream.delay = (delay * 2).min(MAX_DELAY);
        stream.not_before = Instant::now() + delay;
        self.retries.spawn(async move {
            tokio::time::sleep(delay).await;
            key
        });
        true
    }
}

fn container_status<'a>(pod: &'a Pod, name: &str) -> Option<&'a ContainerStatus> {
    let status = pod.status.as_ref()?;
    // Native sidecars are init containers.
    let all = status.container_statuses.iter().flatten().chain(status.init_container_statuses.iter().flatten());
    all.into_iter().find(|s| s.name == name)
}

/// Sends a container's lines with the prefix `kubectl logs --prefix` gives
/// them, until its stream ends.
async fn follow(api: &Api<Pod>, (pod, container): &Key, params: &LogParams, out: &Emitter, degraded: &AtomicBool) -> Result<(), String> {
    let stream = api.log_stream(pod, params).await.map_err(|e| e.to_string())?;
    if degraded.swap(false, Ordering::Relaxed) {
        out.status(SourceStatus::Running).await;
    }
    let mut stream = std::pin::pin!(stream);
    let prefix = format!("[pod/{pod}/{container}] ");
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match stream.read_until(b'\n', &mut buf).await {
            Ok(0) => return Ok(()),
            Err(e) => return Err(e.to_string()),
            Ok(_) => {
                let line = prefix.clone() + &super::decode_line(&buf);
                if !out.line(line).await {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(target: &str, args: &[&str]) -> K8sTarget {
        K8sTarget {
            target: target.into(),
            namespace: None,
            context: None,
            container: None,
            args: args.iter().map(|a| a.to_string()).collect(),
            pods: None,
        }
    }

    #[test]
    fn native_targets_and_options() {
        assert!(supports(&t("pod/x", &[])) && supports(&t("x", &[])) && supports(&t("app=api", &[])));
        assert!(supports(&t("deploy/api", &[])) && supports(&t("deployment.apps/api", &[])) && supports(&t("svc/api", &[])));
        assert!(!supports(&t("cronjob/backup", &[])), "kubectl knows more kinds");
        assert!(supports(&t("deploy/api", &["--timestamps", "--since", "1h", "--all-containers=true"])));
        assert!(!supports(&t("deploy/api", &["--previous"])), "no equivalent: kubectl");

        let options = LogOptions::parse(&["--since=1h30m".into(), "--timestamps=false".into()]).unwrap();
        assert_eq!(options, LogOptions { timestamps: false, since_seconds: Some(5400), since_time: None });
        let options = LogOptions::parse(&["--since-time".into(), "2026-10-22T08:00:00Z".into()]).unwrap();
        assert_eq!(options.since_time, Some("2026-10-22T08:00:00Z".parse().unwrap()));
        assert_eq!(LogOptions::parse(&["--since=soon".into()]), None);
    }

    #[test]
    fn streams_start_with_each_run() {
        use k8s_openapi::api::core::v1::{ContainerState, ContainerStateRunning, ContainerStateTerminated, ContainerStateWaiting};
        let status = |id: Option<&str>, state: ContainerState, last: Option<ContainerState>| ContainerStatus {
            name: "app".into(),
            container_id: id.map(Into::into),
            state: Some(state),
            last_state: last,
            ..Default::default()
        };
        let running = || ContainerState { running: Some(ContainerStateRunning::default()), ..Default::default() };
        let ended = || ContainerState { terminated: Some(ContainerStateTerminated::default()), ..Default::default() };
        let waiting = || ContainerState { waiting: Some(ContainerStateWaiting::default()), ..Default::default() };
        let now = Instant::now();

        let mut stream = Stream::new();
        assert!(!stream.wanted(&status(None, waiting(), None), now), "not started yet: no logs");
        assert!(stream.wanted(&status(Some("c1"), running(), None), now));
        assert!(stream.wanted(&status(Some("c1"), waiting(), Some(ended())), now), "crash loop: the last run's logs");

        stream.active = true;
        stream.read = Some(Some("c1".into()));
        assert!(!stream.wanted(&status(Some("c1"), running(), None), now), "already read");

        // Its stream ended.
        stream.active = false;
        stream.not_before = now + Duration::from_secs(2);
        assert!(!stream.wanted(&status(Some("c1"), running(), None), now), "cut: retried after a delay");
        assert!(stream.wanted(&status(Some("c1"), running(), None), now + Duration::from_secs(2)));
        assert!(!stream.wanted(&status(Some("c1"), ended(), None), now + Duration::from_secs(2)), "over: nothing more");
        assert!(stream.wanted(&status(Some("c2"), running(), Some(ended())), now), "a new run, at once");
    }

    #[test]
    fn go_durations() {
        assert_eq!(go_seconds("90s"), Some(90));
        assert_eq!(go_seconds("1h2m3s"), Some(3723));
        assert_eq!(go_seconds("1.5m"), Some(90));
        assert_eq!(go_seconds("500ms"), Some(1));
        assert_eq!(go_seconds(""), None);
        assert_eq!(go_seconds("5"), None);
        assert_eq!(go_seconds("5d"), None);
    }

    #[test]
    fn workloads_are_watched_by_their_selector() {
        let watch = |t: &K8sTarget| K8sSource::new(t, vec![], 10).map(|s| s.watch);
        assert_eq!(watch(&t("pod/x", &[])), Some(Watch::Pod("x".into())));
        assert_eq!(watch(&t("app=api", &[])), Some(Watch::Labels("app=api".into())));
        assert_eq!(watch(&t("deploy/api", &[])), None, "no selector found: kubectl decides");
        let found = K8sTarget { pods: Some("app=api".into()), ..t("deploy/api", &[]) };
        assert_eq!(watch(&found), Some(Watch::Labels("app=api".into())));
    }
}
