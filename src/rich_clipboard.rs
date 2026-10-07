//! Attachment-local OSC 5522 transactions. Never access the OS clipboard.
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

mod write;

const PREFIX: &[u8] = b"\x1b]5522;";
const MAX_PACKET: usize = 8192;
const MAX_METADATA: usize = 1024;
const MAX_ID: usize = 64;
const MAX_PENDING: usize = 256 * 1024;
const INPUT_RESERVE: usize = 208 * 1024;
const IDLE: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Prefix(Vec<u8>),
    Csi(Vec<u8>),
    Other {
        osc: bool,
        escape: bool,
    },
    Packet {
        bytes: Vec<u8>,
        escape: bool,
        valid: bool,
    },
}

/// Recognize only top-level, seven-bit, ST-terminated OSC 5522 packets.
#[derive(Debug, Default)]
struct Framer {
    state: State,
    paste: bool,
    prefix_since: Option<Instant>,
    bad_packet: bool,
}
impl Framer {
    fn advance(&mut self, byte: u8, pass: &mut Vec<u8>) -> Option<Vec<u8>> {
        let mut packet = None;
        self.state = match std::mem::take(&mut self.state) {
            State::Ground if byte == 0x1b => {
                self.prefix_since = Some(Instant::now());
                State::Escape
            }
            State::Ground => {
                pass.push(byte);
                State::Ground
            }
            State::Escape => match byte {
                b']' if !self.paste => State::Prefix(b"\x1b]".to_vec()),
                b'[' => {
                    pass.extend_from_slice(b"\x1b[");
                    State::Csi(Vec::new())
                }
                b']' | b'P' | b'_' | b'^' | b'X' if !self.paste => {
                    pass.extend_from_slice(&[0x1b, byte]);
                    State::Other {
                        osc: byte == b']',
                        escape: false,
                    }
                }
                0x1b => {
                    pass.push(0x1b);
                    State::Escape
                }
                _ => {
                    pass.extend_from_slice(&[0x1b, byte]);
                    State::Ground
                }
            },
            State::Prefix(mut bytes) => {
                bytes.push(byte);
                if PREFIX.starts_with(&bytes) {
                    if bytes.len() == PREFIX.len() {
                        State::Packet {
                            bytes: Vec::new(),
                            escape: false,
                            valid: true,
                        }
                    } else {
                        State::Prefix(bytes)
                    }
                } else {
                    pass.extend_from_slice(&bytes);
                    if matches!(byte, 7 | 0x18 | 0x1a) {
                        State::Ground
                    } else {
                        State::Other {
                            osc: true,
                            escape: byte == 0x1b,
                        }
                    }
                }
            }
            State::Csi(mut bytes) => {
                pass.push(byte);
                if (0x40..=0x7e).contains(&byte) {
                    if bytes == b"200" && byte == b'~' {
                        self.paste = true;
                    }
                    if bytes == b"201" && byte == b'~' {
                        self.paste = false;
                    }
                    State::Ground
                } else if matches!(byte, 0x18 | 0x1a) {
                    State::Ground
                } else if byte == 0x1b {
                    State::Escape
                } else {
                    if bytes.len() < 32 {
                        bytes.push(byte);
                    }
                    State::Csi(bytes)
                }
            }
            State::Other { osc, escape } => {
                pass.push(byte);
                if matches!(byte, 0x18 | 0x1a) || (osc && byte == 7) || (escape && byte == b'\\') {
                    State::Ground
                } else {
                    State::Other {
                        osc,
                        escape: byte == 0x1b,
                    }
                }
            }
            State::Packet {
                mut bytes,
                escape,
                mut valid,
            } => {
                if matches!(byte, 7 | 0x18 | 0x1a) {
                    self.bad_packet = true;
                    State::Ground
                } else if escape && byte == b'\\' {
                    if valid {
                        packet = Some(bytes);
                    } else {
                        self.bad_packet = true;
                    }
                    State::Ground
                } else {
                    if escape || (byte != 0x1b && !(0x20..=0x7e).contains(&byte)) {
                        valid = false;
                        bytes.clear();
                    }
                    if valid && byte != 0x1b {
                        if bytes.len() == MAX_PACKET {
                            valid = false;
                            bytes.clear();
                        } else {
                            bytes.push(byte);
                        }
                    }
                    State::Packet {
                        bytes,
                        escape: byte == 0x1b,
                        valid,
                    }
                }
            }
        };
        if !matches!(self.state, State::Escape | State::Prefix(_)) {
            self.prefix_since = None;
        }
        packet
    }
    fn expire_prefix(&mut self, now: Instant, pass: &mut Vec<u8>) {
        if self
            .prefix_since
            .is_some_and(|since| now.saturating_duration_since(since) >= Duration::from_millis(50))
        {
            match std::mem::take(&mut self.state) {
                State::Escape => pass.push(0x1b),
                State::Prefix(bytes) => {
                    pass.extend(bytes);
                    self.state = State::Other {
                        osc: true,
                        escape: false,
                    };
                }
                state => self.state = state,
            }
            self.prefix_since = None;
        }
    }
    fn cancel_packet(&mut self) {
        if matches!(self.state, State::Prefix(_) | State::Escape) {
            self.state = State::Other {
                osc: true,
                escape: false,
            };
            self.prefix_since = None;
        }
        if let State::Packet { bytes, valid, .. } = &mut self.state {
            bytes.clear();
            *valid = false;
        }
    }
}

