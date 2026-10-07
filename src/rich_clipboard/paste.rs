//! Stage unsolicited MIME listings; never expose an incomplete notification.
use super::*;

pub(super) const MAX_WIRE: usize = 16 * 1024;

pub(super) struct Event {
    pub owner: Owner,
    pub deadline: Instant,
    password: Option<String>,
    location: Option<String>,
    wire: Vec<u8>,
    listing: Vec<u8>,
    data: bool,
}

impl Event {
    pub fn start(owner: Owner, packet: &Packet<'_>, now: Instant) -> Option<Self> {
        if packet.value("status") != Some("OK") || !packet.payload.is_empty() {
            return None;
        }
        if packet
            .value("loc")
            .is_some_and(|loc| !matches!(loc, "primary" | "clipboard"))
        {
            return None;
        }
        if let Some(password) = packet.value("pw") {
            let decoded = STANDARD.decode(password).ok()?;
            if decoded.is_empty() || decoded.len() > 512 || std::str::from_utf8(&decoded).is_err() {
                return None;
            }
        }
        Some(Self {
            owner,
            deadline: now + IDLE,
            password: packet.value("pw").map(str::to_owned),
            location: packet.value("loc").map(str::to_owned),
            wire: packet.encode(None),
            listing: Vec::new(),
            data: false,
        })
    }

    /// None means invalid; Some(false) awaits more data; Some(true) is complete.
    pub fn accept(&mut self, packet: &Packet<'_>, now: Instant) -> Option<bool> {
        if packet
            .value("pw")
            .is_some_and(|pw| Some(pw) != self.password.as_deref())
            || packet
                .value("loc")
                .is_some_and(|loc| Some(loc) != self.location.as_deref())
        {
            return None;
        }
        let finished = match packet.value("status") {
            Some("DATA") if packet.value("mime") == Some("Lg==") => {
                let bytes = STANDARD.decode(packet.payload).ok()?;
                if bytes.len() > 4096_usize.saturating_sub(self.listing.len()) {
                    return None;
                }
                self.listing.extend(bytes);
                self.data = true;
                false
            }
            Some("DONE") if self.data && packet.payload.is_empty() => {
                if self
                    .listing
                    .iter()
                    .any(|b| !b.is_ascii_whitespace() && !(0x21..=0x7e).contains(b))
                {
                    return None;
                }
                let names: Vec<_> = self
                    .listing
                    .split(u8::is_ascii_whitespace)
                    .filter(|s| !s.is_empty())
                    .collect();
                if names.len() > 128 || names.iter().any(|s| s.len() > 512) {
                    return None;
                }
                true
            }
            _ => return None,
        };
        let encoded = packet.encode(None);
        if encoded.len() > MAX_WIRE.saturating_sub(self.wire.len()) {
            return None;
        }
        self.wire.extend(encoded);
        self.deadline = now + IDLE;
        Some(finished)
    }

