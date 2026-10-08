//! Bindings for the client-side Session Manager, independent of pane modes.
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Key {
    Byte(u8),
    Up,
    Down,
}
impl Key {
    pub fn label(self) -> String {
        match self {
            Self::Up => "Up".into(),
            Self::Down => "Down".into(),
            Self::Byte(27) => "Esc".into(),
            Self::Byte(13) => "Enter".into(),
            Self::Byte(9) => "Tab".into(),
            Self::Byte(127) => "Backspace".into(),
            Self::Byte(b' ') => "Space".into(),
            Self::Byte(n @ 1..=26) => format!("Ctrl-{}", char::from(b'A' + n - 1)),
            Self::Byte(n) => char::from(n).to_string(),
        }
    }
    pub fn byte(n: u8) -> Self {
        Self::Byte(match n {
            8 => 127,
            10 => 13,
            n => n,
        })
    }
    fn parse(text: &str) -> Result<Self, String> {
        let lower = text.to_ascii_lowercase();
        Ok(match lower.as_str() {
            "up" => Self::Up,
            "down" => Self::Down,
            "enter" => Self::Byte(13),
            "esc" | "escape" => Self::Byte(27),
            "tab" => Self::Byte(9),
            "backspace" => Self::Byte(127),
            "space" => Self::Byte(b' '),
            _ if lower.starts_with("ctrl ")
                && lower.len() == 6
                && lower.as_bytes()[5].is_ascii_lowercase() =>
            {
                Self::byte(lower.as_bytes()[5] - b'a' + 1)
            }
            _ if text.len() == 1 && text.as_bytes()[0].is_ascii_graphic() => {
                Self::Byte(text.as_bytes()[0])
            }
            _ => return Err(format!("unsupported session_manager key {text:?}")),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Up,
    Down,
    Search,
    Complete,
    Open,
    Create,
    Save,
    Rename,
    Disconnect,
    Delete,
    Cancel,
    Backspace,
}
impl Action {
    pub const ALL: [(Self, &'static str); 12] = [
        (Self::Up, "up"),
        (Self::Down, "down"),
        (Self::Search, "search"),
        (Self::Complete, "complete"),
        (Self::Open, "open"),
        (Self::Create, "create"),
        (Self::Save, "save"),
        (Self::Rename, "rename"),
        (Self::Disconnect, "disconnect"),
        (Self::Delete, "delete"),
        (Self::Cancel, "cancel"),
        (Self::Backspace, "backspace"),
    ];
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Bindings {
    keys: BTreeMap<Key, Action>,
}
impl Default for Bindings {
    fn default() -> Self {
        use Action::*;
        Self {
            keys: [
                (Key::Up, Up),
                (Key::Byte(b'k'), Up),
                (Key::Down, Down),
                (Key::Byte(b'j'), Down),
                (Key::Byte(b'/'), Search),
                (Key::Byte(9), Complete),
                (Key::Byte(13), Open),
                (Key::Byte(b'a'), Create),
                (Key::Byte(1), Save),
                (Key::Byte(18), Rename),
                (Key::Byte(24), Disconnect),
                (Key::Byte(b'd'), Delete),
                (Key::Byte(27), Cancel),
                (Key::Byte(b'q'), Cancel),
                (Key::Byte(127), Backspace),
            ]
            .into(),
        }
    }
}
impl Bindings {
    pub fn parse(value: Option<&toml::Value>) -> Result<Self, String> {
        let mut result = Self::default();
        if let Some(value) = value {
            let table = value.as_table().ok_or("session_manager must be a table")?;
            // Unimplemented manager actions keep startup compatibility and are
            // reported by config check, like other ignored main-track options.
            let mut overrides = Vec::new();
            for (action, name) in Action::ALL {
                if let Some(value) = table.get(name) {
                    let values = value
                        .as_array()
                        .ok_or_else(|| format!("session_manager.{name} must be an array"))?;
                    if values.len() > 16 {
                        return Err(format!("session_manager.{name} accepts at most 16 keys"));
                    }
                    let mut keys = Vec::new();
                    for v in values {
                        let key = Key::parse(v.as_str().ok_or_else(|| {
                            format!("session_manager.{name} keys must be strings")
                        })?)?;
                        if key == Key::Byte(3) {
                            return Err(
                                "Ctrl c is reserved for interrupting the Session Manager".into()
                            );
                        }
                        if keys.contains(&key) {
                            return Err(format!(
                                "duplicate session_manager.{name} key {}",
                                key.label()
                            ));
                        }
                        keys.push(key);
                    }
                    result.keys.retain(|_, a| *a != action);
                    overrides.push((action, name, keys));
                }
            }
            for (action, name, keys) in overrides {
                for key in keys {
                    if result.keys.insert(key, action).is_some() {
                        return Err(format!(
                            "session_manager.{name} key {} conflicts with another action",
                            key.label()
                        ));
                    }
                }
            }
        }
        Ok(result)
    }
    pub fn action(&self, key: Key, editing: bool) -> Option<Action> {
        let action = *self.keys.get(&key)?;
        if editing && matches!(key,Key::Byte(n) if n.is_ascii_graphic() || n==b' ') {
            return None;
        }
        Some(action)
    }
    pub fn hint(&self, action: Action, label: &str) -> String {
        self.hint_in(action, label, false)
    }
    pub fn hint_in(&self, action: Action, label: &str, editing: bool) -> String {
        let keys = self
            .keys
            .iter()
            .filter(|(key, a)| **a == action && self.action(**key, editing).is_some())
            .map(|(key, _)| key.label())
            .collect::<Vec<_>>()
            .join("/");
        if keys.is_empty() {
            String::new()
        } else {
            format!("<{keys}> {label}")
        }
    }
    pub fn report(&self) -> BTreeMap<String, Vec<String>> {
        Action::ALL
            .into_iter()
            .map(|(action, name)| {
                (
                    name.into(),
                    self.keys
                        .iter()
                        .filter(|(_, a)| **a == action)
                        .map(|(k, _)| match k {
                            Key::Byte(n @ 1..=26) if !matches!(n, 9 | 13) => {
                                format!("Ctrl {}", char::from(b'a' + n - 1))
                            }
                            _ => k.label(),
                        })
                        .collect(),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(s: &str) -> Result<Bindings, String> {
        Bindings::parse(Some(&toml::Value::Table(s.parse::<toml::Table>().unwrap())))
    }
    #[test]
    fn disconnect_replaces_disables_and_validates_keys_without_consuming_editor_text() {
        assert_eq!(
            Bindings::default().action(Key::Byte(24), false),
            Some(Action::Disconnect)
        );
        assert_eq!(
            parse("disconnect=[]").unwrap().action(Key::Byte(24), false),
            None
        );
        let custom = parse("disconnect=['x']").unwrap();
        assert_eq!(
            custom.action(Key::Byte(b'x'), false),
            Some(Action::Disconnect)
        );
        assert_eq!(custom.action(Key::Byte(b'x'), true), None);
        assert_eq!(custom.action(Key::Byte(24), false), None);
        assert!(parse("disconnect=['Ctrl a']").is_err());
        assert!(parse("disconnect=['Ctrl c']").is_err());
        assert!(parse("disconnect=['bad key']").is_err());
        assert!(parse("delete=['Ctrl x']").is_err());
        assert_eq!(
            parse("delete=['Ctrl x']\ndisconnect=[]")
                .unwrap()
                .action(Key::Byte(24), false),
            Some(Action::Delete)
        );
    }

    #[test]
    fn replacements_disabling_and_alias_conflicts() {
        let b = parse("up=['Ctrl k']\ndown=['n']\ncreate=[]").unwrap();
        assert_eq!(b.action(Key::Byte(b'j'), false), None);
        assert_eq!(b.action(Key::Byte(b'n'), false), Some(Action::Down));
        assert_eq!(b.action(Key::Byte(b'a'), false), None);
        assert!(parse("down=['k']").is_err());
        assert!(parse("open=['Ctrl m','enter']").is_err());
        assert!(parse("save=['Ctrl c']").is_err());
        assert!(parse("save=['unknown']").is_err());
        assert!(parse("save='s'").is_err());
        assert_eq!(
            Bindings::default().action(Key::Byte(18), false),
            Some(Action::Rename)
        );
        assert_eq!(
            parse("rename=[]").unwrap().action(Key::Byte(18), false),
            None
        );
        assert_eq!(
            parse("rename=['r']").unwrap().action(Key::Byte(b'r'), true),
            None
        );
        assert!(parse("rename=['Ctrl a']").is_err());
    }
    #[test]
    fn editing_keeps_printable_keys_as_text_and_reports_round_trip() {
        let b = Bindings::default();
        for n in b"ajkdq/ " {
            assert_eq!(b.action(Key::Byte(*n), true), None);
        }
        assert_eq!(b.action(Key::Byte(1), true), Some(Action::Save));
        assert_eq!(b.action(Key::Up, true), Some(Action::Up));
        assert_eq!(b.action(Key::Byte(127), true), Some(Action::Backspace));
        assert_eq!(
            Bindings::parse(Some(&toml::Value::try_from(b.report()).unwrap())).unwrap(),
            b
        );
    }
}