struct Packet<'a> {
    metadata: &'a str,
    payload: &'a [u8],
}
impl<'a> Packet<'a> {
    fn parse(bytes: &'a [u8]) -> Option<Self> {
        let split = bytes.iter().position(|&b| b == b';').unwrap_or(bytes.len());
        if split > MAX_METADATA {
            return None;
        }
        let metadata = std::str::from_utf8(&bytes[..split]).ok()?;
        let mut keys = Vec::new();
        for item in metadata.split(':') {
            let (key, value) = item.split_once('=')?;
            if key.is_empty()
                || value.is_empty()
                || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || !value
                    .bytes()
                    .all(|b| (0x21..=0x7e).contains(&b) && b != b';')
                || keys.contains(&key)
            {
                return None;
            }
            keys.push(key);
        }
        let packet = Self {
            metadata,
            payload: bytes.get(split + 1..).unwrap_or_default(),
        };
        if packet.value("id").is_some_and(|id| {
            id.len() > MAX_ID
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_+.".contains(&b))
        }) {
            return None;
        }
        Some(packet)
    }
    fn value(&self, key: &str) -> Option<&'a str> {
        self.metadata
            .split(':')
            .filter_map(|s| s.split_once('='))
            .find(|&(k, _)| k == key)
            .map(|(_, v)| v)
    }
    fn encode(&self, id: Option<&str>) -> Vec<u8> {
        let mut bytes = PREFIX.to_vec();
        let mut separator = false;
        for item in self
            .metadata
            .split(':')
            .filter(|item| !item.starts_with("id="))
        {
            if separator {
                bytes.push(b':');
            }
            bytes.extend_from_slice(item.as_bytes());
            separator = true;
        }
        if let Some(id) = id {
            bytes.extend_from_slice(b":id=");
            bytes.extend_from_slice(id.as_bytes());
        }
        if !self.payload.is_empty() {
            bytes.push(b';');
            bytes.extend_from_slice(self.payload);
        }
        bytes.extend_from_slice(b"\x1b\\");
        bytes
    }
}
fn error(kind: &str, id: Option<&str>, status: &str) -> Vec<u8> {
    let mut bytes = format!("\x1b]5522;type={kind}:status={status}").into_bytes();
    if let Some(id) = id {
        bytes.extend_from_slice(b":id=");
        bytes.extend_from_slice(id.as_bytes());
    }
    bytes.extend_from_slice(b"\x1b\\");
    bytes
}

#[derive(Debug)]
pub(crate) struct Request {
    body: Vec<u8>,
}
#[derive(Debug, Default)]
pub(crate) struct Observer {
    framer: Framer,
    permission: Option<bool>,
    write_permission: Option<bool>,
    write_started: bool,
    pending: VecDeque<Request>,
    pending_bytes: usize,
}
impl Observer {
    fn invalidate_partial(&mut self) {
        let partial = !self.pending.is_empty()
            || matches!(
                self.framer.state,
                State::Escape | State::Prefix(_) | State::Packet { .. }
            );
        self.framer.cancel_packet();
        self.pending.clear();
        self.pending_bytes = 0;
        if partial && self.write_started {
            self.pending.push_back(Request { body: Vec::new() });
        }
    }
    pub fn configure(&mut self, permission: Option<bool>) {
        if self.permission != permission {
            self.invalidate_partial();
        }
        self.permission = permission;
    }
    pub fn configure_write(&mut self, permission: Option<bool>) {
        if self.write_permission != permission {
            self.invalidate_partial();
        }
        self.write_permission = permission;
        if permission != Some(true) {
            self.write_started = false;
        }
    }
    pub fn take(&mut self) -> Option<Request> {
        let request = self.pending.pop_front()?;
        self.pending_bytes -= request.body.len();
        Some(request)
    }
    fn push(&mut self, body: Vec<u8>, reply: &mut impl FnMut(&[u8])) {
        if body.len() > 2 * MAX_PACKET - self.pending_bytes {
            if let Some(packet) = Packet::parse(&body)
                && let Some(kind @ ("read" | "write")) = packet.value("type")
            {
                reply(&error(kind, packet.value("id"), "EBUSY"));
            }
            if self.write_started && !self.pending.iter().any(|request| request.body.is_empty()) {
                self.pending.push_back(Request { body: Vec::new() });
            }
            self.write_started = false;
            return;
        }
        self.pending_bytes += body.len();
        self.pending.push_back(Request { body });
    }
    pub fn advance(&mut self, bytes: &[u8], reply: &mut impl FnMut(&[u8])) {
        let mut ignored = Vec::new();
        for &byte in bytes {
            let framed = self.framer.advance(byte, &mut ignored);
            if std::mem::take(&mut self.framer.bad_packet) && self.write_started {
                self.push(Vec::new(), reply);
                self.write_started = false;
            }
            if let Some(body) = framed {
                if let Some(packet) = Packet::parse(&body) {
                    let kind = packet.value("type");
                    let id = packet.value("id");
                    match kind {
                        Some("write") => {
                            self.write_started = false;
                            match self.write_permission {
                                None => reply(&error("write", id, "ENOSYS")),
                                Some(false) => reply(&error("write", id, "EPERM")),
                                Some(true) => {
                                    self.write_started = true;
                                    self.push(body, reply);
                                }
                            }
                        }
                        Some("wdata" | "walias") if self.write_started => self.push(body, reply),
                        Some("read") if packet.value("status").is_none() => {
                            let mime = STANDARD.decode(packet.payload).ok();
                            let valid = mime.as_ref().is_some_and(|mime| {
                                !mime.is_empty()
                                    && mime.len() <= 4096
                                    && mime.iter().all(|b| (0x20..=0x7e).contains(b))
                            });
                            if valid {
                                match self.permission {
                                    None => reply(&error("read", id, "ENOSYS")),
                                    Some(false) => reply(&error("read", id, "EPERM")),
                                    Some(true)
                                        if self.pending.iter().any(|r| {
                                            Packet::parse(&r.body)
                                                .is_some_and(|p| p.value("type") == Some("read"))
                                        }) =>
                                    {
                                        reply(&error("read", id, "EBUSY"))
                                    }
                                    Some(true) => self.push(body, reply),
                                }
                            }
                        }
                        _ => {}
                    }
                } else if self.write_started {
                    self.push(Vec::new(), reply);
                    self.write_started = false;
                }
            }
            ignored.clear();
        }
    }
}

