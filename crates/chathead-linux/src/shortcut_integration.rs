//! User-owned Hyprland shortcut detection, persistence, and reversible config generation.

use std::{
    env, fs,
    io::{self, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

use chathead_core::{
    ShortcutAction, ShortcutActionsSnapshot, ShortcutBackend, ShortcutBinding,
    ShortcutCaptureSnapshot, ShortcutConfigFormat, ShortcutConflict, ShortcutIntegrationSnapshot,
    ShortcutState,
};
use gtk::{gdk, glib, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{cell::RefCell, collections::HashSet, rc::Rc};

const LUA_MARKER: &str = "-- chathead-ai managed shortcuts";
const LEGACY_MARKER: &str = "# chathead-ai managed shortcuts";

#[derive(Clone, Debug)]
pub(crate) struct DetectedIntegration {
    pub(crate) snapshot: ShortcutIntegrationSnapshot,
    root_config: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShortcutStore {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    toggle_panel: Option<StoredBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    voice_input: Option<StoredBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredBinding {
    binding: ShortcutBinding,
    replace_existing: bool,
    #[serde(default)]
    session_only: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ShortcutManager {
    integration: DetectedIntegration,
    store_path: PathBuf,
    store: ShortcutStore,
    states: ShortcutActionsSnapshot,
    pending: Option<(ShortcutAction, ShortcutBinding)>,
    capturing: Option<ShortcutAction>,
    pressed_keys: Vec<String>,
}

impl ShortcutManager {
    pub(crate) fn load() -> Self {
        let integration = detect();
        let store_path = config_home().join("chathead-ai/shortcuts.json");
        let store = load_store(&store_path).unwrap_or_default();
        let backend = integration.snapshot.backend;
        let ready = |stored: &Option<StoredBinding>| {
            match (stored, backend) {
            (Some(stored), _) if stored.session_only => ShortcutState::Error {
                message: "This shortcut was session-only because the Hyprland configuration is declarative. Apply the generated Home Manager/Nix snippet, then use Repair integration.".to_owned(),
                recoverable: true,
            },
            (Some(stored), Some(backend)) => ShortcutState::Ready {
                binding: stored.binding.clone(),
                backend,
            },
            (Some(_), None) => ShortcutState::Error {
                message: integration.snapshot.message.clone().unwrap_or_else(|| {
                    "The active Hyprland session could not be verified.".to_owned()
                }),
                recoverable: true,
            },
            (None, _) => ShortcutState::Unconfigured,
        }
        };
        let states = ShortcutActionsSnapshot {
            toggle_panel: ready(&store.toggle_panel),
            voice_input: ready(&store.voice_input),
        };
        Self {
            integration,
            store_path,
            store,
            states,
            pending: None,
            capturing: None,
            pressed_keys: Vec::new(),
        }
    }

    pub(crate) fn integration(&self) -> ShortcutIntegrationSnapshot {
        self.integration.snapshot.clone()
    }

    pub(crate) fn states(&self) -> ShortcutActionsSnapshot {
        self.states.clone()
    }

    pub(crate) fn capture(&self) -> Option<ShortcutCaptureSnapshot> {
        self.capturing.map(|action| ShortcutCaptureSnapshot {
            action,
            pressed_keys: self.pressed_keys.clone(),
        })
    }

    pub(crate) fn update_capture_keys(
        &mut self,
        action: ShortcutAction,
        pressed_keys: Vec<String>,
    ) {
        if self.capturing == Some(action) {
            self.pressed_keys = pressed_keys;
        }
    }

    pub(crate) fn configured_actions(&self) -> Vec<ShortcutAction> {
        let mut actions = Vec::new();
        if matches!(self.states.toggle_panel, ShortcutState::Ready { .. }) {
            actions.push(ShortcutAction::TogglePanel);
        }
        if matches!(self.states.voice_input, ShortcutState::Ready { .. }) {
            actions.push(ShortcutAction::VoiceInput);
        }
        actions
    }

    pub(crate) fn begin_capture(&mut self, action: ShortcutAction) -> Result<(), String> {
        if let Some(active) = self.capturing {
            return Err(format!(
                "A shortcut capture is already active for {active:?}."
            ));
        }
        if !self.integration.snapshot.supported {
            return Err(self
                .integration
                .snapshot
                .message
                .clone()
                .unwrap_or_else(|| {
                    "Global shortcuts are unavailable in this session.".to_owned()
                }));
        }
        self.pending = None;
        self.capturing = Some(action);
        self.pressed_keys.clear();
        *state_mut(&mut self.states, action) = ShortcutState::Capturing;
        Ok(())
    }

    pub(crate) fn cancel_capture(&mut self, action: ShortcutAction) {
        let pending_action = self
            .pending
            .as_ref()
            .map(|(pending_action, _)| *pending_action);
        if self.capturing == Some(action) || pending_action == Some(action) {
            self.capturing = None;
            self.pending = None;
            self.pressed_keys.clear();
            self.restore_action_state(action);
        }
    }

    pub(crate) fn captured(
        &mut self,
        action: ShortcutAction,
        binding: ShortcutBinding,
    ) -> Result<(), String> {
        if self.capturing != Some(action) {
            return Err("The capture session is no longer active.".to_owned());
        }
        validate_binding(&binding)?;
        let other = match action {
            ShortcutAction::TogglePanel => &self.store.voice_input,
            ShortcutAction::VoiceInput => &self.store.toggle_panel,
        };
        if other
            .as_ref()
            .is_some_and(|stored| bindings_equal(&stored.binding, &binding))
        {
            return Err(
                "That binding is already assigned to the other ChatHead action.".to_owned(),
            );
        }
        self.capturing = None;
        self.pressed_keys.clear();
        let mut conflicts = discover_conflicts(&binding)?;
        if binding.modifiers.is_empty() {
            conflicts.insert(
                0,
                ShortcutConflict {
                    description:
                        "This is an unmodified key. Normal typing may be intercepted system-wide."
                            .to_owned(),
                    dispatcher: "high-risk confirmation".to_owned(),
                    argument: binding.key.clone(),
                    submap: String::new(),
                },
            );
        }
        if conflicts.is_empty() {
            *state_mut(&mut self.states, action) = ShortcutState::Applying {
                candidate: binding.clone(),
            };
            self.apply(action, binding, false)
        } else {
            self.pending = Some((action, binding.clone()));
            *state_mut(&mut self.states, action) = ShortcutState::Conflict {
                candidate: binding,
                conflicts,
            };
            Ok(())
        }
    }

    pub(crate) fn confirm_replacement(&mut self, action: ShortcutAction) -> Result<(), String> {
        let Some((pending_action, binding)) = self.pending.take() else {
            return Err("There is no shortcut conflict awaiting confirmation.".to_owned());
        };
        if pending_action != action {
            self.pending = Some((pending_action, binding));
            return Err(
                "The replacement confirmation does not match the pending action.".to_owned(),
            );
        }
        *state_mut(&mut self.states, action) = ShortcutState::Applying {
            candidate: binding.clone(),
        };
        self.apply(action, binding, true)
    }

    pub(crate) fn clear(&mut self, action: ShortcutAction) -> Result<(), String> {
        let previous_store = self.store.clone();
        *stored_mut(&mut self.store, action) = None;
        if let Err(error) = self.commit_configuration() {
            self.store = previous_store;
            *state_mut(&mut self.states, action) = ShortcutState::Error {
                message: error.clone(),
                recoverable: true,
            };
            return Err(error);
        }
        *state_mut(&mut self.states, action) = ShortcutState::Unconfigured;
        Ok(())
    }

    pub(crate) fn repair(&mut self) -> Result<(), String> {
        self.commit_configuration()?;
        for action in [ShortcutAction::TogglePanel, ShortcutAction::VoiceInput] {
            self.restore_action_state(action);
        }
        Ok(())
    }

    pub(crate) fn audit_effective_bindings(&mut self) {
        let binds = run_hyprctl(&["-j", "binds"])
            .ok()
            .and_then(|output| serde_json::from_str::<Vec<Value>>(&output).ok());
        for action in [ShortcutAction::TogglePanel, ShortcutAction::VoiceInput] {
            let Some(stored) = stored(&self.store, action) else {
                continue;
            };
            let effective = binds.as_ref().is_some_and(|values| {
                let mask = modifier_mask(&stored.binding.modifiers);
                values.iter().any(|item| {
                    item["modmask"].as_u64() == Some(mask)
                        && item["key"]
                            .as_str()
                            .is_some_and(|key| key.eq_ignore_ascii_case(&stored.binding.key))
                        && item["description"]
                            .as_str()
                            .is_some_and(|description| description.starts_with("ChatHead AI:"))
                })
            });
            if effective {
                self.restore_action_state(action);
            } else {
                *state_mut(&mut self.states, action) = ShortcutState::Error {
                    message: format!(
                        "Configuration drift detected for {}. Use Repair integration to restore the approved binding.",
                        stored.binding.display
                    ),
                    recoverable: true,
                };
            }
        }
    }

    fn apply(
        &mut self,
        action: ShortcutAction,
        binding: ShortcutBinding,
        replace_existing: bool,
    ) -> Result<(), String> {
        let previous_store = self.store.clone();
        let session_only = self.integration.root_config.as_ref().is_some_and(|root| {
            root.starts_with("/nix/store") || is_read_only(root).unwrap_or(true)
        });
        *stored_mut(&mut self.store, action) = Some(StoredBinding {
            binding: binding.clone(),
            replace_existing,
            session_only,
        });
        if let Err(error) = self.commit_configuration() {
            self.store = previous_store;
            *state_mut(&mut self.states, action) = ShortcutState::Error {
                message: error.clone(),
                recoverable: true,
            };
            return Err(error);
        }
        *state_mut(&mut self.states, action) = ShortcutState::Ready {
            binding,
            backend: self
                .integration
                .snapshot
                .backend
                .ok_or_else(|| "No shortcut backend is active.".to_owned())?,
        };
        if session_only {
            self.integration.snapshot.message = Some(declarative_snippet(
                &self.store,
                self.integration.snapshot.config_format,
            ));
        }
        Ok(())
    }

    fn restore_action_state(&mut self, action: ShortcutAction) {
        let stored = stored(&self.store, action);
        *state_mut(&mut self.states, action) = match (stored, self.integration.snapshot.backend) {
            (Some(stored), Some(backend)) => ShortcutState::Ready {
                binding: stored.binding.clone(),
                backend,
            },
            _ => ShortcutState::Unconfigured,
        };
    }

    fn commit_configuration(&self) -> Result<(), String> {
        let root = self.integration.root_config.as_ref().ok_or_else(|| {
            "The active Hyprland root configuration could not be located.".to_owned()
        })?;
        if root.starts_with("/nix/store") || is_read_only(root)? {
            return self.commit_session_only();
        }

        let format = self
            .integration
            .snapshot
            .config_format
            .ok_or_else(|| "The Hyprland configuration format is unknown.".to_owned())?;
        let fragment = fragment_path(format);
        let original_root = fs::read(root)
            .map_err(|error| format!("Could not read {}: {error}", root.display()))?;
        let original_fragment = fs::read(&fragment).ok();
        let original_store = fs::read(&self.store_path).ok();
        let root_metadata = fs::metadata(root).map_err(|error| error.to_string())?;
        let fragment_mode = fs::metadata(&fragment)
            .map(|metadata| metadata.permissions().mode())
            .unwrap_or(0o600);
        let store_mode = fs::metadata(&self.store_path)
            .map(|metadata| metadata.permissions().mode())
            .unwrap_or(0o600);
        let root_written = std::cell::Cell::new(false);

        let result = (|| {
            if self.store.toggle_panel.is_none() && self.store.voice_input.is_none() {
                let next_root = remove_include(&original_root, format)?;
                ensure_unchanged(root, &original_root)?;
                atomic_write(root, &next_root, Some(root_metadata.permissions().mode()))?;
                root_written.set(true);
                if fragment.exists() {
                    fs::remove_file(&fragment).map_err(|error| {
                        format!("Could not remove {}: {error}", fragment.display())
                    })?;
                }
            } else {
                let next_root = insert_include(&original_root, &fragment, format)?;
                let generated =
                    generate_fragment(&self.store, format, self.integration.snapshot.backend)?;
                if let Some(parent) = fragment.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                atomic_write(&fragment, generated.as_bytes(), Some(fragment_mode))?;
                ensure_unchanged(root, &original_root)?;
                atomic_write(root, &next_root, Some(root_metadata.permissions().mode()))?;
                root_written.set(true);
            }
            reload_and_verify(&self.store)?;
            let json = serde_json::to_vec_pretty(&self.store).map_err(|error| error.to_string())?;
            if let Some(parent) = self.store_path.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            atomic_write(&self.store_path, &json, Some(store_mode))
        })();

        if let Err(error) = result {
            if root_written.get() {
                let _ = atomic_write(
                    root,
                    &original_root,
                    Some(root_metadata.permissions().mode()),
                );
            }
            match original_fragment {
                Some(bytes) => {
                    let _ = atomic_write(&fragment, &bytes, Some(fragment_mode));
                }
                None => {
                    let _ = fs::remove_file(&fragment);
                }
            }
            match original_store {
                Some(bytes) => {
                    let _ = atomic_write(&self.store_path, &bytes, Some(store_mode));
                }
                None => {
                    let _ = fs::remove_file(&self.store_path);
                }
            }
            let _ = run_hyprctl(&["reload"]);
            return Err(error);
        }
        Ok(())
    }

    fn commit_session_only(&self) -> Result<(), String> {
        let format = self
            .integration
            .snapshot
            .config_format
            .ok_or_else(|| "The Hyprland configuration format is unknown.".to_owned())?;
        let generated = generate_fragment(&self.store, format, self.integration.snapshot.backend)?;
        let result = (|| {
            run_hyprctl(&["reload"])?;
            for line in generated.lines().filter(|line| {
                !line.is_empty() && !line.starts_with('#') && !line.starts_with("--")
            }) {
                match format {
                    ShortcutConfigFormat::Lua => {
                        run_hyprctl(&["eval", line])?;
                    }
                    ShortcutConfigFormat::LegacyHyprlang => {
                        let (keyword, value) = line.split_once('=').ok_or_else(|| {
                            "Generated legacy shortcut syntax is invalid.".to_owned()
                        })?;
                        run_hyprctl(&["keyword", keyword.trim(), value.trim()])?;
                    }
                }
            }
            verify_effective(&self.store)?;
            let json = serde_json::to_vec_pretty(&self.store).map_err(|error| error.to_string())?;
            if let Some(parent) = self.store_path.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let mode = fs::metadata(&self.store_path)
                .map(|metadata| metadata.permissions().mode())
                .unwrap_or(0o600);
            atomic_write(&self.store_path, &json, Some(mode))
        })();
        if result.is_err() {
            let _ = run_hyprctl(&["reload"]);
        }
        result
    }
}

pub(crate) fn show_capture_surface<F, C, U>(
    app: &gtk::Application,
    action: ShortcutAction,
    on_complete: F,
    on_cancel: C,
    on_update: U,
) where
    F: Fn(ShortcutBinding) + 'static,
    C: Fn() + 'static,
    U: Fn(Vec<String>) + 'static,
{
    #[derive(Default)]
    struct Capture {
        pressed: HashSet<u32>,
        modifiers: Vec<String>,
        key: Option<String>,
        escape_only: bool,
    }

    let on_complete: Rc<dyn Fn(ShortcutBinding)> = Rc::new(on_complete);
    let on_cancel: Rc<dyn Fn()> = Rc::new(on_cancel);
    let on_update: Rc<dyn Fn(Vec<String>)> = Rc::new(on_update);
    let label = gtk::Label::builder()
        .label("Listening…\nPress a shortcut, then release every key.")
        .wrap(true)
        .justify(gtk::Justification::Center)
        .margin_top(28)
        .margin_bottom(28)
        .margin_start(32)
        .margin_end(32)
        .build();
    let window = gtk::Window::builder()
        .application(app)
        .title(match action {
            ShortcutAction::TogglePanel => "Set Toggle Chat Panel Shortcut",
            ShortcutAction::VoiceInput => "Set Voice Input Shortcut",
        })
        .modal(true)
        .default_width(420)
        .default_height(150)
        .child(&label)
        .build();
    let state = Rc::new(RefCell::new(Capture::default()));
    let completed = Rc::new(std::cell::Cell::new(false));
    let controller = gtk::EventControllerKey::new();
    {
        let state = state.clone();
        let label = label.clone();
        let on_update = on_update.clone();
        controller.connect_key_pressed(move |_, key, keycode, modifiers| {
            let mut state = state.borrow_mut();
            if !state.pressed.insert(keycode) {
                return glib::Propagation::Stop;
            }
            let name = key.name().map(|name| name.to_string()).unwrap_or_default();
            if let Some(modifier) = normalized_modifier(&name) {
                if !state.modifiers.iter().any(|value| value == modifier) {
                    state.modifiers.push(modifier.to_owned());
                }
            } else {
                state.key = Some(normalize_key_name(&name));
            }
            state.escape_only =
                name == "Escape" && modifiers.is_empty() && state.modifiers.is_empty();
            let display = capture_display(&state.modifiers, state.key.as_deref());
            let pressed_keys = state
                .modifiers
                .iter()
                .cloned()
                .chain(state.key.clone())
                .collect();
            on_update(pressed_keys);
            label.set_label(&format!(
                "Listening…\n{display}\nRelease every key to finish."
            ));
            glib::Propagation::Stop
        });
    }
    {
        let state = state.clone();
        let window = window.clone();
        let completed = completed.clone();
        let on_complete = on_complete.clone();
        let on_cancel = on_cancel.clone();
        controller.connect_key_released(move |_, _, keycode, _| {
            let mut state = state.borrow_mut();
            state.pressed.remove(&keycode);
            if !state.pressed.is_empty() {
                return;
            }
            completed.set(true);
            if state.escape_only {
                on_cancel();
                window.close();
                return;
            }
            let Some(key) = state.key.take() else {
                label.set_label("Modifier-only shortcuts are invalid.\nPress another shortcut, or Escape to cancel.");
                state.modifiers.clear();
                completed.set(false);
                return;
            };
            let modifiers = canonical_modifier_order(std::mem::take(&mut state.modifiers));
            let display = capture_display(&modifiers, Some(&key));
            on_complete(ShortcutBinding { modifiers, key, display });
            window.close();
        });
    }
    window.add_controller(controller);
    window.connect_map(|window| {
        if let Some(surface) = window.surface()
            && let Ok(toplevel) = surface.downcast::<gdk::Toplevel>()
        {
            toplevel.inhibit_system_shortcuts(gdk::Event::NONE);
        }
    });
    {
        let completed = completed.clone();
        let on_cancel = on_cancel.clone();
        window.connect_close_request(move |window| {
            if let Some(surface) = window.surface()
                && let Ok(toplevel) = surface.downcast::<gdk::Toplevel>()
            {
                toplevel.restore_system_shortcuts();
            }
            if !completed.get() {
                on_cancel();
            }
            glib::Propagation::Proceed
        });
    }
    window.present();
}

fn normalized_modifier(name: &str) -> Option<&'static str> {
    match name {
        "Super_L" | "Super_R" | "Meta_L" | "Meta_R" => Some("SUPER"),
        "Control_L" | "Control_R" => Some("CTRL"),
        "Alt_L" | "Alt_R" => Some("ALT"),
        "Shift_L" | "Shift_R" => Some("SHIFT"),
        _ => None,
    }
}

fn normalize_key_name(name: &str) -> String {
    if name.len() == 1 {
        name.to_uppercase()
    } else {
        name.to_owned()
    }
}

fn canonical_modifier_order(mut modifiers: Vec<String>) -> Vec<String> {
    const ORDER: [&str; 4] = ["SUPER", "CTRL", "ALT", "SHIFT"];
    modifiers.sort_by_key(|modifier| {
        ORDER
            .iter()
            .position(|value| value == modifier)
            .unwrap_or(ORDER.len())
    });
    modifiers.dedup();
    modifiers
}

fn capture_display(modifiers: &[String], key: Option<&str>) -> String {
    modifiers
        .iter()
        .map(|value| match value.as_str() {
            "SUPER" => "Super",
            "CTRL" => "Ctrl",
            "ALT" => "Alt",
            "SHIFT" => "Shift",
            value => value,
        })
        .chain(key)
        .collect::<Vec<_>>()
        .join(" + ")
}

pub(crate) fn detect() -> DetectedIntegration {
    let unsupported = |message: String| DetectedIntegration {
        snapshot: ShortcutIntegrationSnapshot {
            supported: false,
            hyprland_version: None,
            config_format: None,
            backend: None,
            message: Some(message),
        },
        root_config: None,
    };
    if env::var("XDG_SESSION_TYPE").ok().as_deref() != Some("wayland") {
        return unsupported("Global shortcuts require an active Wayland session.".to_owned());
    }
    let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if !desktop
        .split(':')
        .any(|value| value.eq_ignore_ascii_case("hyprland"))
    {
        return unsupported("The active desktop session is not Hyprland.".to_owned());
    }
    let Ok(signature) = env::var("HYPRLAND_INSTANCE_SIGNATURE") else {
        return unsupported("Hyprland did not publish an instance signature.".to_owned());
    };
    if signature.chars().any(char::is_control) || signature.contains('/') {
        return unsupported("Hyprland published an invalid instance signature.".to_owned());
    }
    let runtime = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let Some(runtime) = runtime else {
        return unsupported("XDG_RUNTIME_DIR is unavailable.".to_owned());
    };
    let event_socket = runtime.join("hypr").join(&signature).join(".socket2.sock");
    if !event_socket.exists() {
        return unsupported(format!(
            "The active Hyprland IPC socket is missing: {}",
            event_socket.display()
        ));
    }
    let instances = run_hyprctl(&["-j", "instances"]);
    let Ok(instances) = instances else {
        return unsupported("hyprctl could not verify the active Hyprland instance.".to_owned());
    };
    let verified = serde_json::from_str::<Vec<Value>>(&instances)
        .ok()
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item["instance"].as_str() == Some(&signature))
        });
    if !verified {
        return unsupported("hyprctl did not report the active Hyprland instance.".to_owned());
    }
    let Ok(version_output) = run_hyprctl(&["version"]) else {
        return unsupported("hyprctl could not report the active Hyprland version.".to_owned());
    };
    let Some(version) = parse_version(&version_output) else {
        return unsupported("The active Hyprland version could not be parsed.".to_owned());
    };
    let format = if version_at_least(&version, 0, 55) {
        ShortcutConfigFormat::Lua
    } else {
        ShortcutConfigFormat::LegacyHyprlang
    };
    let portal_healthy = run_hyprctl(&["-j", "globalshortcuts"]).is_ok()
        && Command::new("busctl")
            .args([
                "--user",
                "--quiet",
                "status",
                "org.freedesktop.impl.portal.desktop.hyprland",
            ])
            .output()
            .is_ok_and(|output| output.status.success());
    let backend = if portal_healthy {
        ShortcutBackend::HyprlandPortal
    } else {
        ShortcutBackend::HyprlandEvent
    };
    let root_config = active_root_config(format, &instances, &signature);
    let message = root_config.is_none().then(|| {
        "Hyprland is active, but its root configuration path could not be located.".to_owned()
    });
    DetectedIntegration {
        snapshot: ShortcutIntegrationSnapshot {
            supported: root_config.is_some(),
            hyprland_version: Some(version),
            config_format: Some(format),
            backend: Some(backend),
            message,
        },
        root_config,
    }
}

fn active_root_config(
    format: ShortcutConfigFormat,
    instances: &str,
    signature: &str,
) -> Option<PathBuf> {
    let pid = serde_json::from_str::<Vec<Value>>(instances)
        .ok()?
        .into_iter()
        .find(|item| item["instance"].as_str() == Some(signature))?["pid"]
        .as_u64()?;
    let cmdline = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args: Vec<_> = cmdline
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    for (index, arg) in args.iter().enumerate() {
        if (arg == "--config" || arg == "-c") && args.get(index + 1).is_some() {
            return args.get(index + 1).map(PathBuf::from);
        }
        if let Some(path) = arg.strip_prefix("--config=") {
            return Some(PathBuf::from(path));
        }
    }
    Some(config_home().join(match format {
        ShortcutConfigFormat::Lua => "hypr/hyprland.lua",
        ShortcutConfigFormat::LegacyHyprlang => "hypr/hyprland.conf",
    }))
    .filter(|path| path.exists())
}

pub(crate) fn parse_version(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let tail = line.trim().strip_prefix("Hyprland ")?;
        let raw = tail.split_whitespace().next()?.trim_start_matches('v');
        let mut parts = raw.split('.');
        parts.next()?.parse::<u32>().ok()?;
        parts.next()?.parse::<u32>().ok()?;
        Some(raw.to_owned())
    })
}

