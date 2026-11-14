//! Incremental venus functionality. Never invokes the production recovery
//! entry, BCB processing, installer, crypto credential checks or slot writes.
use anyhow::{Context, Result, ensure};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

pub const PARTITIONS: &[&str] = &[
    "system",
    "system_ext",
    "product",
    "vendor",
    "odm",
    "mi_ext",
    "vendor_dlkm",
    "odm_dlkm",
    "system_dlkm",
];
#[cfg(target_os = "android")]
const ROOT: &str = "/mnt/skyward-ro";
#[cfg(target_os = "android")]
const SETTINGS_DIR: &str = "/metadata/skyward";
#[cfg(target_os = "android")]
const SETTINGS_NAME: &str = "venus-settings.json";

fn partition(base: &str) -> Result<()> {
    ensure!(
        PARTITIONS.contains(&base),
        "仅支持系统逻辑分区的只读挂载：{base}"
    );
    Ok(())
}

/// Linux filesystem superblock signatures. Recognition only: the kernel
/// remains responsible for full superblock/features validation on mount.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    match bytes.get(1024..1028) {
        Some([0xe2, 0xe1, 0xf5, 0xe0]) => return Some("erofs"),
        Some([0x10, 0x20, 0xf5, 0xf2]) => return Some("f2fs"),
        _ => {}
    }
    (bytes.get(1080..1082) == Some(&[0x53, 0xef])).then_some("ext4")
}

fn ro_options(filesystem: &str) -> Result<Vec<String>> {
    let mut options = vec!["ro", "nosuid", "nodev", "noatime"];
    match filesystem {
        "erofs" => {}
        "ext4" => options.push("noload"),
        "f2fs" => options.push("norecovery"),
        _ => anyhow::bail!("unsupported read-only filesystem {filesystem}"),
    }
    Ok(options.into_iter().map(str::to_owned).collect())
}

/// An open holder can make DM removal fail after the mount has gone.
/// Restore our read-only mount in that case, so a later retry still has
/// the same ownership evidence. Never continue with a half-released map.
fn release_mapping(
    unmount: impl FnOnce() -> Result<()>,
    remove: impl FnOnce() -> Result<()>,
    remount: impl FnOnce() -> Result<()>,
) -> Result<()> {
    unmount()?;
    if let Err(error) = remove() {
        return match remount() {
            Ok(()) => Err(error.context("mapping remained busy; restored its read-only mount")),
            Err(restore) => Err(error.context(format!("mapping removal failed and remount failed too: {restore:#}"))),
        };
    }
    Ok(())
}

/// Settings writes never remount metadata. Use a new regular file and an
/// atomic rename under a real, root-only directory. No arbitrary UI path.
fn atomic_save(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.contains('/') && name != "." && name != "..",
        "invalid file name"
    );
    ensure!(bytes.len() <= 65536, "settings too large");
    let m = fs::symlink_metadata(dir).context("settings directory")?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink(),
        "settings directory must not be a link"
    );
    let final_path = dir.join(name);
    if let Ok(m) = fs::symlink_metadata(&final_path) {
        ensure!(
            m.is_file() && !m.file_type().is_symlink(),
            "settings target must be a regular file"
        );
    }
    let temp = dir.join(format!(".{name}.{}.new", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, &final_path)?;
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) => ensure!(
            m.is_dir() && !m.file_type().is_symlink(),
            "{} is not a real directory",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(target_os = "android")]
pub mod runtime {
    use super::*;
    use skyward_block::{dm, mount};
    use skyward_config::DeviceConfig;
    use skyward_info::DeviceInfo;
    use skyward_ui::app::{App, Effect, MountFailure, MountRow, Shot, Target};
    use skyward_ui::settings::{Keeper, Kept, Settings};
    use skyward_ui::{Canvas, Fonts};
    use skyward_vab::{Slot, SnapshotState};
    use std::io::{Read, Seek, SeekFrom};
    use std::path::PathBuf;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    const RAM: &str = "/tmp/skyward-basic";
    const NOTICE: &str = "此功能仍在适配：暂不执行刷写、清除、解密或切换槽位";

