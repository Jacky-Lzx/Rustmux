//! Bounded, attachment-owned OSC 5113 relay. The outer terminal and child
//! perform all filesystem, compression, delta and permission operations.
use crate::{
    rich_clipboard::Owner,
    terminal_ipc::{Framer, Protocol},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const PREFIX: &[u8] = b"\x1b]5113;";
const MAX_PACKET: usize = 16 * 1024;
const MAX_ID: usize = 64;
const MAX_SESSIONS: usize = 8;
const MAX_PENDING: usize = 256 * 1024;
// One frontend read, one retained packet, ID restoration and local cancellation.
const RESERVE: usize = 208 * 1024;
const IDLE: Duration = Duration::from_secs(30);

fn safe(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_:./@-".contains(&b))
}
struct Packet<'a> {
    fields: Vec<(&'a str, &'a str)>,
}
impl<'a> Packet<'a> {
    fn parse(body: &'a [u8]) -> Option<Self> {
        if body.len() > MAX_PACKET - MAX_ID {
            return None;
        }
        let text = std::str::from_utf8(body).ok()?;
        let mut fields = Vec::new();
        for field in text.split(';') {
            let (key, value) = field.split_once('=')?;
            if key.is_empty()
                || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || !value.bytes().all(|b| (0x20..=0x7e).contains(&b))
                || fields.iter().any(|(k, _)| *k == key)
                || fields.len() == 32
            {
                return None;
            }
            match key {
                "id" => {
                    if value.is_empty() || value.len() > MAX_ID || !safe(value) {
                        return None;
                    }
                }
                "fid" | "pr" => {
                    if value.len() > MAX_ID || !safe(value) {
                        return None;
                    }
                }
                "d" => {
                    if STANDARD.decode(value).ok()?.len() > 4096 {
                        return None;
                    }
                }
                "n" | "st" | "pw" => {
                    let decoded = STANDARD.decode(value).ok()?;
                    if decoded.len() > 4096 || std::str::from_utf8(&decoded).is_err() {
                        return None;
                    }
                }
                "q" => {
                    if !matches!(value, "0" | "1" | "2") {
                        return None;
                    }
                }
                "sz" | "mod" | "prm" => {
                    value.parse::<i64>().ok()?;
                }
                "zip" => {
                    if !matches!(value, "none" | "zlib") {
                        return None;
                    }
                }
                "tt" => {
                    if !matches!(value, "simple" | "rsync") {
                        return None;
                    }
                }
                "ft" if !matches!(value, "regular" | "directory" | "symlink" | "link") => {
                    return None;
                }
                _ => {}
            }
            fields.push((key, value));
        }
        let packet = Self { fields };
        packet.value("id")?;
        if !matches!(
            packet.value("ac")?,
            "send" | "receive" | "file" | "data" | "end_data" | "status" | "finish" | "cancel"
        ) {
            return None;
        }
        Some(packet)
    }
    fn value(&self, key: &str) -> Option<&'a str> {
        self.fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
    fn quiet(&self) -> bool {
        self.value("q") == Some("2")
    }
    fn encode(&self, id: &str) -> Vec<u8> {
        let mut wire = PREFIX.to_vec();
        for (index, &(key, value)) in self.fields.iter().enumerate() {
            if index != 0 {
                wire.push(b';');
            }
            wire.extend_from_slice(key.as_bytes());
            wire.push(b'=');
            wire.extend_from_slice(if key == "id" {
                id.as_bytes()
            } else {
                value.as_bytes()
            });
        }
        wire.extend_from_slice(b"\x1b\\");
        wire
    }
}
fn status(id: &str, code: &str) -> Vec<u8> {
    format!(
        "\x1b]5113;ac=status;id={id};st={}\x1b\\",
        STANDARD.encode(code)
    )
    .into_bytes()
}
fn cancel(id: &str) -> Vec<u8> {
    format!("\x1b]5113;ac=cancel;id={id}\x1b\\").into_bytes()
}