fn version_at_least(version: &str, major: u32, minor: u32) -> bool {
    let mut parts = version
        .split('.')
        .filter_map(|part| part.parse::<u32>().ok());
    (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    ) >= (major, minor)
}

pub(crate) fn validate_binding(binding: &ShortcutBinding) -> Result<(), String> {
    if binding.key.is_empty() {
        return Err("Modifier-only shortcuts are not supported.".to_owned());
    }
    for value in binding
        .modifiers
        .iter()
        .chain([&binding.key, &binding.display])
    {
        if value.chars().any(char::is_control) || value.contains([',', '\n', '\r']) {
            return Err("Shortcut keys contain unsupported control characters.".to_owned());
        }
        if value.len() > 96 {
            return Err("Shortcut key names are too long.".to_owned());
        }
    }
    let allowed = ["SUPER", "CTRL", "ALT", "SHIFT"];
    if binding
        .modifiers
        .iter()
        .any(|modifier| !allowed.contains(&modifier.as_str()))
    {
        return Err("Shortcut modifiers were not normalized by ChatHead.".to_owned());
    }
    Ok(())
}

fn discover_conflicts(binding: &ShortcutBinding) -> Result<Vec<ShortcutConflict>, String> {
    let output = run_hyprctl(&["-j", "binds"])?;
    parse_conflicts(&output, binding)
}

