//! Desktop compositor selection and GNOME extension readiness detection.

use std::{env, path::PathBuf, process::Command};

use chathead_core::{DesktopIntegrationKind, DesktopIntegrationSnapshot, DesktopIntegrationStatus};

pub(crate) const EXTENSION_UUID: &str = "chathead-ai@io.github.chathead-ai";
pub(crate) const READINESS_BUS_NAME: &str = "io.github.chathead_ai.ChatHead.GnomePresentation";

#[must_use]
pub(crate) fn detect() -> DesktopIntegrationSnapshot {
    let session_type = env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let detected = detect_for(
        &session_type,
        &desktop,
        command_output,
        extension_files_installed(),
    );
    if detected.kind == DesktopIntegrationKind::LayerShell && !gtk4_layer_shell::is_supported() {
        unsupported("The active compositor does not provide a supported overlay integration.")
    } else {
        detected
    }
}

fn extension_files_installed() -> bool {
    let Some(home) = env::var_os("HOME") else {
        return false;
    };
    PathBuf::from(home)
        .join(".local/share/gnome-shell/extensions")
        .join(EXTENSION_UUID)
        .join("metadata.json")
        .is_file()
}

fn command_output(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn detect_for(
    session_type: &str,
    desktop: &str,
    run: impl Fn(&str, &[&str]) -> Option<String>,
    extension_files_installed: bool,
) -> DesktopIntegrationSnapshot {
    if !session_type.eq_ignore_ascii_case("wayland") {
        return unsupported("ChatHead overlays require a Wayland session.");
    }

    if desktop
        .split(':')
        .any(|component| component.eq_ignore_ascii_case("gnome"))
    {
        let version =
            run("gnome-shell", &["--version"]).and_then(|output| parse_gnome_version(&output));
        if version.as_deref() != Some("46") {
            return DesktopIntegrationSnapshot {
                kind: DesktopIntegrationKind::GnomeShell,
                status: DesktopIntegrationStatus::Incompatible,
                gnome_version: version,
                message: Some("ChatHead currently supports GNOME Shell 46 only.".to_owned()),
            };
        }

        let owner = run(
            "gdbus",
            &[
                "call",
                "--session",
                "--dest",
                "org.freedesktop.DBus",
                "--object-path",
                "/org/freedesktop/DBus",
                "--method",
                "org.freedesktop.DBus.NameHasOwner",
                READINESS_BUS_NAME,
            ],
        )
        .is_some_and(|output| output.contains("true"));
        let compatible = owner
            && run(
                "gdbus",
                &[
                    "call",
                    "--session",
                    "--dest",
                    READINESS_BUS_NAME,
                    "--object-path",
                    "/io/github/chathead_ai/ChatHead/GnomePresentation",
                    "--method",
                    "io.github.chathead_ai.ChatHead.GnomePresentation1.GetReadiness",
                ],
            )
            .is_some_and(|output| readiness_is_compatible(&output));
        if compatible {
            return DesktopIntegrationSnapshot {
                kind: DesktopIntegrationKind::GnomeShell,
                status: DesktopIntegrationStatus::Ready,
                gnome_version: version,
                message: None,
            };
        }

        let installed = extension_files_installed
            || run("gnome-extensions", &["info", EXTENSION_UUID]).is_some();
        return DesktopIntegrationSnapshot {
            kind: DesktopIntegrationKind::GnomeShell,
            status: if installed {
                DesktopIntegrationStatus::Disabled
            } else {
                DesktopIntegrationStatus::NotInstalled
            },
            gnome_version: version,
            message: Some(if installed {
                "Enable the ChatHead GNOME extension, then log out and back in if GNOME cannot activate it live."
            } else {
                "Install the bundled ChatHead GNOME extension to launch the overlay."
            }
            .to_owned()),
        };
    }

    DesktopIntegrationSnapshot::layer_shell_ready()
}

fn unsupported(message: &str) -> DesktopIntegrationSnapshot {
    DesktopIntegrationSnapshot {
        kind: DesktopIntegrationKind::Unsupported,
        status: DesktopIntegrationStatus::Unavailable,
        gnome_version: None,
        message: Some(message.to_owned()),
    }
}

fn parse_gnome_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|component| {
            component
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_digit())
        })
        .and_then(|component| component.split('.').next())
        .filter(|major| !major.is_empty())
        .map(str::to_owned)
}

fn readiness_is_compatible(output: &str) -> bool {
    let Some(start) = output.find('{') else {
        return false;
    };
    let Some(end) = output.rfind('}') else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&output[start..=end])
        .ok()
        .is_some_and(|value| {
            value["protocolVersion"] == 1
                && value["gnomeVersion"] == "46"
                && value["capabilities"]
                    .as_array()
                    .is_some_and(|capabilities| {
                        [
                            "topChrome",
                            "overviewHiding",
                            "lockHiding",
                            "structuredMarkdown",
                        ]
                        .iter()
                        .all(|required| capabilities.iter().any(|value| value == required))
                    })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_46_requires_the_companion_bus_owner() {
        let snapshot = detect_for(
            "wayland",
            "ubuntu:GNOME",
            |program, arguments| match (program, arguments.first().copied()) {
                ("gnome-shell", _) => Some("GNOME Shell 46.0".to_owned()),
                ("gnome-extensions", _) => Some("State: ENABLED".to_owned()),
                _ => None,
            },
            false,
        );
        assert_eq!(snapshot.status, DesktopIntegrationStatus::Disabled);
    }

    #[test]
    fn unsupported_gnome_version_is_not_advertised() {
        let snapshot = detect_for(
            "wayland",
            "GNOME",
            |program, _| (program == "gnome-shell").then(|| "GNOME Shell 47.2".to_owned()),
            false,
        );
        assert_eq!(snapshot.status, DesktopIntegrationStatus::Incompatible);
        assert_eq!(snapshot.gnome_version.as_deref(), Some("47"));
    }

    #[test]
    fn non_gnome_wayland_keeps_layer_shell_backend() {
        let snapshot = detect_for("wayland", "Hyprland", |_, _| None, false);
        assert_eq!(snapshot, DesktopIntegrationSnapshot::layer_shell_ready());
    }

    #[test]
    fn readiness_requires_matching_protocol_and_capabilities() {
        assert!(readiness_is_compatible(
            r#"('{"protocolVersion":1,"gnomeVersion":"46","capabilities":["topChrome","overviewHiding","lockHiding","structuredMarkdown"]}',)"#
        ));
        assert!(!readiness_is_compatible(
            r#"('{"protocolVersion":2,"gnomeVersion":"46","capabilities":[]}',)"#
        ));
    }

    #[test]
    fn freshly_installed_extension_is_pending_even_before_shell_discovers_it() {
        let snapshot = detect_for(
            "wayland",
            "GNOME",
            |program, _| (program == "gnome-shell").then(|| "GNOME Shell 46.0".to_owned()),
            true,
        );
        assert_eq!(snapshot.status, DesktopIntegrationStatus::Disabled);
        assert!(
            snapshot
                .message
                .as_deref()
                .is_some_and(|message| message.contains("log out"))
        );
    }
}
