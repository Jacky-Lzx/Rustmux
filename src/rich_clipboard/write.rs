//! Assemble a complete write before touching the outer clipboard. Payloads live
//! in one private, automatically removed temporary file, never in session state.
use super::{PREFIX, Packet};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    fs::{File, Permissions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
};

const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MIMES: usize = 128;
const MAX_ALIASES: usize = 128;
// A multiple of three avoids padding in the middle of a MIME's concatenated stream.
const RELAY_BYTES: usize = 4095;

struct Mime {
    name: String,
    size: u64,
    offset: u64,
}
#[derive(Default)]
struct Decoder {
    quartet: Vec<u8>,
    padded: bool,
}
impl Decoder {
    fn decode(&mut self, bytes: &[u8]) -> Result<Vec<u8>, &'static str> {
        let mut decoded = Vec::new();
        for &byte in bytes {
            if self.padded || !(byte.is_ascii_alphanumeric() || b"+/=".contains(&byte)) {
                return Err("EINVAL");
            }
            self.quartet.push(byte);
            if self.quartet.len() == 4 {
                let value = STANDARD.decode(&self.quartet).map_err(|_| "EINVAL")?;
                self.padded = self.quartet.contains(&b'=');
                self.quartet.clear();
                decoded.extend(value);
                if decoded.len() > 4096 {
                    return Err("EINVAL");
                }
            }
        }
        Ok(decoded)
    }
    fn finish(&self) -> Result<(), &'static str> {
        if self.quartet.is_empty() {
            Ok(())
        } else {
            Err("EINVAL")
        }
    }
}
fn mime(value: &str) -> Result<String, &'static str> {
    let decoded = STANDARD.decode(value).map_err(|_| "EINVAL")?;
    if decoded.is_empty()
        || decoded.len() > 512
        || !decoded.iter().all(|b| (0x21..=0x7e).contains(b))
    {
        return Err("EINVAL");
    }
    String::from_utf8(decoded).map_err(|_| "EINVAL")
}