    pub fn guard(config: &Path) -> Result<DeviceConfig> {
        ensure!(
            fs::read_to_string("/skyward/probe-entryfix.txt")?.starts_with("venus-entryfix-v1\n"),
            "venus Recovery required"
        );
        let cfg = DeviceConfig::load(config)?;
        ensure!(
            cfg.device.codename == "venus",
            "venus configuration required"
        );
        Ok(cfg)
    }

    fn slot() -> Result<Slot> {
        skyward_vab::current_slot()?.context("current A/B slot missing")
    }
    fn point(base: &str) -> PathBuf {
        Path::new(ROOT).join(base)
    }
    fn mapping(base: &str, slot: Slot) -> String {
        format!("venus_ro_{base}{}", slot.suffix())
    }
    fn no_update() -> Result<()> {
        ensure!(
            skyward_vab::snapshot_state()? == SnapshotState::None,
            "OTA 快照状态不允许直接映射；请先在主系统完成更新"
        );
        Ok(())
    }
    fn real_dir(path: &Path) -> Result<()> {
        if !path.exists() {
            fs::create_dir(path)?;
        }
        ensure!(
            fs::symlink_metadata(path)?.is_dir(),
            "mount point is not a real directory"
        );
        Ok(())
    }

    /// Refuse unknown existing mappings; only reuse a read-only mapping
    /// that is already mounted at our isolated point with the same devnum.
    fn owned(base: &str, slot: Slot) -> Result<bool> {
        let name = mapping(base, slot);
        let status = dm::status(&name)?;
        let visible = mount::visible(&point(base))?;
        match (status, visible) {
            (None, None) => Ok(false),
            (Some(dev), Some(v)) if dev == v.device && dm::readonly(&name)? => Ok(true),
            _ => anyhow::bail!("mapping or mount point occupied inconsistently: {base}"),
        }
    }

    pub fn mount_one(base: &str) -> Result<PathBuf> {
        partition(base)?;
        no_update()?;
        let slot = slot()?;
        ensure!(
            skyward_vab::slot_bases(slot)?.iter().any(|b| b == base),
            "partition not found in current super slot"
        );
        real_dir(Path::new(ROOT))?;
        let at = point(base);
        real_dir(&at)?;
        if owned(base, slot)? {
            return Ok(at);
        }
        let name = mapping(base, slot);
        let whole = skyward_vab::map_whole(slot, base, &name, true)?;
        let result = (|| -> Result<()> {
            ensure!(dm::readonly(&name)?, "device-mapper must be read-only");
            let filesystem = filesystem(&whole.device)?.context("unrecognized filesystem")?;
            mount::mount(&whole.device, &at, &filesystem, &ro_options(&filesystem)?)?;
            ensure!(
                mount::visible(&at)?
                    .is_some_and(|v| Some(v.device) == dm::status(&name).ok().flatten()),
                "mounted device differs"
            );
            Ok(())
        })();
        if let Err(e) = result {
            if mount::visible(&at)?.is_none() {
                let _ = skyward_vab::unmap_whole(&whole);
            }
            return Err(e);
        }
        eprintln!("VENUS_BASIC_MOUNT_RO {base} {}", at.display());
        Ok(at)
    }

    pub fn unmount_one(base: &str) -> Result<()> {
        partition(base)?;
        no_update()?;
        let slot = slot()?;
        if !owned(base, slot)? {
            return Ok(());
        }
        let at = point(base);
        let name = mapping(base, slot);
        let visible = mount::visible(&at)?.context("owned mount disappeared")?;
        let device = PathBuf::from(format!("/dev/block/dm-{}", visible.device.1));
        let opts = ro_options(&visible.filesystem)?;
        release_mapping(
            || mount::umount(&at),
            || dm::remove(&name),
            || mount::mount(&device, &at, &visible.filesystem, &opts),
        )?;
        eprintln!("VENUS_BASIC_UNMOUNT {base}");
        Ok(())
    }

