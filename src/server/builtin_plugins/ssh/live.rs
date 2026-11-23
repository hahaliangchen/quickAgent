use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

use tokio::sync::broadcast;

const CHANNELS: usize = 32;
const REPLAY_BYTES: usize = 256 * 1024;
const CHUNK_BYTES: usize = 8 * 1024;

#[derive(Clone)]
pub(super) enum LiveMessage {
    Bytes(Arc<[u8]>),
    Activity {
        action: &'static str,
        remote_path: Option<String>,
        detail: String,
    },
}

struct Channel {
    touched: u64,
    replay: VecDeque<u8>,
    sender: broadcast::Sender<LiveMessage>,
}

#[derive(Default)]
struct Hub {
    clock: u64,
    channels: HashMap<(String, String), Channel>,
}

impl Hub {
    fn channel(&mut self, task: &str, server: &str) -> &mut Channel {
        let key = (task.to_owned(), server.to_owned());
        if self.channels.len() >= CHANNELS && !self.channels.contains_key(&key) {
            let oldest = self
                .channels
                .iter()
                .min_by_key(|(_, channel)| channel.touched)
                .map(|(key, _)| key.clone())
                .unwrap();
            self.channels.remove(&oldest);
        }
        self.clock += 1;
        let channel = self.channels.entry(key).or_insert_with(|| Channel {
            touched: 0,
            replay: VecDeque::new(),
            sender: broadcast::channel(256).0,
        });
        channel.touched = self.clock;
        channel
    }
}

impl Channel {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(CHUNK_BYTES) {
            let excess = (self.replay.len() + chunk.len()).saturating_sub(REPLAY_BYTES);
            self.replay.drain(..excess);
            self.replay.extend(chunk);
            let _ = self.sender.send(LiveMessage::Bytes(Arc::from(chunk)));
        }
    }

    fn activity(&self, action: &'static str, remote_path: Option<String>, detail: String) {
        let _ = self.sender.send(LiveMessage::Activity {
            action,
            remote_path,
            detail,
        });
    }
}

fn hub() -> &'static Mutex<Hub> {
    static HUB: OnceLock<Mutex<Hub>> = OnceLock::new();
    HUB.get_or_init(Mutex::default)
}

pub(super) fn subscribe(task: &str, server: &str) -> (Vec<u8>, broadcast::Receiver<LiveMessage>) {
    let mut hub = hub().lock().unwrap();
    let channel = hub.channel(task, server);
    // The snapshot and receiver share the lock with publishers: no duplicate or lost boundary chunk.
    (
        channel.replay.iter().copied().collect(),
        channel.sender.subscribe(),
    )
}

pub(super) fn emit_exec_start(task: &str, server: &str, cwd: Option<&str>, command: &str) {
    let cwd = inline(cwd.unwrap_or("."));
    let command = inline(command);
    let line = format!("\r\n\x1b[36m[AI · {cwd}]\x1b[0m $ {command}\r\n");
    emit_exec_chunk(task, server, line.as_bytes());
}

pub(super) fn emit_exec_chunk(task: &str, server: &str, bytes: &[u8]) {
    if !bytes.is_empty() {
        hub().lock().unwrap().channel(task, server).write(bytes);
    }
}

pub(super) fn emit_exec_end(
    task: &str,
    server: &str,
    cwd: Option<String>,
    code: Option<u32>,
    elapsed_ms: u128,
) {
    let outcome = code.map_or_else(
        || "no exit status".to_owned(),
        |code| format!("exit {code}"),
    );
    let color = if code == Some(0) { 32 } else { 31 };
    let detail = format!("{outcome} · {elapsed_ms} ms");
    let mut hub = hub().lock().unwrap();
    let channel = hub.channel(task, server);
    channel.write(format!("\r\n\x1b[{color}m[{detail}]\x1b[0m\r\n").as_bytes());
    channel.activity("exec", cwd, detail);
}

pub(super) fn emit_sftp_event(
    task: &str,
    server: &str,
    action: &'static str,
    path: String,
    detail: String,
) {
    let line = format!("\x1b[33m[AI SFTP]\x1b[0m {}\r\n", inline(&detail));
    let mut hub = hub().lock().unwrap();
    let channel = hub.channel(task, server);
    channel.write(line.as_bytes());
    channel.activity(action, Some(path), detail);
}

