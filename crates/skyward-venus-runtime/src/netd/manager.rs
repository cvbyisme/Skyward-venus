use std::{collections::BTreeMap, net::IpAddr};
pub type Result<T> = std::result::Result<T, i32>;
pub fn handle(id: u32) -> u64 {
    ((id as u64) << 32) | 0xcafed00d
}
pub fn mark(id: u32) -> u32 {
    id | 0xf0000
}
pub fn netid(handle: u64) -> Result<u32> {
    let id = (handle >> 32) as u32;
    if handle as u32 == 0xcafed00d && (1..=50).contains(&id) {
        Ok(id)
    } else {
        Err(-libc::EINVAL)
    }
}
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 16
        && !["lo", ".", ".."].contains(&name)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Rule,
    Route,
    Mark,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation {
    pub kind: Kind,
    pub family: u8,
    pub table: u32,
    pub index: u32,
    pub id: u32,
    pub priority: u32,
    pub value: u32,
    pub mask: u32,
    pub root_only: bool,
    pub gateway: bool,
    pub prefix: u8,
    pub iface: String,
    pub oif: String,
    pub tag: String,
    pub destination: [u8; 16],
    pub next: [u8; 16],
}
impl Default for Operation {
    fn default() -> Self {
        Self {
            kind: Kind::Rule,
            family: libc::AF_INET as u8,
            table: 0,
            index: 0,
            id: 0,
            priority: 0,
            value: 0,
            mask: 0,
            root_only: false,
            gateway: false,
            prefix: 0,
            iface: String::new(),
            oif: String::new(),
            tag: String::new(),
            destination: [0; 16],
            next: [0; 16],
        }
    }
}
impl Operation {
    pub fn parse_route(&mut self, destination: &str, next: &str) -> Result<()> {
        if destination.len() > 64
            || next.len() > 46
            || destination.contains('\0')
            || next.contains('\0')
        {
            return Err(-libc::EINVAL);
        }
        let (address, bits) = destination.split_once('/').ok_or(-libc::EINVAL)?;
        if bits.is_empty() || bits.len() > 3 || !bits.bytes().all(|c| c.is_ascii_digit()) {
            return Err(-libc::EINVAL);
        }
        let prefix: u8 = bits.parse().map_err(|_| -libc::EINVAL)?;
        let ip: IpAddr = address.parse().map_err(|_| -libc::EINVAL)?;
        let size = match ip {
            IpAddr::V4(ip) => {
                self.family = libc::AF_INET as u8;
                self.destination[..4].copy_from_slice(&ip.octets());
                4
            }
            IpAddr::V6(ip) => {
                self.family = libc::AF_INET6 as u8;
                self.destination = ip.octets();
                16
            }
        };
        if prefix as usize > size * 8 {
            return Err(-libc::EINVAL);
        }
        for bit in prefix as usize..size * 8 {
            if self.destination[bit / 8] & (1 << (7 - bit % 8)) != 0 {
                return Err(-libc::EINVAL);
            }
        }
        self.prefix = prefix;
        self.gateway = !next.is_empty();
        if self.gateway {
            match next.parse::<IpAddr>().map_err(|_| -libc::EINVAL)? {
                IpAddr::V4(ip) if size == 4 => self.next[..4].copy_from_slice(&ip.octets()),
                IpAddr::V6(ip) if size == 16 => self.next = ip.octets(),
                _ => return Err(-libc::EINVAL),
            }
        }
        Ok(())
    }
}
pub fn interface_operations(id: u32, iface: &str, index: u32, tag: &str) -> Vec<Operation> {
    let mut result = Vec::new();
    for family in [libc::AF_INET as u8, libc::AF_INET6 as u8] {
        let mut op = Operation {
            kind: Kind::Mark,
            family,
            id,
            table: 1000 + index,
            index,
            iface: iface.into(),
            tag: tag.into(),
            value: mark(id),
            mask: 0xffefffff,
            ..Operation::default()
        };
        result.push(op.clone());
        op.kind = Kind::Rule;
        op.value = id | 0xd0000;
        op.mask = 0xdffff;
        op.priority = 13000;
        result.push(op.clone());
        op.value = 0;
        op.mask = 0;
        op.priority = 10500;
        op.oif = iface.into();
        op.root_only = true;
        result.push(op.clone());
        op.value = 0xc0000;
        op.mask = 0xc0000;
        op.priority = 14000;
        op.root_only = false;
        result.push(op);
    }
    result
}
pub trait Backend {
    fn index(&mut self, name: &str) -> u32;
    fn unused_table(&mut self, table: u32) -> Result<()>;
    /// True only for one exact object. Ambiguous duplicates are errors.
    fn exists(&mut self, op: &Operation) -> Result<bool>;
    fn add(&mut self, op: &Operation) -> Result<()>;
    fn remove(&mut self, op: &Operation) -> Result<()>;
}
#[derive(Default)]
struct Interface {
    index: u32,
    ready: bool,
    owned: Vec<Operation>,
}
type Network = BTreeMap<String, Interface>;
pub struct Manager<B> {
    pub backend: B,
    tag: String,
    networks: BTreeMap<u32, Network>,
    closing: bool,
}
fn clear<B: Backend>(backend: &mut B, iface: &mut Interface) -> Result<()> {
    let mut first = Ok(());
    for i in (0..iface.owned.len()).rev() {
        let op = &iface.owned[i];
        let result = backend
            .exists(op)
            .and_then(|found| if found { backend.remove(op) } else { Ok(()) })
            .and_then(|()| {
                if backend.exists(op)? {
                    Err(-libc::EBUSY)
                } else {
                    Ok(())
                }
            });
        if result.is_ok() {
            iface.owned.remove(i);
        } else if first.is_ok() {
            first = result;
        }
    }
    iface.ready = false;
    first
}
fn acquire<B: Backend>(backend: &mut B, iface: &mut Interface, op: Operation) -> Result<()> {
    if backend.exists(&op)? {
        return Err(-libc::EEXIST);
    }
    iface.owned.push(op.clone()); // Record before syscall: a lost ACK may have changed the kernel.
    let result = backend.add(&op);
    if result == Err(-libc::EEXIST) {
        iface.owned.pop();
        return result;
    } // Foreign concurrent writer.
    let after = backend.exists(&op);
    let result = result.and(match after {
        Ok(true) => Ok(()),
        Ok(false) => Err(-libc::EIO),
        Err(e) => Err(e),
    });
    if result.is_err() && after == Ok(false) {
        iface.owned.pop();
    }
    result
}
impl<B: Backend> Manager<B> {
    pub fn new(backend: B, tag: String) -> Self {
        Self {
            backend,
            tag,
            networks: BTreeMap::new(),
            closing: false,
        }
    }
    fn lookup(&self, h: u64) -> Result<u32> {
        let id = netid(h)?;
        if self.networks.contains_key(&id) {
            Ok(id)
        } else {
            Err(-libc::ENONET)
        }
    }
    pub fn create(&mut self) -> Result<(u64, u32)> {
        if self.closing {
            return Err(-libc::ESHUTDOWN);
        }
        for id in 1..=50 {
            if let std::collections::btree_map::Entry::Vacant(e) = self.networks.entry(id) {
                e.insert(Network::new());
                return Ok((handle(id), mark(id)));
            }
        }
        Err(-libc::ENONET)
    }
    pub fn add_interface(&mut self, h: u64, name: &str) -> Result<()> {
        if self.closing {
            return Err(-libc::ESHUTDOWN);
        }
        let id = self.lookup(h)?;
        if !valid_name(name) {
            return Err(-libc::EINVAL);
        }
        if let Some(old) = self.networks[&id].get(name) {
            if !old.ready {
                return Err(-libc::EBUSY);
            }
            if self.backend.index(name) != old.index {
                return Err(-libc::ESTALE);
            }
            for op in &old.owned {
                if self.backend.exists(op) != Ok(true) {
                    return Err(-libc::EIO);
                }
            }
            return Ok(());
        }
        if self.networks.values().any(|n| n.contains_key(name)) {
            return Err(-libc::EBUSY);
        }
        let index = self.backend.index(name);
        if index == 0 || index > u32::MAX - 1000 {
            return Err(-libc::ENODEV);
        }
        self.backend.unused_table(index + 1000)?;
        let network = self.networks.get_mut(&id).unwrap();
        let iface = network.entry(name.into()).or_insert(Interface {
            index,
            ..Interface::default()
        });
        for op in interface_operations(id, name, index, &format!("{}-{id}", self.tag)) {
            if let Err(error) = acquire(&mut self.backend, iface, op) {
                let cleanup = clear(&mut self.backend, iface);
                if iface.owned.is_empty() {
                    network.remove(name);
                }
                return cleanup.and(Err(error));
            }
        }
        if self.backend.index(name) != index {
            let cleanup = clear(&mut self.backend, iface);
            if iface.owned.is_empty() {
                network.remove(name);
            }
            return cleanup.and(Err(-libc::ESTALE));
        }
        iface.ready = true;
        Ok(())
    }
    pub fn remove_interface(&mut self, h: u64, name: &str) -> Result<()> {
        let id = self.lookup(h)?;
        if !valid_name(name) {
            return Err(-libc::EINVAL);
        }
        let network = self.networks.get_mut(&id).unwrap();
        let Some(iface) = network.get_mut(name) else {
            return Ok(());
        };
        let result = clear(&mut self.backend, iface);
        if iface.owned.is_empty() {
            network.remove(name);
        }
        result
    }
    pub fn route(
        &mut self,
        h: u64,
        name: &str,
        destination: &str,
        gateway: &str,
        add: bool,
    ) -> Result<()> {
        if self.closing && add {
            return Err(-libc::ESHUTDOWN);
        }
        let id = self.lookup(h)?;
        if !valid_name(name) {
            return Err(-libc::EINVAL);
        }
        let iface = self
            .networks
            .get_mut(&id)
            .unwrap()
            .get_mut(name)
            .filter(|i| i.ready)
            .ok_or(-libc::ENODEV)?;
        if self.backend.index(name) != iface.index {
            return Err(-libc::ESTALE);
        }
        let mut op = Operation {
            kind: Kind::Route,
            id,
            iface: name.into(),
            index: iface.index,
            table: iface.index + 1000,
            ..Operation::default()
        };
        op.parse_route(destination, gateway)?;
        let found = iface.owned.iter().position(|o| *o == op);
        if add {
            if found.is_some() {
                return if self.backend.exists(&op) == Ok(true) {
                    Ok(())
                } else {
                    Err(-libc::EIO)
                };
            }
            return acquire(&mut self.backend, iface, op);
        }
        let index = found.ok_or(-libc::ENOENT)?;
        if self.backend.exists(&op)? {
            self.backend.remove(&op)?;
        }
        if self.backend.exists(&op)? {
            return Err(-libc::EBUSY);
        }
        iface.owned.remove(index);
        Ok(())
    }
    pub fn destroy(&mut self, h: u64) -> Result<()> {
        let id = self.lookup(h)?;
        let network = self.networks.get_mut(&id).unwrap();
        let mut first = Ok(());
        network.retain(|_, iface| {
            let result = clear(&mut self.backend, iface);
            if first.is_ok() {
                first = result;
            }
            !iface.owned.is_empty()
        });
        if network.is_empty() {
            self.networks.remove(&id);
        }
        first
    }
    pub fn shutdown(&mut self) -> Result<()> {
        self.closing = true;
        let ids: Vec<u32> = self.networks.keys().copied().collect();
        let mut first = Ok(());
        for id in ids {
            let result = self.destroy(handle(id));
            if first.is_ok() {
                first = result;
            }
        }
        first
    }
}