fn parse_conflicts(
    output: &str,
    binding: &ShortcutBinding,
) -> Result<Vec<ShortcutConflict>, String> {
    let values: Vec<Value> = serde_json::from_str(output)
        .map_err(|error| format!("Invalid hyprctl binds response: {error}"))?;
    let wanted_mask = modifier_mask(&binding.modifiers);
    Ok(values
        .into_iter()
        .filter(|item| {
            item["modmask"].as_u64() == Some(wanted_mask)
                && item["key"]
                    .as_str()
                    .is_some_and(|key| key.eq_ignore_ascii_case(&binding.key))
                && !item["description"]
                    .as_str()
                    .is_some_and(|description| description.starts_with("ChatHead AI:"))
        })
        .map(|item| ShortcutConflict {
            description: item["description"]
                .as_str()
                .unwrap_or("Undescribed binding")
                .to_owned(),
            dispatcher: item["dispatcher"].as_str().unwrap_or_default().to_owned(),
            argument: item["arg"].as_str().unwrap_or_default().to_owned(),
            submap: item["submap"].as_str().unwrap_or_default().to_owned(),
        })
        .collect())
}

fn generate_fragment(
    store: &ShortcutStore,
    format: ShortcutConfigFormat,
    backend: Option<ShortcutBackend>,
) -> Result<String, String> {
    let backend = backend.ok_or_else(|| "No shortcut backend is active.".to_owned())?;
    let portal_names = (backend == ShortcutBackend::HyprlandPortal)
        .then(discover_global_shortcut_names)
        .flatten();
    let mut output = String::new();
    output.push_str(match format {
        ShortcutConfigFormat::Lua => "-- Generated by ChatHead AI. Changes will be replaced.\n",
        ShortcutConfigFormat::LegacyHyprlang => {
            "# Generated by ChatHead AI. Changes will be replaced.\n"
        }
    });
    for (action, stored) in [
        (ShortcutAction::TogglePanel, &store.toggle_panel),
        (ShortcutAction::VoiceInput, &store.voice_input),
    ] {
        let Some(stored) = stored else { continue };
        validate_binding(&stored.binding)?;
        let keys = key_expression(&stored.binding);
        let target = target_name(action, backend, portal_names.as_ref());
        match format {
            ShortcutConfigFormat::Lua => {
                if stored.replace_existing {
                    output.push_str(&format!("hl.unbind(\"{}\")\n", lua_escape(&keys)));
                }
                let dispatcher = match backend {
                    ShortcutBackend::HyprlandPortal => "global",
                    ShortcutBackend::HyprlandEvent => "event",
                };
                output.push_str(&format!(
                    "hl.bind(\"{}\", hl.dsp.{dispatcher}(\"{}\"), {{ description = \"ChatHead AI: {}\" }})\n",
                    lua_escape(&keys), lua_escape(&target), action_label(action)
                ));
                if action == ShortcutAction::VoiceInput {
                    output.push_str(&format!(
                        "hl.bind(\"{}\", hl.dsp.{}(\"{}\"), {{ release = true, description = \"ChatHead AI: voice release\" }})\n",
                        lua_escape(&keys), dispatcher, lua_escape(&release_target(backend, portal_names.as_ref()))
                    ));
                }
            }
            ShortcutConfigFormat::LegacyHyprlang => {
                let (mods, key) = legacy_parts(&stored.binding);
                if stored.replace_existing {
                    output.push_str(&format!("unbind = {mods}, {key}\n"));
                }
                let dispatcher = match backend {
                    ShortcutBackend::HyprlandPortal => "global",
                    ShortcutBackend::HyprlandEvent => "event",
                };
                output.push_str(&format!(
                    "bindd = {mods}, {key}, ChatHead AI: {}, {dispatcher}, {target}\n",
                    action_label(action)
                ));
                if action == ShortcutAction::VoiceInput {
                    output.push_str(&format!(
                        "bindrd = {mods}, {key}, ChatHead AI: voice release, {dispatcher}, {}\n",
                        release_target(backend, portal_names.as_ref())
                    ));
                }
            }
        }
    }
    Ok(output)
}