    pub fn finish(self) -> (Owner, Vec<u8>) {
        (self.owner, self.wire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner(pane: u64) -> Owner {
        Owner {
            pane,
            incarnation: pane,
        }
    }
    fn packet(meta: &str, data: &[u8]) -> Vec<u8> {
        [
            PREFIX,
            meta.as_bytes(),
            if data.is_empty() { b"" } else { b";" },
            data,
            b"\x1b\\",
        ]
        .concat()
    }
    fn event(data: &[u8]) -> Vec<u8> {
        [
            packet("type=read:status=OK:loc=primary:pw=c2VjcmV0", b""),
            packet(
                "type=read:status=DATA:mime=Lg==:pw=c2VjcmV0",
                STANDARD.encode(data).as_bytes(),
            ),
            packet("type=read:status=DONE:pw=c2VjcmV0", b""),
        ]
        .concat()
    }
    fn input(router: &mut Router, bytes: &[u8], now: Instant) {
        let mut pass = Vec::new();
        router.advance(bytes, &mut pass, now);
        assert!(pass.is_empty());
    }
    fn collect(router: &mut Router) -> Vec<(Owner, Vec<u8>)> {
        let mut result = Vec::new();
        router.drain(|owner, bytes| {
            result.push((owner, bytes.to_vec()));
            true
        });
        result
    }

    #[test]
    fn fragments_pin_owner_at_prefix_and_deliver_only_complete_event() {
        let wire = event(b"text/plain image/png\n");
        let now = Instant::now();
        for split in 1..wire.len() {
            let mut router = Router::default();
            router.set_paste_target(Some(owner(1)));
            input(&mut router, &wire[..split], now);
            assert!(collect(&mut router).is_empty());
            router.set_paste_target(Some(owner(2)));
            input(&mut router, &wire[split..], now);
            assert_eq!(collect(&mut router), [(owner(1), wire.clone())]);
        }
    }
    #[test]
    fn empty_and_whitespace_mime_listings_are_valid_and_bel_is_normalized() {
        for listing in [b"".as_slice(), b" \ttext/plain\nimage/png\r\n"] {
            let wire = event(listing);
            let bel = wire
                .windows(2)
                .enumerate()
                .filter_map(|(i, pair)| (pair == b"\x1b\\").then_some(i))
                .collect::<Vec<_>>();
            let mut input_wire = wire.clone();
            for i in bel.into_iter().rev() {
                input_wire.splice(i..i + 2, [7]);
            }
            let mut router = Router::default();
            router.set_paste_target(Some(owner(1)));
            input(&mut router, &input_wire, Instant::now());
            assert_eq!(collect(&mut router), [(owner(1), wire)]);
        }
    }
    #[test]
    fn malformed_events_never_expose_partial_notifications() {
        let ok = packet("type=read:status=OK:pw=c2VjcmV0", b"");
        for bad in [
            packet(
                "type=read:status=DATA:mime=Lg==:pw=b3RoZXI=",
                b"dGV4dC9wbGFpbg==",
            ),
            packet(
                "type=read:status=DATA:mime=Lg==:loc=primary",
                b"dGV4dC9wbGFpbg==",
            ),
            packet("type=read:status=DATA:mime=dGV4dC9wbGFpbg==", b"eA=="),
            packet("type=read:status=DATA:mime=Lg==", b"!"),
            packet("type=read:status=DONE", b""),
            packet(
                "type=read:status=DATA:mime=Lg==",
                STANDARD.encode(b"text/plain\0").as_bytes(),
            ),
            packet(
                "type=read:status=DATA:mime=Lg==",
                STANDARD.encode(vec![b'x'; 513]).as_bytes(),
            ),
        ] {
            let mut router = Router::default();
            router.set_paste_target(Some(owner(1)));
            input(&mut router, &ok, Instant::now());
            input(&mut router, &bad, Instant::now());
            input(
                &mut router,
                &packet("type=read:status=DONE:pw=c2VjcmV0", b""),
                Instant::now(),
            );
            assert!(collect(&mut router).is_empty());
        }
    }
    #[test]
    fn unsolicited_packets_require_enabled_target_and_initial_ok() {
        let wire = event(b"text/plain");
        let mut router = Router::default();
        input(&mut router, &wire, Instant::now());
        assert!(collect(&mut router).is_empty());
        router.set_paste_target(Some(owner(1)));
        input(
            &mut router,
            &packet("type=read:status=DATA:mime=Lg==", b"Lg=="),
            Instant::now(),
        );
        input(
            &mut router,
            &packet("type=read:status=DONE", b""),
            Instant::now(),
        );
        assert!(collect(&mut router).is_empty());
    }
    #[test]
    fn metadata_and_wire_limits_discard_the_whole_event() {
        for repeated in [false, true] {
            let mut router = Router::default();
            router.set_paste_target(Some(owner(1)));
            input(
                &mut router,
                &packet("type=read:status=OK", b""),
                Instant::now(),
            );
            let data = if repeated {
                Vec::new()
            } else {
                vec![b' '; 4096]
            };
            let chunk = packet(
                "type=read:status=DATA:mime=Lg==",
                STANDARD.encode(data).as_bytes(),
            );
            for _ in 0..if repeated {
                MAX_WIRE / chunk.len() + 1
            } else {
                2
            } {
                input(&mut router, &chunk, Instant::now());
            }
            input(
                &mut router,
                &packet("type=read:status=DONE", b""),
                Instant::now(),
            );
            assert!(collect(&mut router).is_empty());
        }
    }
    #[test]
    fn policy_timeout_and_owner_loss_cancel_pending_or_partial_events() {
        for complete in [false, true] {
            for cancellation in 0..3 {
                let now = Instant::now();
                let mut router = Router::default();
                router.set_paste_target(Some(owner(1)));
                let wire = if complete {
                    event(b"text/plain")
                } else {
                    packet("type=read:status=OK", b"")
                };
                input(&mut router, &wire, now);
                router.drain(|_, _| false);
                if cancellation == 0 {
                    router.tick_paste(now, false, |_| true);
                }
                if cancellation == 1 {
                    router.forget(owner(1), &mut VecDeque::new());
                }
                if cancellation == 2 {
                    router.tick_paste(now + IDLE, true, |_| false);
                }
                router.set_paste_target(Some(owner(2)));
                input(
                    &mut router,
                    &packet("type=read:status=DATA:mime=Lg==", b"Lg=="),
                    now,
                );
                input(&mut router, &packet("type=read:status=DONE", b""), now);
                assert!(collect(&mut router).is_empty());
            }
        }
        let now = Instant::now();
        let mut router = Router::default();
        router.set_paste_target(Some(owner(1)));
        input(&mut router, &packet("type=read:status=OK", b""), now);
        router.tick_paste(now + IDLE, true, |_| true);
        assert!(router.paste.is_none());
    }
    #[test]
    fn disabling_during_the_header_cannot_resume_on_a_new_target() {
        let now = Instant::now();
        let wire = event(b"text/plain");
        let mut router = Router::default();
        router.set_paste_target(Some(owner(1)));
        input(&mut router, &wire[..20], now);
        router.tick_paste(now, false, |_| true);
        router.set_paste_target(Some(owner(2)));
        input(&mut router, &wire[20..], now);
        assert!(collect(&mut router).is_empty());
    }
    #[test]
    fn notification_backpressure_preserves_order_without_occupying_request_lease() {
        let wire = event(b"text/plain");
        let now = Instant::now();
        let mut router = Router::default();
        router.set_paste_target(Some(owner(1)));
        input(&mut router, &wire, now);
        router.drain(|_, _| false);
        assert!(router.idle());
        router.set_paste_target(Some(owner(2)));
        input(&mut router, &wire, now);
        assert_eq!(
            collect(&mut router),
            [(owner(1), wire.clone()), (owner(2), wire)]
        );
    }
    #[test]
    fn undelivered_notifications_do_not_hold_a_completed_lease() {
        let now = Instant::now();
        let mut router = Router::default();
        let mut outer = VecDeque::new();
        router.request(
            owner(1),
            Request {
                body: b"type=read:id=app;Lg==".to_vec(),
            },
            now,
            &mut outer,
            65536,
        );
        let id = router.lease.as_ref().unwrap().id.clone();
        input(
            &mut router,
            &packet(&format!("type=read:status=ENOSYS:id={id}"), b""),
            now,
        );
        router.set_paste_target(Some(owner(2)));
        input(&mut router, &event(b"text/plain"), now);
        router.drain(|target, _| target == owner(1));
        assert!(router.idle());
        assert_eq!(router.pending.len(), 1);
        router.tick_paste(now + IDLE, true, |_| true);
        assert_eq!(collect(&mut router), [(owner(2), event(b"text/plain"))]);
    }

    #[test]
    fn active_read_and_paste_notifications_have_independent_owners() {
        let now = Instant::now();
        let mut router = Router::default();
        let mut outer = VecDeque::new();
        router.request(
            owner(1),
            Request {
                body: b"type=read:id=app;Lg==".to_vec(),
            },
            now,
            &mut outer,
            65536,
        );
        let id = router.lease.as_ref().unwrap().id.clone();
        router.set_paste_target(Some(owner(2)));
        let wire = event(b"text/plain");
        input(&mut router, &wire, now);
        assert_eq!(collect(&mut router), [(owner(2), wire)]);
        input(
            &mut router,
            &packet(&format!("type=read:status=ENOSYS:id={id}"), b""),
            now,
        );
        assert_eq!(
            collect(&mut router),
            [(owner(1), packet("type=read:status=ENOSYS:id=app", b""))]
        );
        assert!(router.idle());
    }
}
