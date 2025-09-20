use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
};

use input_event::{Event, KeyboardEvent, PointerEvent};

pub use self::error::{EmulationCreationError, EmulationError, InputEmulationError};

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Back,
    Forward,
    // Add more actions as needed
}

impl Action {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "back" => Some(Action::Back),
            "forward" => Some(Action::Forward),
            _ => None,
        }
    }
}

#[cfg(windows)]
mod windows;

#[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
mod x11;

#[cfg(all(unix, feature = "wlroots", not(target_os = "macos")))]
mod wlroots;

#[cfg(all(unix, feature = "remote_desktop_portal", not(target_os = "macos")))]
mod xdg_desktop_portal;

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
mod libei;

#[cfg(target_os = "macos")]
mod macos;

/// fallback input emulation (logs events)
mod dummy;
mod error;

pub type EmulationHandle = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    #[cfg(all(unix, feature = "wlroots", not(target_os = "macos")))]
    Wlroots,
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    Libei,
    #[cfg(all(unix, feature = "remote_desktop_portal", not(target_os = "macos")))]
    Xdp,
    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    X11,
    #[cfg(windows)]
    Windows,
    #[cfg(target_os = "macos")]
    MacOs,
    Dummy,
}

impl Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(all(unix, feature = "wlroots", not(target_os = "macos")))]
            Backend::Wlroots => write!(f, "wlroots"),
            #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
            Backend::Libei => write!(f, "libei"),
            #[cfg(all(unix, feature = "remote_desktop_portal", not(target_os = "macos")))]
            Backend::Xdp => write!(f, "xdg-desktop-portal"),
            #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
            Backend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            Backend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            Backend::MacOs => write!(f, "macos"),
            Backend::Dummy => write!(f, "dummy"),
        }
    }
}

pub struct InputEmulation {
    emulation: Box<dyn Emulation>,
    handles: HashSet<EmulationHandle>,
    pressed_keys: HashMap<EmulationHandle, HashSet<u32>>,
    keybindings: HashMap<u32, Action>,
}

impl InputEmulation {
    async fn with_backend(backend: Backend, keybindings: HashMap<u32, Action>) -> Result<InputEmulation, EmulationCreationError> {
        let emulation: Box<dyn Emulation> = match backend {
            #[cfg(all(unix, feature = "wlroots", not(target_os = "macos")))]
            Backend::Wlroots => Box::new(wlroots::WlrootsEmulation::new()?),
            #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
            Backend::Libei => Box::new(libei::LibeiEmulation::new().await?),
            #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
            Backend::X11 => Box::new(x11::X11Emulation::new()?),
            #[cfg(all(unix, feature = "remote_desktop_portal", not(target_os = "macos")))]
            Backend::Xdp => Box::new(xdg_desktop_portal::DesktopPortalEmulation::new().await?),
            #[cfg(windows)]
            Backend::Windows => Box::new(windows::WindowsEmulation::new()?),
            #[cfg(target_os = "macos")]
            Backend::MacOs => Box::new(macos::MacOSEmulation::new()?),
            Backend::Dummy => Box::new(dummy::DummyEmulation::new()),
        };
        Ok(Self {
            emulation,
            handles: HashSet::new(),
            pressed_keys: HashMap::new(),
            keybindings,
        })
    }

    pub async fn new(backend: Option<Backend>, keybindings_raw: HashMap<u32, String>) -> Result<InputEmulation, EmulationCreationError> {
        let keybindings: HashMap<u32, Action> = keybindings_raw.into_iter()
            .filter_map(|(k, v)| Action::from_str(&v).map(|a| (k, a)))
            .collect();
        if let Some(backend) = backend {
            let b = Self::with_backend(backend, keybindings.clone()).await;
            if b.is_ok() {
                log::info!("using emulation backend: {backend}");
            }
            return b;
        }

        for backend in [
            #[cfg(all(unix, feature = "wlroots", not(target_os = "macos")))]
            Backend::Wlroots,
            #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
            Backend::Libei,
            #[cfg(all(unix, feature = "remote_desktop_portal", not(target_os = "macos")))]
            Backend::Xdp,
            #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
            Backend::X11,
            #[cfg(windows)]
            Backend::Windows,
            #[cfg(target_os = "macos")]
            Backend::MacOs,
            Backend::Dummy,
        ] {
            match Self::with_backend(backend, keybindings.clone()).await {
                Ok(b) => {
                    log::info!("using emulation backend: {backend}");
                    return Ok(b);
                }
                Err(e) if e.cancelled_by_user() => return Err(e),
                Err(e) => log::warn!("{e}"),
            }
        }

        Err(EmulationCreationError::NoAvailableBackend)
    }

