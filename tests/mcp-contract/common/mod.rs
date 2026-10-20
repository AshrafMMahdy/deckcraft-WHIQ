//! A small MCP client for the contract tests: drives the built `deckcraft-cli mcp` over stdio,
//! one JSON-RPC line at a time, and checks the contract's invariants on every answer.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The error codes every craft app answers with (CONTRACT-PARITY.md): no other code may appear.
pub const CODES: &[&str] = &[
    "busy",
    "cancelled",
    "disk_full",
    "evicted",
    "failed",
    "internal",
    "invalid_argument",
    "invalid_request",
    "locked",
    "no_document",
    "not_available",
    "not_found",
    "out_of_range",
    "outside_root",
    "password_required",
    "too_large",
    "unsupported",
    "wrong_password",
];

/// The binary under test: `$DECKCRAFT_CLI` (e.g. a release build), else the one cargo built for
/// this test (the same override WordCraft's and GridCraft's suites take).
pub fn bin() -> PathBuf {
    std::env::var_os("DECKCRAFT_CLI").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_deckcraft-cli")))
}

/// A fresh scratch folder for one test.
pub fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dc-mcp-contract-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Canonical, so it compares equal to the roots the server reports.
    plain_path(dir.canonicalize().unwrap())
}

pub struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Value>,
    next: u64,
    stash: Vec<Value>,
    pub stderr_path: PathBuf,
    /// Every answer seen, for global checks.
    pub seen: Vec<Value>,
}

impl Mcp {
    /// `deckcraft-cli mcp <args>` with `env`, stderr to a file in `dir`, then `initialize`.
    pub fn start(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Mcp {
        let stderr_path = dir.join(format!("stderr-{}.txt", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let err = std::fs::File::create(&stderr_path).unwrap();
        let mut cmd = Command::new(bin());
        cmd.arg("mcp").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::from(err)).current_dir(dir);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && tx.send(v).is_err()
                {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        let mut m = Mcp { child, stdin, rx, next: 1, stash: vec![], stderr_path, seen: vec![] };
        let init = m.request("initialize", json!({"protocolVersion": "2025-06-18"}));
        assert_eq!(init["result"]["serverInfo"]["name"], "deckcraft", "{init}");
        m.notify("notifications/initialized");
        m
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn notify(&mut self, method: &str) {
        let line = json!({"jsonrpc": "2.0", "method": method}).to_string();
        self.write(&line);
    }

    pub fn notify_with(&mut self, method: &str, params: Value) {
        let line = json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string();
        self.write(&line);
    }

    fn write(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// Send a request without waiting; returns its id.
    pub fn send(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        self.write(&line);
        id
    }

    /// Send `line` as it is (not necessarily JSON) and wait for the answer without an id.
    pub fn raw(&mut self, line: &str) -> Value {
        self.write(line);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(v) if v["id"].is_null() && v.get("method").is_none() => return v,
                Ok(v) => self.stash.push(v),
                Err(e) => panic!("no answer to a raw line: {e:?}; stderr: {}", self.stderr()),
            }
        }
    }

    pub fn send_call(&mut self, tool: &str, args: Value) -> u64 {
        self.send("tools/call", json!({"name": tool, "arguments": args}))
    }

    /// The reply to `id`, within `timeout`.
    pub fn recv(&mut self, id: u64, timeout: Duration) -> Value {
        if let Some(i) = self.stash.iter().position(|v| v["id"] == json!(id)) {
            return self.stash.remove(i);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(v) => {
                    self.check(&v);
                    self.seen.push(v.clone());
                    if v["id"] == json!(id) {
                        return v;
                    }
                    self.stash.push(v);
                }
                Err(RecvTimeoutError::Timeout) => panic!("no reply to request {id} within {timeout:?}; stderr: {}", self.stderr()),
                Err(RecvTimeoutError::Disconnected) => panic!("server closed before replying to {id}; stderr: {}", self.stderr()),
            }
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.recv(id, Duration::from_secs(120))
    }

    /// `tools/call` → its `CallToolResult`.
    pub fn call(&mut self, tool: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}));
        r["result"].clone()
    }