#[derive(Debug)]
pub(crate) enum Request {
    Packet(Vec<u8>),
    Overflow,
}
#[derive(Debug)]
pub(crate) struct Observer {
    framer: Framer,
    permission: Option<bool>,
    pending: VecDeque<Request>,
    bytes: usize,
}
impl Default for Observer {
    fn default() -> Self {
        Self {
            framer: Framer::file(),
            permission: None,
            pending: VecDeque::new(),
            bytes: 0,
        }
    }
}
impl Observer {
    pub fn configure(&mut self, permission: Option<bool>) {
        if self.permission != permission {
            self.framer.cancel_packet();
            self.pending.clear();
            self.bytes = 0;
        }
        self.permission = permission;
    }
    pub fn take(&mut self) -> Option<Request> {
        let request = self.pending.pop_front()?;
        if let Request::Packet(body) = &request {
            self.bytes -= body.len();
        }
        Some(request)
    }
    pub fn advance(&mut self, bytes: &[u8], reply: &mut impl FnMut(&[u8])) {
        let mut ignored = Vec::new();
        for &byte in bytes {
            if let Some((Protocol::File, body)) = self.framer.advance(byte, &mut ignored)
                && let Some(packet) = Packet::parse(&body)
            {
                if self.permission == Some(true) {
                    if body.len() > 2 * MAX_PACKET - self.bytes {
                        self.pending.clear();
                        self.bytes = 0;
                        self.pending.push_back(Request::Overflow);
                    } else {
                        self.bytes += body.len();
                        self.pending.push_back(Request::Packet(body));
                    }
                } else if matches!(packet.value("ac"), Some("send" | "receive")) && !packet.quiet()
                {
                    reply(&status(
                        packet.value("id").unwrap(),
                        if self.permission.is_some() {
                            "EPERM"
                        } else {
                            "ENOSYS"
                        },
                    ));
                }
            }
            self.framer.bad_packet = None;
            ignored.clear();
        }
    }
}