struct GlobalShortcutNames {
    panel: String,
    voice: String,
}

fn discover_global_shortcut_names() -> Option<GlobalShortcutNames> {
    let output = run_hyprctl(&["-j", "globalshortcuts"]).ok()?;
    let values: Vec<Value> = serde_json::from_str(&output).ok()?;
    let find = |id: &str| {
        values
            .iter()
            .filter_map(|item| item["name"].as_str())
            .find(|name| *name == id || name.ends_with(&format!(":{id}")))
            .map(str::to_owned)
    };
    Some(GlobalShortcutNames {
        panel: find("panel_toggle")?,
        voice: find("voice_toggle")?,
    })
}

fn target_name(
    action: ShortcutAction,
    backend: ShortcutBackend,
    portal_names: Option<&GlobalShortcutNames>,
) -> String {
    match (action, backend) {
        (ShortcutAction::TogglePanel, ShortcutBackend::HyprlandPortal) => portal_names
            .map(|names| names.panel.clone())
            .unwrap_or_else(|| ":panel_toggle".to_owned()),
        (ShortcutAction::VoiceInput, ShortcutBackend::HyprlandPortal) => portal_names
            .map(|names| names.voice.clone())
            .unwrap_or_else(|| ":voice_toggle".to_owned()),
        (ShortcutAction::TogglePanel, ShortcutBackend::HyprlandEvent) => {
            "chathead:toggle-panel".to_owned()
        }
        (ShortcutAction::VoiceInput, ShortcutBackend::HyprlandEvent) => {
            "chathead:voice-pressed".to_owned()
        }
    }
}

