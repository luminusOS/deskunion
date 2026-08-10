use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    rc::Rc,
};

use slab::Slab;

use deskunion_ipc::{ClientConfig, ClientHandle, ClientState, Position};

use crate::config::ConfigClient;

#[derive(Clone, Default)]
pub struct ClientManager {
    clients: Rc<RefCell<Slab<(ClientConfig, ClientState)>>>,
    /// authorized devices that connected in but are not paired to a
    /// client entry yet: fingerprint -> source address. A parked
    /// connection stays up (it answers pings and streams state) but
    /// cannot be entered until the user assigns it a position, which
    /// binds the fingerprint to a client handle.
    parked: Rc<RefCell<HashMap<String, SocketAddr>>>,
}

impl ClientManager {
    /// get all clients
    pub fn clients(&self) -> Vec<(ClientConfig, ClientState)> {
        self.clients
            .borrow()
            .iter()
            .map(|(_, c)| c.clone())
            .collect::<Vec<_>>()
    }

    pub fn add_with_config(&self, config_client: ConfigClient) -> ClientHandle {
        let config = ClientConfig {
            hostname: config_client.hostname,
            fix_ips: config_client.ips.into_iter().collect(),
            port: config_client.port,
            pos: config_client.pos,
            cmd: config_client.enter_hook,
            fingerprint: config_client.fingerprint,
        };
        let state = ClientState {
            active: config_client.active,
            ips: HashSet::from_iter(config.fix_ips.iter().cloned()),
            ..Default::default()
        };
        let handle = self.add_client();
        self.set_config(handle, config);
        self.set_state(handle, state);
        handle
    }

    /// add a new client to this manager
    pub fn add_client(&self) -> ClientHandle {
        self.clients.borrow_mut().insert(Default::default()) as ClientHandle
    }

    /// set the config of the given client
    pub fn set_config(&self, handle: ClientHandle, config: ClientConfig) {
        if let Some((c, _)) = self.clients.borrow_mut().get_mut(handle as usize) {
            *c = config;
        }
    }