struct Transfer {
    owner: Owner,
    original: String,
    outer: String,
    sent: bool,
    quiet: bool,
    finishing: bool,
    finish_sent: bool,
    canceling: bool,
    deadline: Instant,
}
struct Outgoing {
    id: String,
    bytes: Vec<u8>,
    start: bool,
    finish: bool,
}
struct Incoming {
    owner: Owner,
    id: String,
    bytes: Vec<u8>,
}
pub(crate) struct Router {
    namespace: String,
    sequence: u64,
    transfers: Vec<Transfer>,
    outgoing: VecDeque<Outgoing>,
    outgoing_bytes: usize,
    incoming: VecDeque<Incoming>,
    incoming_bytes: usize,
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
            namespace: format!("rmf-{:x}-{time:x}-{serial:x}", std::process::id()),
            sequence: 0,
            transfers: Vec::new(),
            outgoing: VecDeque::new(),
            outgoing_bytes: 0,
            incoming: VecDeque::new(),
            incoming_bytes: 0,
        }
    }
}
impl Router {
    pub fn can_receive(&self) -> bool {
        self.outgoing_bytes <= MAX_PENDING - RESERVE && self.incoming_bytes <= MAX_PENDING - RESERVE
    }
    fn queue_outer(&mut self, id: String, bytes: Vec<u8>, start: bool, finish: bool) {
        assert!(self.outgoing_bytes + bytes.len() <= MAX_PENDING);
        self.outgoing_bytes += bytes.len();
        self.outgoing.push_back(Outgoing {
            id,
            bytes,
            start,
            finish,
        });
    }
    fn queue_child(&mut self, owner: Owner, id: String, bytes: Vec<u8>) {
        assert!(self.incoming_bytes + bytes.len() <= MAX_PENDING);
        self.incoming_bytes += bytes.len();
        self.incoming.push_back(Incoming { owner, id, bytes });
    }
    fn error(&mut self, owner: Owner, packet: &Packet<'_>, code: &str) {
        if !packet.quiet() {
            self.queue_child(
                owner,
                String::new(),
                status(packet.value("id").unwrap(), code),
            );
        }
    }
    pub fn request(&mut self, owner: Owner, request: Request, now: Instant) {
        let Request::Packet(body) = request else {
            self.forget(owner, "EBUSY");
            return;
        };
        let Some(packet) = Packet::parse(&body) else {
            return;
        };
        let original = packet.value("id").unwrap();
        let action = packet.value("ac").unwrap();
        if matches!(action, "send" | "receive") {
            if self.transfers.len() == MAX_SESSIONS
                || !self.can_receive()
                || self
                    .transfers
                    .iter()
                    .any(|t| t.owner == owner && t.original == original)
            {
                self.error(owner, &packet, "EBUSY");
                return;
            }
            if packet.value("pw").is_some_and(|pw| !pw.is_empty()) {
                // Its hash/encrypted token authenticates the original ID. We
                // cannot recompute it when assigning an attachment-local ID.
                self.error(owner, &packet, "ENOTSUP");
                return;
            }
            if action == "receive"
                && packet
                    .value("sz")
                    .and_then(|v| v.parse::<usize>().ok())
                    .is_none_or(|v| !(1..=1024).contains(&v))
            {
                self.error(owner, &packet, "EINVAL");
                return;
            }
            let Some(next) = self.sequence.checked_add(1) else {
                self.error(owner, &packet, "EBUSY");
                return;
            };
            self.sequence = next;
            let outer = format!("{}-{next:x}", self.namespace);
            self.queue_outer(outer.clone(), packet.encode(&outer), true, false);
            self.transfers.push(Transfer {
                owner,
                original: original.to_owned(),
                outer,
                sent: false,
                quiet: packet.quiet(),
                finishing: false,
                finish_sent: false,
                canceling: false,
                deadline: now + IDLE,
            });
            return;
        }
        let Some(index) = self
            .transfers
            .iter()
            .position(|t| t.owner == owner && t.original == original)
        else {
            return;
        };
        if self.transfers[index].finishing || self.transfers[index].canceling {
            return;
        }
        let id = self.transfers[index].outer.clone();
        if action == "cancel" {
            self.remove_queued(&id);
            if !self.transfers[index].sent {
                let transfer = self.transfers.remove(index);
                if !transfer.quiet {
                    self.queue_child(owner, String::new(), status(original, "CANCELED"));
                }
                return;
            }
            self.transfers[index].canceling = true;
        }
        self.transfers[index].finishing = matches!(action, "finish" | "status");
        self.transfers[index].deadline = now + IDLE;
        self.queue_outer(
            id.clone(),
            packet.encode(&id),
            false,
            matches!(action, "finish" | "status"),
        );
    }
    pub fn response(&mut self, body: &[u8], now: Instant) {
        let Some(packet) = Packet::parse(body) else {
            return;
        };
        let Some(index) = self
            .transfers
            .iter()
            .position(|t| packet.value("id") == Some(t.outer.as_str()) && t.sent)
        else {
            return;
        };
        let action = packet.value("ac").unwrap();
        if !matches!(action, "status" | "file" | "data" | "end_data") {
            return;
        }
        let code = packet
            .value("st")
            .and_then(|v| STANDARD.decode(v).ok())
            .and_then(|b| String::from_utf8(b).ok());
        if action == "status" && code.as_deref().is_none_or(str::is_empty) {
            return;
        }
        let global = action == "status" && packet.value("fid").is_none_or(str::is_empty);
        if self.transfers[index].canceling && !(global && code.as_deref() == Some("CANCELED")) {
            return;
        }
        let transfer = &mut self.transfers[index];
        transfer.deadline = now + IDLE;
        let owner = transfer.owner;
        let id = transfer.outer.clone();
        let bytes = packet.encode(&transfer.original);
        let terminal = global
            && code
                .as_deref()
                .is_some_and(|s| !matches!(s, "OK" | "STARTED" | "PROGRESS"));
        if terminal {
            // Keep valid preceding responses ordered; only revoke requests
            // still waiting for the outer queue.
            self.outgoing.retain(|p| p.id != id);
            self.outgoing_bytes = self.outgoing.iter().map(|p| p.bytes.len()).sum();
            self.transfers.remove(index);
        }
        self.queue_child(owner, id, bytes);
    }
    fn remove_queued(&mut self, id: &str) {
        self.outgoing.retain(|p| p.id != id);
        self.outgoing_bytes = self.outgoing.iter().map(|p| p.bytes.len()).sum();
        self.incoming.retain(|p| p.id != id);
        self.incoming_bytes = self.incoming.iter().map(|p| p.bytes.len()).sum();
    }
    fn discard(&mut self, index: usize, code: &str, notify: bool) {
        let transfer = self.transfers.remove(index);
        self.remove_queued(&transfer.outer);
        if transfer.sent && !transfer.finish_sent {
            self.queue_outer(
                transfer.outer.clone(),
                cancel(&transfer.outer),
                false,
                false,
            );
        }
        if notify && !transfer.quiet && !transfer.finish_sent {
            self.queue_child(
                transfer.owner,
                String::new(),
                status(&transfer.original, code),
            );
        }
    }
    pub fn forget(&mut self, owner: Owner, code: &str) {
        // Even replies whose transfer already ended belong to the old process.
        self.incoming.retain(|p| p.owner != owner);
        self.incoming_bytes = self.incoming.iter().map(|p| p.bytes.len()).sum();
        while let Some(index) = self.transfers.iter().position(|t| t.owner == owner) {
            self.discard(index, code, true);
        }
    }
    pub fn cancel_all(&mut self) {
        self.incoming.retain(|p| p.id.is_empty());
        self.incoming_bytes = self.incoming.iter().map(|p| p.bytes.len()).sum();
        while !self.transfers.is_empty() {
            self.discard(0, "CANCELED", true);
        }
    }
    pub fn tick(&mut self, now: Instant, enabled: bool, live: impl Fn(Owner) -> bool) {
        if !enabled {
            self.cancel_all();
            return;
        }
        self.incoming.retain(|p| live(p.owner));
        self.incoming_bytes = self.incoming.iter().map(|p| p.bytes.len()).sum();
        let mut index = 0;
        while index < self.transfers.len() {
            let transfer = &self.transfers[index];
            if !live(transfer.owner)
                && transfer.finishing
                && !transfer.finish_sent
                && now < transfer.deadline
            {
                // A successful transfer tool may exit immediately after writing
                // finish. Preserve its queued tail through the final host write.
                index += 1;
                continue;
            }
            if !live(transfer.owner) || now >= transfer.deadline {
                let notify = live(transfer.owner);
                self.discard(index, "ETIMEDOUT", notify);
            } else {
                index += 1;
            }
        }
    }
    pub fn prepare_exit(&mut self) {
        let mut index = 0;
        while index < self.transfers.len() {
            if self.transfers[index].finishing {
                index += 1;
            } else {
                self.discard(index, "CANCELED", false);
            }
        }
    }
    pub fn has_activity(&self) -> bool {
        !self.transfers.is_empty() || !self.outgoing.is_empty()
    }
    pub fn has_outgoing(&self) -> bool {
        !self.outgoing.is_empty()
    }
    pub fn pump(&mut self, now: Instant, outer: &mut VecDeque<u8>, capacity: usize) {
        while self
            .outgoing
            .front()
            .is_some_and(|p| p.bytes.len() <= capacity.saturating_sub(outer.len()))
        {
            let packet = self.outgoing.pop_front().unwrap();
            self.outgoing_bytes -= packet.bytes.len();
            if let Some(transfer) = self.transfers.iter_mut().find(|t| t.outer == packet.id) {
                if packet.start {
                    transfer.sent = true;
                }
                if packet.finish {
                    transfer.finish_sent = true;
                }
                transfer.deadline = now + IDLE;
            }
            outer.extend(packet.bytes);
        }
    }
    pub fn drain(&mut self, mut deliver: impl FnMut(Owner, &[u8]) -> bool) {
        while let Some(packet) = self.incoming.front() {
            if !deliver(packet.owner, &packet.bytes) {
                break;
            }
            self.incoming_bytes -= packet.bytes.len();
            self.incoming.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: Owner = Owner {
        pane: 7,
        incarnation: 11,
    };
    const B: Owner = Owner {
        pane: 8,
        incarnation: 12,
    };
    fn request(body: &str) -> Request {
        Request::Packet(body.as_bytes().to_vec())
    }
    fn body(wire: &[u8]) -> &[u8] {
        wire.strip_prefix(PREFIX)
            .unwrap()
            .strip_suffix(b"\x1b\\")
            .unwrap()
    }
    fn begin(router: &mut Router, owner: Owner, now: Instant, extra: &str) -> String {
        router.request(owner, request(&format!("ac=send;id=shared{extra}")), now);
        router.transfers.last().unwrap().outer.clone()
    }
    fn pump(router: &mut Router, now: Instant) -> Vec<u8> {
        let mut outer = VecDeque::new();
        router.pump(now, &mut outer, 64 * 1024);
        outer.into()
    }
    fn incoming(router: &mut Router) -> Vec<(Owner, Vec<u8>)> {
        let mut received = Vec::new();
        router.drain(|owner, bytes| {
            received.push((owner, bytes.to_vec()));
            true
        });
        received
    }
    fn response(router: &mut Router, id: &str, code: &str, now: Instant) {
        router.response(body(&status(id, code)), now);
    }

    #[test]
    fn wire_fields_and_binary_payloads_are_validated_without_rewriting_metadata() {
        let data = STANDARD.encode((0..4096).map(|i| (i % 256) as u8).collect::<Vec<_>>());
        let text = format!(
            "ac=end_data;id=client:@/-_;fid=f:1;d={data};n={};sz=-1;tt=rsync;zip=zlib;ft=link;pr=dir;mod=123;prm=420;extension=future value",
            STANDARD.encode("/tmp/中;file")
        );
        let packet = Packet::parse(text.as_bytes()).unwrap();
        let wire = packet.encode("host");
        assert_eq!(
            body(&wire),
            text.replace("id=client:@/-_", "id=host").as_bytes()
        );
        for bad in [
            "ac=send;id=a;id=b",
            "ac=send;id=unsafe+",
            "action=send;id=a",
            "ac=send",
            "ac=send;id=a;q=-1",
            "ac=data;id=a;d=?",
            "ac=file;id=a;n=/w==",
            "ac=data;id=a;sz=99999999999999999999999",
            "ac=data;id=a;tt=unknown",
            "ac=file;id=a;st=\x1b[H",
        ] {
            assert!(Packet::parse(bad.as_bytes()).is_none(), "{bad}");
        }
        assert!(
            Packet::parse(format!("ac=data;id=a;d={}", STANDARD.encode(vec![0; 4097])).as_bytes())
                .is_none()
        );
        assert!(Packet::parse(format!("ac=send;id={}", "a".repeat(65)).as_bytes()).is_none());
    }

    #[test]
    fn observer_preserves_fragmentation_policy_and_consumes_nested_controls() {
        let wire = b"\x1b]5113;ac=send;id=a\x1b\\";
        for split in 0..=wire.len() {
            let mut observer = Observer::default();
            observer.configure(Some(true));
            observer.advance(&wire[..split], &mut |_| panic!());
            observer.advance(&wire[split..], &mut |_| panic!());
            assert!(matches!(observer.take(), Some(Request::Packet(p)) if p == b"ac=send;id=a"));
            assert!(observer.take().is_none());
        }
        for (permission, code) in [(None, "ENOSYS"), (Some(false), "EPERM")] {
            let mut observer = Observer::default();
            observer.configure(permission);
            let mut replies = Vec::new();
            observer.advance(wire, &mut |b| replies.extend_from_slice(b));
            assert_eq!(replies, status("a", code));
            observer.advance(b"\x1b]5113;ac=send;id=a;q=2\x1b\\", &mut |_| panic!());
        }
        let mut observer = Observer::default();
        observer.configure(Some(true));
        observer.advance(b"\x1b]5113;ac=send;id=a", &mut |_| panic!());
        observer.configure(Some(false));
        observer.configure(Some(true));
        observer.advance(b"\x1b\\", &mut |_| panic!());
        assert!(observer.take().is_none());
        for prefix in [b"\x1bP".as_slice(), b"\x1b_", b"\x1b]other;"] {
            let mut wrapped = prefix.to_vec();
            wrapped.extend_from_slice(wire);
            wrapped.extend_from_slice(b"\x1b\\");
            observer.advance(&wrapped, &mut |_| panic!());
            assert!(observer.take().is_none());
        }
        observer.advance(b"\x1b]5113;ac=send;id=a\x07", &mut |_| panic!());
        assert!(observer.take().is_none());
        observer.advance(wire, &mut |_| panic!());
        assert!(observer.take().is_some());
    }

    #[test]
    fn shared_host_framer_demultiplexes_both_protocols_and_keeps_literal_paste() {
        let wire = b"before\x1b]5113;ac=status;id=a;st=T0s=\x1b\\\x1b]5522;type=read:id=b:status=DONE\x07after";
        for split in 0..=wire.len() {
            let mut framer = Framer::host();
            let mut pass = Vec::new();
            let mut packets = Vec::new();
            for &b in wire[..split].iter().chain(&wire[split..]) {
                if let Some(packet) = framer.advance(b, &mut pass) {
                    packets.push(packet);
                }
            }
            assert_eq!(pass, b"beforeafter");
            assert_eq!(packets.len(), 2);
            assert_eq!(packets[0].0, Protocol::File);
            assert_eq!(packets[1].0, Protocol::Clipboard);
        }
        let file = b"\x1b]5113;ac=status;id=a;st=T0s=\x1b\\";
        for split in 0..file.len() {
            let mut framer = Framer::host();
            let mut pass = Vec::new();
            let mut packets = Vec::new();
            for &b in &file[..split] {
                if let Some(p) = framer.advance(b, &mut pass) {
                    packets.push(p);
                }
            }
            framer.cancel_packet();
            for &b in &file[split..] {
                if let Some(p) = framer.advance(b, &mut pass) {
                    packets.push(p);
                }
            }
            assert_eq!(packets, vec![(Protocol::File, body(file).to_vec())]);
            assert!(pass.is_empty());
        }
        let mut framer = Framer::host();
        let mut pass = Vec::new();
        let paste = b"\x1b[200~\x1b]5113;ac=send;id=a\x1b\\\x1b[201~";
        for &b in paste {
            assert!(framer.advance(b, &mut pass).is_none());
        }
        assert_eq!(pass, paste);
        framer.advance(0x1b, &mut pass);
        framer.expire_prefix(Instant::now() + Duration::from_millis(51), &mut pass);
        assert_eq!(pass.last(), Some(&0x1b));
    }

    #[test]
    fn same_child_id_is_isolated_between_panes_and_attachments() {
        let now = Instant::now();
        let mut router = Router::default();
        let a = begin(&mut router, A, now, "");
        let b = begin(&mut router, B, now, "");
        assert_ne!(a, b);
        assert!(
            pump(&mut router, now)
                .windows(a.len())
                .any(|p| p == a.as_bytes())
        );
        response(&mut router, &b, "OK", now);
        response(&mut router, "shared", "OK", now);
        response(&mut router, &a, "OK", now);
        assert_eq!(
            incoming(&mut router),
            vec![(B, status("shared", "OK")), (A, status("shared", "OK"))]
        );
        let before = router.transfers.len();
        router.response(format!("ac=status;id={a};st=").as_bytes(), now);
        assert_eq!(router.transfers.len(), before);
        assert!(incoming(&mut router).is_empty());
        let mut other = Router::default();
        assert_ne!(begin(&mut other, A, now, ""), a);
        let next_process = Owner {
            incarnation: A.incarnation + 1,
            ..A
        };
        router.tick(now, true, |owner| owner != A);
        let wire = pump(&mut router, now);
        assert!(wire.windows(a.len()).any(|p| p == a.as_bytes()));
        let next = begin(&mut router, next_process, now, "");
        assert_ne!(a, next);
        response(&mut router, &a, "OK", now);
        assert!(incoming(&mut router).is_empty());
    }

    #[test]
    fn queues_remain_bounded_and_pump_only_whole_packets() {
        let now = Instant::now();
        let mut router = Router::default();
        let id = begin(&mut router, A, now, "");
        pump(&mut router, now);
        let chunk = format!(
            "ac=data;id=shared;fid=f;d={}",
            STANDARD.encode(vec![0x5a; 4096])
        );
        while router.can_receive() {
            router.request(A, request(&chunk), now);
        }
        assert!(router.outgoing_bytes <= MAX_PENDING);
        let queued = router.outgoing_bytes;
        let mut outer = VecDeque::from(vec![b'x'; 100]);
        router.pump(now, &mut outer, 101);
        assert_eq!(router.outgoing_bytes, queued);
        assert_eq!(outer.len(), 100);
        let wire = pump(&mut router, now);
        assert!(wire.ends_with(b"\x1b\\"));
        assert_eq!(router.outgoing_bytes, 0);
        while router.can_receive() {
            response(&mut router, &id, "PROGRESS", now);
        }
        assert!(router.incoming_bytes <= MAX_PENDING);
        let queued = router.incoming_bytes;
        router.drain(|_, _| false);
        assert_eq!(router.incoming_bytes, queued);
        assert!(!incoming(&mut router).is_empty());
    }

    #[test]
    fn cancel_discards_staged_data_and_waits_for_only_the_cancel_acknowledgment() {
        let now = Instant::now();
        let mut router = Router::default();
        let id = begin(&mut router, A, now, "");
        router.request(A, request("ac=cancel;id=shared"), now);
        assert!(pump(&mut router, now).is_empty());
        assert_eq!(
            incoming(&mut router),
            vec![(A, status("shared", "CANCELED"))]
        );
        let next = begin(&mut router, A, now, "");
        assert_ne!(id, next);
        pump(&mut router, now);
        response(&mut router, &next, "OK", now);
        router.request(A, request("ac=data;id=shared;fid=f;d=AA=="), now);
        router.request(A, request("ac=cancel;id=shared"), now);
        assert_eq!(pump(&mut router, now), cancel(&next));
        assert!(incoming(&mut router).is_empty());
        response(&mut router, &next, "PROGRESS", now);
        assert!(incoming(&mut router).is_empty());
        response(&mut router, &next, "CANCELED", now);
        assert_eq!(
            incoming(&mut router),
            vec![(A, status("shared", "CANCELED"))]
        );
        assert!(router.transfers.is_empty());
    }

    #[test]
    fn idle_timeout_policy_disable_and_finish_cleanup_have_distinct_effects() {
        let now = Instant::now();
        let mut router = Router::default();
        let id = begin(&mut router, A, now, "");
        pump(&mut router, now);
        router.tick(now + IDLE, true, |_| true);
        assert_eq!(pump(&mut router, now), cancel(&id));
        assert_eq!(
            incoming(&mut router),
            vec![(A, status("shared", "ETIMEDOUT"))]
        );
        let id = begin(&mut router, A, now, "");
        pump(&mut router, now);
        response(&mut router, &id, "OK", now);
        router.tick(now, false, |_| true);
        router.tick(now, false, |_| true);
        assert_eq!(pump(&mut router, now), cancel(&id));
        assert_eq!(
            incoming(&mut router),
            vec![(A, status("shared", "CANCELED"))]
        );
        let id = begin(&mut router, A, now, "");
        pump(&mut router, now);
        router.request(A, request("ac=finish;id=shared"), now);
        router.cancel_all();
        assert_eq!(pump(&mut router, now), cancel(&id));
        incoming(&mut router);
        let id = begin(&mut router, A, now, "");
        pump(&mut router, now);
        router.request(A, request("ac=finish;id=shared"), now);
        pump(&mut router, now);
        router.response(body(&status(&id, "EIO:commit failed")), now);
        assert_eq!(
            incoming(&mut router),
            vec![(A, status("shared", "EIO:commit failed"))]
        );
        let _ = begin(&mut router, A, now, "");
        pump(&mut router, now);
        router.request(A, request("ac=finish;id=shared"), now);
        pump(&mut router, now);
        router.tick(now + IDLE, true, |_| true);
        assert!(pump(&mut router, now).is_empty());
        assert!(incoming(&mut router).is_empty());
    }

    #[test]
    fn immediate_process_exit_preserves_the_finish_tail_and_cancels_unfinished_sessions() {
        let now = Instant::now();
        let mut router = Router::default();
        let id = begin(&mut router, A, now, "");
        let other = begin(&mut router, B, now, "");
        pump(&mut router, now);
        router.request(A, request("ac=end_data;id=shared;fid=f;d=AA=="), now);
        router.request(A, request("ac=finish;id=shared"), now);
        router.tick(now, true, |owner| owner != A);
        router.prepare_exit();
        let wire = pump(&mut router, now);
        let expected = format!(
            "\x1b]5113;ac=end_data;id={id};fid=f;d=AA==\x1b\\\x1b]5113;ac=finish;id={id}\x1b\\"
        );
        assert!(wire.starts_with(expected.as_bytes()));
        assert!(wire.ends_with(&cancel(&other)));
        assert!(!router.has_outgoing());
        router.tick(now, true, |_| false);
        assert!(!router.has_activity());
        assert!(pump(&mut router, now).is_empty());
    }
    #[test]
    fn quiet_errors_credential_binding_and_session_limits_do_not_leak_requests() {
        let now = Instant::now();
        let mut router = Router::default();
        router.request(A, request("ac=send;id=secret;pw=c2hhMjU2Omhhc2g="), now);
        assert!(pump(&mut router, now).is_empty());
        assert_eq!(
            incoming(&mut router),
            vec![(A, status("secret", "ENOTSUP"))]
        );
        router.request(A, request("ac=receive;id=bad;sz=0"), now);
        assert_eq!(incoming(&mut router), vec![(A, status("bad", "EINVAL"))]);
        for i in 0..MAX_SESSIONS {
            router.request(A, request(&format!("ac=send;id=s{i}")), now);
        }
        router.request(B, request("ac=send;id=excess"), now);
        assert_eq!(incoming(&mut router), vec![(B, status("excess", "EBUSY"))]);
        router.request(B, request("ac=send;id=excess;q=2"), now);
        assert!(incoming(&mut router).is_empty());
        assert_eq!(router.transfers.len(), MAX_SESSIONS);
        let mut router = Router {
            sequence: u64::MAX,
            ..Default::default()
        };
        router.request(A, request("ac=send;id=no-wrap"), now);
        assert_eq!(incoming(&mut router), vec![(A, status("no-wrap", "EBUSY"))]);
        assert!(pump(&mut router, now).is_empty());
    }
}
