//! In-memory response policy for UI bring-up. This module performs no I/O.
//!
//! The production effect executor is deliberately not used. Future effects
//! are refused by the catch-all, not implicitly enabled in a device probe.
use crate::app::{App, Effect};

pub const NOTICE: &str = "界面测试模式：此操作未执行，分区与数据功能尚未启用";
pub const AUTOSTART_MARKER: &str = "venus-ui-autostart-v1\n";

/// An unbounded session is permitted only in an explicitly marked test image.
/// This helper performs no filesystem or other I/O.
pub fn lifetime(
    seconds: u64,
    until_stopped: bool,
    manifest: Option<&str>,
) -> anyhow::Result<Option<std::time::Duration>> {
    if until_stopped {
        anyhow::ensure!(
            manifest.is_some_and(|s| s.starts_with(AUTOSTART_MARKER)),
            "--until-stopped requires the explicitly built venus UI-autostart image"
        );
        Ok(None)
    } else {
        anyhow::ensure!(
            (1..=1800).contains(&seconds),
            "UI probe duration must be 1..=1800 seconds"
        );
        Ok(Some(std::time::Duration::from_secs(seconds)))
    }
}

/// Consume an effect without dispatching it to any system service. Returns
/// true if it was refused, false for a harmless ignored feedback/cancel.
pub fn refuse(app: &mut App, effect: Effect, now: u64) -> bool {
    match effect {
        Effect::Haptic
        | Effect::Ticks
        | Effect::Cancel
        | Effect::StopJob
        | Effect::StopRelabel
        | Effect::StopBackup
        | Effect::StopDownload
        | Effect::StopSkywardUpdate => return false,
        Effect::ReadMounts => app.set_mounts(Err(NOTICE.into())),
        Effect::ReadSlots => app.set_boot_slots(Err(NOTICE.into())),
        Effect::ReadFilesystems => app.set_filesystems(Err(NOTICE.into())),
        Effect::ReadDir(path) => app.set_files(path, Err(NOTICE.into()), 0),
        Effect::ReadPlace(place, path) => app.set_place(place, path, Err(NOTICE.into()), 0),
        Effect::OpenData | Effect::Unlock { .. } => app.data_failed(NOTICE.into()),
        Effect::FileTransfer(_) => {
            app.state.file_transfer = false;
            app.refresh();
        }
        Effect::FindBackups { .. } => app.set_backups(Vec::new()),
        // No reboot, slot switching, shell, MTP, data unlock, wipe, install,
        // merge, mount, download or file operations are implemented here.
        _ => app.enter_recovery(),
    }
    app.report(NOTICE, now);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Data, Screen, Target, Wipe};

    #[test]
    fn bounded_sessions_remain_bounded() {
        assert_eq!(
            lifetime(900, false, None).unwrap(),
            Some(std::time::Duration::from_secs(900))
        );
        assert!(lifetime(0, false, None).is_err());
        assert!(lifetime(1801, false, None).is_err());
    }

    #[test]
    fn persistent_sessions_require_the_autostart_manifest() {
        assert!(lifetime(900, true, None).is_err());
        assert!(lifetime(900, true, Some("venus-entryfix-v1\n")).is_err());
        assert!(
            lifetime(900, true, Some(AUTOSTART_MARKER))
                .unwrap()
                .is_none()
        );
    }

    fn app() -> App {
        App::new(Screen::Home, 1440, 3200, 3.5)
    }

    #[test]
    fn destructive_actions_are_refused_without_an_executor() {
        for effect in [
            Effect::Reboot(Target::System),
            Effect::FactoryReset,
            Effect::Wipe(vec![Wipe::All]),
            Effect::Install,
            Effect::Sideload,
            Effect::SetSlot(skyward_vab::Slot::B),
            Effect::Merge,
            Effect::Dismiss,
            Effect::StartShell("/".into()),
            Effect::Mount("system".into()),
            Effect::Delete("/data/test".into()),
        ] {
            let mut app = app();
            assert!(refuse(&mut app, effect, 100));
            assert_eq!(app.screen(), Screen::Home);
        }
    }

    #[test]
    fn unlocking_is_reported_as_unavailable_not_attempted() {
        let mut app = app();
        assert!(refuse(&mut app, Effect::OpenData, 100));
        assert!(matches!(app.state.data, Data::Failed(_)));
        assert!(refuse(
            &mut app,
            Effect::Unlock {
                user: 0,
                code: "not-used".into()
            },
            101
        ));
        assert!(matches!(app.state.data, Data::Failed(_)));
    }

    #[test]
    fn usb_and_queries_get_an_in_memory_answer() {
        let mut app = app();
        assert!(refuse(&mut app, Effect::FileTransfer(true), 100));
        assert!(!app.state.file_transfer);
        assert!(refuse(&mut app, Effect::ReadMounts, 101));
        assert!(app.state.mounts.is_err());
        assert!(refuse(&mut app, Effect::ReadSlots, 102));
        assert!(matches!(app.state.boot_slots, Some(Err(_))));
    }

    #[test]
    fn feedback_never_starts_the_haptic_motor() {
        let mut app = app();
        assert!(!refuse(&mut app, Effect::Haptic, 100));
        assert!(!refuse(&mut app, Effect::Ticks, 100));
    }
}
