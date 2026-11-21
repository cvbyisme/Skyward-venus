use super::manager::*;
#[derive(Default)]
pub(super) struct Fake {
    objects: Vec<Operation>,
    adds: usize,
    removes: usize,
    fail_add: usize,
    fail_remove: bool,
    lose_ack: bool,
    foreign_table: bool,
    steal_on_add: bool,
    current_index: u32,
}
impl Fake {
    pub(super) fn new() -> Self {
        Self {
            current_index: 49,
            ..Self::default()
        }
    }
}
impl Backend for Fake {
    fn index(&mut self, _: &str) -> u32 {
        self.current_index
    }
    fn unused_table(&mut self, _: u32) -> Result<()> {
        if self.foreign_table {
            Err(-libc::EEXIST)
        } else {
            Ok(())
        }
    }
    fn exists(&mut self, op: &Operation) -> Result<bool> {
        match self.objects.iter().filter(|o| *o == op).count() {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(-libc::EUCLEAN),
        }
    }
    fn add(&mut self, op: &Operation) -> Result<()> {
        self.adds += 1;
        if self.steal_on_add {
            self.objects.push(op.clone());
            return Err(-libc::EEXIST);
        }
        if self.adds == self.fail_add && !self.lose_ack {
            return Err(-libc::EPERM);
        }
        self.objects.push(op.clone());
        if self.adds == self.fail_add {
            Err(-libc::ETIMEDOUT)
        } else {
            Ok(())
        }
    }
    fn remove(&mut self, op: &Operation) -> Result<()> {
        self.removes += 1;
        if self.fail_remove {
            return Err(-libc::EIO);
        }
        let index = self
            .objects
            .iter()
            .position(|o| o == op)
            .ok_or(-libc::ENOENT)?;
        self.objects.remove(index);
        Ok(())
    }
}
fn manager() -> Manager<Fake> {
    Manager::new(Fake::new(), "test".into())
}
#[test]
fn allocation_is_bounded_and_shutdown_refuses_new_ownership() {
    let mut m = manager();
    for id in 1..=50 {
        assert_eq!(m.create(), Ok((handle(id), mark(id))));
    }
    assert_eq!(m.create(), Err(-libc::ENONET));
    assert_eq!(m.backend.adds, 0);
    assert_eq!(m.destroy(0), Err(-libc::EINVAL));
    assert_eq!(m.destroy(handle(1) ^ 1), Err(-libc::EINVAL));
    m.destroy(handle(1)).unwrap();
    assert_eq!(m.destroy(handle(1)), Err(-libc::ENONET));
    assert_eq!(m.create().unwrap().0, handle(1));
    m.shutdown().unwrap();
    assert_eq!(m.create(), Err(-libc::ESHUTDOWN));
}
#[test]
fn route_validation_and_exact_ownership() {
    let mut m = manager();
    let (h, _) = m.create().unwrap();
    let (other, _) = m.create().unwrap();
    m.add_interface(h, "rmnet_data0").unwrap();
    m.add_interface(h, "rmnet_data0").unwrap();
    assert_eq!(m.backend.objects.len(), 8);
    assert_eq!(m.add_interface(other, "rmnet_data0"), Err(-libc::EBUSY));
    for (dest, gw) in [
        ("192.0.2.0/24", ""),
        ("0.0.0.0/0", "192.0.2.1"),
        ("2001:db8::/64", ""),
        ("::/0", "2001:db8::1"),
    ] {
        m.route(h, "rmnet_data0", dest, gw, true).unwrap();
    }
    let count = m.backend.objects.len();
    for (dest, gw) in [
        ("192.0.2.1/24", ""),
        ("0.0.0.0/0", "2001:db8::1"),
        ("0.0.0.0/0", "192.0.2.1\0bad"),
        ("::/129", ""),
        ("0.0.0.0/-1", ""),
    ] {
        assert_eq!(
            m.route(h, "rmnet_data0", dest, gw, true),
            Err(-libc::EINVAL)
        );
    }
    assert_eq!(m.backend.objects.len(), count);
    assert_eq!(
        m.route(h, "rmnet_data0", "198.51.100.0/24", "", false),
        Err(-libc::ENOENT)
    );
    m.backend.current_index = 50;
    assert_eq!(m.add_interface(h, "rmnet_data0"), Err(-libc::ESTALE));
    assert_eq!(
        m.route(h, "rmnet_data0", "192.0.2.0/24", "", true),
        Err(-libc::ESTALE)
    );
    m.backend.current_index = 49;
    m.destroy(other).unwrap();
    assert_eq!(m.backend.objects.len(), count);
    m.remove_interface(h, "rmnet_data0").unwrap();
    m.remove_interface(h, "rmnet_data0").unwrap();
    m.destroy(h).unwrap();
    assert!(m.backend.objects.is_empty());
}
#[test]
fn every_failed_add_and_lost_ack_is_rolled_back() {
    for lost in [false, true] {
        for failed in 1..=8 {
            let mut m = manager();
            m.backend.fail_add = failed;
            m.backend.lose_ack = lost;
            let h = m.create().unwrap().0;
            assert!(m.add_interface(h, "rmnet_data0").is_err());
            assert!(m.backend.objects.is_empty());
            m.destroy(h).unwrap();
        }
    }
}
#[test]
fn failed_cleanup_retains_receipts_for_shutdown_retry() {
    let mut m = manager();
    let h = m.create().unwrap().0;
    m.add_interface(h, "rmnet_data0").unwrap();
    m.backend.fail_remove = true;
    assert_eq!(m.shutdown(), Err(-libc::EIO));
    assert_eq!(m.backend.objects.len(), 8);
    assert_eq!(m.create(), Err(-libc::ESHUTDOWN));
    m.backend.fail_remove = false;
    m.shutdown().unwrap();
    assert!(m.backend.objects.is_empty());
}
#[test]
fn foreign_tables_and_concurrent_eexist_are_never_adopted() {
    let mut m = manager();
    let h = m.create().unwrap().0;
    m.backend.foreign_table = true;
    assert_eq!(m.add_interface(h, "rmnet_data0"), Err(-libc::EEXIST));
    assert_eq!(m.backend.adds, 0);
    m.backend.foreign_table = false;
    m.backend.steal_on_add = true;
    assert_eq!(m.add_interface(h, "rmnet_data0"), Err(-libc::EEXIST));
    assert_eq!(m.backend.objects.len(), 1);
    assert_eq!(m.backend.removes, 0);
    m.shutdown().unwrap();
    assert_eq!(m.backend.objects.len(), 1);
}