    pub async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        match event {
            Event::Keyboard(KeyboardEvent::Key { key, state, .. }) => {
                // prevent double pressed / released keys
                if self.update_pressed_keys(handle, key, state) {
                    self.emulation.consume(event, handle).await?;
                }
                Ok(())
            }
            Event::Pointer(PointerEvent::Button { time, button, state }) => {
                if let Some(action) = self.keybindings.get(&button) {
                    // Convert action to keyboard event
                    let keyboard_event = self.action_to_keyboard(action, state, time);
                    self.emulation.consume(Event::Keyboard(keyboard_event), handle).await?;
                } else {
                    self.emulation.consume(Event::Pointer(PointerEvent::Button { time, button, state }), handle).await?;
                }
                Ok(())
            }
            _ => self.emulation.consume(event, handle).await,
        }
    }

    pub async fn create(&mut self, handle: EmulationHandle) -> bool {
        if self.handles.insert(handle) {
            self.pressed_keys.insert(handle, HashSet::new());
            self.emulation.create(handle).await;
            true
        } else {
            false
        }
    }

    pub async fn destroy(&mut self, handle: EmulationHandle) {
        let _ = self.release_keys(handle).await;
        if self.handles.remove(&handle) {
            self.pressed_keys.remove(&handle);
            self.emulation.destroy(handle).await
        }
    }

    pub async fn terminate(&mut self) {
        for handle in self.handles.iter().cloned().collect::<Vec<_>>() {
            self.destroy(handle).await
        }
        self.emulation.terminate().await
    }

    pub async fn release_keys(&mut self, handle: EmulationHandle) -> Result<(), EmulationError> {
        if let Some(keys) = self.pressed_keys.get_mut(&handle) {
            let keys = keys.drain().collect::<Vec<_>>();
            for key in keys {
                let event = Event::Keyboard(KeyboardEvent::Key {
                    time: 0,
                    key,
                    state: 0,
                });
                self.emulation.consume(event, handle).await?;
                if let Ok(key) = input_event::scancode::Linux::try_from(key) {
                    log::warn!("releasing stuck key: {key:?}");
                }
            }
        }

        let event = Event::Keyboard(KeyboardEvent::Modifiers {
            depressed: 0,
            latched: 0,
            locked: 0,
            group: 0,
        });
        self.emulation.consume(event, handle).await?;
        Ok(())
    }

    pub fn has_pressed_keys(&self, handle: EmulationHandle) -> bool {
        self.pressed_keys
            .get(&handle)
            .is_some_and(|p| !p.is_empty())
    }

    /// update the pressed_keys for the given handle
    /// returns whether the event should be processed
    fn update_pressed_keys(&mut self, handle: EmulationHandle, key: u32, state: u8) -> bool {
        let Some(pressed_keys) = self.pressed_keys.get_mut(&handle) else {
            return false;
        };

        if state == 0 {
            // currently pressed => can release
            pressed_keys.remove(&key)
        } else {
            // currently not pressed => can press
            pressed_keys.insert(key)
        }
    }

    fn action_to_keyboard(&self, action: &Action, state: u32, time: u32) -> KeyboardEvent {
        let key = match action {
            Action::Back => input_event::scancode::Linux::KeyBack as u32,
            Action::Forward => input_event::scancode::Linux::KeyForward as u32,
        };
        KeyboardEvent::Key { time, key, state: state as u8 }
    }
}

#[async_trait]
trait Emulation: Send {
    async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError>;
    async fn create(&mut self, handle: EmulationHandle);
    async fn destroy(&mut self, handle: EmulationHandle);
    async fn terminate(&mut self);
}