/// A respawn preserves the CLI pane ID but receives a new incarnation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Owner {
    pub pane: u64,
    pub incarnation: u64,
}
struct Lease {
    owner: Owner,
    id: String,
    original_id: Option<String>,
    started: bool,
    finished: bool,
    deadline: Instant,
    write: Option<write::Transaction>,
}

pub(crate) struct Router {
    framer: Framer,
    sequence: u64,
    namespace: String,
    lease: Option<Lease>,
    pending: VecDeque<(Owner, Vec<u8>, bool)>,
    pending_bytes: usize,
}
impl Default for Router {
    fn default() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let time = u64::try_from(time).unwrap_or(u64::MAX);
        Self {
            namespace: format!("{:x}-{time:x}-{serial:x}", std::process::id()),
            framer: Framer::default(),
            sequence: 0,
            lease: None,
            pending: VecDeque::new(),
            pending_bytes: 0,
        }
    }
}
impl Router {
    pub fn can_receive(&self) -> bool {
        self.pending_bytes <= MAX_PENDING - INPUT_RESERVE
    }
    pub fn request(
        &mut self,
        owner: Owner,
        request: Request,
        now: Instant,
        outer: &mut VecDeque<u8>,
        capacity: usize,
    ) {
        let Some(packet) = Packet::parse(&request.body) else {
            if self.lease.as_ref().is_some_and(|lease| {
                lease.owner == owner && lease.write.as_ref().is_some_and(|write| write.collecting())
            }) {
                self.fail_write(owner, "EINVAL", outer);
            }
            return;
        };
        let kind = packet.value("type").unwrap_or("");
        if matches!(kind, "wdata" | "walias") {
            let Some(lease) = self.lease.as_mut() else {
                return;
            };
            if lease.owner != owner {
                return;
            }
            if lease.write.as_ref().is_none_or(|write| !write.collecting()) {
                return;
            }
            if packet
                .value("id")
                .is_some_and(|id| Some(id) != lease.original_id.as_deref())
            {
                self.fail_write(owner, "EINVAL", outer);
                return;
            }
            let transaction = lease.write.as_mut().unwrap();
            let result = transaction.accept(&packet);
            lease.deadline = now + IDLE;
            if let Err(status) = result {
                self.fail_write(owner, status, outer);
            }
            return;
        }
        if !matches!(kind, "read" | "write") {
            return;
        }
        // A new write from the same source restarts its unrelayed staging.
        if kind == "write"
            && self.lease.as_ref().is_some_and(|l| {
                l.owner == owner && l.write.as_ref().is_some_and(|w| w.collecting())
            })
        {
            self.fail_write(owner, "EBUSY", outer);
        }
        if self.lease.is_some() || !self.can_receive() {
            self.queue(owner, error(kind, packet.value("id"), "EBUSY"), false);
            return;
        }
        let Some(sequence) = self.sequence.checked_add(1) else {
            self.queue(owner, error(kind, packet.value("id"), "EBUSY"), false);
            return;
        };
        self.sequence = sequence;
        let id = format!("rmc-{}-{sequence:x}", self.namespace);
        let write = if kind == "write" {
            match write::Transaction::new(&packet) {
                Ok(write) => Some(write),
                Err(status) => {
                    self.queue(owner, error("write", packet.value("id"), status), false);
                    return;
                }
            }
        } else {
            None
        };
        if write.is_none() {
            let encoded = packet.encode(Some(&id));
            if encoded.len() > capacity.saturating_sub(outer.len()) {
                self.queue(owner, error(kind, packet.value("id"), "EBUSY"), false);
                return;
            }
            outer.extend(encoded);
        }
        self.lease = Some(Lease {
            owner,
            id,
            original_id: packet.value("id").map(str::to_owned),
            started: false,
            finished: false,
            deadline: now + IDLE,
            write,
        });
    }
    pub fn idle(&self) -> bool {
        self.lease.is_none()
    }
    fn fail_write(&mut self, owner: Owner, status: &str, outer: &mut VecDeque<u8>) {
        if self
            .lease
            .as_ref()
            .is_none_or(|lease| lease.owner != owner || lease.write.is_none())
        {
            return;
        }
        let lease = self.lease.take().unwrap();
        lease.write.as_ref().unwrap().abort(&lease.id, outer);
        self.pending
            .retain(|(target, _, revocable)| *target != owner || !revocable);
        self.pending_bytes = self.pending.iter().map(|(_, b, _)| b.len()).sum();
        self.queue(
            owner,
            error("write", lease.original_id.as_deref(), status),
            false,
        );
    }
    pub fn pump(&mut self, now: Instant, outer: &mut VecDeque<u8>, capacity: usize) {
        let Some(lease) = self.lease.as_mut() else {
            return;
        };
        if lease.finished {
            return;
        }
        let Some(write) = lease.write.as_mut() else {
            return;
        };
        if !write.collecting() && !write.ended {
            let before = outer.len();
            let result = write.pump(&lease.id, outer, capacity);
            if outer.len() != before {
                lease.deadline = now + IDLE;
            }
            let owner = lease.owner;
            if let Err(status) = result {
                self.fail_write(owner, status, outer);
            }
        }
    }
    pub fn advance(&mut self, bytes: &[u8], pass: &mut Vec<u8>, now: Instant) {
        for &byte in bytes {
            if let Some(body) = self.framer.advance(byte, pass) {
                self.response(&body, now);
            }
        }
    }
    fn response(&mut self, body: &[u8], now: Instant) {
        let Some(packet) = Packet::parse(body) else {
            return;
        };
        let Some(lease) = self.lease.as_mut() else {
            return;
        };
        let kind = if lease.write.is_some() {
            "write"
        } else {
            "read"
        };
        if lease.finished
            || packet.value("id") != Some(lease.id.as_str())
            || packet.value("type") != Some(kind)
        {
            return;
        }
        if let Some(write) = lease.write.as_ref() {
            if !write.started || !packet.payload.is_empty() {
                return;
            }
            match packet.value("status") {
                Some("DONE") if write.ended => {}
                Some("EIO" | "EINVAL" | "ENOSYS" | "EPERM" | "EBUSY" | "EFBIG") => {}
                _ => return,
            }
            lease.finished = true;
            if packet.value("status") != Some("DONE") {
                lease.write.as_mut().unwrap().discard();
            }
            let owner = lease.owner;
            let response = packet.encode(lease.original_id.as_deref());
            self.queue(owner, response, true);
            return;
        }
        let terminal = match packet.value("status") {
            Some("OK") if !lease.started && packet.payload.is_empty() => {
                lease.started = true;
                false
            }
            Some("DATA") if lease.started => {
                let mime = packet
                    .value("mime")
                    .and_then(|mime| STANDARD.decode(mime).ok());
                let data = STANDARD.decode(packet.payload).ok();
                if !mime.is_some_and(|mime| {
                    !mime.is_empty() && mime.iter().all(|b| (0x21..=0x7e).contains(b))
                }) || !data.is_some_and(|data| data.len() <= 4096)
                {
                    return;
                }
                false
            }
            Some("DONE") if lease.started && packet.payload.is_empty() => true,
            Some("ENOSYS" | "EPERM" | "EBUSY") if packet.payload.is_empty() => true,
            _ => return,
        };
        lease.deadline = now + IDLE;
        let owner = lease.owner;
        let response = packet.encode(lease.original_id.as_deref());
        lease.finished = terminal;
        self.queue(owner, response, true);
    }
    fn queue(&mut self, owner: Owner, bytes: Vec<u8>, revocable: bool) {
        // Receive reserves three times the 64 KiB frontend input limit plus
        // a previously buffered packet. Stop new pane reads at the same threshold.
        assert!(self.pending_bytes + bytes.len() <= MAX_PENDING);
        self.pending_bytes += bytes.len();
        self.pending.push_back((owner, bytes, revocable));
    }
    pub fn drain(&mut self, mut deliver: impl FnMut(Owner, &[u8]) -> bool) {
        while let Some((owner, bytes, _)) = self.pending.front() {
            if !deliver(*owner, bytes) {
                break;
            }
            self.pending_bytes -= bytes.len();
            self.pending.pop_front();
        }
        if self.pending.is_empty() && self.lease.as_ref().is_some_and(|lease| lease.finished) {
            self.lease = None;
        }
    }
    pub fn tick(
        &mut self,
        now: Instant,
        enabled: bool,
        write_enabled: bool,
        mut live: impl FnMut(Owner) -> bool,
        pass: &mut Vec<u8>,
        outer: &mut VecDeque<u8>,
    ) {
        self.framer.expire_prefix(now, pass);
        self.pending.retain(|(_, bytes, revocable)| {
            !revocable
                || Packet::parse(&bytes[PREFIX.len()..bytes.len() - 2]).is_some_and(|p| {
                    if p.value("type") == Some("write") {
                        write_enabled
                    } else {
                        enabled
                    }
                })
        });
        self.pending_bytes = self.pending.iter().map(|(_, bytes, _)| bytes.len()).sum();
        let Some(lease) = self.lease.as_ref() else {
            return;
        };
        let alive = live(lease.owner);
        let permitted = if lease.write.is_some() {
            write_enabled
        } else {
            enabled
        };
        if !alive || !permitted || (!lease.finished && now >= lease.deadline) {
            let lease = self.lease.take().unwrap();
            let kind = if lease.write.is_some() {
                "write"
            } else {
                "read"
            };
            if let Some(write) = lease.write {
                write.abort(&lease.id, outer);
            }
            if alive {
                self.queue(
                    lease.owner,
                    error(
                        kind,
                        lease.original_id.as_deref(),
                        if permitted { "EBUSY" } else { "EPERM" },
                    ),
                    false,
                );
            }
        }
    }
    /// Hiding and undoing a pane can both happen before the next loop tick.
    /// Invalidate at the ownership transition, not merely on a later liveness scan.
    pub fn forget(&mut self, owner: Owner, outer: &mut VecDeque<u8>) {
        if self
            .lease
            .as_ref()
            .is_some_and(|lease| lease.owner == owner)
        {
            let lease = self.lease.take().unwrap();
            if let Some(write) = lease.write {
                write.abort(&lease.id, outer);
            }
        }
        self.pending.retain(|(target, _, _)| *target != owner);
        self.pending_bytes = self.pending.iter().map(|(_, bytes, _)| bytes.len()).sum();
    }
    pub fn cancel(
        &mut self,
        outer: &mut VecDeque<u8>,
        mut deliver: impl FnMut(Owner, &[u8]) -> bool,
    ) {
        self.pending.clear();
        self.pending_bytes = 0;
        if let Some(lease) = self.lease.take() {
            let kind = if lease.write.is_some() {
                "write"
            } else {
                "read"
            };
            if let Some(write) = lease.write {
                write.abort(&lease.id, outer);
            }
            deliver(
                lease.owner,
                &error(kind, lease.original_id.as_deref(), "EBUSY"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn read_tick(
        router: &mut Router,
        now: Instant,
        enabled: bool,
        live: impl FnMut(Owner) -> bool,
        pass: &mut Vec<u8>,
    ) {
        router.tick(now, enabled, false, live, pass, &mut VecDeque::new());
    }
    const A: Owner = Owner {
        pane: 1,
        incarnation: 1,
    };
    const B: Owner = Owner {
        pane: 2,
        incarnation: 2,
    };
    fn wire(body: &[u8]) -> Vec<u8> {
        [PREFIX, body, b"\x1b\\"].concat()
    }
    fn request(id: Option<&str>) -> Request {
        let mut body = b"type=read:loc=primary:name=QXBw".to_vec();
        if let Some(id) = id {
            body.extend_from_slice(format!(":id={id}").as_bytes());
        }
        body.extend_from_slice(b";Lg==");
        Request { body }
    }
    fn start(router: &mut Router, owner: Owner, id: Option<&str>) -> String {
        let mut output = VecDeque::new();
        router.request(owner, request(id), Instant::now(), &mut output, 65536);
        let bytes: Vec<_> = output.into();
        let packet = Packet::parse(&bytes[PREFIX.len()..bytes.len() - 2]).unwrap();
        assert_eq!(packet.value("loc"), Some("primary"));
        assert_eq!(packet.value("name"), Some("QXBw"));
        packet.value("id").unwrap().to_owned()
    }
    fn response(router: &mut Router, id: &str, status: &str, extra: &str) {
        let mut pass = Vec::new();
        router.advance(
            &wire(format!("type=read:id={id}:status={status}{extra}").as_bytes()),
            &mut pass,
            Instant::now(),
        );
        assert!(pass.is_empty());
    }
    fn take(router: &mut Router) -> Vec<(Owner, Vec<u8>)> {
        let mut result = Vec::new();
        router.drain(|owner, bytes| {
            result.push((owner, bytes.to_vec()));
            true
        });
        assert_eq!(router.pending_bytes, 0);
        result
    }
    #[test]
    fn fragmented_queries_and_responses_preserve_payload_and_source_identity() {
        let query = wire(&request(Some("original-1")).body);
        for split in 0..=query.len() {
            let mut observer = Observer::default();
            observer.configure(Some(true));
            let mut replies = Vec::new();
            observer.advance(&query[..split], &mut |b| replies.extend_from_slice(b));
            observer.advance(&query[split..], &mut |b| replies.extend_from_slice(b));
            assert!(observer.take().is_some());
            assert!(replies.is_empty());
        }
        let mut router = Router::default();
        let id = start(&mut router, A, Some("original-1"));
        let opening = wire(format!("type=read:status=OK:id={id}").as_bytes());
        for byte in opening {
            router.advance(&[byte], &mut Vec::new(), Instant::now());
        }
        response(&mut router, &id, "DATA", ":mime=dGV4dC9wbGFpbg==;AP8K");
        response(&mut router, &id, "DONE", "");
        let received = take(&mut router);
        assert!(received.iter().all(|(owner, _)| *owner == A));
        assert_eq!(
            received
                .iter()
                .flat_map(|(_, b)| b.clone())
                .collect::<Vec<_>>(),
            [
                wire(b"type=read:status=OK:id=original-1"),
                wire(b"type=read:status=DATA:mime=dGV4dC9wbGFpbg==:id=original-1;AP8K"),
                wire(b"type=read:status=DONE:id=original-1")
            ]
            .concat()
        );
        assert!(router.lease.is_none());
    }
    #[test]
    fn reused_and_missing_application_ids_never_share_an_outer_lease() {
        let mut router = Router::default();
        let first = start(&mut router, A, None);
        router.request(
            B,
            request(None),
            Instant::now(),
            &mut VecDeque::new(),
            65536,
        );
        assert_eq!(take(&mut router), [(B, error("read", None, "EBUSY"))]);
        response(&mut router, &first, "EPERM", "");
        assert_eq!(take(&mut router), [(A, error("read", None, "EPERM"))]);
        let second = start(&mut router, B, Some("same"));
        response(&mut router, &first, "OK", ""); // Delayed old request, swallowed.
        assert!(take(&mut router).is_empty());
        response(&mut router, &second, "ENOSYS", "");
        assert_eq!(
            take(&mut router),
            [(B, error("read", Some("same"), "ENOSYS"))]
        );
        let mut attachment = Router::default();
        assert_ne!(first, start(&mut attachment, A, None));
    }
    #[test]
    fn disabled_detached_and_overlapping_requests_have_bounded_explicit_errors() {
        for (permission, status) in [(None, "ENOSYS"), (Some(false), "EPERM")] {
            let mut observer = Observer::default();
            observer.configure(permission);
            let mut replies = Vec::new();
            observer.advance(&wire(&request(Some("a")).body), &mut |b| {
                replies.extend_from_slice(b)
            });
            assert_eq!(replies, error("read", Some("a"), status));
            assert!(observer.take().is_none());
        }
        let mut observer = Observer::default();
        observer.configure(Some(true));
        let mut replies = Vec::new();
        observer.advance(
            &[
                wire(&request(Some("a")).body),
                wire(&request(Some("b")).body),
                wire(b"type=write:id=c"),
            ]
            .concat(),
            &mut |b| replies.extend_from_slice(b),
        );
        assert!(observer.take().is_some());
        assert_eq!(
            replies,
            [
                error("read", Some("b"), "EBUSY"),
                error("write", Some("c"), "ENOSYS")
            ]
            .concat()
        );
        assert!(
            error("read", Some(&"a".repeat(MAX_ID)), "ENOSYS").len()
                < crate::parser::MAX_REPLY_BYTES
        );
    }
    #[test]
    fn invalid_or_nested_packets_never_escape_and_recover_at_the_next_query() {
        for bad in [
            wire(b"type=read:id=a:id=b;Lg=="),
            wire(b"type=read:id=$;Lg=="),
            wire(b"type=read;%%%"),
            wire(format!("type=read:id={};Lg==", "a".repeat(MAX_ID + 1)).as_bytes()),
            [PREFIX, b"type=read;Lg==\x07"].concat(),
            b"\x1bPtmux;\x1b\x1b]5522;type=read;Lg==\x1b\x1b\\\x1b\\".to_vec(),
            [PREFIX, &vec![b'a'; 1024 * 1024], b"\x1b\\"].concat(),
            [PREFIX, b"type=read;Lg==\x18"].concat(),
            b"\x1b[200~\x1b]5522;type=read;Lg==\x1b\\\x1b[201~".to_vec(),
        ] {
            let mut observer = Observer::default();
            observer.configure(Some(true));
            observer.advance(&bad, &mut |_| panic!("unexpected reply"));
            assert!(observer.take().is_none());
            observer.advance(&wire(&request(None).body), &mut |_| {
                panic!("unexpected reply")
            });
            assert!(observer.take().is_some());
        }
    }
    #[test]
    fn keyboard_paste_and_other_strings_pass_unchanged_without_unsolicited_clipboard() {
        let mut router = Router::default();
        let now = Instant::now();
        let data = b"text\x1b[A\x1b]0;title\x1b\\\x1bP+q436f\x1b\\\x1b[200~\x1b]5522;type=read;Lg==\x1b\\\x1b[201~";
        let mut pass = Vec::new();
        for byte in data {
            router.advance(&[*byte], &mut pass, now);
        }
        assert_eq!(pass, data);
        router.advance(
            &wire(b"type=read:status=DATA:mime=dGV4dC9wbGFpbg==;c2VjcmV0"),
            &mut pass,
            now,
        );
        assert_eq!(pass, data);
        assert!(take(&mut router).is_empty());
        router.advance(b"\x1b", &mut pass, now);
        read_tick(
            &mut router,
            now + Duration::from_secs(1),
            true,
            |_| true,
            &mut pass,
        );
        assert_eq!(pass.last(), Some(&0x1b));
    }
    #[test]
    fn timeout_reload_close_and_detach_cancel_without_replaying_data() {
        let mut router = Router::default();
        let now = Instant::now();
        let id = start(&mut router, A, Some("a"));
        response(&mut router, &id, "OK", "");
        read_tick(
            &mut router,
            now + IDLE + Duration::from_secs(1),
            true,
            |_| true,
            &mut Vec::new(),
        );
        assert_eq!(
            take(&mut router).last(),
            Some(&(A, error("read", Some("a"), "EBUSY")))
        );
        let id = start(&mut router, A, Some("b"));
        response(&mut router, &id, "OK", "");
        read_tick(&mut router, now, false, |_| true, &mut Vec::new());
        assert_eq!(take(&mut router), [(A, error("read", Some("b"), "EPERM"))]);
        start(&mut router, A, None);
        read_tick(&mut router, now, true, |_| false, &mut Vec::new());
        assert!(router.lease.is_none());
        let id = start(&mut router, A, Some("c"));
        response(&mut router, &id, "OK", "");
        let mut cancellations = Vec::new();
        router.cancel(&mut VecDeque::new(), |owner, bytes| {
            cancellations.push((owner, bytes.to_vec()));
            true
        });
        assert_eq!(cancellations, [(A, error("read", Some("c"), "EBUSY"))]);
        response(&mut router, &id, "DONE", "");
        assert!(take(&mut router).is_empty());
    }
    #[test]
    fn malformed_response_order_and_payloads_are_swallowed_until_a_valid_packet() {
        let mut router = Router::default();
        let id = start(&mut router, A, None);
        response(&mut router, &id, "DONE", "");
        response(&mut router, &id, "DATA", ":mime=dGV4dC9wbGFpbg==;YQ==");
        assert!(take(&mut router).is_empty());
        response(&mut router, &id, "OK", "");
        response(&mut router, &id, "DATA", ":mime=%%% ;YQ==");
        response(&mut router, &id, "DATA", ":mime=dGV4dC9wbGFpbg==;%%%%");
        response(
            &mut router,
            &id,
            "DATA",
            &format!(":mime=dGV4dC9wbGFpbg==;{}", STANDARD.encode(vec![1; 4097])),
        );
        assert_eq!(take(&mut router), [(A, wire(b"type=read:status=OK"))]);
        response(&mut router, &id, "DATA", ":mime=dGV4dC9wbGFpbg==;YQ==");
        response(&mut router, &id, "DONE", "");
        assert_eq!(take(&mut router).len(), 2);
    }
    #[test]
    fn a_slow_source_applies_backpressure_without_losing_packet_boundaries() {
        let mut router = Router::default();
        let id = start(&mut router, A, Some("same"));
        response(&mut router, &id, "OK", "");
        let payload = STANDARD.encode(vec![7; 4096]);
        while router.can_receive() {
            response(&mut router, &id, "DATA", &format!(":mime=YQ==;{payload}"));
        }
        let bytes = router.pending_bytes;
        router.drain(|_, _| false);
        assert_eq!(router.pending_bytes, bytes);
        assert!(!router.can_receive());
        let received = take(&mut router);
        assert!(received.iter().all(|(owner, _)| *owner == A));
        assert!(router.can_receive());
        response(&mut router, &id, "DONE", "");
        assert_eq!(take(&mut router).len(), 1);
    }
    #[test]
    fn a_completed_but_undelivered_read_still_belongs_to_its_source_and_can_be_revoked() {
        let mut router = Router::default();
        let id = start(&mut router, A, Some("a"));
        response(&mut router, &id, "OK", "");
        response(&mut router, &id, "DONE", "");
        router.drain(|_, _| false);
        assert!(router.lease.is_some());
        read_tick(
            &mut router,
            Instant::now() + IDLE + Duration::from_secs(1),
            true,
            |_| true,
            &mut Vec::new(),
        );
        assert_eq!(router.pending.len(), 2); // Source backpressure does not append an error after DONE.
        read_tick(
            &mut router,
            Instant::now(),
            false,
            |_| true,
            &mut Vec::new(),
        );
        router.drain(|_, _| false);
        read_tick(
            &mut router,
            Instant::now(),
            false,
            |_| true,
            &mut Vec::new(),
        );
        assert_eq!(take(&mut router), [(A, error("read", Some("a"), "EPERM"))]);
    }
    #[test]
    fn hide_then_immediate_undo_cannot_revive_an_old_request() {
        let mut router = Router::default();
        let old = start(&mut router, A, Some("a"));
        response(&mut router, &old, "OK", "");
        router.forget(A, &mut VecDeque::new());
        let new = start(&mut router, A, Some("a"));
        response(&mut router, &old, "DATA", ":mime=YQ==;YQ==");
        response(&mut router, &old, "DONE", "");
        assert!(take(&mut router).is_empty());
        response(&mut router, &new, "EPERM", "");
        assert_eq!(take(&mut router), [(A, error("read", Some("a"), "EPERM"))]);
    }
    #[test]
    fn changing_permission_drops_every_unfinished_child_prefix() {
        let query = wire(&request(None).body);
        for split in 1..query.len() {
            let mut observer = Observer::default();
            observer.configure(Some(true));
            observer.advance(&query[..split], &mut |_| panic!());
            observer.configure(Some(false));
            observer.configure(Some(true));
            observer.advance(&query[split..], &mut |_| panic!());
            assert!(observer.take().is_none(), "resumed prefix at {split}");
            observer.advance(&query, &mut |_| panic!());
            assert!(observer.take().is_some());
        }
    }
    fn write_request(router: &mut Router, owner: Owner, body: &[u8], outer: &mut VecDeque<u8>) {
        router.request(
            owner,
            Request {
                body: body.to_vec(),
            },
            Instant::now(),
            outer,
            65536,
        );
    }
    fn write_response(router: &mut Router, status: &str) {
        let id = router.lease.as_ref().unwrap().id.clone();
        router.advance(
            &wire(format!("type=write:id={id}:status={status}").as_bytes()),
            &mut Vec::new(),
            Instant::now(),
        );
    }
    #[test]
    fn write_serializes_with_reads_and_routes_final_ack_to_original_owner() {
        for original in [None, Some("app")] {
            let mut router = Router::default();
            let mut outer = VecDeque::new();
            write_request(
                &mut router,
                A,
                format!(
                    "type=write{}",
                    original.map(|id| format!(":id={id}")).unwrap_or_default()
                )
                .as_bytes(),
                &mut outer,
            );
            assert!(outer.is_empty());
            router.request(B, request(Some("read")), Instant::now(), &mut outer, 65536);
            write_request(&mut router, B, b"type=write:id=other", &mut outer);
            write_request(
                &mut router,
                B,
                b"type=wdata:mime=dGV4dC9wbGFpbg==;YmFk",
                &mut outer,
            );
            assert_eq!(
                take(&mut router),
                [
                    (B, error("read", Some("read"), "EBUSY")),
                    (B, error("write", Some("other"), "EBUSY"))
                ]
            );
            write_request(
                &mut router,
                A,
                b"type=wdata:mime=dGV4dC9wbGFpbg==;Z29vZA==",
                &mut outer,
            );
            write_request(&mut router, A, b"type=wdata", &mut outer);
            router.pump(Instant::now(), &mut outer, 65536);
            write_response(&mut router, "DONE");
            assert!(take(&mut router).is_empty());
            for _ in 0..2 {
                router.pump(Instant::now(), &mut outer, 65536);
            }
            write_response(&mut router, "DONE");
            router.drain(|_, _| false);
            assert!(!router.idle());
            assert_eq!(take(&mut router), [(A, error("write", original, "DONE"))]);
            assert!(router.idle());
            let mut router = Router::default();
            start(&mut router, A, None);
            write_request(&mut router, B, b"type=write:id=other", &mut outer);
            assert_eq!(
                take(&mut router),
                [(B, error("write", Some("other"), "EBUSY"))]
            );
        }
    }
    #[test]
    fn write_early_host_error_stops_relay_and_releases_spool() {
        let mut router = Router::default();
        let mut outer = VecDeque::new();
        write_request(&mut router, A, b"type=write:id=app", &mut outer);
        write_request(
            &mut router,
            A,
            b"type=wdata:mime=dGV4dC9wbGFpbg==;YQ==",
            &mut outer,
        );
        write_request(&mut router, A, b"type=wdata", &mut outer);
        router.pump(Instant::now(), &mut outer, 65536);
        outer.clear();
        write_response(&mut router, "EPERM");
        assert!(router.lease.as_ref().unwrap().write.as_ref().unwrap().ended);
        router.pump(Instant::now(), &mut outer, 65536);
        assert!(outer.is_empty());
        assert_eq!(
            take(&mut router),
            [(A, error("write", Some("app"), "EPERM"))]
        );
    }
    #[test]
    fn write_restart_bad_continuation_and_cancellation_never_commit_partial_data() {
        for stop in 0..5 {
            let mut router = Router::default();
            let mut outer = VecDeque::new();
            write_request(&mut router, A, b"type=write:id=old", &mut outer);
            write_request(
                &mut router,
                A,
                b"type=wdata:mime=dGV4dC9wbGFpbg==;YQ==",
                &mut outer,
            );
            write_request(&mut router, A, b"type=write:id=app", &mut outer);
            assert_eq!(
                take(&mut router),
                [(A, error("write", Some("old"), "EBUSY"))]
            );
            write_request(
                &mut router,
                A,
                b"type=wdata:mime=dGV4dC9wbGFpbg==;Yg==",
                &mut outer,
            );
            write_request(&mut router, A, b"type=wdata", &mut outer);
            router.pump(Instant::now(), &mut outer, 65536);
            outer.clear();
            match stop {
                0 => router.tick(
                    Instant::now(),
                    true,
                    false,
                    |_| true,
                    &mut Vec::new(),
                    &mut outer,
                ),
                1 => router.tick(
                    Instant::now() + IDLE,
                    true,
                    true,
                    |_| true,
                    &mut Vec::new(),
                    &mut outer,
                ),
                2 => router.forget(A, &mut outer),
                3 => router.cancel(&mut outer, |_, _| true),
                _ => router.tick(
                    Instant::now(),
                    true,
                    true,
                    |_| false,
                    &mut Vec::new(),
                    &mut outer,
                ),
            }
            let bytes = outer.into_iter().collect::<Vec<_>>();
            assert_eq!(
                Packet::parse(&bytes[PREFIX.len()..bytes.len() - 2])
                    .unwrap()
                    .payload,
                b"!"
            );
            assert!(router.idle());
        }
        let mut router = Router::default();
        let mut outer = VecDeque::new();
        write_request(&mut router, A, b"type=write:id=app", &mut outer);
        write_request(
            &mut router,
            A,
            b"type=wdata:id=wrong:mime=dGV4dC9wbGFpbg==;YQ==",
            &mut outer,
        );
        write_request(&mut router, A, b"type=wdata", &mut outer);
        assert_eq!(
            take(&mut router),
            [(A, error("write", Some("app"), "EINVAL"))]
        );
        assert!(outer.is_empty());
    }
    #[test]
    fn observer_write_framing_errors_and_policy_changes_fail_closed() {
        for bad in [
            wire(b"type=wdata:mime=bad:id=x:id=y"),
            [PREFIX, b"type=wdata\x18"].concat(),
            [PREFIX, &vec![b'a'; MAX_PACKET + 1], b"\x1b\\"].concat(),
        ] {
            let mut observer = Observer::default();
            observer.configure_write(Some(true));
            observer.advance(&wire(b"type=write:id=app"), &mut |_| panic!());
            assert!(observer.take().is_some());
            observer.advance(&bad, &mut |_| panic!());
            assert!(observer.take().unwrap().body.is_empty());
            observer.advance(&wire(b"type=wdata"), &mut |_| panic!());
            assert!(observer.take().is_none());
        }
        for permission in [None, Some(false)] {
            let mut observer = Observer::default();
            observer.configure_write(permission);
            let mut replies = Vec::new();
            observer.advance(&wire(b"type=write:id=app"), &mut |b| {
                replies.extend_from_slice(b)
            });
            assert_eq!(
                replies,
                error(
                    "write",
                    Some("app"),
                    if permission.is_none() {
                        "ENOSYS"
                    } else {
                        "EPERM"
                    }
                )
            );
        }
        let mut observer = Observer::default();
        observer.configure(Some(false));
        observer.configure_write(Some(true));
        observer.advance(&wire(b"type=write:id=app"), &mut |_| panic!());
        observer.take().unwrap();
        observer.advance(
            b"\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;Y",
            &mut |_| panic!(),
        );
        observer.configure(Some(true));
        assert!(observer.take().unwrap().body.is_empty());
        observer.advance(b"Q==\x1b\\", &mut |_| panic!());
        assert!(observer.take().unwrap().body.is_empty());
    }
    #[test]
    fn observer_overflow_aborts_staging_and_bounds_even_repeated_starts() {
        let mut observer = Observer::default();
        observer.configure_write(Some(true));
        let data = wire(
            format!(
                "type=wdata:mime=dGV4dC9wbGFpbg==;{}",
                STANDARD.encode([1; 4095])
            )
            .as_bytes(),
        );
        observer.advance(
            &[
                wire(b"type=write:id=app"),
                data.repeat(8),
                wire(b"type=wdata"),
            ]
            .concat(),
            &mut |_| panic!(),
        );
        assert!(observer.pending_bytes <= 2 * MAX_PACKET);
        assert_eq!(
            observer
                .pending
                .iter()
                .filter(|r| r.body.is_empty())
                .count(),
            1
        );
        let mut router = Router::default();
        let mut outer = VecDeque::new();
        while let Some(request) = observer.take() {
            router.request(A, request, Instant::now(), &mut outer, 65536);
        }
        assert_eq!(
            take(&mut router),
            [(A, error("write", Some("app"), "EINVAL"))]
        );
        assert!(outer.is_empty());
        observer.advance(&wire(b"type=write").repeat(20000), &mut |_| {});
        assert!(observer.pending_bytes <= 2 * MAX_PACKET);
        assert_eq!(
            observer
                .pending
                .iter()
                .filter(|r| r.body.is_empty())
                .count(),
            1
        );
    }
    #[test]
    fn completed_write_keeps_final_reply_despite_stray_continuations() {
        let mut router = Router::default();
        let mut outer = VecDeque::new();
        write_request(&mut router, A, b"type=write:id=app", &mut outer);
        write_request(&mut router, A, b"type=wdata", &mut outer);
        for _ in 0..2 {
            router.pump(Instant::now(), &mut outer, 65536);
        }
        write_response(&mut router, "DONE");
        write_request(
            &mut router,
            A,
            b"type=wdata:id=wrong:mime=dGV4dC9wbGFpbg==;YQ==",
            &mut outer,
        );
        write_request(&mut router, A, b"", &mut outer);
        assert_eq!(
            take(&mut router),
            [(A, error("write", Some("app"), "DONE"))]
        );
    }
}