fn release_target(backend: ShortcutBackend, portal_names: Option<&GlobalShortcutNames>) -> String {
    match backend {
        ShortcutBackend::HyprlandPortal => portal_names
            .map(|names| names.voice.clone())
            .unwrap_or_else(|| ":voice_toggle".to_owned()),
        ShortcutBackend::HyprlandEvent => "chathead:voice-released".to_owned(),
    }
}

fn action_label(action: ShortcutAction) -> &'static str {
    match action {
        ShortcutAction::TogglePanel => "toggle chat panel",
        ShortcutAction::VoiceInput => "voice input",
    }
}

fn key_expression(binding: &ShortcutBinding) -> String {
    binding
        .modifiers
        .iter()
        .cloned()
        .chain([binding.key.clone()])
        .collect::<Vec<_>>()
        .join(" + ")
}

fn legacy_parts(binding: &ShortcutBinding) -> (String, String) {
    (binding.modifiers.join(" "), binding.key.clone())
}

fn modifier_mask(modifiers: &[String]) -> u64 {
    modifiers.iter().fold(0, |mask, modifier| {
        mask | match modifier.as_str() {
            "SHIFT" => 1,
            "CTRL" => 4,
            "ALT" => 8,
            "SUPER" => 64,
            _ => 0,
        }
    })
}