    pub fn mounts() -> Result<Vec<MountRow>> {
        let slot = slot()?;
        skyward_vab::slot_bases(slot)?
            .into_iter()
            .filter(|b| PARTITIONS.contains(&b.as_str()))
            .map(|base| {
                let mounted = owned(&base, slot)?;
                Ok(MountRow {
                    point: point(&base).display().to_string(),
                    base,
                    mounted,
                    holders: Vec::new(),
                })
            })
            .collect()
    }

    fn filesystem(path: &Path) -> Result<Option<String>> {
        let mut head = [0u8; 4096];
        File::open(path)?.read_exact(&mut head)?;
        Ok(sniff(&head).map(str::to_owned))
    }
    fn filesystems() -> Result<Vec<skyward_ui::app::FsRow>> {
        let mut rows = Vec::new();
        for (name, block, point) in [
            ("data", "userdata", "/data"),
            ("metadata", "metadata", "/metadata"),
            ("cache", "cache", "/cache"),
        ] {
            let device = Path::new("/dev/block/by-name").join(block);
            if !device.try_exists()? {
                continue;
            }
            let size = skyward_block::size_bytes(&device)?;
            let filesystem = if name == "data" {
                // Unmounted encrypted data is not reported as a visible filesystem.
                mount::visible(Path::new(point))?.map(|v| v.filesystem)
            } else {
                filesystem(&device)?
            };
            rows.push(skyward_ui::app::FsRow {
                name: name.into(),
                mount_point: point.into(),
                filesystem,
                size,
                others: Vec::new(),
            });
        }
        Ok(rows)
    }

    fn metadata_ready() -> Result<()> {
        let shown = mount::visible(Path::new("/metadata"))?
            .context("Metadata 未挂载；保留现状，不自动格式化或重挂载")?;
        ensure!(
            shown.device == skyward_block::devnum("/dev/block/by-name/metadata")?,
            "unexpected Metadata device"
        );
        Ok(())
    }
    pub fn kept() -> Kept {
        let read = || -> Result<Kept> {
            metadata_ready()?;
            let path = Path::new(SETTINGS_DIR).join(SETTINGS_NAME);
            match fs::read_to_string(path) {
                Ok(s) => Ok(Kept::Settings(Settings::read(&s)?)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Kept::Nothing),
                Err(e) => Err(e.into()),
            }
        };
        read().unwrap_or_else(|e| {
            eprintln!("VENUS_BASIC_SETTINGS_READ_ERROR {e:#}");
            Kept::Unreadable
        })
    }
    pub fn save(settings: &Settings) -> Result<()> {
        metadata_ready()?;
        private_dir(Path::new(SETTINGS_DIR))?;
        atomic_save(
            Path::new(SETTINGS_DIR),
            SETTINGS_NAME,
            settings.text().as_bytes(),
        )
    }

    /// Temporary ro,noload persist read. Never set RTC; only retain a
    /// calibrated wall-clock value inside this process for the UI.
    fn clock_epoch() -> Result<u64> {
        let existing = mount::points_of(skyward_block::devnum(skyward_sys::clock::PERSIST)?)?;
        let bytes = if let Some(at) = existing.first() {
            fs::read(at.join(skyward_sys::clock::OFFSET))?
        } else {
            real_dir(Path::new(RAM))?;
            let at = Path::new(RAM).join("persist-read");
            real_dir(&at)?;
            ensure!(
                mount::visible(&at)?.is_none(),
                "persist-read is already occupied"
            );
            mount::mount(
                Path::new(skyward_sys::clock::PERSIST),
                &at,
                "ext4",
                &ro_options("ext4")?,
            )?;
            let read = fs::read(at.join(skyward_sys::clock::OFFSET));
            let unmount = mount::umount(&at);
            unmount?;
            read?
        };
        let offset = skyward_sys::clock::offset(&bytes)?;
        let rtc: u64 = fs::read_to_string(skyward_sys::clock::RTC)?
            .trim()
            .parse()?;
        let wall = rtc
            .checked_mul(1000)
            .and_then(|r| r.checked_add(offset))
            .context("clock overflow")?;
        ensure!(
            (1_577_836_800_000..4_102_444_800_000).contains(&wall),
            "persist/RTC time is implausible; leave clock unknown"
        );
        Ok(wall)
    }

