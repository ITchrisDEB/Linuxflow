//! Paste injection (Ctrl+V, Ctrl+Shift+V, Shift+Insert): pure planning, then execution
//! by a backend chain chosen at run time.

pub mod backend;
pub mod chain;
pub mod env;
pub mod hyprland;
pub mod modifiers;
pub mod plan;
pub mod portal;
pub mod uinput;
pub mod vkbd;
pub mod xtest;

pub use backend::{Delivery, InjectError, KeyInjector};
pub use chain::PasteChain;
pub use env::{inject_chain, InjectBackendKind, SessionEnv};
pub use modifiers::{ModifierState, ReleasePolicy, Sleeper, ThreadSleeper};
pub use plan::{Key, KeyEvent, PastePlan};

use std::path::PathBuf;
use std::sync::Arc;

use portal::{PortalConfig, PortalInjector, PortalSession};

/// Settings of the injection chain, provided by the caller.
#[derive(Debug, Clone, Default)]
pub struct InjectConfig {
    /// Data folder where to store the portal token (0600). `None`: no persistence.
    pub data_dir: Option<PathBuf>,
    /// Allows `/dev/uinput` as a last resort (disabled by default).
    pub enable_uinput: bool,
    /// evdev code to use for the letter V with uinput (non-QWERTY layout).
    pub uinput_v_code: Option<u16>,
    pub release_policy: ReleasePolicy,
}

/// Builds the chain suited to the detected session. Nothing is opened here: the connections
/// (X11, Wayland, portal and its consent) are established at the first paste.
///
/// `session`: portal session already created by the caller if it shares it with the clipboard;
/// otherwise a private session (without clipboard) is created.
pub fn build_paster(
    env: &SessionEnv,
    config: &InjectConfig,
    modifiers: Option<Arc<dyn ModifierState>>,
    session: Option<Arc<PortalSession>>,
) -> Result<PasteChain, InjectError> {
    let mut backends: Vec<Box<dyn KeyInjector>> = Vec::new();
    let mut session = session;
    for kind in inject_chain(env, config.enable_uinput) {
        match kind {
            InjectBackendKind::XTest => backends.push(Box::new(xtest::XTestInjector::new())),
            InjectBackendKind::VirtualKeyboard => {
                backends.push(Box::new(vkbd::VirtualKeyboardInjector::new()))
            }
            InjectBackendKind::HyprlandShortcut => {
                backends.push(Box::new(hyprland::HyprlandInjector::new(env.hyprland)))
            }
            InjectBackendKind::Portal => {
                if session.is_none() {
                    session = Some(PortalSession::new(PortalConfig {
                        data_dir: config.data_dir.clone(),
                        ..PortalConfig::default()
                    })?);
                }
                if let Some(s) = session.as_ref() {
                    backends.push(Box::new(PortalInjector::new(s.clone())));
                }
            }
            InjectBackendKind::Uinput => {
                backends.push(Box::new(uinput::UinputInjector::new(config.uinput_v_code)))
            }
        }
    }
    let mut chain = PasteChain::new(backends).with_policy(config.release_policy);
    if let Some(m) = modifiers {
        chain = chain.with_modifier_state(m);
    }
    Ok(chain)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn build_paster_follows_the_detected_chain_without_connecting() {
        let env = SessionEnv::from_map(
            &[("XDG_SESSION_TYPE", "x11"), ("DISPLAY", ":0")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        let chain = build_paster(&env, &InjectConfig::default(), None, None).unwrap();
        assert_eq!(chain.backend_names(), vec!["xtest"]);

        let env = SessionEnv::from_map(
            &[
                ("XDG_SESSION_TYPE", "wayland"),
                ("HYPRLAND_INSTANCE_SIGNATURE", "x"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        );
        let cfg = InjectConfig {
            enable_uinput: true,
            ..InjectConfig::default()
        };
        let chain = build_paster(&env, &cfg, None, None).unwrap();
        assert_eq!(
            chain.backend_names(),
            vec![
                "hyprland-sendshortcut",
                "wayland-virtual-keyboard",
                "portal-remote-desktop",
                "uinput"
            ]
        );
    }
}