fn bindings_equal(left: &ShortcutBinding, right: &ShortcutBinding) -> bool {
    modifier_mask(&left.modifiers) == modifier_mask(&right.modifiers)
        && left.key.eq_ignore_ascii_case(&right.key)
}

fn insert_include(
    original: &[u8],
    fragment: &Path,
    format: ShortcutConfigFormat,
) -> Result<Vec<u8>, String> {
    let cleaned = remove_include(original, format)?;
    let mut text = String::from_utf8(cleaned)
        .map_err(|_| "Hyprland root configuration is not UTF-8.".to_owned())?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    match format {
        ShortcutConfigFormat::Lua => text.push_str(&format!(
            "{LUA_MARKER} begin\npcall(require, \"chathead-ai/binds\")\n{LUA_MARKER} end\n"
        )),
        ShortcutConfigFormat::LegacyHyprlang => text.push_str(&format!(
            "{LEGACY_MARKER} begin\nsource = {}\n{LEGACY_MARKER} end\n",
            fragment.display()
        )),
    }
    Ok(text.into_bytes())
}

fn remove_include(original: &[u8], format: ShortcutConfigFormat) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(original)
        .map_err(|_| "Hyprland root configuration is not UTF-8.".to_owned())?;
    let marker = match format {
        ShortcutConfigFormat::Lua => LUA_MARKER,
        ShortcutConfigFormat::LegacyHyprlang => LEGACY_MARKER,
    };
    let mut output = Vec::new();
    let mut managed = false;
    for line in text.lines() {
        if line == format!("{marker} begin") {
            managed = true;
            continue;
        }
        if line == format!("{marker} end") {
            managed = false;
            continue;
        }
        if !managed {
            output.push(line);
        }
    }
    let mut result = output.join("\n");
    if text.ends_with('\n') && !result.is_empty() {
        result.push('\n');
    }
    Ok(result.into_bytes())
}