pub(super) struct Assembler {
    file: File,
    mimes: Vec<Mime>,
    aliases: Vec<(String, String)>,
    decoder: Decoder,
    total: u64,
}
impl Assembler {
    pub fn new(packet: &Packet<'_>) -> Result<Self, &'static str> {
        if !packet.payload.is_empty()
            || packet.value("mime").is_some()
            || packet.value("status").is_some()
        {
            return Err("EINVAL");
        }
        for field in ["name", "pw"] {
            if let Some(value) = packet.value(field) {
                let data = STANDARD.decode(value).map_err(|_| "EINVAL")?;
                std::str::from_utf8(&data).map_err(|_| "EINVAL")?;
            }
        }
        let file = tempfile::tempfile().map_err(|_| "EIO")?;
        // Linux's anonymous tempfile path inherits the process umask. Restrict
        // the empty spool before any clipboard payload can be written to it.
        file.set_permissions(Permissions::from_mode(0o600))
            .map_err(|_| "EIO")?;
        Ok(Self {
            file,
            mimes: Vec::new(),
            aliases: Vec::new(),
            decoder: Decoder::default(),
            total: 0,
        })
    }
    /// True means the complete transaction is validated and ready to relay.
    pub fn accept(&mut self, packet: &Packet<'_>) -> Result<bool, &'static str> {
        if packet.value("status").is_some() {
            return Err("EINVAL");
        }
        match packet.value("type") {
            Some("wdata") => {
                let Some(name) = packet.value("mime") else {
                    if !packet.payload.is_empty() {
                        return Err("EINVAL");
                    }
                    self.decoder.finish()?;
                    return Ok(true);
                };
                let name = mime(name)?;
                if self.mimes.last().is_none_or(|m| m.name != name) {
                    self.decoder.finish()?;
                    if let Some(index) = self.mimes.iter().position(|m| m.name == name) {
                        self.mimes.remove(index);
                    }
                    if self.mimes.len() == MAX_MIMES {
                        return Err("EFBIG");
                    }
                    self.mimes.push(Mime {
                        name,
                        size: 0,
                        offset: self.total,
                    });
                    self.decoder = Decoder::default();
                }
                let data = self.decoder.decode(packet.payload)?;
                if data.len() as u64 > MAX_BYTES - self.total {
                    return Err("EFBIG");
                }
                self.file.write_all(&data).map_err(|_| "EIO")?;
                self.total += data.len() as u64;
                self.mimes.last_mut().unwrap().size += data.len() as u64;
                Ok(false)
            }
            Some("walias") => {
                let target = mime(packet.value("mime").ok_or("EINVAL")?)?;
                let data = STANDARD.decode(packet.payload).map_err(|_| "EINVAL")?;
                if data.is_empty() || data.len() > 4096 {
                    return Err("EINVAL");
                }
                let list = std::str::from_utf8(&data).map_err(|_| "EINVAL")?;
                for alias in list.split(' ') {
                    if alias.is_empty()
                        || alias.len() > 512
                        || !alias.bytes().all(|b| (0x21..=0x7e).contains(&b))
                    {
                        return Err("EINVAL");
                    }
                    if let Some((_, value)) =
                        self.aliases.iter_mut().find(|(name, _)| name == alias)
                    {
                        *value = target.clone();
                    } else {
                        if self.aliases.len() == MAX_ALIASES {
                            return Err("EFBIG");
                        }
                        self.aliases.push((alias.to_owned(), target.clone()));
                    }
                }
                Ok(false)
            }
            _ => Err("EINVAL"),
        }
    }
    pub fn relay(mut self) -> Result<Relay, &'static str> {
        self.file.seek(SeekFrom::Start(0)).map_err(|_| "EIO")?;
        Ok(Relay {
            file: self.file,
            mimes: self.mimes,
            aliases: self.aliases,
            index: 0,
            consumed: 0,
            alias_index: 0,
        })
    }
}
pub(super) struct Relay {
    file: File,
    mimes: Vec<Mime>,
    aliases: Vec<(String, String)>,
    index: usize,
    consumed: u64,
    alias_index: usize,
}
impl Relay {
    /// Produce one canonical packet, at most a 4095-byte decoded data chunk.
    pub fn next(&mut self, id: &str) -> Result<Option<Vec<u8>>, &'static str> {
        if let Some(mime) = self.mimes.get(self.index) {
            let count = (mime.size - self.consumed).min(RELAY_BYTES as u64) as usize;
            let mut data = vec![0; count];
            self.file
                .seek(SeekFrom::Start(mime.offset + self.consumed))
                .map_err(|_| "EIO")?;
            self.file.read_exact(&mut data).map_err(|_| "EIO")?;
            let bytes = encode("wdata", id, Some(&mime.name), &data);
            self.consumed += count as u64;
            if self.consumed == mime.size {
                self.index += 1;
                self.consumed = 0;
            }
            return Ok(Some(bytes));
        }
        if let Some((alias, target)) = self.aliases.get(self.alias_index) {
            self.alias_index += 1;
            return Ok(Some(encode("walias", id, Some(target), alias.as_bytes())));
        }
        Ok(None)
    }
}
fn encode(kind: &str, id: &str, mime: Option<&str>, data: &[u8]) -> Vec<u8> {
    let mut bytes = PREFIX.to_vec();
    bytes.extend_from_slice(format!("type={kind}:id={id}").as_bytes());
    if let Some(mime) = mime {
        bytes.extend_from_slice(format!(":mime={}", STANDARD.encode(mime)).as_bytes());
    }
    if !data.is_empty() {
        bytes.push(b';');
        bytes.extend_from_slice(STANDARD.encode(data).as_bytes());
    }
    bytes.extend_from_slice(b"\x1b\\");
    bytes
}