fn inline(text: &str) -> String {
    text.chars()
        .flat_map(|ch| {
            if ch.is_control() {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}

/// Per output stream: hold back only possible credential prefixes, including across SSH packets.
#[derive(Default)]
pub(super) struct DisplayFilter {
    pending: VecDeque<u8>,
    pem: bool,
    footer: bool,
    last_cr: bool,
}

impl DisplayFilter {
    pub(super) fn push(&mut self, input: &[u8], secret: Option<&[u8]>) -> Vec<u8> {
        const BEGIN: &[u8] = b"-----BEGIN ";
        const END: &[u8] = b"-----END ";
        let mut out = Vec::with_capacity(input.len());
        for &byte in input {
            if self.footer {
                if byte == b'\n' {
                    self.footer = false;
                    self.pem = false;
                    self.write(byte, &mut out);
                }
                continue;
            }
            self.pending.push_back(byte);
            loop {
                let pending = self.pending.make_contiguous();
                if pending.is_empty() {
                    break;
                }
                let marker = if self.pem { END } else { BEGIN };
                if pending == marker {
                    self.pending.clear();
                    if self.pem {
                        self.footer = true;
                    } else {
                        self.pem = true;
                        out.extend(b"[redacted PEM]");
                        self.last_cr = false;
                    }
                    break;
                }
                if !self.pem && secret == Some(pending) {
                    self.pending.clear();
                    out.extend(b"[redacted]");
                    self.last_cr = false;
                    break;
                }
                if marker.starts_with(pending)
                    || (!self.pem && secret.is_some_and(|secret| secret.starts_with(pending)))
                {
                    break;
                }
                let byte = self.pending.pop_front().unwrap();
                if !self.pem {
                    self.write(byte, &mut out);
                }
            }
        }
        out
    }

    fn write(&mut self, byte: u8, out: &mut Vec<u8>) {
        if byte == b'\n' && !self.last_cr {
            out.push(b'\r');
        }
        out.push(byte);
        self.last_cr = byte == b'\r';
    }

    pub(super) fn finish(self) -> Vec<u8> {
        // Never publish an incomplete match (e.g. an interrupted private key or password).
        if self.pem || self.pending.is_empty() {
            Vec::new()
        } else {
            b"[redacted]".to_vec()
        }
    }
}

pub(super) fn redact_text(text: &str, secret: Option<&[u8]>) -> String {
    let mut filter = DisplayFilter::default();
    let mut out = filter.push(text.as_bytes(), secret);
    out.extend(filter.finish());
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_replay_and_evicts_oldest_channel() {
        let mut hub = Hub::default();
        let channel = hub.channel("task", "old");
        let mut receiver = channel.sender.subscribe();
        channel.write(&vec![b'a'; REPLAY_BYTES]);
        channel.write(b"tail");
        assert_eq!(channel.replay.len(), REPLAY_BYTES);
        assert!(channel.replay.make_contiguous().ends_with(b"tail"));
        for i in 0..CHANNELS {
            hub.channel("task", &i.to_string());
        }
        assert_eq!(hub.channels.len(), CHANNELS);
        while receiver.try_recv().is_ok() {}
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
    }

    #[test]
    fn redacts_split_credentials_and_normalizes_crlf() {
        let mut filter = DisplayFilter::default();
        let secret = Some(b"password".as_slice());
        assert_eq!(filter.push(b"A\r", secret), b"A\r");
        assert_eq!(filter.push(b"\npass", secret), b"\n");
        assert_eq!(filter.push(b"word\n---", secret), b"[redacted]\r\n");
        assert_eq!(
            filter.push(b"--BEGIN OPENSSH PRIVATE KEY-----\nbody\n-----E", secret),
            b"[redacted PEM]"
        );
        assert_eq!(
            filter.push(b"ND OPENSSH PRIVATE KEY-----\nB\n", secret),
            b"\r\nB\r\n"
        );
        assert!(filter.finish().is_empty());
    }
}
