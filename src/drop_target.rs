//! Bounded OSC 72 receiving relay. Only the host accesses drop files.
use crate::{
    drag_source::{Packet, View, next_id},
    rich_clipboard::Owner,
    terminal_ipc::{Framer, Protocol},
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
const MAX_PENDING: usize = 256 * 1024;
const RESERVE: usize = 128 * 1024;
const MAX_MIMES: usize = 16 * 1024;
const IDLE: Duration = Duration::from_secs(30);

fn wire(meta: &str, payload: Option<&str>) -> Vec<u8> {
    format!(
        "\x1b]72;{meta}{}\x1b\\",
        payload.map_or(String::new(), |p| format!(";{p}"))
    )
    .into_bytes()
}
fn failure(packet: &Packet<'_>, code: &str) -> Vec<u8> {
    let mut meta = "t=R".to_owned();
    for key in ["i", "x", "y", "Y"] {
        if let Some(value) = packet.value(key) {
            meta.push_str(&format!(":{key}={value}"));
        }
    }
    if packet.value("x").is_none() {
        meta.push_str(":x=0");
    }
    wire(&meta, Some(code))
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
    source: bool,
    chunk: Option<u8>,
    pending: VecDeque<Request>,
    bytes: usize,
}
impl Default for Observer {
    fn default() -> Self {
        Self {
            framer: Framer::drag(),
            permission: None,
            source: false,
            chunk: None,
            pending: VecDeque::new(),
            bytes: 0,
        }
    }
}
impl Observer {
    pub fn configure(&mut self, permission: Option<bool>, source: bool) {
        if self.permission != permission {
            self.framer = Framer::drag();
            self.pending.clear();
            self.bytes = 0;
            self.chunk = None;
        }
        self.permission = permission;
        self.source = source;
    }
    pub fn take(&mut self) -> Option<Request> {
        let r = self.pending.pop_front()?;
        if let Request::Packet(body) = &r {
            self.bytes -= body.len();
        }
        Some(r)
    }
    fn overflow(&mut self) {
        self.pending.clear();
        self.bytes = 0;
        self.chunk = None;
        self.pending.push_back(Request::Overflow);
    }
    pub fn advance(&mut self, bytes: &[u8], reply: &mut impl FnMut(&[u8])) {
        let mut ignored = Vec::new();
        for &byte in bytes {
            if let Some((Protocol::Drag, body)) = self.framer.advance(byte, &mut ignored) {
                if let Some(p) = Packet::parse(&body) {
                    let kind = p
                        .value("t")
                        .and_then(|v| v.bytes().next())
                        .or(self.chunk)
                        .unwrap_or(b'a');
                    if kind != b'q' {
                        self.chunk = p.more().then_some(kind);
                    }
                    if matches!(kind, b'a' | b'A' | b'm' | b'r') || (kind == b'q' && !self.source) {
                        if self.permission == Some(true) {
                            let body = if p.value("t").is_none() && kind != b'a' {
                                [format!("t={}:", char::from(kind)).into_bytes(), body].concat()
                            } else {
                                body
                            };
                            if self.bytes + body.len() <= MAX_MIMES + 8192 {
                                self.bytes += body.len();
                                self.pending.push_back(Request::Packet(body));
                            } else {
                                self.overflow();
                            }
                        } else if kind == b'r' {
                            reply(&failure(
                                &p,
                                if self.permission.is_some() {
                                    "EPERM"
                                } else {
                                    "ENOSYS"
                                },
                            ));
                        }
                    }
                } else if self.permission == Some(true) {
                    self.overflow();
                }
            }
            if self.framer.bad_packet.take() == Some(Protocol::Drag)
                && self.permission == Some(true)
            {
                self.overflow();
            }
            ignored.clear();
        }
    }
}
#[derive(Debug)]
struct Registration {
    owner: Owner,
    id: Option<String>,
    mimes: Option<String>,
    machine: String,
}
#[derive(Debug)]
struct PartialRegistration {
    last: Instant,
    owner: Owner,
    id: Option<String>,
    mimes: String,
}
#[derive(Debug)]
struct Advertised {
    wire: u32,
    signature: Vec<(Owner, String)>,
}
#[derive(Debug)]
struct Drop {
    owner: Owner,
    id: Option<String>,
    wire: u32,
    placed: bool,
    last: Instant,
    requests: Vec<Key>,
    status: Option<StatusChunks>,
}
#[derive(Debug, Default)]
struct StatusChunks {
    packets: Vec<Vec<u8>>,
    bytes: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    x: i64,
    y: i64,
    handle: i64,
}
impl Key {
    fn parse(p: &Packet<'_>) -> Option<Self> {
        Some(Self {
            x: p.number("x")?,
            y: p.number("y").unwrap_or(0),
            handle: p.number("Y").unwrap_or(0),
        })
    }
}
#[derive(Debug)]
struct EventChunks {
    last: Instant,
    wire: u32,
    head: Vec<u8>,
    payload: String,
}
#[derive(Default, Debug)]
pub(crate) struct Router {
    enabled: bool,
    supported: bool,
    allowed: bool,
    probe: Option<(u32, Instant)>,
    registrations: Vec<Registration>,
    partials: Vec<PartialRegistration>,
    advertised: Option<Advertised>,
    active: Option<Drop>,
    mimes: String,
    event_chunks: Option<EventChunks>,
    response_chunk: Option<Key>,
    outgoing: VecDeque<(u32, Vec<u8>)>,
    incoming: VecDeque<(Owner, u32, Vec<u8>)>,
    outgoing_bytes: usize,
    incoming_bytes: usize,
}
impl Router {
    pub fn can_receive(&self) -> bool {
        self.outgoing_bytes < RESERVE && self.incoming_bytes < RESERVE
    }
    pub fn probing(&self) -> bool {
        self.probe.is_some()
    }
    pub fn has_outgoing(&self) -> bool {
        !self.outgoing.is_empty()
    }
    fn outer(&mut self, tag: u32, bytes: Vec<u8>) {
        if self.outgoing_bytes + bytes.len() <= MAX_PENDING {
            self.outgoing_bytes += bytes.len();
            self.outgoing.push_back((tag, bytes));
        }
    }
    fn inner(&mut self, owner: Owner, tag: u32, bytes: Vec<u8>) {
        if self.incoming_bytes + bytes.len() <= MAX_PENDING {
            self.incoming_bytes += bytes.len();
            self.incoming.push_back((owner, tag, bytes));
        }
    }
    pub fn pump(&mut self, output: &mut VecDeque<u8>, limit: usize) {
        while let Some((_, bytes)) = self.outgoing.front() {
            if output.len() + bytes.len() > limit {
                break;
            }
            self.outgoing_bytes -= bytes.len();
            output.extend(self.outgoing.pop_front().unwrap().1);
        }
    }
    pub fn drain(&mut self, mut deliver: impl FnMut(Owner, &[u8]) -> bool) {
        while let Some((owner, _, bytes)) = self.incoming.front() {
            if !deliver(*owner, bytes) {
                break;
            }
            self.incoming_bytes -= bytes.len();
            self.incoming.pop_front();
        }
    }
    fn forget(&mut self, tag: u32) {
        self.outgoing.retain(|(id, _)| *id != tag);
        self.incoming.retain(|(_, id, _)| *id != tag);
        self.outgoing_bytes = self.outgoing.iter().map(|(_, b)| b.len()).sum();
        self.incoming_bytes = self.incoming.iter().map(|(_, _, b)| b.len()).sum();
    }
    fn unadvertise(&mut self) {
        if let Some(a) = self.advertised.take() {
            self.outer(0, wire(&format!("t=A:i={}", a.wire), None));
        }
    }
    fn leave(id: Option<&str>) -> Vec<u8> {
        Packet::parse(b"t=m:x=-1:y=-1:X=0:Y=0:o=0;")
            .unwrap()
            .encode(id, None)
    }
    fn cancel(&mut self, code: &str) {
        if let Some(d) = self.active.take() {
            self.forget(d.wire);
            if d.placed {
                self.outer(0, wire(&format!("t=r:o=0:i={}", d.wire), None));
                let meta = format!("t=r:x={}", d.requests.first().map_or(0, |k| k.x));
                let b = Packet::parse(meta.as_bytes())
                    .unwrap()
                    .encode(d.id.as_deref(), None);
                let p = Packet::parse(&b[5..b.len() - 2]).unwrap();
                self.inner(d.owner, 0, failure(&p, code));
            } else {
                self.outer(0, wire(&format!("t=m:o=0:i={}", d.wire), None));
                self.inner(d.owner, 0, Self::leave(d.id.as_deref()));
            }
        }
        self.unadvertise();
        self.event_chunks = None;
        self.response_chunk = None;
        self.mimes.clear();
    }
    fn finish(&mut self) {
        self.active = None;
        self.response_chunk = None;
        self.event_chunks = None;
        self.mimes.clear();
        self.unadvertise();
    }
    pub fn cancel_all(&mut self) {
        self.cancel("ECANCELED");
        if let Some((id, _)) = self.probe.take() {
            self.forget(id);
        }
        self.registrations.clear();
        self.partials.clear();
        self.enabled = false;
        self.supported = false;
    }
    pub fn tick(
        &mut self,
        now: Instant,
        enabled: bool,
        allowed: bool,
        views: &[View],
        mut live: impl FnMut(Owner) -> bool,
    ) {
        self.allowed = allowed;
        if !enabled {
            if self.enabled {
                self.cancel_all();
            }
            return;
        }
        if !self.enabled {
            self.enabled = true;
            if let Some(id) = next_id() {
                self.probe = Some((id, now));
                self.outer(id, wire(&format!("t=q:i={id}"), None));
            }
        }
        if self
            .probe
            .is_some_and(|(_, t)| now.saturating_duration_since(t) >= Duration::from_secs(1))
        {
            self.probe = None;
        }
        self.registrations.retain(|r| live(r.owner));
        self.partials
            .retain(|r| live(r.owner) && now.saturating_duration_since(r.last) < IDLE);
        if self
            .event_chunks
            .as_ref()
            .is_some_and(|e| now.saturating_duration_since(e.last) >= IDLE)
        {
            self.cancel("ETIMEDOUT");
        }
        if self.active.as_ref().is_some_and(|d| {
            !live(d.owner)
                || !views.iter().any(|v| v.owner == d.owner)
                || !allowed
                || now.saturating_duration_since(d.last) >= IDLE
        }) {
            self.cancel("ECANCELED");
        }
        if self.active.is_some() || self.event_chunks.is_some() {
            return;
        }
        let signature: Vec<_> = self
            .registrations
            .iter()
            .filter(|r| {
                allowed
                    && self.supported
                    && r.mimes.is_some()
                    && views.iter().any(|v| v.owner == r.owner)
            })
            .map(|r| (r.owner, r.mimes.clone().unwrap()))
            .collect();
        if self.advertised.as_ref().map(|a| &a.signature) == Some(&signature) {
            return;
        }
        self.unadvertise();
        if signature.is_empty() {
            return;
        }
        let payload = signature
            .iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if payload.len() > MAX_MIMES {
            return;
        }
        if let Some(id) = next_id() {
            for (i, chunk) in payload.as_bytes().chunks(4096).enumerate() {
                let more = (i + 1) * 4096 < payload.len();
                self.outer(
                    id,
                    wire(
                        &format!("t=a:i={id}:m={}", u8::from(more)),
                        Some(std::str::from_utf8(chunk).unwrap()),
                    ),
                );
            }
            if payload.is_empty() {
                self.outer(id, wire(&format!("t=a:i={id};"), None));
            }
            self.advertised = Some(Advertised {
                wire: id,
                signature,
            });
        }
    }
    pub fn request(&mut self, owner: Owner, r: Request, now: Instant, geometry: bool) {
        let Request::Packet(body) = r else {
            self.registrations.retain(|r| r.owner != owner);
            self.partials.retain(|r| r.owner != owner);
            if self.active.as_ref().is_some_and(|d| d.owner == owner) {
                self.cancel("EFBIG");
            }
            return;
        };
        let Some(p) = Packet::parse(&body) else {
            return;
        };
        if !self.enabled {
            return;
        }
        let kind = p.value("t");
        if kind == Some("q") {
            if self.supported && geometry && !p.more() && p.payload_empty() {
                self.inner(
                    owner,
                    0,
                    Packet::parse(b"t=q").unwrap().encode(p.value("i"), None),
                );
            }
            return;
        }
        if kind == Some("a") && p.number("x") == Some(1) {
            if !p.more() {
                if let Some(r) = self.registrations.iter_mut().find(|r| r.owner == owner) {
                    r.machine = p.payload().to_owned();
                } else if self.registrations.len() < 64 {
                    self.registrations.push(Registration {
                        owner,
                        id: None,
                        mimes: None,
                        machine: p.payload().to_owned(),
                    });
                }
            }
            return;
        }
        if kind == Some("a") || kind.is_none() {
            let mut reg = if kind == Some("a") || !self.partials.iter().any(|r| r.owner == owner) {
                self.partials.retain(|r| r.owner != owner);
                PartialRegistration {
                    last: now,
                    owner,
                    id: p.value("i").map(str::to_owned),
                    mimes: String::new(),
                }
            } else {
                let index = self.partials.iter().position(|r| r.owner == owner).unwrap();
                self.partials.remove(index)
            };
            if p.value("i").is_some_and(|id| {
                id.parse::<u32>().unwrap()
                    != reg.id.as_deref().map_or(0, |i| i.parse::<u32>().unwrap())
            }) {
                return;
            }
            if !p.payload().is_ascii() || reg.mimes.len() + p.payload().len() > MAX_MIMES {
                return;
            }
            reg.mimes.push_str(p.payload());
            reg.last = now;
            if p.more() {
                if self.partials.len() < 64 {
                    self.partials.push(reg);
                }
                return;
            }
            if self.active.as_ref().is_some_and(|d| d.owner == owner) {
                self.cancel("ECANCELED");
            }
            let machine = self
                .registrations
                .iter()
                .find(|r| r.owner == owner)
                .map_or(String::new(), |r| r.machine.clone());
            self.registrations.retain(|r| r.owner != owner);
            if self.registrations.len() < 64 {
                self.registrations.push(Registration {
                    owner,
                    id: reg.id,
                    mimes: Some(reg.mimes),
                    machine,
                });
            }
            return;
        }
        if kind == Some("A") {
            self.partials.retain(|r| r.owner != owner);
            self.registrations.retain(|r| r.owner != owner);
            if self.active.as_ref().is_some_and(|d| d.owner == owner) {
                self.cancel("ECANCELED");
            }
            return;
        }
        let matches = self.active.as_ref().is_some_and(|d| {
            d.owner == owner
                && d.id.as_deref().map_or(0, |i| i.parse::<u32>().unwrap())
                    == p.number("i").unwrap_or(0) as u32
                || (d.owner == owner
                    && kind == Some("m")
                    && p.value("i").is_none()
                    && d.status.is_some())
        });
        if !matches {
            if kind == Some("r") {
                self.inner(
                    owner,
                    0,
                    failure(
                        &p,
                        if self.supported && geometry {
                            "EPERM"
                        } else {
                            "ENOSYS"
                        },
                    ),
                );
            }
            return;
        }
        let d = self.active.as_mut().unwrap();
        d.last = now;
        let id = d.wire;
        if kind == Some("m")
            && !d.placed
            && (d.status.is_some() || p.number("o").is_some_and(|o| (0..=2).contains(&o)))
        {
            let mut status = d.status.take().unwrap_or_default();
            let bytes = p.encode(Some(&id.to_string()), None);
            if !p.payload().is_ascii() || status.bytes + bytes.len() > MAX_MIMES + 8192 {
                self.cancel("EFBIG");
                return;
            }
            status.bytes += bytes.len();
            status.packets.push(bytes);
            if p.more() {
                self.active.as_mut().unwrap().status = Some(status);
            } else {
                for bytes in status.packets {
                    self.outer(id, bytes);
                }
            }
        } else if kind == Some("r") && !p.more() && p.payload_empty() {
            if !d.placed {
                self.inner(owner, 0, failure(&p, "EPERM"));
                return;
            }
            if p.number("x").unwrap_or(0) == 0
                && p.number("o").is_some_and(|o| (0..=2).contains(&o))
            {
                self.forget(id);
                self.outer(id, p.encode(Some(&id.to_string()), None));
                self.finish();
            } else if let Some(key) = Key::parse(&p) {
                if key.x <= 0 || key.y < 0 || key.handle < 0 {
                    self.inner(owner, 0, failure(&p, "EINVAL"));
                } else if d.requests.len() >= 64 {
                    self.inner(owner, 0, failure(&p, "EMFILE"));
                } else {
                    d.requests.push(key);
                    self.outer(id, p.encode(Some(&id.to_string()), None));
                }
            }
        }
    }
    pub fn response(&mut self, body: &[u8], now: Instant, views: &[View]) {
        let Some(p) = Packet::parse(body) else {
            return;
        };
        let id = p.number("i").and_then(|i| u32::try_from(i).ok());
        if p.value("t") == Some("q") {
            if self.probe.is_some_and(|(i, _)| Some(i) == id) && !p.more() {
                self.probe = None;
                self.supported = true;
            }
            return;
        }
        if matches!(p.value("t"), Some("m" | "M")) || self.event_chunks.is_some() {
            let Some(a) = self.advertised.as_ref() else {
                return;
            };
            let mut event = if let Some(event) = self.event_chunks.take() {
                if id.is_some_and(|i| i != event.wire)
                    || p.value("t").is_some_and(|t| !matches!(t, "m" | "M"))
                {
                    self.cancel("EPROTO");
                    return;
                }
                event
            } else {
                if id != Some(a.wire) || !self.allowed {
                    return;
                }
                EventChunks {
                    last: now,
                    wire: a.wire,
                    head: body.split(|&b| b == b';').next().unwrap().to_vec(),
                    payload: String::new(),
                }
            };
            if !p.payload().is_ascii() || event.payload.len() + p.payload().len() > MAX_MIMES {
                self.cancel("EFBIG");
                return;
            }
            event.last = now;
            event.payload.push_str(p.payload());
            if p.more() {
                self.event_chunks = Some(event);
                return;
            }
            self.event(event, now, views);
            return;
        }
        let Some(d) = self
            .active
            .as_mut()
            .filter(|d| d.placed && id.or(self.response_chunk.map(|_| d.wire)) == Some(d.wire))
        else {
            return;
        };
        let kind = p.value("t");
        let key = self.response_chunk.or_else(|| Key::parse(&p));
        let Some(key) = key.filter(|key| d.requests.contains(key)) else {
            return;
        };
        if !matches!(kind, Some("r" | "R") | None)
            || (kind.is_none() && self.response_chunk.is_none())
        {
            return;
        }
        d.last = now;
        let owner = d.owner;
        let tag = d.wire;
        let original = d.id.clone();
        if kind == Some("R") {
            if p.more() {
                self.cancel("EFBIG");
                return;
            }
            self.forget(tag);
            self.inner(owner, 0, p.encode(original.as_deref(), None));
            self.outer(0, wire(&format!("t=r:o=0:i={tag}"), None));
            self.finish();
            return;
        }
        self.response_chunk = p.more().then_some(key);
        if !p.more() && p.payload_empty() {
            d.requests.retain(|k| *k != key);
        }
        self.inner(owner, tag, p.encode(original.as_deref(), None));
    }
    fn event(&mut self, e: EventChunks, now: Instant, views: &[View]) {
        let Some(p) = Packet::parse(&e.head) else {
            return;
        };
        let kind = p.value("t").unwrap();
        if self.active.as_ref().is_some_and(|d| d.placed) {
            return;
        }
        let Some((x, y)) = p.number("x").zip(p.number("y")) else {
            return;
        };
        if x == -1 && y == -1 {
            self.cancel("ECANCELED");
            return;
        }
        let Some((px, py)) = p.number("X").zip(p.number("Y")) else {
            return;
        };
        let target = views.iter().find_map(|v| {
            let local = (
                x - i64::from(v.rect.column),
                y - i64::from(v.rect.row),
                px - i64::from(v.rect.column) * i64::from(v.pixels.0),
                py - i64::from(v.rect.row) * i64::from(v.pixels.1),
            );
            (local.0 >= 0
                && local.1 >= 0
                && local.0 < i64::from(v.rect.columns)
                && local.1 < i64::from(v.rect.rows)
                && local.2 >= 0
                && local.3 >= 0
                && local.2 < i64::from(v.rect.columns) * i64::from(v.pixels.0)
                && local.3 < i64::from(v.rect.rows) * i64::from(v.pixels.1))
            .then(|| {
                self.registrations
                    .iter()
                    .find(|r| r.owner == v.owner && r.mimes.is_some())
                    .map(|r| (r.owner, r.id.clone(), r.machine.clone(), local))
            })
            .flatten()
        });
        if kind == "M" && e.payload.is_empty() {
            self.cancel("EINVAL");
            return;
        }
        if !e.payload.is_empty() {
            self.mimes = e.payload;
        }
        if kind == "M" && self.mimes.is_empty() {
            self.cancel("EINVAL");
            return;
        }
        if self.active.as_ref().map(|d| d.owner) != target.as_ref().map(|(o, _, _, _)| *o) {
            if let Some(old) = self.active.take() {
                self.forget(old.wire);
                self.inner(old.owner, 0, Self::leave(old.id.as_deref()));
            }
            self.outer(0, wire(&format!("t=m:o=0:i={}", e.wire), None));
        }
        let Some((owner, id, machine, local)) = target else {
            if kind == "M" {
                self.cancel("ECANCELED");
            }
            return;
        };
        if self.active.is_none() {
            self.outer(
                e.wire,
                wire(&format!("t=a:x=1:i={}", e.wire), Some(&machine)),
            );
            self.active = Some(Drop {
                owner,
                id: id.clone(),
                wire: e.wire,
                placed: false,
                last: now,
                requests: Vec::new(),
                status: None,
            });
        }
        let d = self.active.as_mut().unwrap();
        d.placed = kind == "M";
        if d.placed {
            d.status = None;
        }
        d.last = now;
        let meta = format!(
            "t={kind}:x={}:y={}:X={}:Y={}:o={}",
            local.0,
            local.1,
            local.2,
            local.3,
            p.number("o").unwrap_or(0)
        );
        let payload = self.mimes.clone();
        let chunks = payload.len().div_ceil(4096).max(1);
        for index in 0..chunks {
            let part = &payload[index * 4096..((index + 1) * 4096).min(payload.len())];
            let header = if index == 0 {
                format!("{meta}:m={}", u8::from(index + 1 < chunks))
            } else {
                format!("m={}", u8::from(index + 1 < chunks))
            };
            let b = wire(&header, Some(part));
            let p = Packet::parse(&b[5..b.len() - 2]).unwrap();
            self.inner(owner, e.wire, p.encode(id.as_deref(), None));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Rect;
    fn owner(pane: u64) -> Owner {
        Owner {
            pane,
            incarnation: 0,
        }
    }
    fn views() -> Vec<View> {
        (1..=2)
            .map(|pane| View {
                owner: owner(pane),
                rect: Rect {
                    column: if pane == 1 { 1 } else { 41 },
                    row: 2,
                    columns: 30,
                    rows: 20,
                },
                pixels: (10, 20),
            })
            .collect()
    }
    fn request(r: &mut Router, pane: u64, body: &[u8], now: Instant) {
        r.request(owner(pane), Request::Packet(body.to_vec()), now, true);
    }
    fn drain(r: &mut Router) -> Vec<(Owner, Vec<u8>)> {
        let mut out = Vec::new();
        r.drain(|o, b| {
            out.push((o, b.to_vec()));
            true
        });
        out
    }
    fn ready(now: Instant) -> Router {
        let mut r = Router::default();
        r.tick(now, true, true, &views(), |_| true);
        let id = r.probe.unwrap().0;
        r.response(format!("t=q:i={id}").as_bytes(), now, &views());
        request(&mut r, 1, b"t=a:i=7;text/uri-list", now);
        request(&mut r, 2, b"t=a:i=7;image/png", now);
        r.tick(now, true, true, &views(), |_| true);
        r
    }
    fn event(r: &mut Router, kind: &str, pane: u64, now: Instant) {
        let id = r.advertised.as_ref().unwrap().wire;
        let x = if pane == 1 { 2 } else { 42 };
        r.response(
            format!(
                "t={kind}:i={id}:x={x}:y=3:X={}:Y=65:o=3;text/uri-list image/png",
                x * 10 + 5
            )
            .as_bytes(),
            now,
            &views(),
        );
    }
    #[test]
    fn observer_permission_split_paste_and_query_ownership() {
        let bytes = wire("t=a:i=7", Some("text/uri-list"));
        for split in 0..=bytes.len() {
            let mut o = Observer::default();
            o.configure(Some(true), false);
            let mut reply = Vec::new();
            o.advance(&bytes[..split], &mut |b| reply.extend_from_slice(b));
            o.advance(&bytes[split..], &mut |b| reply.extend_from_slice(b));
            assert!(matches!(o.take(), Some(Request::Packet(_))));
            assert!(o.take().is_none());
            assert!(reply.is_empty());
        }
        let mut o = Observer::default();
        let mut reply = Vec::new();
        o.advance(&wire("t=r:x=1:i=7", None), &mut |b| {
            reply.extend_from_slice(b)
        });
        assert_eq!(reply, wire("t=R:i=7:x=1", Some("ENOSYS")));
        o.configure(Some(false), false);
        reply.clear();
        o.advance(&wire("t=r:x=1", None), &mut |b| reply.extend_from_slice(b));
        assert_eq!(reply, wire("t=R:x=1", Some("EPERM")));
        o.configure(Some(true), true);
        o.advance(&wire("t=q", None), &mut |_| {});
        assert!(o.take().is_none());
        o.configure(Some(true), false);
        o.advance(b"\x1b[200~\x1b]72;t=a\x1b\\\x1b[201~", &mut |_| {});
        assert!(o.take().is_none());
        o.advance(b"\x1bP\x1b]72;t=a\x1b\\\x1b\\", &mut |_| {});
        assert!(o.take().is_none());
    }
    #[test]
    fn observers_separate_directions_and_preserve_chunks_across_queries() {
        let mut source = crate::drag_source::Observer::default();
        source.configure(Some(true));
        let mut drop = Observer::default();
        drop.configure(Some(true), true);
        let commands = [
            wire("t=m:o=1:i=7:m=1", Some("text/")),
            wire("t=q:i=9", None),
            wire("m=0", Some("plain")),
            wire("t=o:x=1:i=7", None),
        ]
        .concat();
        source.advance(&commands, &mut |_| {});
        drop.advance(&commands, &mut |_| {});
        assert!(
            matches!(source.take(), Some(crate::drag_source::Request::Packet(b)) if b == b"t=q:i=9")
        );
        assert!(
            matches!(source.take(), Some(crate::drag_source::Request::Packet(b)) if b == b"t=o:x=1:i=7")
        );
        assert!(source.take().is_none());
        assert!(matches!(drop.take(), Some(Request::Packet(b)) if b == b"t=m:o=1:i=7:m=1;text/"));
        assert!(matches!(drop.take(), Some(Request::Packet(b)) if b == b"t=m:m=0;plain"));
        assert!(drop.take().is_none());
        assert_eq!(drop.bytes, 0);
        let now = Instant::now();
        let mut r = ready(now);
        event(&mut r, "m", 1, now);
        drain(&mut r);
        r.outgoing.clear();
        r.outgoing_bytes = 0;
        request(&mut r, 1, b"t=m:o=1:i=7:m=1;text/", now);
        assert!(!r.has_outgoing());
        request(&mut r, 2, b"t=m:m=0;wrong-owner", now);
        assert!(!r.has_outgoing());
        request(&mut r, 1, b"t=q:i=9", now);
        request(&mut r, 1, b"t=m:m=0;plain", now);
        assert_eq!(r.outgoing.len(), 2);
        assert!(r.outgoing.back().unwrap().1.ends_with(b";plain\x1b\\"));
        assert!(r.active.as_ref().unwrap().status.is_none());
    }
    #[test]
    fn capability_and_visible_mime_registration_are_verified() {
        let now = Instant::now();
        let mut r = Router::default();
        r.tick(now, true, true, &views(), |_| true);
        request(&mut r, 1, b"t=q:i=9", now);
        assert!(drain(&mut r).is_empty());
        r.response(b"t=q:i=4294967295", now, &views());
        assert!(!r.supported);
        let id = r.probe.unwrap().0;
        r.response(
            format!("t=q:i={id};future=unsupported").as_bytes(),
            now,
            &views(),
        );
        request(&mut r, 1, b"t=q:i=9", now);
        assert_eq!(drain(&mut r), vec![(owner(1), wire("t=q:i=9", None))]);
        request(&mut r, 1, b"t=q:i=9:f=unknown-future-claim", now);
        assert_eq!(drain(&mut r), vec![(owner(1), wire("t=q:i=9", None))]);
        request(&mut r, 1, b"t=a:i=7;text/uri-list", now);
        request(&mut r, 2, b"t=a:i=7;image/png", now);
        r.tick(now, true, true, &views(), |_| true);
        assert_eq!(r.advertised.as_ref().unwrap().signature.len(), 2);
        let old = r.advertised.as_ref().unwrap().wire;
        r.tick(now, true, true, &views()[..1], |_| true);
        assert_ne!(r.advertised.as_ref().unwrap().wire, old);
        r.tick(now, true, true, &[], |_| true);
        assert!(r.advertised.is_none());
        r.tick(now, false, true, &views(), |_| true);
        assert!(!r.enabled);
        assert!(r.registrations.is_empty());
    }
    #[test]
    fn hover_routes_to_pointer_not_focus_and_sends_leave_and_cached_mimes() {
        let now = Instant::now();
        let mut r = ready(now);
        event(&mut r, "m", 1, now);
        let first = drain(&mut r);
        assert_eq!(first[0].0, owner(1));
        assert!(
            first[0]
                .1
                .starts_with(b"\x1b]72;t=m:x=1:y=1:X=15:Y=25:o=3:m=0:i=7;")
        );
        let id = r.advertised.as_ref().unwrap().wire;
        r.response(
            format!("t=m:i={id}:x=42:y=3:X=425:Y=65:o=3").as_bytes(),
            now,
            &views(),
        );
        let next = drain(&mut r);
        assert_eq!(next.len(), 2);
        assert_eq!(next[0], (owner(1), Router::leave(Some("7"))));
        assert_eq!(next[1].0, owner(2));
        assert!(next[1].1.ends_with(b";text/uri-list image/png\x1b\\"));
        request(&mut r, 1, b"t=m:o=1:i=7;text/uri-list", now);
        assert_eq!(r.active.as_ref().unwrap().owner, owner(2));
        request(&mut r, 1, b"t=r:x=1:i=7", now);
        assert!(drain(&mut r)[0].1.ends_with(b";EPERM\x1b\\"));
        r.response(
            format!("t=m:i={id}:x=40:y=3:X=405:Y=65:o=3").as_bytes(),
            now,
            &views(),
        );
        assert!(r.active.is_none());
        assert_eq!(drain(&mut r)[0].0, owner(2));
    }
    #[test]
    fn machine_registration_alone_does_not_accept_a_drop() {
        let now = Instant::now();
        let mut r = ready(now);
        request(&mut r, 2, b"t=A:i=7", now);
        request(&mut r, 2, b"t=a:x=1;machine-B", now);
        r.tick(now, true, true, &views(), |_| true);
        assert_eq!(r.advertised.as_ref().unwrap().signature.len(), 1);
        event(&mut r, "m", 2, now);
        assert!(r.active.is_none());
        assert!(drain(&mut r).is_empty());
        event(&mut r, "M", 2, now);
        assert!(r.active.is_none());
        assert!(drain(&mut r).is_empty());
        assert!(r.advertised.is_none());
    }
    #[test]
    fn data_requires_physical_drop_and_preserves_chunks_indices_and_owner() {
        let now = Instant::now();
        let mut r = ready(now);
        event(&mut r, "m", 2, now);
        drain(&mut r);
        request(&mut r, 2, b"t=r:x=1:i=7", now);
        assert_eq!(drain(&mut r)[0].1, wire("t=R:i=7:x=1", Some("EPERM")));
        event(&mut r, "M", 2, now);
        drain(&mut r);
        let id = r.active.as_ref().unwrap().wire;
        request(&mut r, 2, b"t=r:x=1:i=7", now);
        r.response(
            format!("t=r:x=2:i={id};UNSOLICITED").as_bytes(),
            now,
            &views(),
        );
        assert!(drain(&mut r).is_empty());
        r.response(format!("t=r:x=1:i={id}:m=1;AA").as_bytes(), now, &views());
        r.response(b"m=0;==", now, &views());
        r.response(format!("t=r:x=1:i={id};").as_bytes(), now, &views());
        let data = drain(&mut r);
        assert_eq!(data.len(), 3);
        assert!(data.iter().all(|(o, _)| *o == owner(2)));
        assert_eq!(data[1].1, wire("m=0:i=7", Some("==")));
        assert!(r.active.as_ref().unwrap().requests.is_empty());
        request(&mut r, 1, b"t=r:o=1:i=7", now);
        assert!(r.active.is_some());
        drain(&mut r);
        request(&mut r, 2, b"t=r:o=1:i=7", now);
        assert!(r.active.is_none());
        assert!(r.advertised.is_none());
        r.response(format!("t=r:x=1:i={id};STALE").as_bytes(), now, &views());
        assert!(drain(&mut r).is_empty());
        assert!(
            r.outgoing
                .iter()
                .any(|(_, b)| *b == wire(&format!("t=r:o=1:i={id}"), None))
        );
    }
    #[test]
    fn cancel_timeout_modes_exit_and_respawn_retire_drop_ids() {
        let now = Instant::now();
        let mut r = ready(now);
        event(&mut r, "M", 1, now);
        drain(&mut r);
        let id = r.active.as_ref().unwrap().wire;
        request(&mut r, 1, b"t=r:x=1:i=7", now);
        r.tick(now, true, false, &views(), |_| true);
        assert!(r.active.is_none());
        assert!(r.advertised.is_none());
        assert!(r.outgoing.iter().all(|(tag, _)| *tag != id));
        assert_eq!(drain(&mut r)[0].1, wire("t=R:i=7:x=1", Some("ECANCELED")));
        r.tick(now, true, true, &views(), |_| true);
        event(&mut r, "m", 2, now);
        drain(&mut r);
        r.tick(now + IDLE, true, true, &views(), |_| true);
        assert!(r.active.is_none());
        assert!(drain(&mut r)[0].1.starts_with(b"\x1b]72;t=m:x=-1:y=-1"));
        event(&mut r, "M", 2, now);
        drain(&mut r);
        r.tick(now, true, true, &views(), |o| o != owner(2));
        assert!(r.active.is_none());
        r.cancel_all();
        assert!(r.registrations.is_empty());
        r.response(
            format!("t=M:i={id}:x=2:y=3:X=25:Y=65:o=3;text/uri-list").as_bytes(),
            now,
            &views(),
        );
        assert!(r.active.is_none());
    }
    #[test]
    fn fragmented_registration_and_remote_machine_ids_are_pane_owned() {
        let now = Instant::now();
        let mut r = ready(now);
        request(&mut r, 1, b"t=a:x=1;1:machine-A", now);
        request(&mut r, 1, b"t=a:i=8:m=1;text/", now);
        request(&mut r, 2, b"t=a:i=9:m=1;image/", now);
        request(&mut r, 1, b"m=0;uri-list", now);
        request(&mut r, 2, b"m=0;png", now);
        assert_eq!(r.registrations[0].mimes.as_deref(), Some("text/uri-list"));
        assert_eq!(r.registrations[1].mimes.as_deref(), Some("image/png"));
        r.tick(now, true, true, &views(), |_| true);
        event(&mut r, "m", 1, now);
        assert!(
            r.outgoing
                .iter()
                .any(|(_, b)| b.ends_with(b";1:machine-A\x1b\\"))
        );
        assert!(drain(&mut r)[0].1.windows(4).any(|w| w == b"i=8;"));
        request(&mut r, 1, b"t=a:m=1;i", now);
        r.tick(now + IDLE, true, true, &views(), |_| true);
        assert!(r.partials.is_empty());
    }
    #[test]
    fn host_event_chunks_are_bounded_and_timeout_without_leaking_to_keyboard() {
        let now = Instant::now();
        let mut r = ready(now);
        let id = r.advertised.as_ref().unwrap().wire;
        r.response(
            format!("t=m:i={id}:x=2:y=3:X=25:Y=65:o=3:m=1;text/").as_bytes(),
            now,
            &views(),
        );
        assert!(r.active.is_none());
        r.response(b"m=0;uri-list", now, &views());
        assert!(drain(&mut r)[0].1.ends_with(b";text/uri-list\x1b\\"));
        r.cancel("ECANCELED");
        drain(&mut r);
        r.tick(now, true, true, &views(), |_| true);
        let id = r.advertised.as_ref().unwrap().wire;
        r.response(
            format!("t=m:i={id}:x=2:y=3:X=25:Y=65:o=3:m=1;text/").as_bytes(),
            now,
            &views(),
        );
        r.tick(now + IDLE, true, true, &views(), |_| true);
        assert!(r.event_chunks.is_none());
        let mut o = Observer::default();
        o.configure(Some(true), false);
        for _ in 0..10 {
            o.advance(&wire("t=a:m=1", Some(&"a".repeat(4096))), &mut |_| {});
        }
        assert!(o.pending.iter().any(|r| matches!(r, Request::Overflow)));
        assert!(o.bytes <= MAX_MIMES + 8192);
    }
    #[test]
    fn chunked_errors_cancel_without_leaving_child_waiting_for_continuation() {
        let now = Instant::now();
        let mut r = ready(now);
        event(&mut r, "M", 1, now);
        drain(&mut r);
        request(&mut r, 1, b"t=r:x=1:i=7", now);
        let id = r.active.as_ref().unwrap().wire;
        r.response(format!("t=R:x=1:i={id}:m=1;EIO:").as_bytes(), now, &views());
        assert!(r.active.is_none());
        assert_eq!(
            drain(&mut r),
            vec![(owner(1), wire("t=R:i=7:x=1", Some("EFBIG")))]
        );
        r.response(b"m=0;error", now, &views());
        assert!(drain(&mut r).is_empty());
    }
    #[test]
    fn response_errors_and_backpressure_keep_packets_whole() {
        let now = Instant::now();
        let mut r = ready(now);
        event(&mut r, "M", 1, now);
        drain(&mut r);
        request(&mut r, 1, b"t=r:x=1:i=7", now);
        let id = r.active.as_ref().unwrap().wire;
        r.response(
            format!("t=R:x=1:i={id};EIO:failure").as_bytes(),
            now,
            &views(),
        );
        assert!(r.active.is_none());
        assert!(r.outgoing.iter().all(|(queued_tag, _)| *queued_tag != id));
        assert_eq!(drain(&mut r)[0].1, wire("t=R:x=1:i=7", Some("EIO:failure")));
        let before = r.outgoing_bytes;
        let mut out = VecDeque::new();
        r.pump(&mut out, 1);
        assert!(out.is_empty());
        r.pump(&mut out, MAX_PENDING);
        assert_eq!(out.len(), before);
        r.inner(owner(1), 0, wire("t=R:x=1", Some("EIO")));
        let before = r.incoming_bytes;
        r.drain(|_, _| false);
        assert_eq!(r.incoming_bytes, before);
        assert_eq!(drain(&mut r).len(), 1);
    }
}