    fn id() -> String {
        format!("{}-{}", crate::input::monotonic_ms(), std::process::id())
    }
    fn tail(path: &Path, max: u64) -> Result<Vec<u8>> {
        let mut f = File::open(path)?;
        let len = f.metadata()?.len();
        if len > max {
            f.seek(SeekFrom::Start(len - max))?;
        }
        let mut bytes = Vec::new();
        f.take(max).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    pub fn logs(recovery: bool, kernel: bool) -> Result<PathBuf> {
        private_dir(Path::new(RAM))?;
        let folder = Path::new(RAM).join(format!("logs-{}", id()));
        private_dir(&folder)?;
        if recovery {
            for (name, src) in [
                ("ui.log", "/tmp/skyward-ui-probe/ui.log"),
                ("probe.log", "/tmp/skyward-probe/probe.log"),
                ("basic.log", "/tmp/skyward-basic/ui.log"),
            ] {
                match tail(Path::new(src), 2 * 1024 * 1024) {
                    Ok(data) => fs::write(folder.join(name), data)?,
                    Err(e) => fs::write(folder.join(format!("{name}.error")), e.to_string())?,
                }
            }
        }
        if kernel {
            fs::write(folder.join("kernel.log"), skyward_sys::kernel_log()?)?;
        }
        fs::write(folder.join("mounts.txt"), fs::read("/proc/mounts")?)?;
        eprintln!("VENUS_BASIC_LOGS {}", folder.display());
        Ok(folder)
    }
    pub fn screenshot(canvas: &Canvas) -> Result<PathBuf> {
        private_dir(Path::new(RAM))?;
        let at = Path::new(RAM).join(format!("screen-{}.png", id()));
        let f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&at)?;
        canvas.write_png(&f)?;
        eprintln!("VENUS_BASIC_SCREENSHOT {}", at.display());
        Ok(at)
    }

    fn regular_text(path: &Path) -> Result<Option<Vec<u8>>> {
        let canonical = path.canonicalize()?;
        ensure!(
            !canonical.starts_with("/dev")
                && !canonical.starts_with("/proc")
                && !canonical.starts_with("/sys"),
            "特殊设备或内核接口不作为文本打开"
        );
        skyward_files::read_up_to(&canonical, skyward_ui::editor::MAX_BYTES as u64)
    }

