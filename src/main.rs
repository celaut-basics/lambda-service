//! lambda-service: the parent.
//!
//! It takes a Python function and an input over HTTP, runs the function in a MicroPython
//! child service, and answers with the result. The child has no server and no network
//! client: the two talk only through `/lambda`, the directory that this service exports to
//! its children (nodo's parent -> child shared filesystem).
//!
//! The child that runs a function is built here, from the packed child that comes with
//! this service (its dependency `LAMBDA_CHILD`) plus one file, `/fn.py`, with the code. So
//! the id of the child that runs a function is a hash of that function (and of the
//! interpreter and the entry script): the same code always runs as the same service id,
//! and anyone can compute that id from the code (`POST /id`). With `"mode": "generic"`
//! the packed child runs instead and reads the code from the share; its id is then the
//! same for every function.

mod wire;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bee_rpc::frame::{parse_all, serialize_part, ParseOptions, Part};
use bee_rpc::hash::HashAlgorithm;
use celaut_proto::celaut::metadata::hash_tag::Hash;
use celaut_proto::celaut::metadata::HashTag;
use celaut_proto::celaut::service::container::filesystem::item_branch::Item;
use celaut_proto::celaut::service::container::filesystem::ItemBranch;
use celaut_proto::celaut::service::container::Filesystem;
use celaut_proto::celaut::{Amount, BytesKeyValue, Configuration, EstimatedCost, Metadata};
use prost::Message;
use serde_json::{json, Value};
use service_library::config::NodeConfig;
use service_library::gateway::{Gateway, Node, SHA3_256_TYPE};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What the parent exports to its children (service.json: role "shared", tag "lambda").
const SHARE: &str = "/lambda";
/// Where the Dockerfile puts the dependency directories that the packer adds.
const APP: &str = "/app";
const PORT: u16 = 8080;
const MAX_BODY: usize = 1 << 20;
const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 600;
/// st_mode of a regular file with mode 0644.
const FILE_MODE: &str = "33188";

/// The packed child, read once at the start.
struct Base {
    id: [u8; 32],
    /// The Service, expanded (every block in place): the bytes that its id hashes.
    service: Vec<u8>,
    /// The attributes for `/fn.py`: those of `/lambda.py`, with the mode of a plain file.
    xattrs: Vec<BytesKeyValue>,
}