    /// set the state of the given client
    pub fn set_state(&self, handle: ClientHandle, state: ClientState) {
        if let Some((_, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            *s = state;
        }
    }

    /// activate the given client
    /// returns, whether the client was activated
    pub fn activate_client(&self, handle: ClientHandle) -> bool {
        let mut clients = self.clients.borrow_mut();
        match clients.get_mut(handle as usize) {
            Some((_, s)) if !s.active => {
                s.active = true;
                true
            }
            _ => false,
        }
    }

    /// deactivate the given client
    /// returns, whether the client was deactivated
    pub fn deactivate_client(&self, handle: ClientHandle) -> bool {
        let mut clients = self.clients.borrow_mut();
        match clients.get_mut(handle as usize) {
            Some((_, s)) if s.active => {
                s.active = false;
                true
            }
            _ => false,
        }
    }

    /// find a client by its address
    pub fn get_client(&self, addr: SocketAddr) -> Option<ClientHandle> {
        // since there shouldn't be more than a handful of clients at any given
        // time this is likely faster than using a HashMap
        self.clients
            .borrow()
            .iter()
            .find_map(|(k, (_, s))| {
                if s.active && s.ips.contains(&addr.ip()) {
                    Some(k)
                } else {
                    None
                }
            })
            .map(|p| p as ClientHandle)
    }

    /// find a client by its paired certificate fingerprint — the stable
    /// identity of an incoming connection (source IPs break behind NAT)
    pub fn get_client_by_fingerprint(&self, fingerprint: &str) -> Option<ClientHandle> {
        self.clients
            .borrow()
            .iter()
            .find_map(|(k, (c, _))| {
                if c.fingerprint.as_deref() == Some(fingerprint) {
                    Some(k)
                } else {
                    None
                }
            })
            .map(|p| p as ClientHandle)
    }

    /// find a client by the address its accepted connection is bound to
    pub(crate) fn get_client_by_active_addr(&self, addr: SocketAddr) -> Option<ClientHandle> {
        self.clients
            .borrow()
            .iter()
            .find_map(|(k, (_, s))| {
                if s.active_addr == Some(addr) {
                    Some(k)
                } else {
                    None
                }
            })
            .map(|p| p as ClientHandle)
    }

    /// remember a connected-but-unpaired device
    pub(crate) fn park(&self, fingerprint: String, addr: SocketAddr) {
        self.parked.borrow_mut().insert(fingerprint, addr);
    }

    /// take the address of a parked device (on pairing via position
    /// assignment); `None` if the device is not currently connected
    pub(crate) fn unpark(&self, fingerprint: &str) -> Option<SocketAddr> {
        self.parked.borrow_mut().remove(fingerprint)
    }

    /// drop a parked device by its connection address (on disconnect);
    /// returns the fingerprint if the address belonged to a parked device
    pub(crate) fn unpark_addr(&self, addr: SocketAddr) -> Option<String> {
        let fingerprint = self
            .parked
            .borrow()
            .iter()
            .find(|(_, a)| **a == addr)
            .map(|(f, _)| f.clone());
        if let Some(fingerprint) = &fingerprint {
            self.parked.borrow_mut().remove(fingerprint);
        }
        fingerprint
    }

    /// get the client at the given position
    pub fn client_at(&self, pos: Position) -> Option<ClientHandle> {
        self.clients
            .borrow()
            .iter()
            .find_map(|(k, (c, s))| {
                if s.active && c.pos == pos {
                    Some(k)
                } else {
                    None
                }
            })
            .map(|p| p as ClientHandle)
    }

    /// first screen edge with no configured client, in pairing
    /// preference order (right → left → top → bottom). A configured
    /// client blocks its edge whether or not it is currently active —
    /// used to auto-pair a newly connected device without asking for
    /// a position. `None` when all four edges are occupied.
    pub(crate) fn first_free_position(&self) -> Option<Position> {
        [
            Position::Right,
            Position::Left,
            Position::Top,
            Position::Bottom,
        ]
        .into_iter()
        .find(|pos| {
            !self
                .clients
                .borrow()
                .iter()
                .any(|(_, (c, _))| c.pos == *pos)
        })
    }

    pub(crate) fn get_hostname(&self, handle: ClientHandle) -> Option<String> {
        self.clients
            .borrow_mut()
            .get_mut(handle as usize)
            .and_then(|(c, _)| c.hostname.clone())
    }

    /// get the position of the corresponding client
    pub(crate) fn get_pos(&self, handle: ClientHandle) -> Option<Position> {
        self.clients
            .borrow()
            .get(handle as usize)
            .map(|(c, _)| c.pos)
    }

    /// remove a client from the list
    pub fn remove_client(&self, client: ClientHandle) -> Option<(ClientConfig, ClientState)> {
        // remove id from occupied ids
        self.clients.borrow_mut().try_remove(client as usize)
    }

    /// get the config & state of the given client
    pub fn get_state(&self, handle: ClientHandle) -> Option<(ClientConfig, ClientState)> {
        self.clients.borrow().get(handle as usize).cloned()
    }

    /// get the current config & state of all clients
    pub fn get_client_states(&self) -> Vec<(ClientHandle, ClientConfig, ClientState)> {
        self.clients
            .borrow()
            .iter()
            .map(|(k, v)| (k as ClientHandle, v.0.clone(), v.1.clone()))
            .collect()
    }

    /// update the fix ips of the client
    pub fn set_fix_ips(&self, handle: ClientHandle, fix_ips: Vec<IpAddr>) {
        if let Some((c, _)) = self.clients.borrow_mut().get_mut(handle as usize) {
            c.fix_ips = fix_ips
        }
        self.update_ips(handle);
    }

    /// update the dns-ips of the client
    pub fn set_dns_ips(&self, handle: ClientHandle, dns_ips: Vec<IpAddr>) {
        if let Some((_, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            s.dns_ips = dns_ips
        }
        self.update_ips(handle);
    }

    fn update_ips(&self, handle: ClientHandle) {
        if let Some((c, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            s.ips = c
                .fix_ips
                .iter()
                .cloned()
                .chain(s.dns_ips.iter().cloned())
                .collect::<HashSet<_>>();
        }
    }

    /// update the hostname of the given client
    /// this automatically clears the active ip address and ips from dns
    pub fn set_hostname(&self, handle: ClientHandle, hostname: Option<String>) -> bool {
        let mut clients = self.clients.borrow_mut();
        let Some((c, s)) = clients.get_mut(handle as usize) else {
            return false;
        };

        // hostname changed
        if c.hostname != hostname {
            c.hostname = hostname;
            s.active_addr = None;
            s.dns_ips.clear();
            drop(clients);
            self.update_ips(handle);
            true
        } else {
            false
        }
    }

    /// update the port of the client
    pub(crate) fn set_port(&self, handle: ClientHandle, port: u16) {
        match self.clients.borrow_mut().get_mut(handle as usize) {
            Some((c, s)) if c.port != port => {
                c.port = port;
                s.active_addr = s.active_addr.map(|a| SocketAddr::new(a.ip(), port));
            }
            _ => {}
        };
    }

    /// update the position of the client
    /// returns true, if a change in capture position is required (pos changed & client is active)
    pub(crate) fn set_pos(&self, handle: ClientHandle, pos: Position) -> bool {
        match self.clients.borrow_mut().get_mut(handle as usize) {
            Some((c, s)) if c.pos != pos => {
                log::info!("update pos {handle} {} -> {}", c.pos, pos);
                c.pos = pos;
                s.active
            }
            _ => false,
        }
    }

    /// update the enter hook command of the client
    pub(crate) fn set_enter_hook(&self, handle: ClientHandle, enter_hook: Option<String>) {
        if let Some((c, _s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            c.cmd = enter_hook;
        }
    }

    /// set resolving status of the client
    pub(crate) fn set_resolving(&self, handle: ClientHandle, status: bool) {
        if let Some((_, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            s.resolving = status;
        }
    }

    /// get the enter hook command
    pub(crate) fn get_enter_cmd(&self, handle: ClientHandle) -> Option<String> {
        self.clients
            .borrow()
            .get(handle as usize)
            .and_then(|(c, _)| c.cmd.clone())
    }

    /// returns all clients that are currently registered
    pub(crate) fn registered_clients(&self) -> Vec<ClientHandle> {
        self.clients
            .borrow()
            .iter()
            .map(|(h, _)| h as ClientHandle)
            .collect()
    }

    /// returns all clients that are currently active
    pub(crate) fn active_clients(&self) -> Vec<ClientHandle> {
        self.clients
            .borrow()
            .iter()
            .filter(|(_, (_, s))| s.active)
            .map(|(h, _)| h as ClientHandle)
            .collect()
    }

    pub(crate) fn set_active_addr(&self, handle: ClientHandle, addr: Option<SocketAddr>) {
        if let Some((_, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            s.active_addr = addr;
        }
    }

    pub(crate) fn set_alive(&self, handle: ClientHandle, alive: bool) {
        if let Some((_, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            s.alive = alive;
        }
    }

    pub(crate) fn set_peer_commit(&self, handle: ClientHandle, commit: Option<[u8; 8]>) {
        if let Some((_, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            s.peer_commit = commit;
        }
    }

    pub(crate) fn active_addr(&self, handle: ClientHandle) -> Option<SocketAddr> {
        self.clients
            .borrow()
            .get(handle as usize)
            .and_then(|(_, s)| s.active_addr)
    }

    pub(crate) fn alive(&self, handle: ClientHandle) -> bool {
        self.clients
            .borrow()
            .get(handle as usize)
            .map(|(_, s)| s.alive)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn paired_manager(fingerprint: &str) -> (ClientManager, ClientHandle) {
        let manager = ClientManager::default();
        let handle = manager.add_client();
        manager.set_config(
            handle,
            ClientConfig {
                fingerprint: Some(fingerprint.to_owned()),
                ..Default::default()
            },
        );
        (manager, handle)
    }

    #[test]
    fn fingerprint_lookup_finds_paired_client() {
        let (manager, handle) = paired_manager("aa:bb");
        assert_eq!(manager.get_client_by_fingerprint("aa:bb"), Some(handle));
        assert_eq!(manager.get_client_by_fingerprint("cc:dd"), None);
    }

    #[test]
    fn active_addr_reverse_lookup() {
        let (manager, handle) = paired_manager("aa:bb");
        let addr: SocketAddr = "10.0.2.15:4242".parse().expect("addr");
        assert_eq!(manager.get_client_by_active_addr(addr), None);
        manager.set_active_addr(handle, Some(addr));
        assert_eq!(manager.get_client_by_active_addr(addr), Some(handle));
        manager.set_active_addr(handle, None);
        assert_eq!(manager.get_client_by_active_addr(addr), None);
    }

    /// a connected-but-unpaired device is parked: it has no client
    /// handle, so no capture barrier can ever be created for it and
    /// `Enter` can never be routed to it — until the user assigns a
    /// position, which binds fingerprint -> handle -> active addr.
    #[test]
    fn parked_device_cannot_be_entered() {
        let manager = ClientManager::default();
        let addr: SocketAddr = "10.0.2.15:4242".parse().expect("addr");
        manager.park("aa:bb".to_owned(), addr);

        // no handle exists for the parked fingerprint ...
        assert_eq!(manager.get_client_by_fingerprint("aa:bb"), None);
        // ... and no client is bound to its address
        assert_eq!(manager.get_client_by_active_addr(addr), None);

        // assigning a position pairs the device: the parked connection
        // becomes the new client's active address
        let handle = manager.add_client();
        manager.set_config(
            handle,
            ClientConfig {
                fingerprint: Some("aa:bb".to_owned()),
                ..Default::default()
            },
        );
        let parked_addr = manager.unpark("aa:bb");
        assert_eq!(parked_addr, Some(addr));
        manager.set_active_addr(handle, parked_addr);
        manager.activate_client(handle);
        assert_eq!(manager.get_client_by_fingerprint("aa:bb"), Some(handle));
        assert_eq!(manager.get_client_by_active_addr(addr), Some(handle));
    }

    /// auto-pairing picks the right edge first and only falls back to
    /// the manual prompt once all four edges are configured
    #[test]
    fn first_free_position_prefers_right_then_falls_back() {
        let manager = ClientManager::default();
        assert_eq!(manager.first_free_position(), Some(Position::Right));

        let occupy = |pos: Position| {
            let handle = manager.add_client();
            manager.set_config(
                handle,
                ClientConfig {
                    pos,
                    ..Default::default()
                },
            );
        };

        occupy(Position::Right);
        assert_eq!(manager.first_free_position(), Some(Position::Left));
        occupy(Position::Left);
        assert_eq!(manager.first_free_position(), Some(Position::Top));
        occupy(Position::Top);
        assert_eq!(manager.first_free_position(), Some(Position::Bottom));
        occupy(Position::Bottom);
        assert_eq!(manager.first_free_position(), None);
    }

    /// an inactive client still blocks its edge: pairing a new device
    /// there would silently deactivate the existing one
    #[test]
    fn first_free_position_counts_inactive_clients() {
        let manager = ClientManager::default();
        let handle = manager.add_client();
        manager.set_config(
            handle,
            ClientConfig {
                pos: Position::Right,
                ..Default::default()
            },
        );
        manager.deactivate_client(handle);
        assert_eq!(manager.first_free_position(), Some(Position::Left));
    }

    #[test]
    fn unpark_addr_only_removes_parked_devices() {
        let (manager, handle) = paired_manager("aa:bb");
        let addr: SocketAddr = "10.0.2.15:4242".parse().expect("addr");
        manager.set_active_addr(handle, Some(addr));
        // a paired connection's address must not be confused with a
        // parked one on disconnect
        assert_eq!(manager.unpark_addr(addr), None);
        assert_eq!(manager.get_client_by_active_addr(addr), Some(handle));

        let parked_addr: SocketAddr = "10.0.2.16:4242".parse().expect("addr");
        manager.park("cc:dd".to_owned(), parked_addr);
        assert_eq!(manager.unpark_addr(parked_addr).as_deref(), Some("cc:dd"));
        assert_eq!(manager.unpark("cc:dd"), None);
    }
}