    /// A successful call's `structuredContent`.
    pub fn ok(&mut self, tool: &str, args: Value) -> Value {
        let r = self.call(tool, args.clone());
        assert_eq!(r["isError"], false, "{tool} {args} failed: {r}");
        r["structuredContent"].clone()
    }

    /// A failed call's `structuredContent.error`, checked for the contract's shape.
    pub fn err(&mut self, tool: &str, args: Value) -> Value {
        let r = self.call(tool, args.clone());
        assert_eq!(r["isError"], true, "{tool} {args} should have failed: {r}");
        r["structuredContent"]["error"].clone()
    }

    /// The contract's invariants on every answer: a tool result has structuredContent (an
    /// object) and, when it is an error, a code (one of [`CODES`]), a reason and a next step.
    fn check(&self, v: &Value) {
        let Some(r) = v.get("result") else { return };
        if r.get("content").is_none() {
            return; // not a tool call
        }
        let sc = &r["structuredContent"];
        assert!(sc.is_object(), "every tool result carries structuredContent: {v}");
        if r["isError"] == true {
            let e = &sc["error"];
            for k in ["code", "reason", "next_step"] {
                assert!(e[k].as_str().is_some_and(|s| !s.is_empty()), "error without `{k}`: {v}");
            }
            let c = e["code"].as_str().unwrap_or("");
            assert!(CODES.contains(&c), "`{c}` is not a shared error code: {v}");
        }
    }

    pub fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }

    /// Close stdin and wait for the server to end.
    pub fn close(mut self) -> (bool, String) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                return (st.success(), self.stderr());
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("server did not exit after its input closed; stderr: {}", self.stderr());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// SIGTERM (unix), then wait.
    #[cfg(unix)]
    pub fn terminate(mut self) -> (bool, String) {
        let st = Command::new("kill").arg("-TERM").arg(self.child.id().to_string()).status().unwrap();
        assert!(st.success());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                return (st.success(), self.stderr());
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("server did not exit on SIGTERM; stderr: {}", self.stderr());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Mcp {
    /// A hard kill (SIGKILL on unix, `TerminateProcess` on Windows): nothing runs on the way out.
    pub fn kill(mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stderr()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
    }
}

/// Files in `dir` whose name contains `needle`.
pub fn files_with(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().contains(needle))
        // Not an atomic write's temporary file (`<name>.<pid>.<n>.saving`).
        .filter(|p| needle.contains(".saving") || !p.to_string_lossy().ends_with(".saving"))
        .collect();
    v.sort();
    v
}

/// One part of a zip package as text.
pub fn zip_text(path: &Path, part: &str) -> String {
    let f = std::fs::File::open(path).unwrap();
    let mut z = zip::ZipArchive::new(f).unwrap();
    let mut s = String::new();
    std::io::Read::read_to_string(&mut z.by_name(part).unwrap(), &mut s).unwrap();
    s
}

pub fn zip_names(path: &Path) -> Vec<String> {
    let f = std::fs::File::open(path).unwrap();
    let z = zip::ZipArchive::new(f).unwrap();
    let mut v: Vec<String> = z.file_names().map(String::from).collect();
    v.sort();
    v
}

pub fn zip_bytes(path: &Path, part: &str) -> Vec<u8> {
    let f = std::fs::File::open(path).unwrap();
    let mut z = zip::ZipArchive::new(f).unwrap();
    let mut b = vec![];
    std::io::Read::read_to_end(&mut z.by_name(part).unwrap(), &mut b).unwrap();
    b
}

/// Width and height from a PNG's IHDR.
pub fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert!(bytes.starts_with(b"\x89PNG"), "not a PNG");
    let be = |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    (be(16), be(20))
}

/// The canonical path in the form the server reports it: on Windows, canonicalize gives the verbatim `\\?\C:\...` form, while
/// the server (and the person reading its answers) uses the plain `C:\...` path to the same folder. A UNC path keeps its prefix.
pub fn plain_path(p: PathBuf) -> PathBuf {
    if let Some(rest) = p.to_str().and_then(|s| s.strip_prefix(r"\\?\"))
        && !rest.starts_with(r"UNC\")
    {
        return PathBuf::from(rest);
    }
    p
}
