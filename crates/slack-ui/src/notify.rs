//! Telling you something arrived.
//!
//! Kept behind a narrow seam because sound is a platform capability, not a
//! GPUI one: macOS plays a system sound, and every other platform is a
//! documented no-op rather than a silent failure.
//!
//! The policy lives here too, so every caller cannot disagree about it: never
//! for your own messages, never while notifications are paused, and never for
//! the conversation you are already reading — you can see those arrive.

use gpui::{App, AppContext as _};

use slack_api::models::DndState;

/// Whether an arrival should make a sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arrival {
    /// Written by the signed-in user.
    pub is_own: bool,
    /// In the conversation currently on screen.
    pub is_active: bool,
}

/// Decide, from the arrival and the current do-not-disturb state.
pub fn should_sound(arrival: Arrival, dnd: &DndState) -> bool {
    if arrival.is_own || arrival.is_active {
        return false;
    }
    !(dnd.snooze_enabled || dnd.dnd_enabled)
}

/// Play the new-message sound, if this arrival warrants one.
pub fn message_arrived(arrival: Arrival, dnd: &DndState, cx: &mut App) {
    if !should_sound(arrival, dnd) {
        return;
    }
    play(cx);
}

#[cfg(target_os = "macos")]
fn play(cx: &mut App) {
    // `afplay` rather than a linked audio stack: one system sound does not
    // justify pulling an audio engine into the binary, and this cannot block
    // the window because it is spawned and never waited on.
    const SOUND: &str = "/System/Library/Sounds/Tink.aiff";

    cx.background_spawn(async {
        match std::process::Command::new("/usr/bin/afplay")
            .arg(SOUND)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                // Reaped so a long session does not accumulate zombies.
                let _ = child.wait();
            }
            Err(err) => log::debug!("could not play the notification sound: {err}"),
        }
    })
    .detach();
}

#[cfg(not(target_os = "macos"))]
fn play(_: &mut App) {
    log::debug!("notification sounds are not implemented on this platform");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet() -> DndState {
        DndState::default()
    }

    fn arrival() -> Arrival {
        Arrival {
            is_own: false,
            is_active: false,
        }
    }

    #[test]
    fn an_ordinary_arrival_sounds() {
        assert!(should_sound(arrival(), &quiet()));
    }

    #[test]
    fn your_own_message_never_sounds() {
        let mine = Arrival {
            is_own: true,
            ..arrival()
        };
        assert!(!should_sound(mine, &quiet()));
    }

    #[test]
    fn the_conversation_you_are_reading_never_sounds() {
        let visible = Arrival {
            is_active: true,
            ..arrival()
        };
        assert!(!should_sound(visible, &quiet()));
    }

    #[test]
    fn paused_notifications_are_respected() {
        let snoozing = DndState {
            snooze_enabled: true,
            ..quiet()
        };
        assert!(!should_sound(arrival(), &snoozing));

        let dnd = DndState {
            dnd_enabled: true,
            ..quiet()
        };
        assert!(!should_sound(arrival(), &dnd));
    }
}
