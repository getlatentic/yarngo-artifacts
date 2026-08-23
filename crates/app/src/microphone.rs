//! Whether this application may record, asked before it needs to.
//!
//! macOS answers a refused microphone with silence, not an error: the stream
//! opens, every buffer is zeros, and the recording fails the quality check with
//! advice about moving closer to the microphone. Someone follows that advice
//! forever. So permission is a question asked in its own right, and the answer
//! is something the interface can act on.
//!
//! The prompt itself appears once per installation and never again — a second
//! `requestAccess` after a refusal returns the refusal without showing
//! anything. That is why a refusal has to lead somewhere: System Settings is
//! the only place it can be undone.

/// What the system says about recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    /// Never asked. Asking is what shows the system prompt, and it can only
    /// be shown once.
    Undecided,
    /// Refused, or withdrawn later in System Settings. Only settings can
    /// change this; asking again shows nothing.
    Refused,
    /// Not the person's to give — a managed Mac, or Screen Time.
    Restricted,
    Granted,
}

impl Permission {
    /// What `AVAuthorizationStatus` means: 0 notDetermined, 1 restricted,
    /// 2 denied, 3 authorized.
    ///
    /// Separate from asking, because the branch that matters most — a status
    /// this build has never heard of — cannot be produced by asking a system
    /// that answers correctly.
    pub fn from_status(raw: isize) -> Self {
        match raw {
            0 => Self::Undecided,
            1 => Self::Restricted,
            2 => Self::Refused,
            3 => Self::Granted,
            // Never consent. A number this build does not know is a system
            // saying something it has not been taught, and the cost of guessing
            // wrong is a silent recording blamed on the room.
            _ => Self::Refused,
        }
    }

    /// What to tell someone, and whether recording can go ahead.
    pub fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Granted | Self::Undecided => None,
            Self::Refused => Some("enrol.microphone_refused"),
            Self::Restricted => Some("enrol.microphone_restricted"),
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::Permission;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {
        /// The media type constant, linked rather than spelled out, so this
        /// cannot drift from what the framework means by it.
        static AVMediaTypeAudio: &'static NSString;
    }

    pub fn status() -> Permission {
        // AVAuthorizationStatus: 0 notDetermined, 1 restricted, 2 denied,
        // 3 authorized.
        let raw: isize = unsafe {
            msg_send![
                class!(AVCaptureDevice),
                authorizationStatusForMediaType: AVMediaTypeAudio
            ]
        };
        Permission::from_status(raw)
    }

    /// Show the system prompt, and answer once the person has.
    ///
    /// The handler runs on whichever queue the system chooses, so `answer` has
    /// to be able to cross threads and must not touch the interface itself.
    pub fn request(answer: impl FnOnce(Permission) + Send + 'static) {
        // The block's type is `Fn`, and the system promises to call it once.
        // Holding the callback where it can be taken honours both: called
        // twice, the second call does nothing rather than running it again.
        let answer = std::sync::Mutex::new(Some(answer));
        let handler = RcBlock::new(move |granted: Bool| {
            let taken = answer.lock().ok().and_then(|mut held| held.take());
            if let Some(answer) = taken {
                // Asked again rather than inferred from the flag: a refusal and
                // a restriction both arrive as `false`, and they are not the
                // same thing to say.
                answer(if granted.as_bool() { Permission::Granted } else { status() });
            }
        });
        unsafe {
            let _: () = msg_send![
                class!(AVCaptureDevice),
                requestAccessForMediaType: AVMediaTypeAudio,
                completionHandler: &*handler,
            ];
        }
    }

    /// Open the pane where a refusal can be undone. Nowhere else can.
    pub fn open_settings() {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            .spawn();
    }
}

/// Everywhere else, recording is between the person and their operating
/// system: there is no status to read, and pretending otherwise would put a
/// refusal in front of someone who was never refused.
#[cfg(not(target_os = "macos"))]
mod platform {
    use super::Permission;

    pub fn status() -> Permission {
        Permission::Granted
    }

    pub fn request(answer: impl FnOnce(Permission) + Send + 'static) {
        answer(Permission::Granted);
    }

    pub fn open_settings() {}
}

pub use platform::{open_settings, request, status};

#[cfg(test)]
mod tests {
    use super::*;

    /// Only one answer opens the microphone. Anything else — including a
    /// status this build does not recognise — has to be a refusal, because the
    /// cost of reading it as consent is a silent recording and advice about
    /// the room.
    #[test]
    fn nothing_but_granted_counts_as_permission() {
        for permission in [
            Permission::Undecided,
            Permission::Refused,
            Permission::Restricted,
            Permission::Granted,
        ] {
            assert_eq!(
                permission == Permission::Granted,
                permission.refusal().is_none() && permission != Permission::Undecided,
                "{permission:?} is on the wrong side of the line"
            );
        }
    }

    /// Every number the framework documents, and one it does not.
    #[test]
    fn a_status_this_build_has_never_heard_of_is_not_consent() {
        assert_eq!(Permission::from_status(0), Permission::Undecided);
        assert_eq!(Permission::from_status(1), Permission::Restricted);
        assert_eq!(Permission::from_status(2), Permission::Refused);
        assert_eq!(Permission::from_status(3), Permission::Granted);

        for unknown in [-1, 4, 99, isize::MAX, isize::MIN] {
            assert_eq!(
                Permission::from_status(unknown),
                Permission::Refused,
                "status {unknown} was read as something other than a refusal"
            );
        }
    }

    /// Undecided has no message because it is not an answer yet — it is the
    /// state that leads to the prompt.
    #[test]
    fn only_a_settled_refusal_has_something_to_say() {
        assert_eq!(Permission::Undecided.refusal(), None);
        assert_eq!(Permission::Granted.refusal(), None);
        assert!(Permission::Refused.refusal().is_some());
        assert!(Permission::Restricted.refusal().is_some());
    }

    /// The system's answer, whatever it is, must be one this build handles.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_answers_with_something_we_understand() {
        let asked = status();
        assert!(
            matches!(
                asked,
                Permission::Undecided
                    | Permission::Refused
                    | Permission::Restricted
                    | Permission::Granted
            ),
            "{asked:?}"
        );
    }
}
