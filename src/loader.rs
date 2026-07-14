use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin},
    sync::{Arc, Weak, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;

use crate::{
    bridge::Bridge,
    protocol::{BRIDGE_PROTOCOL, FileInfo, MeshData, PlotData, read_plot},
};

const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PlotKey {
    pub path: PathBuf,
    pub variable: String,
    pub zone: usize,
    protocol: u32,
    size: u64,
    modified_ns: u128,
}

impl PlotKey {
    pub fn for_file(
        path: impl Into<PathBuf>,
        variable: String,
        zone: usize,
    ) -> Result<Self, String> {
        let requested_path = path.into();
        let path = requested_path
            .canonicalize()
            .map_err(|error| format!("resolving {}: {error}", requested_path.display()))?;
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("reading metadata for {}: {error}", path.display()))?;
        let modified_ns = metadata
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Ok(Self {
            path,
            variable,
            zone,
            protocol: BRIDGE_PROTOCOL,
            size: metadata.len(),
            modified_ns,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestPriority {
    Foreground,
    Overlay,
    Prefetch,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub used_bytes: usize,
    pub limit_bytes: usize,
}

pub enum LoaderEvent {
    Inspected {
        request_id: u64,
        epoch: u64,
        path: PathBuf,
        result: Result<FileInfo, String>,
    },
    Plot {
        request_id: u64,
        epoch: u64,
        key: PlotKey,
        priority: RequestPriority,
        from_cache: bool,
        result: Result<Arc<PlotData>, String>,
    },
    CacheStats(CacheStats),
}

enum LoaderCommand {
    Inspect(InspectJob),
    Plot(PlotJob),
    SetLimit(usize),
    Clear,
    CancelAuxiliary,
    Shutdown,
}

#[derive(Clone)]
struct InspectJob {
    request_id: u64,
    epoch: u64,
    path: PathBuf,
    attempt: u8,
}

#[derive(Clone)]
struct PlotJob {
    request_id: u64,
    epoch: u64,
    key: PlotKey,
    priority: RequestPriority,
    reuse_mesh: Option<Arc<MeshData>>,
    attempt: u8,
    mesh_retry: bool,
}

#[derive(Clone)]
enum Job {
    Inspect(InspectJob),
    Plot(PlotJob),
}

impl Job {
    fn request_id(&self) -> u64 {
        match self {
            Self::Inspect(job) => job.request_id,
            Self::Plot(job) => job.request_id,
        }
    }

    fn priority(&self) -> RequestPriority {
        match self {
            Self::Inspect(_) => RequestPriority::Foreground,
            Self::Plot(job) => job.priority,
        }
    }

    fn output_path(&self) -> Option<PathBuf> {
        match self {
            Self::Inspect(_) => None,
            Self::Plot(job) => Some(exchange_path(job.epoch, job.request_id)),
        }
    }
}

pub struct PlotLoader {
    sender: mpsc::Sender<LoaderCommand>,
    receiver: mpsc::Receiver<LoaderEvent>,
    next_request_id: u64,
    worker: Option<thread::JoinHandle<()>>,
}

impl PlotLoader {
    pub fn new(bridge: Bridge, limit_bytes: usize) -> Self {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let worker =
            thread::spawn(move || run_loader(bridge, limit_bytes, command_receiver, event_sender));
        Self {
            sender: command_sender,
            receiver: event_receiver,
            next_request_id: 1,
            worker: Some(worker),
        }
    }

    fn request_id(&mut self) -> u64 {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        request_id
    }

    pub fn inspect(&mut self, epoch: u64, path: PathBuf) -> u64 {
        let request_id = self.request_id();
        let _ = self.sender.send(LoaderCommand::Inspect(InspectJob {
            request_id,
            epoch,
            path,
            attempt: 0,
        }));
        request_id
    }

    pub fn load(
        &mut self,
        epoch: u64,
        key: PlotKey,
        priority: RequestPriority,
        reuse_mesh: Option<Arc<MeshData>>,
    ) -> u64 {
        let request_id = self.request_id();
        let _ = self.sender.send(LoaderCommand::Plot(PlotJob {
            request_id,
            epoch,
            key,
            priority,
            reuse_mesh,
            attempt: 0,
            mesh_retry: false,
        }));
        request_id
    }

    pub fn set_limit_bytes(&self, limit: usize) {
        let _ = self.sender.send(LoaderCommand::SetLimit(limit));
    }

    pub fn clear(&self) {
        let _ = self.sender.send(LoaderCommand::Clear);
    }

    pub fn cancel_auxiliary(&self) {
        let _ = self.sender.send(LoaderCommand::CancelAuxiliary);
    }

    pub fn try_recv(&self) -> Result<LoaderEvent, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for PlotLoader {
    fn drop(&mut self) {
        let _ = self.sender.send(LoaderCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Deserialize)]
struct BridgeError {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

#[derive(Deserialize)]
struct BridgeResponse {
    protocol: u32,
    id: u64,
    ok: bool,
    #[serde(default)]
    result: serde_json::Value,
    error: Option<BridgeError>,
}

enum ProcessMessage {
    Response {
        generation: u64,
        response: Result<BridgeResponse, String>,
    },
    Closed {
        generation: u64,
        error: Option<String>,
    },
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    generation: u64,
}

impl Server {
    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn shutdown(mut self) {
        let request = serde_json::json!({
            "protocol": BRIDGE_PROTOCOL,
            "id": 0,
            "method": "shutdown",
            "params": {},
        });
        let sent = serde_json::to_writer(&mut self.stdin, &request).is_ok()
            && self.stdin.write_all(b"\n").is_ok()
            && self.stdin.flush().is_ok();
        if sent {
            for _ in 0..20 {
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct CacheEntry {
    plot: Arc<PlotData>,
}

struct MeshUsage {
    bytes: usize,
    entries: usize,
}

struct PlotCache {
    entries: HashMap<PlotKey, CacheEntry>,
    order: VecDeque<PlotKey>,
    mesh_usage: HashMap<String, MeshUsage>,
    mesh_registry: HashMap<String, Weak<MeshData>>,
    scalar_bytes: usize,
    mesh_bytes: usize,
    limit_bytes: usize,
}

impl PlotCache {
    fn new(limit_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            mesh_usage: HashMap::new(),
            mesh_registry: HashMap::new(),
            scalar_bytes: 0,
            mesh_bytes: 0,
            limit_bytes,
        }
    }

    fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.entries.len(),
            used_bytes: self.scalar_bytes.saturating_add(self.mesh_bytes),
            limit_bytes: self.limit_bytes,
        }
    }

    fn get(&mut self, key: &PlotKey) -> Option<Arc<PlotData>> {
        let plot = self.entries.get(key)?.plot.clone();
        self.touch(key);
        Some(plot)
    }

    fn intern_mesh(&mut self, plot: &mut PlotData) {
        if let Some(existing) = self
            .mesh_registry
            .get(&plot.mesh.id)
            .and_then(Weak::upgrade)
        {
            plot.mesh = existing;
        } else {
            self.mesh_registry
                .insert(plot.mesh.id.clone(), Arc::downgrade(&plot.mesh));
        }
    }

    fn insert(&mut self, key: PlotKey, mut plot: PlotData) -> Arc<PlotData> {
        self.intern_mesh(&mut plot);
        let plot = Arc::new(plot);
        self.remove(&key);
        self.scalar_bytes = self.scalar_bytes.saturating_add(plot.scalar_bytes());
        let mesh = self
            .mesh_usage
            .entry(plot.mesh.id.clone())
            .or_insert_with(|| {
                self.mesh_bytes = self.mesh_bytes.saturating_add(plot.mesh.numeric_bytes());
                MeshUsage {
                    bytes: plot.mesh.numeric_bytes(),
                    entries: 0,
                }
            });
        mesh.entries += 1;
        self.entries
            .insert(key.clone(), CacheEntry { plot: plot.clone() });
        self.order.push_back(key);
        self.evict_to_limit();
        plot
    }

    fn touch(&mut self, key: &PlotKey) {
        if let Some(index) = self.order.iter().position(|candidate| candidate == key) {
            self.order.remove(index);
        }
        self.order.push_back(key.clone());
    }

    fn remove(&mut self, key: &PlotKey) {
        let Some(entry) = self.entries.remove(key) else {
            return;
        };
        self.scalar_bytes = self.scalar_bytes.saturating_sub(entry.plot.scalar_bytes());
        let mesh_id = &entry.plot.mesh.id;
        if let Some(usage) = self.mesh_usage.get_mut(mesh_id) {
            usage.entries = usage.entries.saturating_sub(1);
            if usage.entries == 0 {
                self.mesh_bytes = self.mesh_bytes.saturating_sub(usage.bytes);
                self.mesh_usage.remove(mesh_id);
            }
        }
        if let Some(index) = self.order.iter().position(|candidate| candidate == key) {
            self.order.remove(index);
        }
    }

    fn evict_to_limit(&mut self) {
        while self.stats().used_bytes > self.limit_bytes {
            let Some(key) = self.order.front().cloned() else {
                break;
            };
            self.remove(&key);
        }
        self.mesh_registry.retain(|_, mesh| mesh.strong_count() > 0);
    }

    fn set_limit(&mut self, limit_bytes: usize) {
        self.limit_bytes = limit_bytes;
        self.evict_to_limit();
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.mesh_usage.clear();
        self.mesh_registry.clear();
        self.scalar_bytes = 0;
        self.mesh_bytes = 0;
    }
}

fn run_loader(
    bridge: Bridge,
    limit_bytes: usize,
    commands: mpsc::Receiver<LoaderCommand>,
    events: mpsc::Sender<LoaderEvent>,
) {
    let (process_sender, process_receiver) = mpsc::channel();
    let mut cache = PlotCache::new(limit_bytes);
    let _ = events.send(LoaderEvent::CacheStats(cache.stats()));
    let mut server: Option<Server> = None;
    let mut server_generation = 0_u64;
    let mut current: Option<Job> = None;
    let mut foreground: Option<Job> = None;
    let mut overlay = VecDeque::<Job>::new();
    let mut prefetch = VecDeque::<Job>::new();

    loop {
        match commands.recv_timeout(POLL_INTERVAL) {
            Ok(LoaderCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(command) => handle_command(
                command,
                &events,
                &mut cache,
                &mut server,
                &mut current,
                &mut foreground,
                &mut overlay,
                &mut prefetch,
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        while let Ok(message) = process_receiver.try_recv() {
            handle_process_message(
                message,
                &events,
                &mut cache,
                &mut server,
                &mut current,
                &mut foreground,
                &mut overlay,
                &mut prefetch,
            );
        }
        if current.is_none() {
            let next = foreground
                .take()
                .or_else(|| overlay.pop_front())
                .or_else(|| prefetch.pop_front());
            if let Some(job) = next {
                if server.is_none() {
                    server_generation = server_generation.wrapping_add(1);
                    match start_server(&bridge, server_generation, process_sender.clone()) {
                        Ok(started) => server = Some(started),
                        Err(error) => {
                            finish_job_error(job, error, &events);
                            continue;
                        }
                    }
                }
                if let Some(active_server) = &mut server {
                    match send_job(active_server, &job) {
                        Ok(()) => current = Some(job),
                        Err(error) => {
                            let stopped = server.take().unwrap();
                            stopped.stop();
                            retry_or_fail(
                                job,
                                error,
                                &events,
                                &mut foreground,
                                &mut overlay,
                                &mut prefetch,
                            );
                        }
                    }
                }
            }
        }
    }
    if let Some(server) = server.take() {
        server.shutdown();
    }
    if let Some(job) = current {
        cleanup_job(&job);
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_command(
    command: LoaderCommand,
    events: &mpsc::Sender<LoaderEvent>,
    cache: &mut PlotCache,
    server: &mut Option<Server>,
    current: &mut Option<Job>,
    foreground: &mut Option<Job>,
    overlay: &mut VecDeque<Job>,
    prefetch: &mut VecDeque<Job>,
) {
    match command {
        LoaderCommand::Inspect(job) => {
            cancel_active(server, current);
            *foreground = Some(Job::Inspect(job));
            overlay.clear();
            prefetch.clear();
        }
        LoaderCommand::Plot(job) => {
            if let Some(plot) = cache.get(&job.key) {
                if job.priority == RequestPriority::Foreground {
                    cancel_active(server, current);
                    overlay.clear();
                    prefetch.clear();
                }
                let _ = events.send(LoaderEvent::Plot {
                    request_id: job.request_id,
                    epoch: job.epoch,
                    key: job.key,
                    priority: job.priority,
                    from_cache: true,
                    result: Ok(plot),
                });
                let _ = events.send(LoaderEvent::CacheStats(cache.stats()));
                return;
            }
            let queued = Job::Plot(job.clone());
            if job.priority == RequestPriority::Foreground {
                cancel_active(server, current);
                *foreground = Some(queued);
                overlay.clear();
                prefetch.clear();
            } else if job.priority == RequestPriority::Overlay {
                if !job_is_queued(&job.key, current, foreground, overlay, prefetch) {
                    if overlay.len() == 2 {
                        overlay.pop_front();
                    }
                    overlay.push_back(queued);
                }
            } else if !job_is_queued(&job.key, current, foreground, overlay, prefetch) {
                if prefetch.len() == 2 {
                    prefetch.pop_front();
                }
                prefetch.push_back(queued);
            }
        }
        LoaderCommand::SetLimit(limit) => {
            cache.set_limit(limit);
            let _ = events.send(LoaderEvent::CacheStats(cache.stats()));
        }
        LoaderCommand::Clear => {
            overlay.clear();
            prefetch.clear();
            if current
                .as_ref()
                .is_some_and(|job| job.priority() != RequestPriority::Foreground)
            {
                cancel_active(server, current);
            }
            cache.clear();
            let _ = events.send(LoaderEvent::CacheStats(cache.stats()));
        }
        LoaderCommand::CancelAuxiliary => {
            overlay.clear();
            prefetch.clear();
            if current
                .as_ref()
                .is_some_and(|job| job.priority() != RequestPriority::Foreground)
            {
                cancel_active(server, current);
            }
        }
        LoaderCommand::Shutdown => {}
    }
}

fn job_is_queued(
    key: &PlotKey,
    current: &Option<Job>,
    foreground: &Option<Job>,
    overlay: &VecDeque<Job>,
    prefetch: &VecDeque<Job>,
) -> bool {
    current
        .iter()
        .chain(foreground.iter())
        .chain(overlay.iter())
        .chain(prefetch.iter())
        .any(|job| matches!(job, Job::Plot(plot) if &plot.key == key))
}

fn cancel_active(server: &mut Option<Server>, current: &mut Option<Job>) {
    if let Some(job) = current.take() {
        if let Some(server) = server.take() {
            server.stop();
        }
        cleanup_job(&job);
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_process_message(
    message: ProcessMessage,
    events: &mpsc::Sender<LoaderEvent>,
    cache: &mut PlotCache,
    server: &mut Option<Server>,
    current: &mut Option<Job>,
    foreground: &mut Option<Job>,
    overlay: &mut VecDeque<Job>,
    prefetch: &mut VecDeque<Job>,
) {
    let generation = match &message {
        ProcessMessage::Response { generation, .. } | ProcessMessage::Closed { generation, .. } => {
            *generation
        }
    };
    if server.as_ref().map(|server| server.generation) != Some(generation) {
        return;
    }
    match message {
        ProcessMessage::Response { response, .. } => {
            let Some(job) = current.take() else { return };
            let result = response.and_then(|response| validate_response(&job, response));
            match (job, result) {
                (Job::Inspect(job), Ok(response)) => {
                    let result = serde_json::from_value(response.result)
                        .map_err(|error| format!("invalid inspect response: {error}"));
                    let _ = events.send(LoaderEvent::Inspected {
                        request_id: job.request_id,
                        epoch: job.epoch,
                        path: job.path,
                        result,
                    });
                }
                (Job::Plot(mut job), Ok(_response)) => {
                    let output = exchange_path(job.epoch, job.request_id);
                    let parsed = read_plot(&output, job.reuse_mesh.clone())
                        .map_err(|error| error.to_string());
                    cleanup_path(&output);
                    match parsed {
                        Ok(plot) => {
                            let plot = cache.insert(job.key.clone(), plot);
                            let _ = events.send(LoaderEvent::Plot {
                                request_id: job.request_id,
                                epoch: job.epoch,
                                key: job.key,
                                priority: job.priority,
                                from_cache: false,
                                result: Ok(plot),
                            });
                            let _ = events.send(LoaderEvent::CacheStats(cache.stats()));
                        }
                        Err(error)
                            if !job.mesh_retry
                                && error.contains("references a mesh that is not available") =>
                        {
                            job.reuse_mesh = None;
                            job.mesh_retry = true;
                            requeue(Job::Plot(job), foreground, overlay, prefetch);
                        }
                        Err(error) => finish_plot_error(job, error, events),
                    }
                }
                (job, Err(error)) => finish_job_error(job, error, events),
            }
        }
        ProcessMessage::Closed { error, .. } => {
            let Some(job) = current.take() else { return };
            if let Some(server) = server.take() {
                server.stop();
            }
            retry_or_fail(
                job,
                error.unwrap_or_else(|| "persistent bridge exited unexpectedly".into()),
                events,
                foreground,
                overlay,
                prefetch,
            );
        }
    }
}

fn validate_response(job: &Job, response: BridgeResponse) -> Result<BridgeResponse, String> {
    if response.protocol != BRIDGE_PROTOCOL {
        return Err(format!("unsupported bridge protocol {}", response.protocol));
    }
    if response.id != job.request_id() {
        return Err(format!(
            "bridge response {} did not match request {}",
            response.id,
            job.request_id()
        ));
    }
    if !response.ok {
        let error = response.error.map_or_else(
            || "bridge request failed".to_owned(),
            |error| format!("{}: {}", error.kind, error.message),
        );
        return Err(error);
    }
    Ok(response)
}

fn retry_or_fail(
    mut job: Job,
    error: String,
    events: &mpsc::Sender<LoaderEvent>,
    foreground: &mut Option<Job>,
    overlay: &mut VecDeque<Job>,
    prefetch: &mut VecDeque<Job>,
) {
    cleanup_job(&job);
    let attempt = match &mut job {
        Job::Inspect(job) => &mut job.attempt,
        Job::Plot(job) => &mut job.attempt,
    };
    if *attempt == 0 {
        *attempt = 1;
        requeue(job, foreground, overlay, prefetch);
    } else {
        finish_job_error(job, error, events);
    }
}

fn requeue(
    job: Job,
    foreground: &mut Option<Job>,
    overlay: &mut VecDeque<Job>,
    prefetch: &mut VecDeque<Job>,
) {
    match job.priority() {
        RequestPriority::Foreground => *foreground = Some(job),
        RequestPriority::Overlay => overlay.push_front(job),
        RequestPriority::Prefetch => prefetch.push_front(job),
    }
}

fn finish_job_error(job: Job, error: String, events: &mpsc::Sender<LoaderEvent>) {
    cleanup_job(&job);
    match job {
        Job::Inspect(job) => {
            let _ = events.send(LoaderEvent::Inspected {
                request_id: job.request_id,
                epoch: job.epoch,
                path: job.path,
                result: Err(error),
            });
        }
        Job::Plot(job) => finish_plot_error(job, error, events),
    }
}

fn finish_plot_error(job: PlotJob, error: String, events: &mpsc::Sender<LoaderEvent>) {
    let _ = events.send(LoaderEvent::Plot {
        request_id: job.request_id,
        epoch: job.epoch,
        key: job.key,
        priority: job.priority,
        from_cache: false,
        result: Err(error),
    });
}

fn start_server(
    bridge: &Bridge,
    generation: u64,
    sender: mpsc::Sender<ProcessMessage>,
) -> Result<Server, String> {
    let mut child = bridge.spawn_server().map_err(|error| error.to_string())?;
    let stdin = child.stdin.take().ok_or("persistent bridge has no stdin")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("persistent bridge has no stdout")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("persistent bridge has no stderr")?;
    let response_sender = sender.clone();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = response_sender.send(ProcessMessage::Closed {
                        generation,
                        error: None,
                    });
                    break;
                }
                Ok(_) => {
                    let response = serde_json::from_str(&line)
                        .map_err(|error| format!("invalid bridge response: {error}"));
                    let _ = response_sender.send(ProcessMessage::Response {
                        generation,
                        response,
                    });
                }
                Err(error) => {
                    let _ = response_sender.send(ProcessMessage::Closed {
                        generation,
                        error: Some(format!("reading persistent bridge output: {error}")),
                    });
                    break;
                }
            }
        }
    });
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("BATSView bridge: {line}");
        }
    });
    Ok(Server {
        child,
        stdin,
        generation,
    })
}

fn send_job(server: &mut Server, job: &Job) -> Result<(), String> {
    let request = match job {
        Job::Inspect(job) => serde_json::json!({
            "protocol": BRIDGE_PROTOCOL,
            "id": job.request_id,
            "method": "inspect",
            "params": {"path": job.path},
        }),
        Job::Plot(job) => {
            let output = exchange_path(job.epoch, job.request_id);
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    format!("creating exchange directory {}: {error}", parent.display())
                })?;
            }
            serde_json::json!({
                "protocol": BRIDGE_PROTOCOL,
                "id": job.request_id,
                "method": "load",
                "params": {
                    "path": job.key.path,
                    "variable": job.key.variable,
                    "zone": job.key.zone,
                    "output": output,
                    "cache": true,
                    "reuse_mesh_id": job.reuse_mesh.as_ref().map(|mesh| &mesh.id),
                },
            })
        }
    };
    serde_json::to_writer(&mut server.stdin, &request)
        .map_err(|error| format!("encoding bridge request: {error}"))?;
    server
        .stdin
        .write_all(b"\n")
        .and_then(|()| server.stdin.flush())
        .map_err(|error| format!("sending bridge request: {error}"))
}

fn exchange_path(epoch: u64, request_id: u64) -> PathBuf {
    std::env::temp_dir()
        .join("batsview-exchange")
        .join(format!("{}-{epoch}-{request_id}.bpv", std::process::id()))
}

fn cleanup_job(job: &Job) {
    if let Some(path) = job.output_path() {
        cleanup_path(&path);
    }
}

fn cleanup_path(path: &Path) {
    let _ = fs::remove_file(path);
    let temporary = path.with_extension("bpv.tmp");
    let _ = fs::remove_file(temporary);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{MeshData, PlotHeader, Position};

    fn key(name: &str) -> PlotKey {
        PlotKey {
            path: name.into(),
            variable: "rho".into(),
            zone: 0,
            protocol: BRIDGE_PROTOCOL,
            size: 1,
            modified_ns: 1,
        }
    }

    fn plot(mesh_id: &str, scalar_count: usize) -> PlotData {
        PlotData {
            header: PlotHeader {
                protocol: BRIDGE_PROTOCOL,
                path: "test.plt".into(),
                title: "test".into(),
                section: Some("z=0".into()),
                zone: "zone".into(),
                variable: "rho".into(),
                source_variable: "Rho".into(),
                unit: None,
                x_label: "X".into(),
                y_label: "Y".into(),
                point_count: scalar_count,
                triangle_count: 1,
                mesh_id: mesh_id.into(),
                mesh_included: true,
                bounds: [0.0, 1.0, 0.0, 1.0],
                value_range: [0.0, 1.0],
                positive_range: Some([0.1, 1.0]),
            },
            mesh: Arc::new(MeshData {
                id: mesh_id.into(),
                positions: vec![Position { x: 0.0, y: 0.0 }; scalar_count],
                indices: vec![0, 0, 0],
            }),
            values: vec![1.0; scalar_count],
        }
    }

    #[test]
    fn cache_touches_and_evicts_least_recently_used_entries() {
        let bytes_per_plot = plot("00000000000000000000000000000001", 4).scalar_bytes()
            + plot("00000000000000000000000000000001", 4)
                .mesh
                .numeric_bytes();
        let mut cache = PlotCache::new(bytes_per_plot + 4 * size_of::<f32>());
        cache.insert(key("a"), plot("00000000000000000000000000000001", 4));
        cache.insert(key("b"), plot("00000000000000000000000000000001", 4));
        assert!(cache.get(&key("a")).is_some());
        cache.insert(key("c"), plot("00000000000000000000000000000001", 4));
        assert!(cache.get(&key("b")).is_none());
        assert!(cache.get(&key("a")).is_some());
        assert!(cache.get(&key("c")).is_some());
    }

    #[test]
    fn shared_mesh_is_counted_once_and_limit_reduction_evicts() {
        let mut cache = PlotCache::new(usize::MAX);
        let first = cache.insert(key("a"), plot("00000000000000000000000000000001", 8));
        let second = cache.insert(key("b"), plot("00000000000000000000000000000001", 8));
        assert!(Arc::ptr_eq(&first.mesh, &second.mesh));
        let expected = first.mesh.numeric_bytes() + first.scalar_bytes() + second.scalar_bytes();
        assert_eq!(cache.stats().used_bytes, expected);
        cache.set_limit(0);
        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().used_bytes, 0);
    }

    #[test]
    fn oversized_plot_is_returned_but_not_retained() {
        let mut cache = PlotCache::new(1);
        let inserted = cache.insert(key("large"), plot("00000000000000000000000000000001", 8));
        assert_eq!(inserted.values.len(), 8);
        assert!(cache.get(&key("large")).is_none());
    }

    #[test]
    fn plot_key_uses_canonical_path_file_identity_and_protocol() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("frame.plt");
        fs::write(&path, b"first").unwrap();
        let first = PlotKey::for_file(&path, "rho".into(), 0).unwrap();
        assert_eq!(first.path, path.canonicalize().unwrap());
        assert_eq!(first.protocol, BRIDGE_PROTOCOL);

        fs::write(&path, b"second version").unwrap();
        let changed = PlotKey::for_file(&path, "rho".into(), 0).unwrap();
        assert_ne!(first, changed);
    }

    #[test]
    fn newest_foreground_wins_and_prefetch_queue_is_bounded() {
        let (events, _receiver) = mpsc::channel();
        let mut cache = PlotCache::new(usize::MAX);
        let mut server = None;
        let mut current = None;
        let mut foreground = None;
        let mut overlay = VecDeque::new();
        let mut prefetch = VecDeque::new();
        let job = |request_id, name: &str, priority| {
            LoaderCommand::Plot(PlotJob {
                request_id,
                epoch: 1,
                key: key(name),
                priority,
                reuse_mesh: None,
                attempt: 0,
                mesh_retry: false,
            })
        };

        handle_command(
            job(1, "foreground-a", RequestPriority::Foreground),
            &events,
            &mut cache,
            &mut server,
            &mut current,
            &mut foreground,
            &mut overlay,
            &mut prefetch,
        );
        handle_command(
            job(2, "foreground-b", RequestPriority::Foreground),
            &events,
            &mut cache,
            &mut server,
            &mut current,
            &mut foreground,
            &mut overlay,
            &mut prefetch,
        );
        assert_eq!(foreground.as_ref().map(Job::request_id), Some(2));

        for (request_id, name) in [(3, "previous"), (4, "next"), (5, "new-next")] {
            handle_command(
                job(request_id, name, RequestPriority::Prefetch),
                &events,
                &mut cache,
                &mut server,
                &mut current,
                &mut foreground,
                &mut overlay,
                &mut prefetch,
            );
        }
        assert_eq!(prefetch.len(), 2);
        let ids: Vec<_> = prefetch.iter().map(Job::request_id).collect();
        assert_eq!(ids, [4, 5]);

        for (request_id, name) in [(6, "vector-x"), (7, "vector-y")] {
            handle_command(
                job(request_id, name, RequestPriority::Overlay),
                &events,
                &mut cache,
                &mut server,
                &mut current,
                &mut foreground,
                &mut overlay,
                &mut prefetch,
            );
        }
        assert_eq!(overlay.len(), 2);
        let overlay_ids: Vec<_> = overlay.iter().map(Job::request_id).collect();
        assert_eq!(overlay_ids, [6, 7]);
        assert_eq!(prefetch.len(), 2);
    }
}
