//! Fixed-order, typed configuration layers. Only the document root accepts overrides.
use std::collections::BTreeMap;
use std::ffi::OsString;

use super::*;
use serde::{Deserialize, Serialize};

const OS_NAMES: &[&str] = &[
    "aix",
    "android",
    "apple",
    "dragonfly",
    "emscripten",
    "espidf",
    "fortanix",
    "freebsd",
    "fuchsia",
    "haiku",
    "hermit",
    "horizon",
    "hurd",
    "illumos",
    "ios",
    "l4re",
    "linux",
    "macos",
    "netbsd",
    "nto",
    "openbsd",
    "qnx",
    "redox",
    "solaris",
    "solid_asp3",
    "tvos",
    "uefi",
    "vexos",
    "visionos",
    "vita",
    "vxworks",
    "wasi",
    "watchos",
    "windows",
    "xous",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Environment {
    pub os: String,
    pub machine: Option<String>,
    pub machine_source: &'static str,
    pub note: Option<String>,
}

impl Environment {
    pub fn detect(os: Option<&str>, machine: Option<&str>) -> Result<Self, String> {
        let os = os.unwrap_or(env::consts::OS);
        validate_os(os)?;
        let (machine, machine_source, note) =
            select_machine(machine, env::var_os("RUSTMUX_MACHINE"), system_hostname)?;
        Ok(Self {
            os: os.to_owned(),
            machine,
            machine_source,
            note,
        })
    }
}

fn validate_os(os: &str) -> Result<(), String> {
    if OS_NAMES.contains(&os) {
        Ok(())
    } else {
        Err(format!(
            "unknown operating system '{os}'; use a Rust OS identifier such as macos or linux"
        ))
    }
}

fn validate_machine(machine: &str) -> Result<(), String> {
    if machine.trim().is_empty() || machine.chars().any(char::is_control) {
        Err("machine name must be nonempty and contain no control characters".to_owned())
    } else {
        Ok(())
    }
}

fn select_machine(
    explicit: Option<&str>,
    variable: Option<OsString>,
    hostname: impl FnOnce() -> Result<String, String>,
) -> Result<(Option<String>, &'static str, Option<String>), String> {
    let selected = if let Some(machine) = explicit {
        Some((machine.to_owned(), "--machine"))
    } else if let Some(variable) = variable {
        Some((
            variable
                .into_string()
                .map_err(|_| "RUSTMUX_MACHINE must be valid UTF-8")?,
            "RUSTMUX_MACHINE",
        ))
    } else {
        match hostname() {
            Ok(machine) if validate_machine(&machine).is_ok() => Some((machine, "system hostname")),
            Ok(_) => {
                return Ok((
                    None,
                    "unavailable",
                    Some(
                        "system hostname is empty or invalid; machine overrides skipped".to_owned(),
                    ),
                ));
            }
            Err(error) => {
                return Ok((
                    None,
                    "unavailable",
                    Some(format!(
                        "could not read system hostname: {error}; machine overrides skipped"
                    )),
                ));
            }
        }
    };
    let (machine, source) = selected.expect("all hostname branches return or select a machine");
    validate_machine(&machine).map_err(|error| format!("{source}: {error}"))?;
    Ok((Some(machine), source, None))
}

fn system_hostname() -> Result<String, String> {
    let mut bytes = [0_u8; 1024];
    // SAFETY: gethostname receives a writable buffer and its exact capacity.
    if unsafe { nix::libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or("system hostname exceeds the supported length")?;
    String::from_utf8(bytes[..end].to_vec()).map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Selection {
    pub environment: Environment,
    pub layers: Vec<String>,
    pub sources: BTreeMap<String, String>,
}

impl Selection {
    pub fn explain(&self) -> String {
        let mut text = format!(
            "os: {}\nmachine: {} (from {})\nlayers: {}\nfield sources:\n",
            self.environment.os,
            self.environment.machine.as_deref().unwrap_or("unavailable"),
            self.environment.machine_source,
            self.layers.join(" -> ")
        );
        if let Some(note) = &self.environment.note {
            text.push_str(&format!("note: {note}\n"));
        }
        for (field, source) in &self.sources {
            text.push_str(&format!("  {field} <- {source}\n"));
        }
        text
    }

    fn record(&mut self, table: &toml::Table, layer: &str) {
        self.layers.push(layer.to_owned());
        for (name, value) in table {
            if name == "keybinds" {
                for (mode, bindings) in value.as_table().into_iter().flatten() {
                    for (key, binding) in bindings.as_table().into_iter().flatten() {
                        if let Some(fields) = binding.as_table() {
                            let key = display::parse_display_key(key)
                                .map(canonical_name)
                                .unwrap_or_else(|| key.clone());
                            for field in fields.keys() {
                                self.sources.insert(
                                    format!("keybinds.{mode}.{key:?}.{field}"),
                                    layer.to_owned(),
                                );
                            }
                        }
                    }
                }
            } else if let Some(fields) = value.as_table() {
                if name == "theme" && fields.contains_key("preset") {
                    for color in crate::theme::Theme::default().report().keys() {
                        self.sources
                            .insert(format!("theme.colors.{color}"), layer.to_owned());
                    }
                }
                for (field, value) in fields {
                    if name == "theme" && field == "colors" {
                        for color in value
                            .as_table()
                            .into_iter()
                            .flatten()
                            .map(|(color, _)| color)
                        {
                            self.sources
                                .insert(format!("theme.colors.{color}"), layer.to_owned());
                        }
                    } else {
                        self.sources
                            .insert(format!("{name}.{field}"), layer.to_owned());
                    }
                }
            } else {
                self.sources.insert(name.to_owned(), layer.to_owned());
            }
        }
    }
}

// The override schema is strict; the shared root retains main's compatibility warnings.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    shell: Option<String>,
    tab_name: Option<String>,
    tab_close_focus: Option<String>,
    default_mode: Option<String>,
    compact: Option<bool>,
    idle_frame_coalescing: Option<bool>,
    scrollback_lines: Option<usize>,
    remain_on_exit: Option<bool>,
    mouse_hover_cursor: Option<bool>,
    clipboard_write: Option<bool>,
    clipboard_read: Option<bool>,
    file_transfer: Option<bool>,
    drag_source: Option<bool>,
    drop_target: Option<bool>,
    clear_defaults: Option<bool>,
    autosave_interval_seconds: Option<u64>,
    save_scrollback: Option<bool>,
    save_scrollback_colors: Option<bool>,
    notifications: Option<NotificationPatch>,
    shortcuts: Option<BTreeMap<String, String>>,
    session_manager: Option<BTreeMap<String, Vec<String>>>,
    theme: Option<crate::theme::ThemeConfig>,
    keybinds: Option<BTreeMap<String, BTreeMap<String, BindingPatch>>>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NotificationPatch {
    enabled: Option<bool>,
    desktop: Option<bool>,
    long_command_bell: Option<bool>,
    command_duration_seconds: Option<u64>,
    exclude_applications: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum BindingPatch {
    Actions(Vec<ActionPatch>),
    Detailed(BindingFields),
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BindingFields {
    actions: Option<Vec<ActionPatch>>,
    display: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
enum ActionPatch {
    Name(String),
    Detailed {
        action: String,
        mode: Option<String>,
        index: Option<usize>,
    },
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct OverrideTables {
    #[serde(default)]
    os: BTreeMap<String, toml::Table>,
    #[serde(default)]
    machine: BTreeMap<String, toml::Table>,
}

pub(super) struct Compiled {
    pub document: toml::Table,
    pub selection: Selection,
    disabled: Vec<(BindingMode, HistoryKey)>,
}

fn table_mut<'a>(
    document: &'a mut toml::Table,
    field: &str,
) -> Result<&'a mut toml::Table, String> {
    document
        .entry(field)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| format!("{field} must be a table"))
}

fn validate_local(table: &toml::Table) -> Result<(), String> {
    let mut scalars = table.clone();
    for field in [
        "keybinds",
        "shortcuts",
        "session_manager",
        "clear_defaults",
        "default_mode",
    ] {
        scalars.remove(field);
    }
    parse_document(&scalars)?;
    DefaultMode::parse(table.get("default_mode"), Shortcuts::default())?;
    if let Some(value) = table.get("clear_defaults")
        && !value.is_bool()
    {
        return Err("clear_defaults must be a boolean".into());
    }
    if let Some(value) = table.get("session_manager") {
        manager::Bindings::validate_local(value)?;
    }
    if let Some(value) = table.get("shortcuts") {
        for (name, key) in value.as_table().ok_or("shortcuts must be a table")? {
            let one = toml::Value::Table(toml::Table::from_iter([(name.clone(), key.clone())]));
            parse_shortcuts(Some(&one), true)?;
        }
    }
    Ok(())
}

fn validate_patch(table: &toml::Table) -> Result<(), String> {
    let patch: Patch = toml::Value::Table(table.clone())
        .try_into()
        .map_err(|error| error.to_string())?;
    validate_local(table)?;
    if let Some(manager) = patch.session_manager {
        for name in manager.keys() {
            if !manager::Action::ALL.iter().any(|(_, known)| name == known) {
                return Err(format!("unknown session_manager action '{name}'"));
            }
        }
    }
    if let Some(modes) = patch.keybinds {
        for (mode, bindings) in modes {
            display::mode(&mode).ok_or_else(|| format!("unsupported keybinds mode '{mode}'"))?;
            let mut seen = Vec::new();
            for (key, patch) in bindings {
                let canonical = display::parse_display_key(&key)
                    .ok_or_else(|| format!("unsupported keybinds.{mode}.{key} key"))?;
                if seen.contains(&canonical) {
                    return Err(format!("duplicate canonical keybinds.{mode}.{key} key"));
                }
                seen.push(canonical);
                let actions = match patch {
                    BindingPatch::Actions(actions) => Some(actions),
                    BindingPatch::Detailed(binding) => {
                        if let Some(display) = &binding.display
                            && !matches!(
                                display.as_str(),
                                "always" | "help" | "help-menu" | "hidden" | "never"
                            )
                        {
                            return Err(format!("unsupported keybinds.{mode}.{key}.display"));
                        }
                        if binding.actions.is_none() && binding.display.is_none() {
                            return Err(format!(
                                "keybinds.{mode}.{key} requires actions or display"
                            ));
                        }
                        binding.actions
                    }
                };
                if let Some(actions) = actions {
                    for action in actions {
                        validate_action(action)?;
                    }
                    let raw = &table["keybinds"][&mode][&key];
                    let normalized = normalize_binding(raw);
                    if !empty_actions(&normalized) {
                        let one = toml::Value::Table(toml::Table::from_iter([(
                            mode.clone(),
                            toml::Value::Table(toml::Table::from_iter([(key.clone(), normalized)])),
                        )]));
                        let baseline = Shortcuts {
                            clear_defaults: true,
                            locked_enter: 0,
                            ..Shortcuts::default()
                        };
                        let result = parse_keybinds(Some(&one), baseline)?;
                        if result == baseline {
                            return Err(format!("unsupported binding keybinds.{mode}.{key}"));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_action(action: ActionPatch) -> Result<(), String> {
    match action {
        ActionPatch::Name(name) => {
            // Detailed compatibility is decided by the native per-mode binding parser.
            if name.is_empty() {
                return Err("action name cannot be empty".into());
            }
        }
        ActionPatch::Detailed {
            action,
            mode,
            index,
        } => match action.as_str() {
            "switch-mode"
                if index.is_none() && mode.as_deref().and_then(display::mode).is_some() => {}
            "go-to-window"
                if mode.is_none() && index.is_some_and(|index| (1..=10).contains(&index)) => {}
            _ => return Err(format!("unsupported action or arguments '{action}'")),
        },
    }
    Ok(())
}

fn normalize_binding(value: &toml::Value) -> toml::Value {
    match value {
        toml::Value::Array(_) => {
            toml::Value::Table(toml::Table::from_iter([("actions".into(), value.clone())]))
        }
        _ => value.clone(),
    }
}
fn empty_actions(value: &toml::Value) -> bool {
    value
        .get("actions")
        .and_then(toml::Value::as_array)
        .is_some_and(Vec::is_empty)
}

fn canonical_name(key: HistoryKey) -> String {
    match key {
        HistoryKey::Byte(27) => "esc".into(),
        HistoryKey::Byte(13) => "enter".into(),
        HistoryKey::Byte(9) => "tab".into(),
        HistoryKey::Byte(byte @ 1..=26) => format!("Ctrl {}", char::from(b'a' + byte - 1)),
        HistoryKey::Byte(byte) => char::from(byte).to_string(),
        HistoryKey::Up => "up".into(),
        HistoryKey::Down => "down".into(),
        HistoryKey::Left => "left".into(),
        HistoryKey::Right => "right".into(),
        HistoryKey::PageUp => "pageup".into(),
        HistoryKey::PageDown => "pagedown".into(),
        HistoryKey::Home => "home".into(),
        HistoryKey::End => "end".into(),
    }
}

// Removing a remapped action must not revive its displaced implicit default.
fn displaced_normal_defaults(
    mode: BindingMode,
    key: HistoryKey,
    name: &str,
    binding: &toml::Value,
) -> Vec<HistoryKey> {
    if mode != BindingMode::Normal {
        return Vec::new();
    }
    let HistoryKey::Byte(byte) = key else {
        return Vec::new();
    };
    let one = toml::Value::Table(toml::Table::from_iter([(
        "normal".into(),
        toml::Value::Table(toml::Table::from_iter([(name.to_owned(), binding.clone())])),
    )]));
    let baseline = Shortcuts {
        clear_defaults: true,
        locked_enter: 0,
        ..Shortcuts::default()
    };
    let Ok(parsed) = parse_keybinds(Some(&one), baseline) else {
        return Vec::new();
    };
    let mut displaced = Vec::new();
    for (index, default) in DEFAULT_SHORTCUT_KEYS.iter().enumerate() {
        if parsed.legacy_configured[index] && parsed.keys[index] == byte && *default != byte {
            displaced.push(HistoryKey::Byte(*default));
        }
    }
    for &(configured, action) in parsed.normal_actions.iter().flatten() {
        if configured == byte && configured != action && action != b'[' {
            displaced.push(HistoryKey::Byte(action));
        }
    }
    displaced
}

pub(super) fn compile(source: &str, environment: &Environment) -> Result<Compiled, String> {
    let mut document: toml::Table = source
        .parse()
        .map_err(|error: toml::de::Error| error.to_string())?;
    let overrides: OverrideTables = document
        .remove("overrides")
        .map(|value| {
            value
                .try_into()
                .map_err(|error| format!("overrides: {error}"))
        })
        .transpose()?
        .unwrap_or_default();
    validate_local(&document).map_err(|error| format!("config: {error}"))?;
    for (name, patch) in &overrides.os {
        validate_os(name)
            .and_then(|_| validate_patch(patch))
            .map_err(|error| format!("overrides.os.{name}: {error}"))?;
    }
    for (name, patch) in &overrides.machine {
        validate_machine(name)
            .and_then(|_| validate_patch(patch))
            .map_err(|error| format!("overrides.machine.{name}: {error}"))?;
    }
    let mut selection = Selection {
        environment: environment.clone(),
        layers: Vec::new(),
        sources: BTreeMap::new(),
    };
    selection.record(
        &default_config().parse().expect("built-in defaults"),
        "built-in defaults",
    );
    selection.record(&document, "config");
    let mut compiled = Compiled {
        document,
        selection,
        disabled: Vec::new(),
    };
    if let Some(patch) = overrides.os.get(&environment.os) {
        compiled.apply(patch, &format!("overrides.os.{}", environment.os))?;
    }
    if let Some(machine) = &environment.machine
        && let Some(patch) = overrides.machine.get(machine)
    {
        compiled.apply(patch, &format!("overrides.machine.{machine}"))?;
    }
    Ok(compiled)
}

impl Compiled {
    fn apply(&mut self, patch: &toml::Table, layer: &str) -> Result<(), String> {
        if patch.get("clear_defaults").and_then(toml::Value::as_bool) == Some(true) {
            self.document.remove("keybinds");
            self.document.remove("shortcuts");
            self.disabled.clear();
            self.selection.sources.retain(|field, _| {
                !field.starts_with("keybinds.") && !field.starts_with("shortcuts.")
            });
        }
        for (field, value) in patch {
            match field.as_str() {
                "keybinds" => self.merge_bindings(value, layer)?,
                "theme" => {
                    if value.get("preset").is_some() {
                        self.document.remove("theme");
                        self.selection
                            .sources
                            .retain(|field, _| !field.starts_with("theme."));
                    }
                    let theme = table_mut(&mut self.document, "theme")?;
                    for (field, value) in value.as_table().expect("validated theme") {
                        if field == "colors" {
                            table_mut(theme, "colors")?
                                .extend(value.as_table().expect("validated colors").clone());
                        } else {
                            theme.insert(field.clone(), value.clone());
                        }
                    }
                }
                "shortcuts" => {
                    for key in value.as_table().expect("validated shortcuts").values() {
                        let byte = key.as_str().expect("validated shortcut key").as_bytes()[0];
                        let canonical = HistoryKey::Byte(byte);
                        self.disabled
                            .retain(|entry| *entry != (BindingMode::Normal, canonical));
                        self.selection.sources.remove(&format!(
                            "keybinds.normal.{:?} (removed)",
                            canonical_name(canonical)
                        ));
                    }
                    table_mut(&mut self.document, field)?
                        .extend(value.as_table().expect("validated shortcuts").clone());
                }
                "notifications" | "session_manager" => {
                    table_mut(&mut self.document, field)?
                        .extend(value.as_table().expect("validated table").clone());
                }
                "clear_defaults"
                    if value.as_bool() == Some(false)
                        && self.document.get(field).and_then(toml::Value::as_bool)
                            == Some(true) => {}
                _ => {
                    self.document.insert(field.clone(), value.clone());
                }
            }
        }
        // Binding origins are recorded by merge_bindings after aliases and removals resolve.
        let mut other = patch.clone();
        other.remove("keybinds");
        if other.get("clear_defaults").and_then(toml::Value::as_bool) == Some(false)
            && self
                .document
                .get("clear_defaults")
                .and_then(toml::Value::as_bool)
                == Some(true)
        {
            other.remove("clear_defaults");
        }
        self.selection.record(&other, layer);
        Ok(())
    }

    fn merge_bindings(&mut self, value: &toml::Value, layer: &str) -> Result<(), String> {
        for (mode, bindings) in value.as_table().expect("validated keybinds") {
            let binding_mode = display::mode(mode).expect("validated mode");
            for (key, value) in bindings.as_table().expect("validated bindings") {
                let canonical = display::parse_display_key(key).expect("validated key");
                let bindings = table_mut(table_mut(&mut self.document, "keybinds")?, mode)?;
                let inherited = bindings
                    .keys()
                    .find(|name| display::parse_display_key(name) == Some(canonical))
                    .cloned();
                let name = canonical_name(canonical);
                let prefix = format!("keybinds.{mode}.{name:?}");
                let shorthand = value.is_array();
                let value = normalize_binding(value);
                if empty_actions(&value) {
                    if let Some(name) = &inherited {
                        let removed = bindings.remove(name).expect("inherited binding");
                        for key in
                            displaced_normal_defaults(binding_mode, canonical, name, &removed)
                        {
                            if !self.disabled.contains(&(binding_mode, key)) {
                                self.disabled.push((binding_mode, key));
                            }
                        }
                    }
                    if !self.disabled.contains(&(binding_mode, canonical)) {
                        self.disabled.push((binding_mode, canonical));
                    }
                    self.selection
                        .sources
                        .retain(|field, _| !field.starts_with(&prefix));
                    self.selection
                        .sources
                        .insert(format!("{prefix} (removed)"), layer.to_owned());
                } else {
                    if value.get("actions").is_none()
                        && self.disabled.contains(&(binding_mode, canonical))
                    {
                        return Err(format!(
                            "{layer}: {prefix} needs actions because the binding was removed"
                        ));
                    }
                    self.disabled
                        .retain(|entry| *entry != (binding_mode, canonical));
                    let old = inherited.and_then(|name| bindings.remove(&name));
                    let mut result = old
                        .as_ref()
                        .and_then(toml::Value::as_table)
                        .cloned()
                        .unwrap_or_default();
                    if !result.contains_key("actions") && value.get("actions").is_none() {
                        self.selection
                            .sources
                            .entry(format!("{prefix}.actions"))
                            .or_insert_with(|| "built-in defaults".into());
                    }
                    if shorthand {
                        result.clear();
                        self.selection
                            .sources
                            .retain(|field, _| !field.starts_with(&format!("{prefix}.")));
                        result.insert("display".into(), toml::Value::String("always".into()));
                        self.selection
                            .sources
                            .insert(format!("{prefix}.display"), layer.to_owned());
                    }
                    for (field, value) in value.as_table().expect("validated binding") {
                        result.insert(field.clone(), value.clone());
                        self.selection
                            .sources
                            .insert(format!("{prefix}.{field}"), layer.to_owned());
                    }
                    bindings.insert(key.clone(), toml::Value::Table(result));
                    self.selection
                        .sources
                        .remove(&format!("{prefix} (removed)"));
                }
            }
        }
        Ok(())
    }

    pub(super) fn disable_bindings(&self, shortcuts: &mut Shortcuts) -> Result<(), String> {
        for &(mode, key) in &self.disabled {
            shortcuts.disable(mode, key);
        }
        if shortcuts.disabled(
            BindingMode::Locked,
            HistoryKey::Byte(shortcuts.locked_entry_key()),
        ) {
            return Err(
                "removing the active locked-mode entry key requires a replacement binding".into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile_for(
        source: &str,
        os: &str,
        machine: &str,
    ) -> Result<(ParsedConfig, Selection, Vec<String>), String> {
        parse_config_for(source, &Environment::detect(Some(os), Some(machine))?)
    }

    #[test]
    fn priority_is_fixed_and_unmatched_layers_do_not_apply() {
        let source = "scrollback_lines=10\ncompact=true\n[overrides.machine.'desk.local']\nscrollback_lines=30\n[overrides.os.linux]\nscrollback_lines=20\nshell='/usr/bin/fish'";
        let (parsed, selection, warnings) = compile_for(source, "linux", "desk.local").unwrap();
        assert_eq!(parsed.scrollback_lines, Some(30));
        assert!(parsed.compact);
        assert_eq!(parsed.shell.as_deref(), Some("/usr/bin/fish"));
        assert_eq!(
            selection.layers,
            [
                "built-in defaults",
                "config",
                "overrides.os.linux",
                "overrides.machine.desk.local"
            ]
        );
        assert_eq!(
            selection.sources["scrollback_lines"],
            "overrides.machine.desk.local"
        );
        assert!(warnings.is_empty());
        let (parsed, _, _) = compile_for(source, "macos", "desk").unwrap();
        assert_eq!(parsed.scrollback_lines, Some(10));
        assert!(parsed.shell.is_none());
    }

    #[test]
    fn tables_merge_fields_arrays_replace_and_presets_reset_colors() {
        let source = r##"
[theme]
preset='light'
[theme.colors]
accent='#010203'
[notifications]
command_duration_seconds=23
exclude_applications=['nvim']
[overrides.os.linux.theme.colors]
background='#040506'
[overrides.os.linux.notifications]
enabled=false
[overrides.machine.desk.notifications]
exclude_applications=[]
[overrides.machine.reset.theme]
preset='mocha'
"##;
        let (parsed, _, _) = compile_for(source, "linux", "desk").unwrap();
        assert_eq!(parsed.theme.accent, (1, 2, 3));
        assert_eq!(parsed.theme.background, (4, 5, 6));
        assert_eq!(parsed.theme.foreground, (36, 43, 59));
        assert!(!parsed.notifications.enabled);
        assert_eq!(
            parsed.notifications.command_duration,
            Duration::from_secs(23)
        );
        assert!(parsed.notifications.exclude_applications.is_empty());
        let (parsed, selection, _) = compile_for(source, "linux", "reset").unwrap();
        assert_eq!(parsed.theme, crate::theme::Theme::default());
        assert_eq!(
            selection.sources["theme.colors.accent"],
            "overrides.machine.reset"
        );
        assert_eq!(
            selection.sources["theme.colors.background"],
            "overrides.machine.reset"
        );
        assert_eq!(selection.sources["theme.preset"], "overrides.machine.reset");
    }

    #[test]
    fn native_bindings_merge_partial_fields_and_removals_suppress_defaults() {
        let source = r#"
[keybinds.normal]
N={actions=['new-window',{action='switch-mode',mode='locked'}],display='hidden'}
[keybinds.history]
u={actions=['scroll-down']}
[overrides.os.linux.keybinds.normal]
N={display='help'}
x=[]
left=[]
[overrides.os.linux.keybinds.history]
u=[]
j=[]
[overrides.machine.desk.keybinds.normal]
N=['new-window',{action='switch-mode',mode='locked'}]
"#;
        let (parsed, selection, warnings) = compile_for(source, "linux", "desk").unwrap();
        assert_eq!(parsed.shortcuts.resolve(b'N'), Some(b'c'));
        assert_eq!(
            parsed
                .shortcuts
                .display(BindingMode::Normal, HistoryKey::Byte(b'N')),
            BindingDisplay::Always
        );
        assert_eq!(parsed.shortcuts.resolve(b'x'), None);
        assert_eq!(parsed.shortcuts.resolve(130), Some(130));
        assert!(
            parsed
                .shortcuts
                .disabled(BindingMode::Normal, HistoryKey::Left)
        );
        assert!(
            parsed
                .shortcuts
                .history_binding(HistoryKey::Byte(b'u'))
                .unwrap()
                .actions
                .iter()
                .all(Option::is_none)
        );
        assert!(
            parsed
                .shortcuts
                .history_binding(HistoryKey::Byte(b'j'))
                .unwrap()
                .actions
                .iter()
                .all(Option::is_none)
        );
        assert!(
            !parsed
                .shortcuts
                .hints(BindingMode::Normal, true, true)
                .iter()
                .any(|hint| hint.key == HistoryKey::Byte(b'x'))
        );
        assert_eq!(
            selection.sources["keybinds.normal.\"N\".display"],
            "overrides.machine.desk"
        );
        assert!(warnings.is_empty());
        let (parsed, _, _) = compile_for(source, "linux", "other").unwrap();
        assert_eq!(
            parsed
                .shortcuts
                .display(BindingMode::Normal, HistoryKey::Byte(b'N')),
            BindingDisplay::Help
        );
    }

    #[test]
    fn removing_remapped_actions_does_not_revive_displaced_defaults() {
        let source = "[keybinds.normal]\nN={actions=['new-window',{action='switch-mode',mode='locked'}]}\n[overrides.os.linux.keybinds.normal]\nN=[]";
        let (parsed, _, _) = compile_for(source, "linux", "desk").unwrap();
        assert_eq!(parsed.shortcuts.resolve(b'N'), None);
        assert_eq!(parsed.shortcuts.resolve(b'c'), None);
        let (parsed, _, _) = compile_for(&format!("{source}\n[overrides.machine.desk.keybinds.normal]\nc=['new-window',{{action='switch-mode',mode='locked'}}]"), "linux", "desk").unwrap();
        assert_eq!(parsed.shortcuts.resolve(b'c'), Some(b'c'));
    }

    #[test]
    fn a_higher_legacy_shortcut_can_reenable_a_removed_physical_key() {
        let source = "[keybinds.normal]\nN={actions=['new-window',{action='switch-mode',mode='locked'}]}\n[overrides.os.linux.keybinds.normal]\nN=[]\n[overrides.machine.desk.shortcuts]\nnew_window='c'";
        let (parsed, selection, _) = compile_for(source, "linux", "desk").unwrap();
        assert_eq!(parsed.shortcuts.resolve(b'c'), Some(b'c'));
        assert_eq!(
            selection.sources["shortcuts.new_window"],
            "overrides.machine.desk"
        );
    }

    #[test]
    fn physical_aliases_override_and_report_inherited_metadata_once() {
        let source = "[keybinds.normal]\n'Ctrl t'={actions=[{action='switch-mode',mode='tab'}],display='hidden'}\n[overrides.os.linux.keybinds.normal]\n'Ctrl T'={display='help'}";
        let (parsed, selection, _) = compile_for(source, "linux", "desk").unwrap();
        assert!(parsed.shortcuts.enters_tab(20));
        assert_eq!(
            parsed
                .shortcuts
                .display(BindingMode::Normal, HistoryKey::Byte(20)),
            BindingDisplay::Help
        );
        assert_eq!(
            selection.sources["keybinds.normal.\"Ctrl t\".actions"],
            "config"
        );
        assert!(
            !selection
                .sources
                .keys()
                .any(|field| field.starts_with("keybinds.normal.\"Ctrl T\""))
        );
        assert!(
            compile_for(
                "[overrides.os.linux.keybinds.normal]\nenter=[]\n'Ctrl m'=[]",
                "macos",
                "desk"
            )
            .unwrap_err()
            .contains("duplicate canonical")
        );
    }

    #[test]
    fn a_display_overlay_does_not_activate_legacy_ignored_root_array_actions() {
        let source = "[keybinds.normal]\nc=['close-window',{action='switch-mode',mode='locked'}]\n[overrides.os.linux.keybinds.normal]\nc={display='help'}";
        let (parsed, _, _) = compile_for(source, "linux", "desk").unwrap();
        assert_eq!(parsed.shortcuts.resolve(b'c'), Some(b'c'));
        assert_eq!(
            parsed
                .shortcuts
                .display(BindingMode::Normal, HistoryKey::Byte(b'c')),
            BindingDisplay::Help
        );
    }

    #[test]
    fn clear_defaults_discards_inherited_maps_and_false_does_not_restore() {
        let source = r#"
[shortcuts]
new_window='N'
[keybinds.normal]
x={actions=['close-window',{action='switch-mode',mode='locked'}]}
[overrides.os.linux]
clear_defaults=true
default_mode='pane'
[overrides.os.linux.keybinds.locked]
'Ctrl b'=[{action='switch-mode',mode='normal'}]
[overrides.machine.desk]
clear_defaults=false
[overrides.machine.desk.keybinds.pane]
h=['focus-left']
"#;
        let (parsed, selection, _) = compile_for(source, "linux", "desk").unwrap();
        assert!(parsed.shortcuts.clear_defaults());
        assert_eq!(parsed.default_mode, DefaultMode::Pane);
        assert_eq!(parsed.shortcuts.resolve(b'N'), None);
        assert_eq!(parsed.shortcuts.resolve(b'x'), None);
        assert_eq!(
            parsed.shortcuts.pane_binding(b'h').unwrap().action,
            PaneAction::FocusLeft
        );
        assert_eq!(selection.sources["clear_defaults"], "overrides.os.linux");
        assert!(!selection.sources.contains_key("shortcuts.new_window"));
        assert!(compile_for(source, "linux", "other").is_err());
    }

    #[test]
    fn cross_layer_manager_conflicts_are_checked_only_on_the_combined_result() {
        let source =
            "[session_manager]\ndown=['k']\n[overrides.machine.desk.session_manager]\nup=['u']";
        assert!(compile_for(source, "linux", "desk").is_ok());
        assert!(
            compile_for(source, "linux", "other")
                .unwrap_err()
                .contains("conflicts")
        );
    }

    #[test]
    fn inactive_overrides_are_strict_but_root_compatibility_warnings_remain() {
        for fragment in [
            "compact_typo=true",
            "shell=''",
            "scrollback_lines=-1",
            "compact=3",
            "overrides={}",
            "default_mode='custom'",
            "notifications={typo=true}",
            "notifications={command_duration_seconds=0}",
            "session_manager={typo=[]}",
            "shortcuts={new_window='two'}",
            "theme={preset='light',colors={accent='red'}}",
            "keybinds={normal={x=['future-action']}}",
            "keybinds={normal={x=[{action='switch-mode',mode='future'}]}}",
            "keybinds={normal={x=[{action='switch-mode',mode='locked',typo=true}]}}",
            "keybinds={scroll={x=[]}}",
            "keybinds={normal={x={display='bad'}}}",
        ] {
            let error = compile_for(
                &format!("[overrides.machine.other]\n{fragment}"),
                "linux",
                "desk",
            )
            .unwrap_err();
            assert!(error.contains("overrides.machine.other"), "{error}");
        }
        assert!(
            compile_for("[overrides.os.maco]\ncompact=true", "linux", "desk")
                .unwrap_err()
                .contains("unknown operating system")
        );
        assert!(compile_for("[overrides.typo]\ncompact=true", "linux", "desk").is_err());
        let (_, _, warnings) = compile_for("future_option=true", "linux", "desk").unwrap();
        assert_eq!(warnings, ["ignored top-level option \"future_option\""]);
    }

    #[test]
    fn removed_bindings_need_actions_to_be_reenabled_and_entry_remains_accessible() {
        assert!(compile_for("[overrides.os.linux.keybinds.normal]\nx=[]\n[overrides.machine.desk.keybinds.normal]\nx={display='help'}", "linux", "desk").is_err());
        assert!(
            compile_for(
                "[overrides.os.linux.keybinds.locked]\n'Ctrl b'=[]",
                "linux",
                "desk"
            )
            .is_err()
        );
        assert!(compile_for("[overrides.os.linux.keybinds.locked]\n'Ctrl b'=[]\n'Ctrl a'=[{action='switch-mode',mode='normal'}]", "linux", "desk").is_ok());
    }

    #[test]
    fn machine_identity_handles_precedence_invalid_input_and_hostname_failure() {
        let no_hostname = || panic!("hostname must not be consulted");
        let (machine, source, _) =
            select_machine(Some("cli"), Some("environment".into()), no_hostname).unwrap();
        assert_eq!(machine.as_deref(), Some("cli"));
        assert_eq!(source, "--machine");
        let (machine, source, _) =
            select_machine(None, Some("environment".into()), no_hostname).unwrap();
        assert_eq!(machine.as_deref(), Some("environment"));
        assert_eq!(source, "RUSTMUX_MACHINE");
        let (machine, source, note) =
            select_machine(None, None, || Ok("desk.local".into())).unwrap();
        assert_eq!(machine.as_deref(), Some("desk.local"));
        assert_eq!(source, "system hostname");
        assert!(note.is_none());
        assert!(select_machine(None, Some("".into()), no_hostname).is_err());
        assert!(select_machine(Some("bad\nname"), None, no_hostname).is_err());
        use std::os::unix::ffi::OsStringExt;
        assert!(select_machine(None, Some(OsString::from_vec(vec![255])), no_hostname).is_err());
        let (machine, _, note) = select_machine(None, None, || Err("failure".into())).unwrap();
        assert!(machine.is_none());
        assert!(note.unwrap().contains("machine overrides skipped"));
        assert!(Environment::detect(Some("Linux"), Some("desk")).is_err());
    }
}
