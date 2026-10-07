//! Bounded OSC 72 source-side relay. Data stays opaque; no filesystem access.
use crate::{
    layout::Rect,
    rich_clipboard::Owner,
    terminal_ipc::{Framer, Protocol},
};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

const MAX_PACKET: usize = 8192;
const MAX_PENDING: usize = 256 * 1024;
// A 64 KiB frontend batch can grow when restoring a longer nested-client ID.
const RESERVE: usize = 128 * 1024;
const IDLE: Duration = Duration::from_secs(30);
const PROBE: Duration = Duration::from_secs(1);

struct Packet<'a> {
    fields: Vec<(&'a str, &'a str)>,
    payload: &'a str,
    has_payload: bool,
}
impl<'a> Packet<'a> {
    fn parse(body: &'a [u8]) -> Option<Self> {
        let text = std::str::from_utf8(body).ok()?;
        let (metadata, payload) = text.split_once(';').unwrap_or((text, ""));
        if metadata.len() > 512 || payload.len() > 4096 || text.chars().any(char::is_control) {
            return None;
        }
        let mut fields = Vec::new();
        for field in metadata.split(':') {
            let (key, value) = field.split_once('=')?;
            if key.len() != 1
                || !key.bytes().all(|b| b.is_ascii_alphabetic())
                || fields.iter().any(|(k, _)| *k == key)
                || fields.len() == 16
            {
                return None;
            }
            match key {
                "t" if !matches!(
                    value,
                    "a" | "A" | "o" | "p" | "P" | "e" | "E" | "k" | "q" | "m" | "M" | "r" | "R"
                ) =>
                {
                    return None;
                }
                "i" | "o" => {
                    value.parse::<u32>().ok()?;
                }
                "m" if !matches!(value, "0" | "1") => return None,
                "x" | "y" | "X" | "Y"
                    if value.parse::<i32>().is_err() && value.parse::<u32>().is_err() =>
                {
                    return None;
                }
                _ => {}
            }
            fields.push((key, value));
        }
        Some(Self {
            fields,
            payload,
            has_payload: text.contains(';'),
        })
    }
    fn value(&self, key: &str) -> Option<&'a str> {
        self.fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
    fn number(&self, key: &str) -> Option<i64> {
        self.value(key)?.parse().ok()
    }
    fn more(&self) -> bool {
        self.value("m") == Some("1")
    }
    fn encode(&self, id: Option<&str>, position: Option<(i64, i64, i64, i64)>) -> Vec<u8> {
        let mut parts = Vec::new();
        for &(k, v) in &self.fields {
            if k == "i" || (position.is_some() && matches!(k, "x" | "y" | "X" | "Y")) {
                continue;
            }
            parts.push(format!("{k}={v}"));
        }
        if let Some((x, y, px, py)) = position {
            parts.extend([
                format!("x={x}"),
                format!("y={y}"),
                format!("X={px}"),
                format!("Y={py}"),
            ]);
        }
        if let Some(id) = id {
            parts.push(format!("i={id}"));
        }
        let mut wire = format!("\x1b]72;{}", parts.join(":")).into_bytes();
        if self.has_payload {
            wire.push(b';');
            wire.extend_from_slice(self.payload.as_bytes());
        }
        wire.extend_from_slice(b"\x1b\\");
        wire
    }
}
fn command(fields: &str, id: u32) -> Vec<u8> {
    format!("\x1b]72;{fields}:i={id}\x1b\\").into_bytes()
}
fn error(id: Option<&str>, code: &str) -> Vec<u8> {
    let id = id.map_or(String::new(), |id| format!(":i={id}"));
    format!("\x1b]72;t=E{id};{code}\x1b\\").into_bytes()
}
fn next_id() -> Option<u32> {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    allocate_id(&NEXT)
}
fn allocate_id(counter: &AtomicU32) -> Option<u32> {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.checked_add(1)?;
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Some(current),
            Err(value) => current = value,
        }
    }
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
            framer: Framer::drag(),
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
            if let Some((Protocol::Drag, body)) = self.framer.advance(byte, &mut ignored) {
                let Some(packet) = Packet::parse(&body) else {
                    if self.permission == Some(true) {
                        self.overflow();
                    }
                    ignored.clear();
                    continue;
                };
                if self.permission == Some(true) {
                    if self.bytes + body.len() <= 2 * MAX_PACKET {
                        self.bytes += body.len();
                        self.pending.push_back(Request::Packet(body));
                    } else {
                        self.overflow();
                    }
                } else if packet.value("t") == Some("o")
                    && packet.number("o").is_some_and(|o| o > 0)
                {
                    reply(&error(
                        packet.value("i"),
                        if self.permission.is_some() {
                            "EPERM"
                        } else {
                            "ENOSYS"
                        },
                    ));
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
    fn overflow(&mut self) {
        self.pending.clear();
        self.bytes = 0;
        self.pending.push_back(Request::Overflow);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Registration {
    owner: Owner,
    id: Option<String>,
    body: Vec<u8>,
}
#[derive(Debug)]
struct Advertised {
    registration: Registration,
    wire: u32,
}
#[derive(Debug)]
struct Gesture {
    owner: Owner,
    id: Option<String>,
    wire: u32,
    last: Instant,
    offered: bool,
    started: bool,
    outgoing_chunk: bool,
}
#[derive(Clone, Copy)]
pub(crate) struct View {
    pub owner: Owner,
    pub rect: Rect,
    pub pixels: (u16, u16),
}

#[derive(Default, Debug)]
pub(crate) struct Router {
    enabled: bool,
    supported: bool,
    geometry_ready: bool,
    probe: Option<(u32, Instant)>,
    registrations: Vec<Registration>,
    advertised: Option<Advertised>,
    gesture: Option<Gesture>,
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
    fn outer(&mut self, wire: u32, bytes: Vec<u8>) {
        if self.outgoing_bytes + bytes.len() <= MAX_PENDING {
            self.outgoing_bytes += bytes.len();
            self.outgoing.push_back((wire, bytes));
        }
    }
    fn inner(&mut self, owner: Owner, wire: u32, bytes: Vec<u8>) {
        if self.incoming_bytes + bytes.len() <= MAX_PENDING {
            self.incoming_bytes += bytes.len();
            self.incoming.push_back((owner, wire, bytes));
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
    fn forget_wire(&mut self, wire: u32) {
        self.outgoing.retain(|(id, _)| *id != wire);
        self.incoming.retain(|(_, id, _)| *id != wire);
        self.outgoing_bytes = self.outgoing.iter().map(|(_, b)| b.len()).sum();
        self.incoming_bytes = self.incoming.iter().map(|(_, _, b)| b.len()).sum();
    }
    fn unadvertise(&mut self) {
        if let Some(ad) = self.advertised.take() {
            self.forget_wire(ad.wire);
            self.outer(0, command("t=o:x=2", ad.wire));
        }
    }
    fn cancel_gesture(&mut self, code: &str) {
        if let Some(g) = self.gesture.take() {
            self.forget_wire(g.wire);
            self.outer(0, command("t=E:y=-1", g.wire));
            self.inner(g.owner, 0, error(g.id.as_deref(), code));
        }
        self.unadvertise();
    }
    pub fn cancel_all(&mut self) {
        self.cancel_gesture("ECANCELED");
        self.registrations.clear();
        if let Some((id, _)) = self.probe.take() {
            self.forget_wire(id);
        }
        self.supported = false;
        self.enabled = false;
    }
    pub fn tick(
        &mut self,
        now: Instant,
        enabled: bool,
        allowed: bool,
        view: Option<View>,
        mut live: impl FnMut(Owner) -> bool,
    ) {
        self.geometry_ready = view.is_some();
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
                self.outer(id, command("t=q", id));
            }
        }
        if self
            .probe
            .is_some_and(|(_, t)| now.saturating_duration_since(t) >= PROBE)
        {
            self.probe = None;
        }
        self.registrations.retain(|r| live(r.owner));
        self.incoming.retain(|(owner, _, _)| live(*owner));
        self.incoming_bytes = self.incoming.iter().map(|(_, _, b)| b.len()).sum();
        if self.gesture.as_ref().is_some_and(|g| {
            !live(g.owner) || now.saturating_duration_since(g.last) >= IDLE || !allowed
        }) {
            self.cancel_gesture("ECANCELED");
        }
        if self.gesture.is_some() {
            return;
        }
        let selected = view
            .filter(|_| allowed && self.supported)
            .and_then(|v| self.registrations.iter().find(|r| r.owner == v.owner))
            .cloned();
        if self.advertised.as_ref().map(|a| &a.registration) == selected.as_ref() {
            return;
        }
        self.unadvertise();
        if let Some(registration) = selected
            && let Some(wire) = next_id()
        {
            let packet = Packet::parse(&registration.body).unwrap();
            self.outer(wire, packet.encode(Some(&wire.to_string()), None));
            self.advertised = Some(Advertised { registration, wire });
        }
    }
    pub fn request(&mut self, owner: Owner, request: Request, now: Instant) {
        let Request::Packet(body) = request else {
            self.registrations.retain(|r| r.owner != owner);
            if self.gesture.as_ref().is_some_and(|g| g.owner == owner) {
                self.cancel_gesture("EFBIG");
            }
            return;
        };
        let Some(packet) = Packet::parse(&body) else {
            return;
        };
        if !self.enabled {
            return;
        }
        let kind = packet.value("t");
        if kind == Some("q") {
            // Echo verified host support; do not pass future optional claims.
            if self.supported && self.geometry_ready && !packet.more() && packet.payload.is_empty()
            {
                self.inner(
                    owner,
                    0,
                    Packet::parse(b"t=q")
                        .unwrap()
                        .encode(packet.value("i"), None),
                );
            }
            return;
        }
        if kind == Some("o") && packet.number("o").unwrap_or(0) == 0 {
            match packet.number("x") {
                Some(1) if !packet.more() => {
                    if self.gesture.as_ref().is_some_and(|g| g.owner == owner) {
                        self.cancel_gesture("ECANCELED");
                    }
                    self.registrations.retain(|r| r.owner != owner);
                    if self.registrations.len() < 64 {
                        self.registrations.push(Registration {
                            owner,
                            id: packet.value("i").map(str::to_owned),
                            body,
                        });
                    }
                }
                Some(2) => {
                    self.registrations.retain(|r| r.owner != owner);
                    if self.gesture.as_ref().is_some_and(|g| g.owner == owner) {
                        self.cancel_gesture("ECANCELED");
                    }
                }
                _ => {}
            }
            return;
        }
        let Some(g) = self.gesture.as_mut().filter(|g| {
            g.owner == owner
                && (g.id.as_deref().map_or(0, |id| id.parse::<i64>().unwrap())
                    == packet.number("i").unwrap_or(0)
                    || (g.outgoing_chunk && packet.value("i").is_none()))
        }) else {
            if kind == Some("o") && packet.number("o").is_some_and(|o| o > 0) {
                self.inner(
                    owner,
                    0,
                    error(
                        packet.value("i"),
                        if self.supported && self.geometry_ready {
                            "EPERM"
                        } else {
                            "ENOSYS"
                        },
                    ),
                );
            }
            return;
        };
        if kind == Some("E") && packet.number("y") == Some(-1) {
            let wire = g.wire;
            self.gesture = None;
            self.forget_wire(wire);
            self.outer(0, packet.encode(Some(&wire.to_string()), None));
            self.unadvertise();
            return;
        }
        let permitted = if g.outgoing_chunk {
            kind.is_none() || matches!(kind, Some("o" | "p" | "e" | "k"))
        } else {
            match kind {
                Some("o")
                    if !g.offered && packet.number("o").is_some_and(|o| (1..=3).contains(&o)) =>
                {
                    g.offered = true;
                    true
                }
                Some("p" | "P") => g.offered,
                Some("e" | "k") => g.started,
                Some("E") => true,
                _ => false,
            }
        };
        if !permitted {
            return;
        }
        g.outgoing_chunk = packet.more();
        g.last = now;
        let wire = g.wire;
        self.outer(wire, packet.encode(Some(&wire.to_string()), None));
    }
    pub fn response(&mut self, body: &[u8], now: Instant, view: Option<View>) {
        let Some(packet) = Packet::parse(body) else {
            return;
        };
        if packet.value("t") == Some("o") {
            if packet.more() {
                return;
            }
            if self.gesture.is_some() {
                return;
            }
            // Kitty assigns its source client ID when it receives the MIME
            // offer, not the registration. The initial physical gesture can
            // therefore omit i, or carry the previous offer's ID. Bind it to
            // the sole currently advertised source; later replies must echo
            // the ID we put on that source's offer.
            let Some(ad) = self.advertised.as_ref() else {
                return;
            };
            let wire = ad.wire;
            let Some(view) = view.filter(|v| v.owner == ad.registration.owner) else {
                return;
            };
            let Some((x, y)) = packet.number("x").zip(packet.number("y")) else {
                return;
            };
            let rect = view.rect;
            let x = x - i64::from(rect.column);
            let y = y - i64::from(rect.row);
            if x < 0 || y < 0 || x >= i64::from(rect.columns) || y >= i64::from(rect.rows) {
                return;
            }
            let Some((px, py)) = packet.number("X").zip(packet.number("Y")) else {
                return;
            };
            let px = px - i64::from(rect.column) * i64::from(view.pixels.0);
            let py = py - i64::from(rect.row) * i64::from(view.pixels.1);
            if px < 0
                || py < 0
                || px >= i64::from(rect.columns) * i64::from(view.pixels.0)
                || py >= i64::from(rect.rows) * i64::from(view.pixels.1)
            {
                return;
            }
            let owner = ad.registration.owner;
            let id = ad.registration.id.clone();
            self.gesture = Some(Gesture {
                owner,
                id: id.clone(),
                wire,
                last: now,
                offered: false,
                started: false,
                outgoing_chunk: false,
            });
            self.inner(
                owner,
                wire,
                packet.encode(id.as_deref(), Some((x, y, px, py))),
            );
            return;
        }
        let Some(wire) = packet.value("i").and_then(|v| v.parse::<u32>().ok()) else {
            return;
        };
        if packet.value("t") == Some("q") {
            if self.probe.is_some_and(|(id, _)| id == wire) && !packet.more() {
                self.probe = None;
                self.supported = true;
            }
            return;
        }
        if packet.more() && self.gesture.as_ref().is_some_and(|g| g.wire == wire) {
            self.cancel_gesture("EFBIG");
            return;
        }
        let Some(g) = self.gesture.as_mut().filter(|g| g.wire == wire) else {
            return;
        };
        let kind = packet.value("t");
        if kind == Some("E") && packet.payload.is_empty() {
            return;
        }
        if !matches!(kind, Some("e" | "E" | "k")) {
            return;
        }
        if kind == Some("e") && !packet.number("x").is_some_and(|x| (1..=5).contains(&x)) {
            return;
        }
        if kind == Some("k") && !packet.number("x").is_some_and(|x| x > 0) {
            return;
        }
        g.last = now;
        if kind == Some("E") && packet.payload == "OK" {
            if !g.offered {
                return;
            }
            g.started = true;
        }
        let finished = (kind == Some("e") && packet.number("x") == Some(4))
            || (kind == Some("E") && packet.payload != "OK");
        let owner = g.owner;
        let id = g.id.clone();
        self.inner(owner, wire, packet.encode(id.as_deref(), None));
        if finished {
            self.outgoing.retain(|(id, _)| *id != wire);
            self.outgoing_bytes = self.outgoing.iter().map(|(_, bytes)| bytes.len()).sum();
            self.gesture = None; /* Keep the final reply for ordered delivery. */
            if let Some(ad) = self.advertised.take() {
                self.outer(0, command("t=o:x=2", ad.wire));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner(pane: u64) -> Owner {
        Owner {
            pane,
            incarnation: 0,
        }
    }
    fn view(pane: u64) -> View {
        View {
            owner: owner(pane),
            rect: Rect {
                column: 40,
                row: 2,
                columns: 30,
                rows: 20,
            },
            pixels: (10, 20),
        }
    }
    fn request(router: &mut Router, pane: u64, body: &[u8], now: Instant) {
        router.request(owner(pane), Request::Packet(body.to_vec()), now);
    }
    fn ready(now: Instant) -> Router {
        let mut r = Router::default();
        r.tick(now, true, true, Some(view(1)), |_| true);
        let id = r.probe.unwrap().0;
        r.response(format!("t=q:i={id}").as_bytes(), now, None);
        r.outgoing.clear();
        r.outgoing_bytes = 0;
        r
    }
    fn register(r: &mut Router, pane: u64, id: u32, now: Instant) -> u32 {
        request(r, pane, format!("t=o:x=1:i={id};1:machine").as_bytes(), now);
        r.tick(now, true, true, Some(view(pane)), |_| true);
        r.advertised.as_ref().unwrap().wire
    }
    fn gesture(r: &mut Router, pane: u64, now: Instant) -> u32 {
        let id = register(r, pane, 7, now);
        r.response(
            format!("t=o:i={id}:x=43:y=4:X=435:Y=87").as_bytes(),
            now,
            Some(view(pane)),
        );
        id
    }
    fn deliveries(r: &mut Router) -> Vec<(Owner, Vec<u8>)> {
        let mut output = Vec::new();
        r.drain(|owner, bytes| {
            output.push((owner, bytes.to_vec()));
            true
        });
        output
    }
    #[test]
    fn framing_validation_permission_and_nested_paste_are_bounded() {
        let wire = b"\x1b]72;t=o:x=1:i=7;1:machine\x1b\\";
        for split in 0..=wire.len() {
            let mut o = Observer::default();
            o.configure(Some(true));
            let mut reply = Vec::new();
            o.advance(&wire[..split], &mut |b| reply.extend_from_slice(b));
            o.advance(&wire[split..], &mut |b| reply.extend_from_slice(b));
            assert!(matches!(o.take(), Some(Request::Packet(_))));
            assert!(o.take().is_none());
            assert!(reply.is_empty());
        }
        for bad in [
            b"t=o:t=o".as_slice(),
            b"t=o:i=-1",
            b"t=o:x=4294967296",
            b"t=o:m=2",
            b"t=o;bad\x1b",
            b"t=o;bad\n",
        ] {
            assert!(Packet::parse(bad).is_none());
        }
        assert!(Packet::parse(format!("t=p;{}", "A".repeat(4097)).as_bytes()).is_none());
        let mut o = Observer::default();
        let mut replies = Vec::new();
        o.advance(b"\x1b]72;t=o:o=1:i=9;text/plain\x1b\\", &mut |b| {
            replies.extend_from_slice(b)
        });
        assert_eq!(replies, error(Some("9"), "ENOSYS"));
        o.configure(Some(false));
        replies.clear();
        o.advance(b"\x1b]72;t=o:o=1;text/plain\x1b\\", &mut |b| {
            replies.extend_from_slice(b)
        });
        assert_eq!(replies, error(None, "EPERM"));
        o.configure(Some(true));
        for nested in [b"\x1bP".as_slice(), b"\x1b_", b"\x1b]ignored;"] {
            let mut input = nested.to_vec();
            input.extend(wire);
            input.extend(b"\x1b\\");
            o.advance(&input, &mut |_| {});
            assert!(o.take().is_none());
        }
        let mut input = b"\x1b[200~".to_vec();
        input.extend(wire);
        input.extend(b"\x1b[201~");
        o.advance(&input, &mut |_| {});
        assert!(o.take().is_none());
        o.advance(&wire[..10], &mut |_| {});
        o.configure(Some(false));
        o.configure(Some(true));
        o.advance(&wire[10..], &mut |_| {});
        assert!(matches!(o.take(), Some(Request::Overflow)));
        assert!(o.take().is_none());
    }
    #[test]
    fn capability_is_verified_and_queries_and_drop_requests_are_isolated() {
        let now = Instant::now();
        let mut r = Router::default();
        r.tick(now, true, true, Some(view(1)), |_| true);
        request(&mut r, 1, b"t=q:i=7", now);
        assert!(deliveries(&mut r).is_empty());
        let probe = r.probe.unwrap().0;
        r.response(b"t=q:i=4294967295", now, None);
        assert!(!r.supported);
        r.response(
            format!("t=q:i={probe};future=anything").as_bytes(),
            now,
            None,
        );
        assert!(r.supported);
        request(&mut r, 1, b"t=q:i=7", now);
        assert_eq!(
            deliveries(&mut r),
            vec![(owner(1), b"\x1b]72;t=q:i=7\x1b\\".to_vec())]
        );
        request(&mut r, 1, b"t=q;i=spoof", now);
        assert!(deliveries(&mut r).is_empty());
        request(&mut r, 1, b"t=a;i=anything", now);
        assert!(r.registrations.is_empty());
        r.tick(now, false, true, Some(view(1)), |_| true);
        assert!(!r.supported);
        r.tick(now, true, true, Some(view(1)), |_| true);
        let old = r.probe.unwrap().0;
        r.tick(now + PROBE, true, true, Some(view(1)), |_| true);
        assert!(!r.probing());
        r.response(format!("t=q:i={old}").as_bytes(), now, None);
        assert!(!r.supported);
    }
    #[test]
    fn coordinates_focus_and_original_identifiers_follow_the_source() {
        let now = Instant::now();
        let mut r = ready(now);
        let id = gesture(&mut r, 1, now);
        assert_eq!(
            deliveries(&mut r),
            vec![(
                owner(1),
                b"\x1b]72;t=o:x=3:y=2:X=35:Y=47:i=7\x1b\\".to_vec()
            )]
        );
        request(&mut r, 1, b"t=o:o=1:i=7;text/plain", now);
        r.tick(now, true, true, Some(view(2)), |_| true);
        assert_eq!(r.advertised.as_ref().unwrap().wire, id);
        r.response(format!("t=E:i={id};OK").as_bytes(), now, Some(view(2)));
        r.response(format!("t=e:x=5:y=0:i={id}").as_bytes(), now, Some(view(2)));
        let output = deliveries(&mut r);
        assert_eq!(output.len(), 2);
        assert!(output.iter().all(|(o, _)| *o == owner(1)));
        r.response(format!("t=e:x=4:y=0:i={id}").as_bytes(), now, None);
        assert!(r.gesture.is_none());
        assert_eq!(deliveries(&mut r).len(), 1);
        let fresh = register(&mut r, 1, 7, now);
        assert_ne!(fresh, id);
        r.response(format!("t=e:x=5:y=0:i={id}").as_bytes(), now, None);
        assert!(deliveries(&mut r).is_empty());
        r.response(
            format!("t=o:i={fresh}:x=39:y=4:X=390:Y=80").as_bytes(),
            now,
            Some(view(1)),
        );
        assert!(r.gesture.is_none());
        r.response(
            format!("t=o:i={fresh}:x=43:y=4:X=900:Y=80").as_bytes(),
            now,
            Some(view(1)),
        );
        assert!(r.gesture.is_none());
    }
    #[test]
    fn kitty_gestures_use_the_advertised_source_before_offer_id_assignment() {
        let now = Instant::now();
        let mut r = ready(now);
        let first = register(&mut r, 1, 7, now);
        // An untagged event cannot target a different pane or a border.
        r.response(b"t=o:x=43:y=4:X=435:Y=87", now, Some(view(2)));
        r.response(b"t=o:x=39:y=4:X=395:Y=87", now, Some(view(1)));
        assert!(r.gesture.is_none());
        r.response(b"t=o:x=43:y=4:X=435:Y=87", now, Some(view(1)));
        assert_eq!(r.gesture.as_ref().unwrap().wire, first);
        assert_eq!(
            deliveries(&mut r),
            vec![(
                owner(1),
                b"\x1b]72;t=o:x=3:y=2:X=35:Y=47:i=7\x1b\\".to_vec()
            )]
        );
        request(&mut r, 1, b"t=o:o=3:i=7;text/uri-list", now);
        r.response(b"t=E;OK", now, None);
        assert!(!r.gesture.as_ref().unwrap().started);
        r.response(format!("t=E:i={first};OK").as_bytes(), now, None);
        assert!(r.gesture.as_ref().unwrap().started);
        r.response(format!("t=e:x=4:y=0:i={first}").as_bytes(), now, None);
        deliveries(&mut r);
        let second = register(&mut r, 2, 9, now);
        // Kitty retains its last MIME offer ID even after unregistering. It
        // is not the current registration's correlation ID on a new gesture.
        r.response(
            format!("t=o:i={first}:x=43:y=4:X=435:Y=87").as_bytes(),
            now,
            Some(view(2)),
        );
        assert_eq!(r.gesture.as_ref().unwrap().wire, second);
        assert_eq!(deliveries(&mut r)[0].0, owner(2));
        request(&mut r, 2, b"t=o:o=3:i=9;text/uri-list", now);
        r.response(format!("t=E:i={first};OK").as_bytes(), now, None);
        assert!(!r.gesture.as_ref().unwrap().started);
        assert!(deliveries(&mut r).is_empty());
        r.response(format!("t=E:i={second};OK").as_bytes(), now, None);
        assert!(r.gesture.as_ref().unwrap().started);
        r.cancel_all();
        deliveries(&mut r);
        r.response(b"t=o:x=43:y=4:X=435:Y=87", now, Some(view(2)));
        assert!(r.gesture.is_none());
    }
    #[test]
    fn chunk_continuations_and_backpressure_preserve_whole_packets() {
        let now = Instant::now();
        let mut r = ready(now);
        let id = gesture(&mut r, 1, now);
        deliveries(&mut r);
        request(&mut r, 1, b"t=p:x=0:i=7;QQ==", now);
        assert!(!r.gesture.as_ref().unwrap().offered);
        request(&mut r, 1, b"t=o:o=1:i=7;text/plain", now);
        request(&mut r, 1, b"t=p:x=0:m=1:i=7;AA", now);
        request(&mut r, 1, b"m=0;==", now);
        assert!(!r.gesture.as_ref().unwrap().outgoing_chunk);
        let continuation = r.outgoing.back().unwrap().1.clone();
        assert_eq!(
            continuation,
            format!("\x1b]72;m=0:i={id};==\x1b\\").into_bytes()
        );
        let big = format!("t=p:x=0:i=7;m=not-metadata{}", "A".repeat(4000));
        while r.can_receive() {
            request(&mut r, 1, big.as_bytes(), now);
        }
        assert!(r.outgoing_bytes <= MAX_PENDING);
        let mut out = VecDeque::new();
        r.pump(&mut out, 10);
        assert!(out.is_empty());
        let before = r.outgoing_bytes;
        let mut all = VecDeque::new();
        r.pump(&mut all, MAX_PENDING);
        assert_eq!(all.len(), before);
        r.response(format!("t=E:i={id};OK").as_bytes(), now, None);
        while r.can_receive() {
            r.response(
                format!("t=e:x=5:y=0:i={id};{}", "A".repeat(4000)).as_bytes(),
                now,
                None,
            );
        }
        assert!(r.incoming_bytes <= MAX_PENDING);
        let before = r.incoming_bytes;
        r.drain(|_, _| false);
        assert_eq!(r.incoming_bytes, before);
        deliveries(&mut r);
        assert_eq!(r.incoming_bytes, 0);
    }
    #[test]
    fn revocation_idle_and_respawn_retire_ids_and_unsent_data() {
        let now = Instant::now();
        let mut r = ready(now);
        let id = gesture(&mut r, 1, now);
        deliveries(&mut r);
        request(&mut r, 1, b"t=o:o=1:i=7;text/plain", now);
        request(&mut r, 1, b"t=p:x=0:i=7;QQ==", now);
        r.tick(now + IDLE, true, true, Some(view(1)), |_| true);
        assert!(r.gesture.is_none());
        assert!(r.outgoing.iter().all(|(wire, _)| *wire != id));
        assert_eq!(
            deliveries(&mut r),
            vec![(owner(1), error(Some("7"), "ECANCELED"))]
        );
        let id = gesture(&mut r, 1, now);
        deliveries(&mut r);
        r.tick(now, true, true, Some(view(2)), |_| false);
        assert!(r.gesture.is_none());
        deliveries(&mut r);
        r.tick(now, true, true, Some(view(1)), |_| true);
        r.response(format!("t=E:i={id};OK").as_bytes(), now, None);
        assert!(deliveries(&mut r).is_empty());
        let _ = gesture(&mut r, 1, now);
        deliveries(&mut r);
        r.cancel_all();
        assert!(r.registrations.is_empty());
        assert_eq!(deliveries(&mut r).len(), 1);
        r.cancel_all();
        assert!(deliveries(&mut r).is_empty());
    }
    #[test]
    fn explicit_cancel_revokes_queued_data_and_host_completion_drops_unsent_tail() {
        let now = Instant::now();
        let mut r = ready(now);
        let wire = gesture(&mut r, 1, now);
        deliveries(&mut r);
        request(&mut r, 1, b"t=o:o=1:i=7;text/plain", now);
        request(&mut r, 1, b"t=p:x=0:i=7;QQ==", now);
        request(&mut r, 1, b"t=E:y=-1:i=7", now);
        assert!(r.gesture.is_none());
        assert!(r.outgoing.iter().all(|(id, _)| *id != wire));
        assert!(
            r.outgoing
                .iter()
                .any(|(_, bytes)| *bytes == command("t=E:y=-1", wire))
        );
        assert!(deliveries(&mut r).is_empty());
        r.response(format!("t=E:i={wire};OK").as_bytes(), now, None);
        assert!(deliveries(&mut r).is_empty());
        let wire = gesture(&mut r, 1, now);
        deliveries(&mut r);
        request(&mut r, 1, b"t=o:o=1:i=7;text/plain", now);
        request(&mut r, 1, b"t=p:x=0:i=7;QQ==", now);
        r.response(format!("t=E:i={wire};EIO").as_bytes(), now, None);
        assert!(r.outgoing.iter().all(|(id, _)| *id != wire));
        assert_eq!(
            deliveries(&mut r),
            vec![(owner(1), error(Some("7"), "EIO"))]
        );
    }

    #[test]
    fn unavailable_geometry_modes_and_id_exhaustion_are_conservative() {
        let counter = AtomicU32::new(u32::MAX - 1);
        assert_eq!(allocate_id(&counter), Some(u32::MAX - 1));
        assert_eq!(allocate_id(&counter), None);
        assert_eq!(counter.load(Ordering::Relaxed), u32::MAX);
        let now = Instant::now();
        let mut r = ready(now);
        register(&mut r, 1, 7, now);
        r.tick(now, true, true, None, |_| true);
        assert!(r.advertised.is_none());
        request(&mut r, 1, b"t=q:i=7", now);
        assert!(deliveries(&mut r).is_empty());
        request(&mut r, 1, b"t=o:o=1:i=7;text/plain", now);
        assert_eq!(
            deliveries(&mut r),
            vec![(owner(1), error(Some("7"), "ENOSYS"))]
        );
        gesture(&mut r, 1, now);
        deliveries(&mut r);
        r.tick(now, true, false, Some(view(1)), |_| true);
        assert!(r.gesture.is_none());
        assert!(r.advertised.is_none());
        assert_eq!(
            deliveries(&mut r),
            vec![(owner(1), error(Some("7"), "ECANCELED"))]
        );
        r.tick(now, true, true, Some(view(1)), |_| true);
        assert!(r.advertised.is_some());
        // Zero is the protocol default; nested clients can omit it after registration.
        let mut zero = ready(now);
        let id = register(&mut zero, 1, 0, now);
        zero.response(
            format!("t=o:i={id}:x=43:y=4:X=435:Y=87").as_bytes(),
            now,
            Some(view(1)),
        );
        deliveries(&mut zero);
        request(&mut zero, 1, b"t=o:o=1;text/plain", now);
        assert!(zero.gesture.as_ref().unwrap().offered);
        assert!(deliveries(&mut zero).is_empty());
        let mut probing = Router::default();
        probing.tick(now, true, true, Some(view(1)), |_| true);
        probing.cancel_all();
        assert!(probing.outgoing.is_empty());
    }

    #[test]
    fn shared_host_framing_keeps_escape_latency_and_survives_clipboard_cancellation() {
        let wire = b"\x1b]72;t=e:x=5:y=0:i=17\x1b\\";
        for split in 0..=wire.len() {
            let mut framer = Framer::host();
            let mut pass = Vec::new();
            let mut frames = Vec::new();
            for b in &wire[..split] {
                if let Some(p) = framer.advance(*b, &mut pass) {
                    frames.push(p);
                }
            }
            framer.cancel_packet();
            for b in &wire[split..] {
                if let Some(p) = framer.advance(*b, &mut pass) {
                    frames.push(p);
                }
            }
            assert_eq!(frames, vec![(Protocol::Drag, b"t=e:x=5:y=0:i=17".to_vec())]);
            assert!(pass.is_empty());
        }
        let mut f = Framer::host();
        let mut pass = Vec::new();
        f.advance(0x1b, &mut pass);
        f.expire_prefix(Instant::now() + Duration::from_millis(60), &mut pass);
        assert_eq!(pass, b"\x1b");
    }
}