fn reload_and_verify(store: &ShortcutStore) -> Result<(), String> {
    run_hyprctl(&["reload"])?;
    let errors = run_hyprctl(&["configerrors"]).unwrap_or_default();
    if !errors.trim().is_empty() && errors.trim() != "ok" {
        return Err(format!(
            "Hyprland rejected the generated shortcut configuration: {}",
            errors.trim()
        ));
    }
    verify_effective(store)
}

fn verify_effective(store: &ShortcutStore) -> Result<(), String> {
    let binds = run_hyprctl(&["-j", "binds"])?;
    let values: Vec<Value> = serde_json::from_str(&binds).map_err(|error| error.to_string())?;
    for stored in [&store.toggle_panel, &store.voice_input]
        .into_iter()
        .flatten()
    {
        let mask = modifier_mask(&stored.binding.modifiers);
        if !values.iter().any(|item| {
            item["modmask"].as_u64() == Some(mask)
                && item["key"]
                    .as_str()
                    .is_some_and(|key| key.eq_ignore_ascii_case(&stored.binding.key))
                && item["description"]
                    .as_str()
                    .is_some_and(|description| description.starts_with("ChatHead AI:"))
        }) {
            return Err(format!(
                "Hyprland did not activate {}.",
                stored.binding.display
            ));
        }
    }
    Ok(())
}

fn declarative_snippet(store: &ShortcutStore, format: Option<ShortcutConfigFormat>) -> String {
    let generated = format
        .and_then(|format| {
            generate_fragment(store, format, Some(ShortcutBackend::HyprlandEvent)).ok()
        })
        .unwrap_or_default();
    format!(
        "The active Hyprland configuration is read-only or declarative. Add this generated Home Manager/Nix snippet, then use Repair integration:\nwayland.windowManager.hyprland.extraConfig = ''\n{generated}'';"
    )
}

fn fragment_path(format: ShortcutConfigFormat) -> PathBuf {
    config_home().join(match format {
        ShortcutConfigFormat::Lua => "hypr/chathead-ai/binds.lua",
        ShortcutConfigFormat::LegacyHyprlang => "hypr/chathead-ai/binds.conf",
    })
}

fn config_home() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
}

fn load_store(path: &Path) -> Result<ShortcutStore, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid shortcut store: {error}")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ShortcutStore::default()),
        Err(error) => Err(format!("Could not read {}: {error}", path.display())),
    }
}

fn atomic_write(path: &Path, contents: &[u8], mode: Option<u32>) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Cannot write a path without a parent directory.".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = parent.join(format!(".chathead-ai-{}-{nonce}.tmp", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options
        .open(&temp)
        .map_err(|error| format!("Could not create {}: {error}", temp.display()))?;
    if let Some(mode) = mode {
        file.set_permissions(fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|error| error.to_string())?;
    }
    let result = file
        .write_all(contents)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|error| format!("Could not atomically write {}: {error}", path.display()))
}

fn ensure_unchanged(path: &Path, expected: &[u8]) -> Result<(), String> {
    let current =
        fs::read(path).map_err(|error| format!("Could not re-read {}: {error}", path.display()))?;
    if current != expected {
        return Err(format!(
            "{} changed while ChatHead was preparing the shortcut. No changes were applied.",
            path.display()
        ));
    }
    Ok(())
}

