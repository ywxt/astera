use std::{collections::BTreeMap, fmt::Write as _, fs, path::Path};

use astera_core::{CameraPolicy, Direction, WindowMode};
use kdl::{KdlDocument, KdlNode, KdlValue};
use thiserror::Error;
use xkbcommon::xkb;

#[derive(Clone, Debug)]
pub struct Config {
    pub gap: i64,
    pub camera: CameraPolicy,
    pub key_repeat: KeyRepeatConfig,
    pub bindings: Bindings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    /// Canonical keys make differently spelled but equivalent bindings collide during loading.
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

    pub fn from_state(ctrl: bool, alt: bool, shift: bool, super_key: bool) -> Self {
        Self(
            (u8::from(ctrl) * Self::CTRL)
                | (u8::from(alt) * Self::ALT)
                | (u8::from(shift) * Self::SHIFT)
                | (u8::from(super_key) * Self::SUPER),
        )
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum KeyTrigger {
    /// Layout-aware XKB symbol used by normal human-readable bindings.
    Keysym(u32),
    /// Linux evdev code available as an explicit physical-key escape hatch.
    Code(u32),
}

impl BindingKey {
    pub fn keysym(modifiers: Modifiers, keysym: u32) -> Self {
        Self {
            modifiers,
            trigger: KeyTrigger::Keysym(keysym),
        }
    }
    pub fn code(modifiers: Modifiers, code: u32) -> Self {
        Self {
            modifiers,
            trigger: KeyTrigger::Code(code),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub action: Action,
    pub repeat: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Spawn(Vec<String>),
    FocusWorkspace {
        workspace: WorkspaceSelector,
    },
    MoveWindowToWorkspace {
        workspace: WorkspaceSelector,
        activate: bool,
    },
    FocusOutput {
        output: String,
    },
    MoveWorkspaceToOutput {
        output: String,
        index: Option<usize>,
        activate: bool,
    },
    FocusDirection(CardinalDirection),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardinalDirection {
    Left,
    Right,
    Up,
    Down,
}

impl CardinalDirection {
    pub fn as_direction(self) -> Direction {
        match self {
            Self::Left => Direction::new(-1.0, 0.0),
            Self::Right => Direction::new(1.0, 0.0),
            Self::Up => Direction::new(0.0, -1.0),
            Self::Down => Direction::new(0.0, 1.0),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceSelector {
    Index(usize, Option<String>),
    Name(String),
    Id(u32),
}

impl Action {
    pub fn can_repeat(&self) -> bool {
        // Exclude toggles and process actions to prevent mode oscillation or process storms.
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
        insert_builtin(
            &mut entries,
            "Super+Return",
            Action::Spawn(vec!["kitty".into()]),
            false,
        );
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
            camera: CameraPolicy::KeepVisible { margin: 32 },
            key_repeat: KeyRepeatConfig::default(),
            bindings: Bindings::built_in(),
        }
    }
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let contents = fs::read_to_string(path)?;
        Self::from_kdl(&contents)
    }

    pub fn from_kdl(contents: &str) -> Result<Self, ConfigError> {
        let document: KdlDocument = contents
            .parse()
            .map_err(|error: kdl::KdlError| ConfigError::Parse(format_kdl_error(&error)))?;
        let defaults = Self::default();
        let mut gap = defaults.gap;
        let mut camera = defaults.camera;
        let mut key_repeat = defaults.key_repeat;
        let mut entries = BTreeMap::new();
        let mut sections = BTreeMap::<&str, ()>::new();

        for node in document.nodes() {
            let name = node.name().value();
            let result = (|| -> Result<(), ConfigError> {
                match name {
                    "general" | "input" | "camera" => {
                        if sections.insert(name, ()).is_some() {
                            return invalid(format!("duplicate `{name}` section"));
                        }
                        require_no_entries(node)?;
                        let children = node.children().ok_or_else(|| {
                            invalid_error(format!("`{name}` requires a child block"))
                        })?;
                        match name {
                            "general" => parse_general(children, &mut gap)?,
                            "input" => parse_input(children, &mut key_repeat)?,
                            "camera" => camera = parse_camera(children)?,
                            _ => unreachable!(),
                        }
                        Ok(())
                    }
                    "bind" => {
                        let (source, binding) = parse_bind(node)?;
                        let key = parse_binding_key(&source)?;
                        if entries.insert(key, binding).is_some() {
                            return invalid(format!(
                                "binding {source:?} duplicates another normalized binding"
                            ));
                        }
                        Ok(())
                    }
                    _ => unknown("top-level node", name, TOP_LEVEL_NAMES),
                }
            })();
            if let Err(error) = result {
                return Err(error.with_location(contents, node.span().offset()));
            }
        }
        let config = Self {
            gap,
            camera,
            key_repeat,
            bindings: Bindings { entries },
        };
        config.validate()?;
        Ok(config)
    }

    pub fn format_kdl(contents: &str) -> Result<String, ConfigError> {
        Self::from_kdl(contents)?;
        let mut document: KdlDocument = contents
            .parse()
            .map_err(|error: kdl::KdlError| ConfigError::Parse(format_kdl_error(&error)))?;
        document.autoformat();
        Ok(document.to_string())
    }

    pub fn generated_kdl() -> String {
        let mut output = GENERATED_CONFIG_HEADER.to_owned();
        for index in 1..=9 {
            writeln!(
                output,
                "bind \"Super+{index}\" {{\n    focus-workspace {index}\n}}\n\
                 bind \"Super+Shift+{index}\" {{\n    move-window-to-workspace {index}\n}}"
            )
            .expect("writing to a String cannot fail");
        }
        output.push_str(GENERATED_CONFIG_TRAILER);
        Self::format_kdl(&output).expect("generated configuration is valid KDL")
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

const TOP_LEVEL_NAMES: &[&str] = &["general", "input", "camera", "bind"];

fn parse_general(document: &KdlDocument, gap: &mut i64) -> Result<(), ConfigError> {
    let mut seen = false;
    for node in document.nodes() {
        if node.name().value() != "gap" {
            return unknown("general setting", node.name().value(), &["gap"]);
        }
        if seen {
            return invalid("duplicate `gap` setting");
        }
        *gap = one_i64(node, "gap")?;
        seen = true;
    }
    Ok(())
}

fn parse_input(document: &KdlDocument, input: &mut KeyRepeatConfig) -> Result<(), ConfigError> {
    let mut delay = false;
    let mut rate = false;
    for node in document.nodes() {
        match node.name().value() {
            "repeat-delay" if !delay => {
                input.delay_ms = one_u64(node, "repeat-delay")?;
                delay = true;
            }
            "repeat-rate" if !rate => {
                input.rate = one_u32(node, "repeat-rate")?;
                rate = true;
            }
            "repeat-delay" | "repeat-rate" => {
                return invalid(format!("duplicate `{}` setting", node.name().value()));
            }
            name => return unknown("input setting", name, &["repeat-delay", "repeat-rate"]),
        }
    }
    Ok(())
}

fn parse_camera(document: &KdlDocument) -> Result<CameraPolicy, ConfigError> {
    let [node] = document.nodes() else {
        return invalid("camera requires exactly one policy");
    };
    match node.name().value() {
        "centered" => {
            require_no_entries(node)?;
            require_no_children(node)?;
            Ok(CameraPolicy::Centered)
        }
        "keep-visible" => {
            require_no_children(node)?;
            require_properties(node, &["margin"])?;
            let margin = property_i64(node, "margin")?.unwrap_or(32);
            Ok(CameraPolicy::KeepVisible { margin })
        }
        name => unknown("camera policy", name, &["centered", "keep-visible"]),
    }
}

fn parse_bind(node: &KdlNode) -> Result<(String, Binding), ConfigError> {
    require_properties(node, &["repeat"])?;
    let args = positional(node);
    let [key] = args.as_slice() else {
        return invalid("`bind` requires exactly one key string");
    };
    let key = key
        .as_string()
        .ok_or_else(|| invalid_error("binding key must be a string"))?
        .to_owned();
    let repeat = property_bool(node, "repeat")?.unwrap_or(false);
    let children = node
        .children()
        .ok_or_else(|| invalid_error(format!("binding {key:?} requires an action block")))?;
    let [action_node] = children.nodes() else {
        return invalid(format!("binding {key:?} must contain exactly one action"));
    };
    let action = parse_action(action_node)?;
    if repeat && !action.can_repeat() {
        return invalid(format!(
            "binding {key:?} enables repeat for a non-repeatable action"
        ));
    }
    validate_action(&action, &key)?;
    Ok((key, Binding { action, repeat }))
}

fn parse_action(node: &KdlNode) -> Result<Action, ConfigError> {
    require_no_children(node)?;
    let name = node.name().value();
    Ok(match name {
        "spawn" => {
            require_properties(node, &[])?;
            let argv = positional_strings(node)?;
            Action::Spawn(argv)
        }
        "focus-workspace" => {
            require_properties(node, &["output", "id"])?;
            Action::FocusWorkspace {
                workspace: parse_workspace(node)?,
            }
        }
        "move-window-to-workspace" => {
            require_properties(node, &["output", "id", "activate"])?;
            Action::MoveWindowToWorkspace {
                workspace: parse_workspace(node)?,
                activate: property_bool(node, "activate")?.unwrap_or(false),
            }
        }
        "focus-output" => Action::FocusOutput {
            output: one_string(node, "focus-output")?,
        },
        "move-workspace-to-output" => {
            require_properties(node, &["index", "activate"])?;
            Action::MoveWorkspaceToOutput {
                output: first_string(node, "move-workspace-to-output")?,
                index: property_usize(node, "index")?,
                activate: property_bool(node, "activate")?.unwrap_or(true),
            }
        }
        "focus-window" => Action::FocusDirection(parse_cardinal(node)?),
        "pan-camera" => {
            require_properties(node, &[])?;
            let args = positional(node);
            let [x, y] = args.as_slice() else {
                return invalid("`pan-camera` requires x and y integers");
            };
            Action::PanCamera {
                x: value_i64(x, "pan-camera x")?,
                y: value_i64(y, "pan-camera y")?,
            }
        }
        "set-window-mode" => {
            Action::SetWindowMode(parse_mode(&one_string(node, "set-window-mode")?)?)
        }
        "toggle-floating" => empty_action(node, Action::ToggleFloating)?,
        "toggle-fullscreen" => empty_action(node, Action::ToggleFullscreen)?,
        "close-window" => empty_action(node, Action::CloseWindow)?,
        "quit" => empty_action(node, Action::Quit)?,
        _ => return unknown("action", name, ACTION_NAMES),
    })
}

const ACTION_NAMES: &[&str] = &[
    "spawn",
    "focus-workspace",
    "move-window-to-workspace",
    "focus-output",
    "move-workspace-to-output",
    "focus-window",
    "pan-camera",
    "set-window-mode",
    "toggle-floating",
    "toggle-fullscreen",
    "close-window",
    "quit",
];

fn parse_workspace(node: &KdlNode) -> Result<WorkspaceSelector, ConfigError> {
    if let Some(id) = property_usize(node, "id")? {
        if !positional(node).is_empty() || node.get("output").is_some() {
            return invalid("workspace `id` cannot be combined with an argument or `output`");
        }
        return Ok(WorkspaceSelector::Id(
            u32::try_from(id).map_err(|_| invalid_error("workspace id is out of range"))?,
        ));
    }
    let args = positional(node);
    let [selector] = args.as_slice() else {
        return invalid(format!(
            "`{}` requires one workspace selector",
            node.name().value()
        ));
    };
    match selector {
        KdlValue::Integer(index) => Ok(WorkspaceSelector::Index(
            usize::try_from(*index)
                .map_err(|_| invalid_error("workspace index is out of range"))?,
            property_string(node, "output")?,
        )),
        KdlValue::String(name) => {
            if node.get("output").is_some() {
                return invalid("workspace names cannot be combined with `output`");
            }
            Ok(WorkspaceSelector::Name(name.clone()))
        }
        _ => invalid("workspace selector must be an integer index or string name"),
    }
}

fn parse_cardinal(node: &KdlNode) -> Result<CardinalDirection, ConfigError> {
    match one_string(node, "focus-window")?.as_str() {
        "left" => Ok(CardinalDirection::Left),
        "right" => Ok(CardinalDirection::Right),
        "up" => Ok(CardinalDirection::Up),
        "down" => Ok(CardinalDirection::Down),
        value => invalid(format!(
            "unknown direction {value:?}; expected left, right, up, or down"
        )),
    }
}

fn parse_mode(value: &str) -> Result<WindowMode, ConfigError> {
    match value {
        "tiled" => Ok(WindowMode::Tiled),
        "floating" => Ok(WindowMode::Floating),
        "maximized" => Ok(WindowMode::Maximized),
        "fullscreen" => Ok(WindowMode::Fullscreen),
        _ => invalid(format!("unknown window mode {value:?}")),
    }
}

fn positional(node: &KdlNode) -> Vec<&KdlValue> {
    node.entries()
        .iter()
        .filter(|entry| entry.name().is_none())
        .map(|entry| entry.value())
        .collect()
}

fn positional_strings(node: &KdlNode) -> Result<Vec<String>, ConfigError> {
    positional(node)
        .into_iter()
        .map(|value| {
            value.as_string().map(str::to_owned).ok_or_else(|| {
                invalid_error(format!(
                    "`{}` arguments must be strings",
                    node.name().value()
                ))
            })
        })
        .collect()
}

fn require_properties(node: &KdlNode, allowed: &[&str]) -> Result<(), ConfigError> {
    let mut seen = BTreeMap::new();
    for entry in node.entries().iter().filter(|entry| entry.name().is_some()) {
        let name = entry.name().unwrap().value();
        if !allowed.contains(&name) {
            return unknown("property", name, allowed);
        }
        if seen.insert(name, ()).is_some() {
            return invalid(format!("duplicate `{name}` property"));
        }
    }
    Ok(())
}

fn require_no_entries(node: &KdlNode) -> Result<(), ConfigError> {
    if node.entries().is_empty() {
        Ok(())
    } else {
        invalid(format!(
            "`{}` does not accept values or properties",
            node.name().value()
        ))
    }
}

fn require_no_children(node: &KdlNode) -> Result<(), ConfigError> {
    if node.children().is_none() {
        Ok(())
    } else {
        invalid(format!(
            "`{}` does not accept a child block",
            node.name().value()
        ))
    }
}

fn empty_action(node: &KdlNode, action: Action) -> Result<Action, ConfigError> {
    require_no_entries(node)?;
    Ok(action)
}

fn one_string(node: &KdlNode, label: &str) -> Result<String, ConfigError> {
    require_properties(node, &[])?;
    first_string(node, label).and_then(|value| {
        if positional(node).len() == 1 {
            Ok(value)
        } else {
            invalid(format!("`{label}` requires exactly one string"))
        }
    })
}

fn first_string(node: &KdlNode, label: &str) -> Result<String, ConfigError> {
    positional(node)
        .first()
        .and_then(|value| value.as_string())
        .map(str::to_owned)
        .ok_or_else(|| invalid_error(format!("`{label}` requires a string")))
}

fn one_i64(node: &KdlNode, label: &str) -> Result<i64, ConfigError> {
    require_properties(node, &[])?;
    require_no_children(node)?;
    let args = positional(node);
    let [value] = args.as_slice() else {
        return invalid(format!("`{label}` requires one integer"));
    };
    value_i64(value, label)
}

fn one_u64(node: &KdlNode, label: &str) -> Result<u64, ConfigError> {
    u64::try_from(one_i64(node, label)?)
        .map_err(|_| invalid_error(format!("`{label}` must be non-negative")))
}
fn one_u32(node: &KdlNode, label: &str) -> Result<u32, ConfigError> {
    u32::try_from(one_i64(node, label)?)
        .map_err(|_| invalid_error(format!("`{label}` is out of range")))
}
fn value_i64(value: &KdlValue, label: &str) -> Result<i64, ConfigError> {
    value
        .as_integer()
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| invalid_error(format!("`{label}` must be an integer")))
}

fn property_string(node: &KdlNode, name: &str) -> Result<Option<String>, ConfigError> {
    node.get(name)
        .map(|value| {
            value
                .as_string()
                .map(str::to_owned)
                .ok_or_else(|| invalid_error(format!("`{name}` must be a string")))
        })
        .transpose()
}
fn property_i64(node: &KdlNode, name: &str) -> Result<Option<i64>, ConfigError> {
    node.get(name)
        .map(|value| value_i64(value, name))
        .transpose()
}
fn property_usize(node: &KdlNode, name: &str) -> Result<Option<usize>, ConfigError> {
    node.get(name)
        .map(|value| {
            value
                .as_integer()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| invalid_error(format!("`{name}` must be a non-negative integer")))
        })
        .transpose()
}
fn property_bool(node: &KdlNode, name: &str) -> Result<Option<bool>, ConfigError> {
    node.get(name)
        .map(|value| {
            value
                .as_bool()
                .or_else(|| match value.as_string() {
                    Some("true") => Some(true),
                    Some("false") => Some(false),
                    _ => None,
                })
                .ok_or_else(|| invalid_error(format!("`{name}` must be true or false")))
        })
        .transpose()
}

fn format_kdl_error(error: &kdl::KdlError) -> String {
    error
        .diagnostics
        .iter()
        .map(|diagnostic| {
            let offset = diagnostic.span.offset();
            let prefix = &error.input[..offset.min(error.input.len())];
            let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
            let column = prefix.rsplit('\n').next().map(str::len).unwrap_or(0) + 1;
            format!("{} at {line}:{column}", diagnostic)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn invalid<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(invalid_error(message))
}
fn invalid_error(message: impl Into<String>) -> ConfigError {
    ConfigError::Invalid(message.into())
}

fn unknown<T>(kind: &str, name: &str, candidates: &[&str]) -> Result<T, ConfigError> {
    let suggestion = candidates
        .iter()
        .min_by_key(|candidate| edit_distance(name, candidate));
    let suffix = suggestion
        .filter(|candidate| edit_distance(name, candidate) <= 3)
        .map(|candidate| format!("; did you mean `{candidate}`?"))
        .unwrap_or_default();
    invalid(format!("unknown {kind} `{name}`{suffix}"))
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.bytes().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, b) in right.bytes().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (row[j + 1] + 1)
                .min(row[j] + 1)
                .min(previous + usize::from(a != b));
            previous = old;
        }
    }
    row[right.len()]
}

const GENERATED_CONFIG_HEADER: &str = r#"// Astera configuration. A file replaces all built-in bindings.
general {
    gap 8
}

input {
    repeat-delay 300
    repeat-rate 25
}

camera {
    keep-visible margin=32
}

bind "Super+Return" {
    spawn "kitty"
}
"#;

const GENERATED_CONFIG_TRAILER: &str = r#"
bind "Super+Space" {
    toggle-floating
}
bind "Super+F" {
    toggle-fullscreen
}
bind "Super+Left" repeat=#true {
    pan-camera -160 0
}
bind "Super+Right" repeat=#true {
    pan-camera 160 0
}
bind "Super+Up" repeat=#true {
    pan-camera 0 -160
}
bind "Super+Down" repeat=#true {
    pan-camera 0 160
}
"#;

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
            // Config uses conventional evdev codes; the runtime adds XKB's offset of eight.
            let code = code
                .strip_prefix("0x")
                .map(|hex| u32::from_str_radix(hex, 16))
                .unwrap_or_else(|| code.parse())
                .map_err(|_| ConfigError::Invalid(format!("invalid keycode in {source:?}")))?;
            if code > 0x2ff {
                return Err(ConfigError::Invalid(format!(
                    "keycode is outside the Linux evdev range in {source:?}"
                )));
            }
            KeyTrigger::Code(code)
        } else {
            // Normalize printable keys so uppercase behavior remains an explicit Shift modifier.
            let normalized;
            let component = if component.len() == 1 && component.is_ascii() {
                normalized = component.to_ascii_lowercase();
                &normalized
            } else {
                component
            };
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

fn validate_action(action: &Action, source: &str) -> Result<(), ConfigError> {
    let workspace = match action {
        Action::FocusWorkspace { workspace } | Action::MoveWindowToWorkspace { workspace, .. } => {
            Some(workspace)
        }
        _ => None,
    };
    if matches!(workspace, Some(WorkspaceSelector::Index(0, _))) {
        return Err(ConfigError::Invalid(format!(
            "binding {source:?} uses workspace index zero"
        )));
    }
    if let Some(WorkspaceSelector::Index(index, _)) = workspace
        && u32::try_from(*index).is_err()
    {
        return Err(ConfigError::Invalid(format!(
            "binding {source:?} uses a workspace index larger than u32::MAX"
        )));
    }
    match action {
        Action::Spawn(argv) if argv.is_empty() || argv[0].is_empty() => Err(ConfigError::Invalid(
            format!("binding {source:?} has an empty Spawn argv"),
        )),
        Action::MoveWorkspaceToOutput { index: Some(0), .. } => Err(ConfigError::Invalid(format!(
            "binding {source:?} uses target index zero"
        ))),
        _ => Ok(()),
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read configuration: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse KDL configuration: {0}")]
    Parse(String),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

impl ConfigError {
    fn with_location(self, source: &str, offset: usize) -> Self {
        let Self::Invalid(message) = self else {
            return self;
        };
        let offset = offset.min(source.len());
        let prefix = &source[..offset];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let column = prefix.rsplit('\n').next().map(str::len).unwrap_or(0) + 1;
        let source_line = source.lines().nth(line - 1).unwrap_or("");
        Self::Invalid(format!(
            "{message}\n  --> {line}:{column}\n   |\n{line:>3} | {source_line}\n   | {}^",
            " ".repeat(column.saturating_sub(1))
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_uses_built_in_bindings_but_file_does_not() {
        assert!(!Config::default().bindings.is_empty());
        assert!(Config::from_kdl("").unwrap().bindings.is_empty());
    }

    #[test]
    fn parses_sections_actions_and_physical_bindings() {
        let config = Config::from_kdl(
            r#"
                general { gap 12 }
                input { repeat-delay 400; repeat-rate 30 }
                camera { keep-visible margin=40 }
                bind "Super+Return" { spawn "kitty" }
                bind "Super+Right" repeat=#true { pan-camera 10 0 }
                bind "Super+code:0x7b" { close-window }
                bind "Super+1" { focus-workspace 1 }
                bind "Super+2" { focus-workspace 2 output="DP-1" }
                bind "Super+3" { spawn "sh" "-c" "echo hello" }
            "#,
        )
        .unwrap();
        assert_eq!(config.bindings.len(), 6);
        assert_eq!(config.gap, 12);
        assert_eq!(config.key_repeat.delay_ms, 400);
    }

    #[test]
    fn formatter_preserves_comments_and_generated_config_is_valid() {
        let source = "// keep me\ngeneral { gap 8 }\n";
        let formatted = Config::format_kdl(source).unwrap();
        assert!(formatted.contains("// keep me"));
        let generated_source = Config::generated_kdl();
        assert_eq!(
            Config::format_kdl(&generated_source).unwrap(),
            generated_source
        );
        let generated = Config::from_kdl(&generated_source).unwrap();
        assert_eq!(generated.bindings.len(), Config::default().bindings.len());
    }

    #[test]
    fn rejects_duplicates_unsafe_repeat_and_unknown_names() {
        assert!(
            Config::from_kdl(
                r#"bind "Super+Q" { close-window }
                   bind "super+q" { close-window }"#
            )
            .is_err()
        );
        assert!(Config::from_kdl(r#"bind "Super+Return" repeat=#true { spawn "kitty" }"#).is_err());
        assert!(Config::from_kdl(r#"bind "Super+1" { focus-workpace 1 }"#).is_err());
        let error = Config::from_kdl(r#"bind "Super+1" { focus-workpace 1 }"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("did you mean `focus-workspace`"));
        assert!(error.contains("--> 1:1"));
        assert!(
            Config::from_kdl(r#"bind "Super+1" { focus-workspace 1 activate=#true }"#).is_err()
        );
        assert!(
            Config::from_kdl(r#"bind "Super+1" { focus-workspace id=7 output="DP-1" }"#).is_err()
        );
    }
}