pub(super) struct Transaction {
    begin: Vec<u8>,
    assembler: Option<Assembler>,
    relay: Option<Relay>,
    pending: Option<Vec<u8>>,
    pub started: bool,
    pub ended: bool,
}
impl Transaction {
    pub fn new(packet: &Packet<'_>) -> Result<Self, &'static str> {
        Ok(Self {
            begin: packet.metadata.as_bytes().to_vec(),
            assembler: Some(Assembler::new(packet)?),
            relay: None,
            pending: None,
            started: false,
            ended: false,
        })
    }
    pub fn discard(&mut self) {
        self.assembler = None;
        self.relay = None;
        self.pending = None;
        self.ended = true;
    }
    pub fn collecting(&self) -> bool {
        self.assembler.is_some()
    }
    pub fn accept(&mut self, packet: &Packet<'_>) -> Result<(), &'static str> {
        let assembler = self.assembler.as_mut().ok_or("EINVAL")?;
        if assembler.accept(packet)? {
            self.relay = Some(self.assembler.take().unwrap().relay()?);
        }
        Ok(())
    }
    pub fn pump(
        &mut self,
        id: &str,
        outer: &mut std::collections::VecDeque<u8>,
        capacity: usize,
    ) -> Result<(), &'static str> {
        if self.collecting() || self.ended {
            return Ok(());
        }
        if self.pending.is_none() {
            self.pending = Some(if !self.started {
                Packet::parse(&self.begin).unwrap().encode(Some(id))
            } else if let Some(next) = self.relay.as_mut().unwrap().next(id)? {
                next
            } else {
                encode("wdata", id, None, b"")
            });
        }
        let packet = self.pending.as_ref().unwrap();
        if packet.len() > capacity.saturating_sub(outer.len()) {
            return Ok(());
        }
        let end = self.started
            && Packet::parse(&packet[PREFIX.len()..packet.len() - 2])
                .unwrap()
                .value("mime")
                .is_none();
        outer.extend(self.pending.take().unwrap());
        self.started = true;
        if end {
            self.ended = true;
            self.relay = None;
        }
        Ok(())
    }
    /// Deliberately invalid Base64 forces the terminal to discard staging.
    /// Never send an end packet to cancel a partially relayed transaction.
    pub fn abort(&self, id: &str, outer: &mut std::collections::VecDeque<u8>) {
        if self.started && !self.ended {
            let mut packet = encode("wdata", id, Some("text/plain"), b"");
            packet.truncate(packet.len() - 2);
            packet.extend_from_slice(b";!\x1b\\");
            outer.extend(packet);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    fn begin() -> Assembler {
        Assembler::new(&Packet::parse(b"type=write:id=app:name=QXBw:loc=primary").unwrap()).unwrap()
    }
    fn data(mime: &str, payload: &[u8]) -> Vec<u8> {
        [
            format!("type=wdata:mime={};", STANDARD.encode(mime)).as_bytes(),
            payload,
        ]
        .concat()
    }
    fn accept(a: &mut Assembler, bytes: &[u8]) -> Result<bool, &'static str> {
        a.accept(&Packet::parse(bytes).unwrap())
    }
    fn body(bytes: &[u8]) -> Packet<'_> {
        Packet::parse(&bytes[PREFIX.len()..bytes.len() - 2]).unwrap()
    }
    #[test]
    fn every_base64_split_and_bytewise_fragments_preserve_binary() {
        let expected = [0, 255, 1, 2, 3, 4, 254];
        let encoded = STANDARD.encode(expected);
        for split in 0..=encoded.len() {
            let mut a = begin();
            accept(
                &mut a,
                &data("application/octet-stream", &encoded.as_bytes()[..split]),
            )
            .unwrap();
            accept(
                &mut a,
                &data("application/octet-stream", &encoded.as_bytes()[split..]),
            )
            .unwrap();
            assert!(accept(&mut a, b"type=wdata").unwrap());
            let packet = a.relay().unwrap().next("outer").unwrap().unwrap();
            assert_eq!(STANDARD.decode(body(&packet).payload).unwrap(), expected);
        }
        let mut decoder = Decoder::default();
        let decoded: Vec<_> = encoded
            .bytes()
            .flat_map(|b| decoder.decode(&[b]).unwrap())
            .collect();
        decoder.finish().unwrap();
        assert_eq!(decoded, expected);
    }
    #[test]
    fn malformed_padding_incomplete_stream_and_oversized_chunks_fail() {
        for payload in [b"!!!!".as_slice(), b"Zg==AAAA", b"Zh==", b"=AAA", b"A==="] {
            assert_eq!(
                accept(&mut begin(), &data("text/plain", payload)),
                Err("EINVAL")
            );
        }
        for boundary in [b"type=wdata".to_vec(), data("text/html", b"YQ==")] {
            let mut a = begin();
            accept(&mut a, &data("text/plain", b"YQ")).unwrap();
            assert_eq!(accept(&mut a, &boundary), Err("EINVAL"));
        }
        assert_eq!(
            accept(
                &mut begin(),
                &data("text/plain", STANDARD.encode(vec![1; 4097]).as_bytes())
            ),
            Err("EINVAL")
        );
        assert_eq!(accept(&mut begin(), b"type=wdata;YQ=="), Err("EINVAL"));
        assert_eq!(accept(&mut begin(), b"type=wdata:mime=%%%"), Err("EINVAL"));
    }
    #[test]
    fn mime_replacement_aliases_and_empty_entries_relay_canonically() {
        let mut a = begin();
        accept(
            &mut a,
            format!(
                "type=walias:mime={};{}",
                STANDARD.encode("text/plain"),
                STANDARD.encode("text/x-a text/x-b")
            )
            .as_bytes(),
        )
        .unwrap();
        accept(&mut a, &data("text/plain", b"b2xk")).unwrap();
        accept(&mut a, &data("text/html", b"PGI+PC9iPg==")).unwrap();
        accept(&mut a, &data("text/plain", b"bmV3")).unwrap();
        accept(&mut a, &data("application/empty", b"")).unwrap();
        accept(&mut a, b"type=wdata").unwrap();
        let mut relay = a.relay().unwrap();
        let mut found = Vec::new();
        while let Some(bytes) = relay.next("outer").unwrap() {
            let p = body(&bytes);
            assert_eq!(p.value("id"), Some("outer"));
            found.push((
                p.value("type").unwrap().to_owned(),
                STANDARD.decode(p.value("mime").unwrap()).unwrap(),
                STANDARD.decode(p.payload).unwrap(),
            ));
        }
        assert_eq!(found.len(), 5);
        assert_eq!(found[0].2, b"<b></b>");
        assert_eq!(found[1].2, b"new");
        assert!(found[2].2.is_empty());
        assert_eq!(found[3].0, "walias");
        assert_eq!(found[4].2, b"text/x-b");
    }
    #[test]
    fn alias_order_and_reassignments_preserve_terminal_interpretation() {
        let mut a = begin();
        for (target, alias) in [
            ("text/b", "text/z"),
            ("text/a", "text/b"),
            ("text/c", "text/z"),
        ] {
            accept(
                &mut a,
                format!(
                    "type=walias:mime={};{}",
                    STANDARD.encode(target),
                    STANDARD.encode(alias)
                )
                .as_bytes(),
            )
            .unwrap();
        }
        accept(&mut a, b"type=wdata").unwrap();
        let mut relay = a.relay().unwrap();
        for (target, alias) in [("text/c", "text/z"), ("text/a", "text/b")] {
            let bytes = relay.next("outer").unwrap().unwrap();
            let packet = body(&bytes);
            assert_eq!(
                STANDARD.decode(packet.value("mime").unwrap()).unwrap(),
                target.as_bytes()
            );
            assert_eq!(STANDARD.decode(packet.payload).unwrap(), alias.as_bytes());
        }
    }
    #[test]
    fn metadata_and_alias_budgets_are_bounded() {
        for bytes in [
            b"type=write;YQ==".as_slice(),
            b"type=write:name=%%%",
            b"type=write:pw=/w==",
            b"type=write:status=DONE",
        ] {
            assert!(Assembler::new(&Packet::parse(bytes).unwrap()).is_err());
        }
        let mut a = begin();
        for index in 0..MAX_MIMES {
            accept(&mut a, &data(&format!("text/x-{index}"), b"")).unwrap();
        }
        assert_eq!(accept(&mut a, &data("text/overflow", b"")), Err("EFBIG"));
        let mut a = begin();
        for index in 0..MAX_ALIASES {
            accept(
                &mut a,
                format!(
                    "type=walias:mime=dGV4dC9wbGFpbg==;{}",
                    STANDARD.encode(format!("text/x-{index}"))
                )
                .as_bytes(),
            )
            .unwrap();
        }
        assert_eq!(
            accept(
                &mut a,
                b"type=walias:mime=dGV4dC9wbGFpbg==;dGV4dC9vdmVyZmxvdw=="
            ),
            Err("EFBIG")
        );
        for alias in ["a  b", "a ", "é", ""] {
            assert_eq!(
                accept(
                    &mut begin(),
                    format!(
                        "type=walias:mime=dGV4dC9wbGFpbg==;{}",
                        STANDARD.encode(alias)
                    )
                    .as_bytes()
                ),
                Err("EINVAL")
            );
        }
    }
    #[test]
    fn exactly_64_mib_fits_and_one_more_byte_fails_without_large_memory() {
        let mut a = begin();
        let packet = data(
            "application/octet-stream",
            STANDARD.encode([42; 4095]).as_bytes(),
        );
        for _ in 0..MAX_BYTES / 4095 {
            accept(&mut a, &packet).unwrap();
        }
        accept(
            &mut a,
            &data(
                "application/octet-stream",
                STANDARD
                    .encode(vec![42; (MAX_BYTES % 4095) as usize])
                    .as_bytes(),
            ),
        )
        .unwrap();
        assert_eq!(a.total, MAX_BYTES);
        assert!(accept(&mut a, b"type=wdata").unwrap());
        assert_eq!(
            accept(&mut a, &data("application/overflow", b"Kg==")),
            Err("EFBIG")
        );
    }
    #[test]
    fn private_spool_and_queue_pressure_keep_packets_whole() {
        use std::os::unix::fs::PermissionsExt;
        let a = begin();
        assert_eq!(
            a.file.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut transaction =
            Transaction::new(&Packet::parse(b"type=write:id=app").unwrap()).unwrap();
        let mut outer = VecDeque::new();
        transaction.pump("outer", &mut outer, 65536).unwrap();
        assert!(outer.is_empty());
        // A single protocol chunk cannot exceed 4096 decoded bytes.
        for _ in 0..2 {
            transaction
                .accept(
                    &Packet::parse(&data(
                        "text/plain",
                        STANDARD.encode(vec![99; 4095]).as_bytes(),
                    ))
                    .unwrap(),
                )
                .unwrap();
        }
        transaction
            .accept(&Packet::parse(b"type=wdata").unwrap())
            .unwrap();
        transaction.pump("outer", &mut outer, 1).unwrap();
        assert!(outer.is_empty());
        transaction.pump("outer", &mut outer, 65536).unwrap();
        assert_eq!(
            body(&outer.drain(..).collect::<Vec<_>>()).value("type"),
            Some("write")
        );
        for _ in 0..2 {
            transaction.pump("outer", &mut outer, 1).unwrap();
            assert!(outer.is_empty());
            transaction.pump("outer", &mut outer, 65536).unwrap();
            let wire = outer.drain(..).collect::<Vec<_>>();
            assert_eq!(body(&wire).payload.len(), 5460);
            assert!(!body(&wire).payload.contains(&b'='));
        }
        transaction.pump("outer", &mut outer, 65536).unwrap();
        assert!(transaction.ended && transaction.relay.is_none());
        assert_eq!(
            body(&outer.into_iter().collect::<Vec<_>>()).value("mime"),
            None
        );
    }
    #[test]
    fn cancel_only_aborts_a_relay_that_has_not_sent_end() {
        let mut transaction = Transaction::new(&Packet::parse(b"type=write").unwrap()).unwrap();
        let mut outer = VecDeque::new();
        transaction.abort("outer", &mut outer);
        assert!(outer.is_empty());
        transaction
            .accept(&Packet::parse(b"type=wdata").unwrap())
            .unwrap();
        transaction.pump("outer", &mut outer, 65536).unwrap();
        outer.clear();
        transaction.abort("outer", &mut outer);
        assert_eq!(body(&outer.drain(..).collect::<Vec<_>>()).payload, b"!");
        transaction.pump("outer", &mut outer, 65536).unwrap();
        outer.clear();
        transaction.abort("outer", &mut outer);
        assert!(outer.is_empty());
    }
}