    pub struct Basic {
        cfg: DeviceConfig,
        keeper: Keeper,
        clock: Option<(u64, Instant)>,
        refreshed: Instant,
    }
    impl Basic {
        pub fn open(cfg: &DeviceConfig, app: &mut App) -> Result<Self> {
            let kept = kept();
            if let Some(s) = kept.settings() {
                app.take_settings(Some(s), None);
            }
            app.state.file_transfer = false;
            app.state.vibrate = false;
            let clock = match clock_epoch() {
                Ok(ms) => {
                    eprintln!("VENUS_BASIC_CLOCK epoch_ms={ms}");
                    Some((ms, Instant::now()))
                }
                Err(e) => {
                    eprintln!("VENUS_BASIC_CLOCK_UNAVAILABLE {e:#}");
                    None
                }
            };
            app.set_mounts(mounts().map_err(|e| format!("{e:#}")));
            app.set_update(skyward_vab::standing(slot()?)?);
            app.set_boot_slots(Err(
                "当前槽位已识别；BootControl 服务尚未适配，未推测另一个槽位的可启动状态".into(),
            ));
            eprintln!("VENUS_BASIC_READY ro-mount browse text settings logs screenshot power");
            Ok(Self {
                cfg: cfg.clone(),
                keeper: Keeper::new(&kept),
                clock,
                refreshed: Instant::now(),
            })
        }
        pub fn clock(&self, zone: usize) -> String {
            match &self.clock {
                Some((ms, at)) => skyward_ui::fmt::clock(
                    ((ms + at.elapsed().as_millis() as u64) / 1000) as i64,
                    zone,
                ),
                None => "--:--".into(),
            }
        }
        pub fn tick(&mut self, app: &mut App, info: &mut DeviceInfo, now: u64) {
            if let Some(due) = self.keeper.due(app.settings(), now) {
                match save(&due) {
                    Ok(()) => {
                        self.keeper.written(due);
                        eprintln!("VENUS_BASIC_SETTINGS_SAVED");
                    }
                    Err(e) => {
                        self.keeper.failed();
                        eprintln!("VENUS_BASIC_SETTINGS_WRITE_ERROR {e:#}");
                        app.report("设置未保存，详见日志", now);
                    }
                }
            }
            if self.refreshed.elapsed() >= Duration::from_secs(5) {
                self.refreshed = Instant::now();
                match skyward_info::collect(&self.cfg) {
                    Ok(mut updated) => {
                        updated.panel = info.panel;
                        if updated != *info {
                            *info = updated;
                            app.refresh();
                        }
                    }
                    Err(e) => eprintln!("VENUS_BASIC_INFO_ERROR {e:#}"),
                }
            }
        }
        pub fn handle(
            &mut self,
            app: &mut App,
            fx: Effect,
            now: u64,
            fonts: &Fonts,
            canvas: &Canvas,
        ) {
            let result = (|| -> Result<()> {
                match fx {
                    Effect::ReadMounts => app.set_mounts(mounts().map_err(|e| format!("{e:#}"))),
                    Effect::ReadFilesystems => app.set_filesystems(filesystems().map_err(|e| format!("{e:#}"))),
                    Effect::ReadUpdate => app.set_update(skyward_vab::standing(slot()?)?),
                    Effect::ReadSlots => app.set_boot_slots(Err("BootControl 服务尚未适配；当前槽位可见，切槽暂不可用".into())),
                    Effect::ReadDir(path) => {
                        let rows = crate::ui::read_folder(Path::new(&path)).map_err(|e| format!("{e:#}"));
                        let unix = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
                        app.set_files(path, rows, unix);
                    }
                    Effect::OpenText(path) => {
                        let read = regular_text(Path::new(&path)).map_err(|e| format!("{e:#}"));
                        app.text_opened(fonts, path, read, now);
                    }
                    Effect::SaveText { .. } => app.text_saved(Err("文本只读查看，尚未开放修改".into()), now),
                    Effect::Mount(base) => self.mount_effect(app, base, true),
                    Effect::Unmount(base) => self.mount_effect(app, base, false),
                    Effect::ExportLogs { recovery, kernel, storage: false } => {
                        let to = logs(recovery, kernel)?;
                        app.report(&format!("已导出到 {}，可用 ADB 拉取", to.display()), now);
                    }
                    Effect::Screenshot { storage: false } => {
                        let to = screenshot(canvas)?;
                        app.screenshot_saved(Ok(Shot::Held), now);
                        app.report(&format!("截图：{}", to.display()), now);
                    }
                    Effect::Reboot(target) => {
                        let power = match target {
                            Target::System => skyward_sys::Power::System,
                            Target::Bootloader => skyward_sys::Power::Bootloader,
                            Target::PowerOff => skyward_sys::Power::Off,
                            _ => anyhow::bail!("临时 Boot 暂仅支持重启系统、Bootloader 和关机；不会写 BCB"),
                        };
                        if let Err(e) = save(&app.settings()) {
                            eprintln!("VENUS_BASIC_SETTINGS_BEFORE_REBOOT_ERROR {e:#}");
                        }
                        unsafe { libc::sync(); }
                        skyward_sys::power(power)?;
                    }
                    Effect::OpenData | Effect::Unlock { .. } => app.data_failed("解密服务未适配：尚未启动 Keymaster/Gatekeeper；未尝试密钥或密码，未格式化 Data".into()),
                    other => { if skyward_ui::probe::refuse(app, other, now) { app.report(NOTICE, now); } },
                }
                Ok(())
            })();
            if let Err(e) = result {
                eprintln!("VENUS_BASIC_OPERATION_ERROR {e:#}");
                app.report(&format!("{e:#}"), now);
            }
        }
        pub fn mount_effect(&mut self, app: &mut App, base: String, mounting: bool) {
            let result = if mounting {
                mount_one(&base).map(|_| ())
            } else {
                unmount_one(&base)
            };
            app.set_mounts(mounts().map_err(|e| format!("{e:#}")));
            if let Err(e) = result {
                app.mount_failed(MountFailure {
                    base,
                    mounting,
                    detail: format!("{e:#}"),
                });
            }
        }
    }

