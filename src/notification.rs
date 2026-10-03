//! Bounded, reply-free OSC 99 completion messages for the attached terminal.
use base64::{Engine, engine::general_purpose::STANDARD};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) struct Ids {
    prefix: String,
    serial: u64,
}
impl Default for Ids {
    fn default() -> Self {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            prefix: format!("rustmux-{}-{epoch}", std::process::id()),
            serial: 0,
        }
    }
}
impl Ids {
    pub fn allocate(&mut self) -> Option<String> {
        self.serial = self.serial.checked_add(1)?;
        Some(format!("{}-{}", self.prefix, self.serial))
    }
}

pub(crate) fn encode(
    id: &str,
    window: &str,
    pane: u64,
    title: &str,
    duration: Duration,
) -> Vec<u8> {
    let clean = |text: &str| {
        text.chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect::<String>()
    };
    let elapsed = if duration.as_secs() >= 60 {
        format!("{}m {}s", duration.as_secs() / 60, duration.as_secs() % 60)
    } else {
        format!("{:.1}s", duration.as_secs_f64())
    };
    let body = STANDARD.encode(format!(
        "Window {} / pane {pane} ({}) completed in {elapsed}",
        clean(window),
        clean(title)
    ));
    let title = STANDARD.encode("rustmux: command finished");
    // Disable activation reports/focus actions and close events. No terminal replies are requested.
    format!(
        "\x1b]99;i={id}:d=0:e=1:a=-focus:c=0:f=cnVzdG11eA==:o=always:p=title;{title}\x1b\\\
             \x1b]99;i={id}:d=1:e=1:a=-focus:c=0:p=body;{body}\x1b\\"
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn messages_bound_unicode_and_escape_untrusted_titles() {
        let wire = encode(
            "rustmux-1-2-3",
            "work",
            9,
            &("字\x1b\x07\n".repeat(1000)),
            Duration::from_secs(65),
        );
        assert!(wire.len() < 2048);
        assert_eq!(wire.iter().filter(|&&b| b == 0x1b).count(), 4);
        assert!(!wire.contains(&7));
        let text = String::from_utf8(wire).unwrap();
        let body = text
            .split("p=body;")
            .nth(1)
            .unwrap()
            .trim_end_matches("\x1b\\");
        let decoded = String::from_utf8(STANDARD.decode(body).unwrap()).unwrap();
        assert!(decoded.contains(&"字".repeat(80)));
        assert!(decoded.ends_with("1m 5s"));
        assert!(text.contains("a=-focus:c=0"));
    }
    #[test]
    fn ids_are_distinct_and_exhaustion_does_not_wrap() {
        let mut ids = Ids::default();
        assert_ne!(ids.allocate(), ids.allocate());
        ids.serial = u64::MAX;
        assert_eq!(ids.allocate(), None);
        assert_eq!(ids.allocate(), None);
    }
}
