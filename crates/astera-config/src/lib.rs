use std::{collections::BTreeMap, fs, path::Path, time::Duration};

use astera_core::{CameraPolicy, Direction, WindowMode};
use serde::{Deserialize, Deserializer};
use thiserror::Error;
use xkbcommon::xkb;

#[derive(Clone, Debug)]
pub struct Config {
    pub gap: i64,
    pub animation_ms: u64,
    pub camera: CameraPolicy,
    pub key_repeat: KeyRepeatConfig,
    pub bindings: Bindings,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct KeyRepeatConfig {
    pub delay_ms: u64,
    pub rate: u32,
}

impl Default for KeyRepeatConfig {
    fn default() -> Self {
        Self {
            delay_ms: 300,
            rate: 25,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Bindings {
    entries: BTreeMap<BindingKey, Binding>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BindingKey {
    pub modifiers: Modifiers,
    pub trigger: KeyTrigger,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const CTRL: u8 = 1;
    pub const ALT: u8 = 2;
    pub const SHIFT: u8 = 4;
    pub const SUPER: u8 = 8;

    pub fn ctrl(self) -> bool {
        self.0 & Self::CTRL != 0
    }
    pub fn alt(self) -> bool {
        self.0 & Self::ALT != 0
    }
    pub fn shift(self) -> bool {
        self.0 & Self::SHIFT != 0
    }
    pub fn super_key(self) -> bool {
        self.0 & Self::SUPER != 0
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum KeyTrigger {
    Keysym(u32),
    Code(u32),
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub action: Action,
    #[serde(default)]
    pub repeat: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub enum Action {
    Spawn(Vec<String>),
    SpawnShell(String),
    FocusWorkspace {
        workspace: WorkspaceSelector,
    },
    MoveWindowToWorkspace {
        workspace: WorkspaceSelector,
        #[serde(default)]
        activate: bool,
    },
    FocusOutput {
        output: String,
    },
    MoveWorkspaceToOutput {
        output: String,
        #[serde(default)]
        index: Option<usize>,
        #[serde(default = "default_true")]
        activate: bool,
    },
    FocusDirection(Direction),
    PanCamera {
        x: i64,
        y: i64,
    },
    SetWindowMode(WindowMode),
    ToggleFloating,
    ToggleFullscreen,
    CloseWindow,
    Quit,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub enum WorkspaceSelector {
    Index(usize, #[serde(default)] Option<String>),
    Name(String),
}

impl Action {
    pub fn can_repeat(&self) -> bool {
        matches!(
            self,
            Self::FocusWorkspace { .. }
                | Self::MoveWindowToWorkspace { .. }
                | Self::FocusDirection(_)
                | Self::PanCamera { .. }
        )
    }
}

impl Bindings {
    pub fn empty() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, key: &BindingKey) -> Option<&Binding> {
        self.entries.get(key)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&BindingKey, &Binding)> {
        self.entries.iter()
    }

    pub fn built_in() -> Self {
        let mut entries = BTreeMap::new();
        for index in 1..=9 {
            insert_builtin(
                &mut entries,
                &format!("Super+{index}"),
                Action::FocusWorkspace {
                    workspace: WorkspaceSelector::Index(index, None),
                },
                false,
            );
            insert_builtin(
                &mut entries,
                &format!("Super+Shift+{index}"),
                Action::MoveWindowToWorkspace {
                    workspace: WorkspaceSelector::Index(index, None),
                    activate: false,
                },
                false,
            );
        }
        insert_builtin(&mut entries, "Super+space", Action::ToggleFloating, false);
        insert_builtin(&mut entries, "Super+f", Action::ToggleFullscreen, false);
        for (key, x, y) in [
            ("Left", -160, 0),
            ("Right", 160, 0),
            ("Up", 0, -160),
            ("Down", 0, 160),
        ] {
            insert_builtin(
                &mut entries,
                &format!("Super+{key}"),
                Action::PanCamera { x, y },
                true,
            );
        }
        Self { entries }
    }
}

fn insert_builtin(
    entries: &mut BTreeMap<BindingKey, Binding>,
    key: &str,
    action: Action,
    repeat: bool,
) {
    entries.insert(
        parse_binding_key(key).expect("built-in binding is valid"),
        Binding { action, repeat },
    );
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gap: 8,
            animation_ms: 280,
            camera: CameraPolicy::KeepVisible { margin: 32 },
            key_repeat: KeyRepeatConfig::default(),
            bindings: Bindings::built_in(),
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    gap: i64,
    animation_ms: u64,
    camera: CameraPolicy,
    key_repeat: KeyRepeatConfig,
    bindings: BindingMap,
}

impl Default for FileConfig {
    fn default() -> Self {
        let defaults = Config::default();
        Self {
            gap: defaults.gap,
            animation_ms: defaults.animation_ms,
            camera: defaults.camera,
            key_repeat: defaults.key_repeat,
            bindings: BindingMap::default(),
        }
    }
}

#[derive(Default)]
struct BindingMap(BTreeMap<String, BindingValue>);

#[derive(Deserialize)]
#[serde(untagged)]
enum BindingValue {
    Detailed(Binding),
    Shorthand(Action),
}

impl<'de> Deserialize<'de> for BindingMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        BTreeMap::<String, BindingValue>::deserialize(deserializer).map(Self)
    }
}

impl Config {
    pub fn animation_duration(&self) -> Duration {
        Duration::from_millis(self.animation_ms)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let contents = fs::read_to_string(path)?;
        Self::from_ron(&contents)
    }

    pub fn from_ron(contents: &str) -> Result<Self, ConfigError> {
        let normalized = normalize_workspace_selectors(contents);
        let raw: FileConfig = ron::from_str(&normalized)?;
        let mut entries = BTreeMap::new();
        for (source, value) in raw.bindings.0 {
            let key = parse_binding_key(&source)?;
            let binding = match value {
                BindingValue::Detailed(binding) => binding,
                BindingValue::Shorthand(action) => Binding {
                    action,
                    repeat: false,
                },
            };
            if binding.repeat && !binding.action.can_repeat() {
                return Err(ConfigError::Invalid(format!(
                    "binding {source:?} enables repeat for a non-repeatable action"
                )));
            }
            if entries.insert(key, binding).is_some() {
                return Err(ConfigError::Invalid(format!(
                    "binding {source:?} duplicates another normalized binding"
                )));
            }
        }
        let config = Self {
            gap: raw.gap,
            animation_ms: raw.animation_ms,
            camera: raw.camera,
            key_repeat: raw.key_repeat,
            bindings: Bindings { entries },
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.gap < 0 {
            return Err(ConfigError::Invalid("gap cannot be negative".into()));
        }
        if !(1..=5000).contains(&self.key_repeat.delay_ms) {
            return Err(ConfigError::Invalid(
                "key repeat delay must be 1..=5000 ms".into(),
            ));
        }
        if !(1..=200).contains(&self.key_repeat.rate) {
            return Err(ConfigError::Invalid(
                "key repeat rate must be 1..=200".into(),
            ));
        }
        Ok(())
    }
}

fn normalize_workspace_selectors(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(relative) = source[cursor..].find("Index(") {
        let start = cursor + relative;
        output.push_str(&source[cursor..start]);
        let arguments_start = start + "Index(".len();
        let Some(end) = find_selector_end(source, arguments_start) else {
            output.push_str(&source[start..]);
            return output;
        };
        let arguments = &source[arguments_start..end];
        if let Some(comma) = find_unquoted_comma(arguments) {
            let index = arguments[..comma].trim();
            let key = arguments[comma + 1..].trim();
            output.push_str(&format!("Index({index},Some({key}))"));
        } else {
            output.push_str(&format!("Index({},None)", arguments.trim()));
        }
        cursor = end + 1;
    }
    output.push_str(&source[cursor..]);
    output
}

fn find_selector_end(source: &str, start: usize) -> Option<usize> {
    let mut quoted = false;
    let mut escaped = false;
    for (offset, character) in source[start..].char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
        } else if character == '"' {
            quoted = true;
        } else if character == ')' {
            return Some(start + offset);
        }
    }
    None
}

fn find_unquoted_comma(source: &str) -> Option<usize> {
    let mut quoted = false;
    let mut escaped = false;
    for (offset, character) in source.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
        } else if character == '"' {
            quoted = true;
        } else if character == ',' {
            return Some(offset);
        }
    }
    None
}

fn parse_binding_key(source: &str) -> Result<BindingKey, ConfigError> {
    let mut modifiers = 0_u8;
    let mut trigger = None;
    for component in source.split('+') {
        let component = component.trim();
        if component.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "empty component in binding {source:?}"
            )));
        }
        let modifier = match component.to_ascii_lowercase().as_str() {
            "ctrl" => Some(Modifiers::CTRL),
            "alt" => Some(Modifiers::ALT),
            "shift" => Some(Modifiers::SHIFT),
            "super" => Some(Modifiers::SUPER),
            _ => None,
        };
        if let Some(modifier) = modifier {
            if modifiers & modifier != 0 {
                return Err(ConfigError::Invalid(format!(
                    "duplicate modifier in {source:?}"
                )));
            }
            modifiers |= modifier;
            continue;
        }
        if trigger.is_some() {
            return Err(ConfigError::Invalid(format!(
                "multiple keys in binding {source:?}"
            )));
        }
        trigger = Some(if let Some(code) = component.strip_prefix("code:") {
            let code = code
                .strip_prefix("0x")
                .map(|hex| u32::from_str_radix(hex, 16))
                .unwrap_or_else(|| code.parse())
                .map_err(|_| ConfigError::Invalid(format!("invalid keycode in {source:?}")))?;
            KeyTrigger::Code(code)
        } else {
            let keysym = xkb::keysym_from_name(component, xkb::KEYSYM_CASE_INSENSITIVE);
            if keysym == xkb::Keysym::NoSymbol {
                return Err(ConfigError::Invalid(format!(
                    "unknown keysym in {source:?}"
                )));
            }
            KeyTrigger::Keysym(keysym.raw())
        });
    }
    Ok(BindingKey {
        modifiers: Modifiers(modifiers),
        trigger: trigger
            .ok_or_else(|| ConfigError::Invalid(format!("missing key in {source:?}")))?,
    })
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read configuration: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse RON configuration: {0}")]
    Parse(#[from] ron::error::SpannedError),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_uses_built_in_bindings_but_file_does_not() {
        assert!(!Config::default().bindings.is_empty());
        assert!(Config::from_ron("()").unwrap().bindings.is_empty());
    }

    #[test]
    fn parses_shorthand_detailed_and_physical_bindings() {
        let config = Config::from_ron(
            r#"(
                bindings: {
                    "Super+Return": Spawn(["foot"]),
                    "Super+Right": (action: PanCamera(x: 10, y: 0), repeat: true),
                    "Super+code:0x7b": CloseWindow,
                    "Super+1": FocusWorkspace(workspace: Index(1)),
                    "Super+2": FocusWorkspace(workspace: Index(2, "DP-1")),
                },
            )"#,
        )
        .unwrap();
        assert_eq!(config.bindings.len(), 5);
    }

    #[test]
    fn rejects_normalized_duplicates_and_unsafe_repeat() {
        assert!(
            Config::from_ron(
                r#"(bindings: {"Super+Return": CloseWindow, "super+return": CloseWindow})"#
            )
            .is_err()
        );
        assert!(
            Config::from_ron(
                r#"(bindings: {"Super+Return": (action: Spawn(["foot"]), repeat: true)})"#
            )
            .is_err()
        );
    }
}