    pub fn cli(config: &Path, action: &str, name: Option<&str>) -> Result<()> {
        let cfg = guard(config)?;
        match action {
            "status" => {
                println!(
                    "slot={:?} snapshot={:?}",
                    slot()?,
                    skyward_vab::snapshot_state()?
                );
                for row in mounts()? {
                    println!("{} mounted={} {}", row.base, row.mounted, row.point);
                }
            }
            "mount" => println!("{}", mount_one(name.context("--name required")?)?.display()),
            "unmount" => unmount_one(name.context("--name required")?)?,
            "list" => println!(
                "entries={}",
                crate::ui::read_folder(Path::new(name.unwrap_or("/")))?.len()
            ),
            "clock" => println!("wall_epoch_ms={}", clock_epoch()?),
            "logs" => println!("{}", logs(true, true)?.display()),
            "settings-test" => {
                metadata_ready()?;
                private_dir(Path::new(SETTINGS_DIR))?;
                let filename = format!("venus-selftest-{}.json", id());
                let at = Path::new(SETTINGS_DIR).join(&filename);
                let settings = Settings::default();
                atomic_save(
                    Path::new(SETTINGS_DIR),
                    &filename,
                    settings.text().as_bytes(),
                )?;
                let read = Settings::read(&fs::read_to_string(&at)?);
                fs::remove_file(&at)?;
                ensure!(read? == settings, "settings roundtrip mismatch");
                println!("SETTINGS_ROUNDTRIP_PASS; existing settings unchanged");
            }
            "selftest" => {
                let before = fs::read("/proc/mounts")?;
                println!(
                    "DEVICE_INFO_PASS model={}",
                    skyward_info::collect(&cfg)?.model
                );
                let rows = mounts()?;
                for row in rows {
                    if row.mounted {
                        println!("ALREADY_MOUNTED {}", row.base);
                        continue;
                    }
                    let at = mount_one(&row.base)?;
                    let name = mapping(&row.base, slot()?);
                    let count = fs::read_dir(&at)?.count();
                    println!(
                        "RO_MOUNT_PASS {} readonly={} entries={count}",
                        row.base,
                        dm::readonly(&name)?
                    );
                    unmount_one(&row.base)?;
                }
                ensure!(
                    before == fs::read("/proc/mounts")?,
                    "mount list did not return to baseline"
                );
                println!("MOUNTS_RESTORED_PASS");
                println!("FILESYSTEM_ROWS={}", filesystems()?.len());
            }
            _ => anyhow::bail!("unknown basic action"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_mapping_is_remounted_and_still_reports_failure() {
        use std::cell::RefCell;
        let calls = RefCell::new(Vec::new());
        let result = release_mapping(
            || { calls.borrow_mut().push("unmount"); Ok(()) },
            || { calls.borrow_mut().push("remove"); anyhow::bail!("EBUSY") },
            || { calls.borrow_mut().push("restore-ro"); Ok(()) },
        );
        assert!(format!("{:#}", result.unwrap_err()).contains("EBUSY"));
        assert_eq!(*calls.borrow(), ["unmount", "remove", "restore-ro"]);
    }

    #[test]
    fn a_failed_unmount_never_removes_the_mapping() {
        let result = release_mapping(
            || anyhow::bail!("mount is busy"),
            || panic!("must not remove a still-mounted device"),
            || panic!("must not cover the still-existing mount"),
        );
        assert!(result.is_err());
    }
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn superblock_signatures_require_correct_offsets() {
        for (bytes, name) in [
            ([0xe2, 0xe1, 0xf5, 0xe0], "erofs"),
            ([0x10, 0x20, 0xf5, 0xf2], "f2fs"),
        ] {
            let mut head = [0u8; 4096];
            head[1024..1028].copy_from_slice(&bytes);
            assert_eq!(sniff(&head), Some(name));
            head[1024..1028].fill(0);
            head[..4].copy_from_slice(&bytes);
            assert_eq!(sniff(&head), None);
        }
        let mut head = [0u8; 4096];
        head[1080..1082].copy_from_slice(&[0x53, 0xef]);
        assert_eq!(sniff(&head), Some("ext4"));
        assert_eq!(sniff(&head[..100]), None);
        assert_eq!(sniff(&[0xff; 4096]), None);
    }
    #[test]
    fn mount_names_are_whitelisted() {
        for name in ["system", "vendor", "odm", "product", "mi_ext"] {
            assert!(partition(name).is_ok());
        }
        for name in [
            "",
            "../vendor",
            "/system",
            "userdata",
            "metadata",
            "boot",
            "system_b",
            "misc",
            "super",
        ] {
            assert!(partition(name).is_err());
        }
    }
    #[test]
    fn readonly_ext4_does_not_replay_journal() {
        assert!(ro_options("ext4").unwrap().contains(&"noload".into()));
    }
    #[test]
    fn readonly_f2fs_does_not_recover() {
        assert!(ro_options("f2fs").unwrap().contains(&"norecovery".into()));
    }
    #[test]
    fn unknown_filesystem_is_not_mounted() {
        assert!(ro_options("auto").is_err());
    }
    #[test]
    fn atomic_settings_are_private_and_complete() {
        let tmp = tempfile::tempdir().unwrap();
        atomic_save(tmp.path(), "settings.json", b"{\"dark\":true}").unwrap();
        atomic_save(tmp.path(), "settings.json", b"{}").unwrap();
        assert_eq!(fs::read(tmp.path().join("settings.json")).unwrap(), b"{}");
        assert_eq!(
            fs::metadata(tmp.path().join("settings.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }
    #[test]
    fn settings_refuse_symlinks_and_bad_names() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("original"), b"keep").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("original"), tmp.path().join("settings"))
            .unwrap();
        assert!(atomic_save(tmp.path(), "settings", b"change").is_err());
        for name in ["../other", ".", "..", "a/b"] {
            assert!(atomic_save(tmp.path(), name, b"x").is_err());
        }
        assert_eq!(fs::read(tmp.path().join("original")).unwrap(), b"keep");
    }
    #[test]
    fn private_folder_rejects_link() {
        let tmp = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(tmp.path(), tmp.path().join("link")).unwrap();
        assert!(private_dir(&tmp.path().join("link")).is_err());
    }
}