fn is_read_only(path: &Path) -> Result<bool, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    let uid = unsafe_uid();
    let writable = if metadata.uid() == uid {
        metadata.permissions().mode() & 0o200 != 0
    } else {
        metadata.permissions().mode() & 0o022 != 0
    };
    Ok(!writable)
}

fn unsafe_uid() -> u32 {
    // `/proc/self/status` avoids adding libc and does not require unsafe Rust.
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("Uid:"))
                .map(str::to_owned)
        })
        .and_then(|line| line.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(u32::MAX)
}

fn run_hyprctl(args: &[&str]) -> Result<String, String> {
    let output = Command::new("hyprctl")
        .args(args)
        .output()
        .map_err(|error| format!("Could not run hyprctl: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn stored(store: &ShortcutStore, action: ShortcutAction) -> &Option<StoredBinding> {
    match action {
        ShortcutAction::TogglePanel => &store.toggle_panel,
        ShortcutAction::VoiceInput => &store.voice_input,
    }
}

fn stored_mut(store: &mut ShortcutStore, action: ShortcutAction) -> &mut Option<StoredBinding> {
    match action {
        ShortcutAction::TogglePanel => &mut store.toggle_panel,
        ShortcutAction::VoiceInput => &mut store.voice_input,
    }
}

fn state_mut(states: &mut ShortcutActionsSnapshot, action: ShortcutAction) -> &mut ShortcutState {
    match action {
        ShortcutAction::TogglePanel => &mut states.toggle_panel,
        ShortcutAction::VoiceInput => &mut states.voice_input,
    }
}

fn lua_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> ShortcutBinding {
        ShortcutBinding {
            modifiers: vec!["SUPER".to_owned(), "SHIFT".to_owned()],
            key: "W".to_owned(),
            display: "Super + Shift + W".to_owned(),
        }
    }

    #[test]
    fn parses_release_and_tagged_versions() {
        assert_eq!(
            parse_version("Hyprland 0.54.3 built from branch"),
            Some("0.54.3".to_owned())
        );
        assert_eq!(
            parse_version("Hyprland v0.55.0\nDate: today"),
            Some("0.55.0".to_owned())
        );
    }

    #[test]
    fn binding_validation_rejects_injection_and_modifier_only_values() {
        assert!(validate_binding(&binding()).is_ok());
        let mut bad = binding();
        bad.key = "W\nbind =".to_owned();
        assert!(validate_binding(&bad).is_err());
        bad.key.clear();
        assert!(validate_binding(&bad).is_err());
    }

    #[test]
    fn normalizes_left_and_right_modifier_variants() {
        assert_eq!(normalized_modifier("Super_L"), Some("SUPER"));
        assert_eq!(normalized_modifier("Super_R"), Some("SUPER"));
        assert_eq!(normalized_modifier("Control_L"), Some("CTRL"));
        assert_eq!(normalized_modifier("Shift_R"), Some("SHIFT"));
        assert_eq!(normalized_modifier("A"), None);
    }

    #[test]
    fn parses_every_matching_hyprctl_conflict_and_ignores_other_keys() {
        let output = r#"[
          {"modmask":64,"key":"W","description":"Wallpaper","dispatcher":"exec","arg":"wallpaper.sh","submap":""},
          {"modmask":64,"key":"W","description":"Workspace","dispatcher":"workspace","arg":"2","submap":"resize"},
          {"modmask":64,"key":"Q","description":"Terminal","dispatcher":"exec","arg":"kitty","submap":""}
        ]"#;
        let binding = ShortcutBinding {
            modifiers: vec!["SUPER".to_owned()],
            key: "W".to_owned(),
            display: "Super + W".to_owned(),
        };
        let conflicts = parse_conflicts(output, &binding).unwrap();
        assert_eq!(conflicts.len(), 2);
        assert_eq!(conflicts[1].submap, "resize");
    }

    #[test]
    fn lua_fragment_uses_native_lua_bind_and_unbind_apis() {
        let store = ShortcutStore {
            toggle_panel: Some(StoredBinding {
                binding: binding(),
                replace_existing: true,
                session_only: false,
            }),
            voice_input: None,
        };
        let output = generate_fragment(
            &store,
            ShortcutConfigFormat::Lua,
            Some(ShortcutBackend::HyprlandPortal),
        )
        .unwrap();
        assert!(output.contains("hl.unbind(\"SUPER + SHIFT + W\")"));
        assert!(output.contains("hl.dsp.global(\":panel_toggle\")"));
        assert!(!output.contains("bind ="));
    }

    #[test]
    fn legacy_fragment_is_isolated_to_hyprlang() {
        let store = ShortcutStore {
            toggle_panel: Some(StoredBinding {
                binding: binding(),
                replace_existing: true,
                session_only: false,
            }),
            voice_input: None,
        };
        let output = generate_fragment(
            &store,
            ShortcutConfigFormat::LegacyHyprlang,
            Some(ShortcutBackend::HyprlandEvent),
        )
        .unwrap();
        assert!(output.contains("unbind = SUPER SHIFT, W"));
        assert!(output.contains("event, chathead:toggle-panel"));
    }

    #[test]
    fn include_markers_are_idempotent_and_removable() {
        let root = b"hl.config({})\n";
        let fragment = Path::new("/tmp/binds.lua");
        let once = insert_include(root, fragment, ShortcutConfigFormat::Lua).unwrap();
        let twice = insert_include(&once, fragment, ShortcutConfigFormat::Lua).unwrap();
        assert_eq!(once, twice);
        assert_eq!(
            remove_include(&once, ShortcutConfigFormat::Lua).unwrap(),
            root
        );
    }
}