#[test]
#[ignore = "requires an isolated user/network namespace and netdtest0 dummy device"]
fn isolated_real_kernel_rules_routes_and_mark_cleanup() {
    use super::backend::LinuxBackend;
    use std::os::unix::fs::MetadataExt;
    let initial: u64 = std::env::var("SKYWARD_TEST_INITIAL_NETNS")
        .expect("original netns required")
        .parse()
        .unwrap();
    let current = std::fs::metadata("/proc/self/ns/net").unwrap().ino();
    assert!(
        initial != 0 && current != initial,
        "must not mutate the caller's network namespace"
    );
    let mut kernel = LinuxBackend {
        tool_dir: "/usr/sbin".into(),
    };
    let index = kernel.index("netdtest0");
    assert!(index > 0);
    kernel.unused_table(index + 1000).unwrap();
    let mut owned = Vec::new();
    for op in interface_operations(1, "netdtest0", index, "test")
        .into_iter()
        .filter(|o| o.kind != Kind::Mark)
    {
        assert_eq!(kernel.exists(&op), Ok(false));
        kernel.add(&op).unwrap();
        assert_eq!(kernel.exists(&op), Ok(true));
        owned.push(op);
    }
    assert_eq!(kernel.unused_table(index + 1000), Err(-libc::EEXIST));
    for (dest, gw) in [
        ("192.0.2.0/24", ""),
        ("0.0.0.0/0", "192.0.2.1"),
        ("2001:db8::/64", ""),
        ("::/0", "2001:db8::1"),
    ] {
        let mut op = Operation {
            kind: Kind::Route,
            iface: "netdtest0".into(),
            index,
            table: index + 1000,
            ..Operation::default()
        };
        op.parse_route(dest, gw).unwrap();
        assert_eq!(kernel.exists(&op), Ok(false));
        kernel.add(&op).unwrap();
        assert_eq!(kernel.exists(&op), Ok(true));
        assert_eq!(kernel.add(&op), Err(-libc::EEXIST));
        owned.push(op);
    }
    for op in owned.iter().rev() {
        kernel.remove(op).unwrap();
        assert_eq!(kernel.exists(op), Ok(false));
    }
    kernel.unused_table(index + 1000).unwrap();
    let mut manager = Manager::new(kernel, format!("skyward-oem-test-{}", std::process::id()));
    let h = manager.create().unwrap().0;
    manager.add_interface(h, "netdtest0").unwrap();
    for (dest, gw) in [
        ("192.0.2.0/24", ""),
        ("0.0.0.0/0", "192.0.2.1"),
        ("2001:db8::/64", ""),
        ("::/0", "2001:db8::1"),
    ] {
        manager.route(h, "netdtest0", dest, gw, true).unwrap();
    }
    manager.shutdown().unwrap();
    manager.backend.unused_table(index + 1000).unwrap();
}