struct State {
    gateway: Gateway,
    base: Base,
    /// The children that the node has: they start by hash.
    known: Mutex<HashSet<[u8; 32]>>,
    /// The tokens of the children that run now, by job, to stop them at a notice.
    running: Mutex<HashMap<String, String>>,
    jobs: Mutex<u64>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

fn sha3(bytes: &[u8]) -> [u8; 32] {
    HashAlgorithm::Sha3_256.digest(bytes).try_into().expect("32 bytes")
}

/// Read the packed child from the dependency directories, and check that its bytes hash
/// to its id: the ids built from it are only right if this is.
fn load_base() -> Result<Base, String> {
    let deps = std::fs::read_to_string(format!("{APP}/.dependencies"))
        .map_err(|e| format!("{APP}/.dependencies: {e}"))?;
    let id_hex = deps
        .lines()
        .find_map(|l| l.strip_prefix("LAMBDA_CHILD="))
        .ok_or("no LAMBDA_CHILD in .dependencies")?
        .trim()
        .to_string();
    let id = unhex(&id_hex).ok_or("LAMBDA_CHILD is not a SHA3-256 id")?;
    let store = bee_rpc::store::Store::new(format!("{APP}/__block__"));
    let buffers = store
        .read_message(6, format!("{APP}/__services__/{id_hex}"))
        .map_err(|e| format!("reading the child: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("reading the child: {e}"))?;
    let service = parse_all(buffers, ParseOptions::default())
        .map_err(|e| format!("parsing the child: {e}"))?
        .into_iter()
        .find(|p| p.index == 6)
        .ok_or("the child has no Service")?
        .data;
    if sha3(&service) != id {
        return Err(format!(
            "the child does not hash to its id: {} != {id_hex}",
            hex(&sha3(&service))
        ));
    }
    let filesystem = Filesystem::decode(wire::get(wire::get(&service, 2)?, 2)?)
        .map_err(|e| format!("the filesystem of the child: {e}"))?;
    if filesystem.branch.iter().any(|b| b.name == "fn.py") {
        return Err("the packed child already has /fn.py".into());
    }
    let entry = filesystem
        .branch
        .iter()
        .find(|b| b.name == "lambda.py")
        .ok_or("the child has no /lambda.py")?;
    let xattrs = entry
        .xattrs
        .iter()
        .map(|kv| {
            let mut kv = kv.clone();
            if kv.key == "mode" {
                kv.value = Some(FILE_MODE.as_bytes().to_vec());
            }
            kv
        })
        .collect();
    Ok(Base { id, service, xattrs })
}

/// The Service of the child that runs `code`, and its id: the packed child plus `/fn.py`.
/// Only the filesystem changes, by bytes: the rest of the Service is as it was packed.
fn child_for(base: &Base, code: &str) -> Result<(Vec<u8>, [u8; 32]), String> {
    let container = wire::get(&base.service, 2)?;
    let filesystem = wire::get(container, 2)?;
    let branch = ItemBranch {
        name: "fn.py".into(),
        item: Some(Item::File(code.as_bytes().to_vec())),
        xattrs: base.xattrs.clone(),
        // celaut.proto 4b7533fd (nodo 66721709): no policies, so the bytes are as before.
        policies: Vec::new(),
    };
    let mut new_filesystem = filesystem.to_vec();
    new_filesystem.extend(wire::field(1, &branch.encode_to_vec()));
    let new_container = wire::replace(container, 2, &new_filesystem)?;
    let service = wire::replace(&base.service, 2, &new_container)?;
    let id = sha3(&service);
    Ok((service, id))
}

/// A `.bee` file with the Metadata and the Service, for `Node::start_package`.
fn write_package(service: &[u8], id: [u8; 32]) -> Result<PathBuf, String> {
    let metadata = Metadata {
        hashtag: Some(HashTag {
            hash: vec![Hash { r#type: SHA3_256_TYPE.to_vec(), value: id.to_vec() }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let path = PathBuf::from(format!("/tmp/{}.bee", hex(&id)));
    let mut buffers: Vec<_> = serialize_part(&Part::new(5, metadata.encode_to_vec())).collect();
    buffers.extend(serialize_part(&Part::new(6, service.to_vec())));
    let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    bee_rpc::bee_file::write_bee(&mut file, buffers).map_err(|e| e.to_string())?;
    Ok(path)
}

fn kv(key: &str, value: &str) -> BytesKeyValue {
    BytesKeyValue { key: key.into(), value: Some(value.as_bytes().to_vec()) }
}

/// The most that the start of a child may cost: the start cost (with the build of a
/// child that the node has not built yet) and enough maintenance periods for `timeout`,
/// both doubled because the price can move. What the child does not spend comes back at
/// the stop. Without it the node funds a child for its default number of periods
/// (hours), which can be more than the parent holds.
fn budget(quote: &EstimatedCost, timeout: Duration) -> Result<u128, String> {
    let mu = |a: &Option<Amount>| service_library::gateway::amount(a.as_ref()).map_err(|e| e.to_string());
    let cost = mu(&quote.cost)?;
    let period = mu(&quote.init_maintenance_cost)?.max(mu(&quote.max_maintenance_cost)?);
    let loop_secs = u64::try_from(quote.maintenance_seconds_loop).unwrap_or(0).max(1);
    let periods = u128::from(timeout.as_secs().div_ceil(loop_secs) + 1);
    Ok(2 * cost + 2 * period * periods)
}

/// `GetServiceEstimatedCost` with the service in the call, for a child that the node does
/// not have yet: the node keeps the service and quotes it, build included, so the start
/// that follows goes by hash.
async fn quote_package(gateway: &Gateway, path: &Path) -> Result<EstimatedCost, String> {
    use tokio_stream::StreamExt;
    let info = service_library::package::inspect(path).map_err(|e| e.to_string())?;
    let head = service_library::gateway::start_parts(info.service_id, &Configuration::default());
    let file: service_library::relay::Buffers = Box::pin(
        service_library::package::read_package(path.to_path_buf(), &info).map(|item| {
            item.map_err(|e| service_library::Error::Protocol(format!("the package: {e}")))
        }),
    );
    let skips = service_library::skips::Skips::new();
    let answer = service_library::relay::call_with_service(
        gateway.channel(),
        "/celaut.Gateway/GetServiceEstimatedCost",
        head,
        skips.wrap(file),
        &[1],
        service_library::gateway::START_TIMEOUT,
        service_library::relay::EARLY_ANSWER_WAIT,
        &skips,
    )
    .await
    .map_err(|e| e.to_string())?;
    let part = answer.iter().find(|p| p.index == 1).ok_or("the node gave no EstimatedCost")?;
    EstimatedCost::decode(part.data.as_slice()).map_err(|e| e.to_string())
}

async fn invoke(state: &State, request: &Value) -> Value {
    let t0 = Instant::now();
    let code = match request.get("code").and_then(Value::as_str) {
        Some(c) if !c.is_empty() => c.to_string(),
        _ => return json!({"ok": false, "error": "`code` (the Python source, with handler(event)) is required"}),
    };
    let input = request.get("input").cloned().unwrap_or(Value::Null);
    let generic = match request.get("mode").and_then(Value::as_str) {
        None | Some("built-in") => false,
        Some("generic") => true,
        Some(other) => return json!({"ok": false, "error": format!("unknown mode {other:?}: built-in or generic")}),
    };
    let timeout = Duration::from_secs(
        request
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS),
    );

    let job = {
        let mut n = state.jobs.lock().unwrap();
        *n += 1;
        format!("j{}-{}", *n, std::process::id())
    };
    let dir = Path::new(SHARE).join(&job);
    let prepared = (|| -> Result<_, String> {
        std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::write(dir.join("input.json"), input.to_string()).map_err(|e| e.to_string())?;
        if generic {
            std::fs::write(dir.join("fn.py"), &code).map_err(|e| e.to_string())?;
            Ok((None, state.base.id))
        } else {
            let (service, id) = child_for(&state.base, &code)?;
            Ok((Some(service), id))
        }
    })();
    let (service, id) = match prepared {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return json!({"ok": false, "error": e});
        }
    };

    // A child that the node does not have yet is sent with its quote; then every start
    // goes by hash.
    let known = state.known.lock().unwrap().contains(&id) || id == state.base.id;
    let t_start = Instant::now();
    let quote = if known {
        state
            .gateway
            .estimated_cost(id, Configuration::default())
            .await
            .map_err(|e| e.to_string())
    } else {
        match write_package(service.as_deref().unwrap_or_default(), id) {
            Ok(path) => {
                let quote = quote_package(&state.gateway, &path).await;
                let _ = std::fs::remove_file(&path);
                quote
            }
            Err(e) => Err(e),
        }
    };
    let quote_ms = t_start.elapsed().as_millis();
    let budget = match request.get("initial_mu").and_then(Value::as_u64) {
        Some(mu) => Some(u128::from(mu)),
        None => match quote.as_ref().map_err(|e| e.clone()).and_then(|q| budget(q, timeout)) {
            Ok(mu) => Some(mu),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return json!({"ok": false, "error": format!("quoting the child: {e}"), "child_service_id": hex(&id)});
            }
        },
    };
    let configuration = Configuration {
        environment_variables: vec![
            kv("LAMBDA_JOB", &job),
            kv("__nodo_instance_name", &format!("lam-{job}")),
        ],
        initial_mu: budget.map(|mu| Amount { n: mu.to_string() }),
        ..Default::default()
    };
    let t_start = Instant::now();
    let started = state.gateway.start(id, configuration).await;
    let instance = match started {
        Ok(i) => i,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return json!({"ok": false, "error": format!("starting the child: {e}"), "child_service_id": hex(&id)});
        }
    };
    let start_ms = t_start.elapsed().as_millis();
    state.known.lock().unwrap().insert(id);
    state.running.lock().unwrap().insert(job.clone(), instance.token.clone());

    let output = dir.join("output.json");
    let result = loop {
        if let Ok(text) = std::fs::read_to_string(&output) {
            break serde_json::from_str::<Value>(&text)
                .unwrap_or_else(|e| json!({"ok": false, "error": format!("the child wrote bad JSON: {e}")}));
        }
        if t0.elapsed() > timeout {
            break json!({"ok": false, "error": format!("no result in {} s", timeout.as_secs())});
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let result_ms = t0.elapsed().as_millis();

    // The child waits for its job directory to go, then exits; the stop ends it at once.
    let _ = std::fs::remove_dir_all(&dir);
    state.running.lock().unwrap().remove(&job);
    let stop = match state.gateway.stop_with_retries(&instance.token).await {
        Ok(refund) => json!({"stopped": true, "refund_mu": refund.to_string()}),
        Err(e) => json!({"stopped": false, "error": e.to_string()}),
    };

    let mut answer = json!({
        "child_service_id": hex(&id),
        "mode": if generic { "generic" } else { "built-in" },
        "timings_ms": {"quote": quote_ms, "start_service": start_ms, "until_result": result_ms, "total": t0.elapsed().as_millis()},
        "stop": stop,
        "initial_mu": budget.map(|mu| mu.to_string()),
    });
    if let (Value::Object(a), Value::Object(r)) = (&mut answer, result) {
        for (k, v) in r {
            match k.as_str() {
                "ok" | "output" | "error" | "traceback" => a.insert(k, v),
                _ => a.entry("child").or_insert_with(|| json!({})).as_object_mut().unwrap().insert(k, v),
            };
        }
    }
    answer
}

async fn route(state: &State, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let parse = || serde_json::from_slice::<Value>(body);
    match (method, path) {
        ("GET", "/health") => (200, json!({"ok": true, "generic_child_id": hex(&state.base.id)})),
        ("POST", "/id") => match parse() {
            Ok(req) => match req.get("code").and_then(Value::as_str) {
                Some(code) => match child_for(&state.base, code) {
                    Ok((_, id)) => (200, json!({"child_service_id": hex(&id)})),
                    Err(e) => (500, json!({"error": e})),
                },
                None => (400, json!({"error": "`code` is required"})),
            },
            Err(e) => (400, json!({"error": format!("the body is not JSON: {e}")})),
        },
        ("POST", "/invoke") => match parse() {
            Ok(req) => {
                let answer = invoke(state, &req).await;
                (200, answer)
            }
            Err(e) => (400, json!({"error": format!("the body is not JSON: {e}")})),
        },
        _ => (404, json!({"error": "GET /health, POST /id, POST /invoke"})),
    }
}

/// One HTTP/1.1 request and its answer, then the connection closes.
async fn serve(state: Arc<State>, mut stream: TcpStream) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 16384 {
            return;
        }
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut words = head.split_whitespace();
    let (method, path) = (words.next().unwrap_or("").to_string(), words.next().unwrap_or("").to_string());
    let length = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    let (status, answer) = if length > MAX_BODY {
        (413, json!({"error": "the body is larger than 1 MiB"}))
    } else {
        while buf.len() < header_end + length {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        route(&state, &method, &path, &buf[header_end..header_end + length]).await
    };
    let body = answer.to_string();
    let reason = match status { 200 => "OK", 400 => "Bad Request", 404 => "Not Found", 413 => "Payload Too Large", _ => "Error" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

/// The node writes a notice to /dev/hvc0 before it stops this instance: stop the
/// children that run, and exit, so the node does not wait.
async fn on_notice(state: Arc<State>) {
    let mut notices = match service_library::notice::watch_device(service_library::notice::DEVICE).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("lambda: no notices: {e}");
            return;
        }
    };
    if let Some(notice) = notices.next().await {
        eprintln!("lambda: notice {:?}, stopping the children", notice.verb);
        let tokens: Vec<String> = state.running.lock().unwrap().values().cloned().collect();
        for token in tokens {
            let _ = state.gateway.stop_with_retries(&token).await;
        }
        std::process::exit(0);
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let config = NodeConfig::load().unwrap_or_else(|e| {
        eprintln!("lambda: the configuration of the node: {e}");
        std::process::exit(1)
    });
    let gateway = Gateway::new(&config.gateway).unwrap_or_else(|e| {
        eprintln!("lambda: the gateway: {e}");
        std::process::exit(1)
    });
    let base = tokio::task::spawn_blocking(load_base).await.expect("load").unwrap_or_else(|e| {
        eprintln!("lambda: the packed child: {e}");
        std::process::exit(1)
    });
    eprintln!("lambda: generic child {}, gateway {}", hex(&base.id), config.gateway);
    let state = Arc::new(State {
        gateway,
        base,
        known: Mutex::new(HashSet::new()),
        running: Mutex::new(HashMap::new()),
        jobs: Mutex::new(0),
    });
    tokio::spawn(on_notice(state.clone()));
    let listener = TcpListener::bind(("0.0.0.0", PORT)).await.unwrap_or_else(|e| {
        eprintln!("lambda: port {PORT}: {e}");
        std::process::exit(1)
    });
    eprintln!("lambda: listening on {PORT}");
    loop {
        if let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(state.clone(), stream));
        }
    }
}
