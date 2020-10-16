use std::cmp::{Eq, PartialEq};
use std::default::Default;
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::{Arc, RwLock};
use std::thread::{spawn, JoinHandle};
use std::time::{Duration, SystemTime};
/// SSTP multicast address.
const DEFAULT_V4_MULTICAST_ADDRESS: [u8; 4] = [239, 255, 255, 250];

/// Default port number.
const DEFAULT_PORT_NUMBER: u16 = 9911;

/// Default binding address.
const INADDR_ANY: [u8; 4] = [0, 0, 0, 0];

/// Default amount of time between multicast transmission.
const DEFAULT_TRANSMISSION_DELAY: Duration = Duration::from_millis(250);

/// Default timeout time.
const DEFAULT_TRANSMISSION_TIMEOUT: Duration = Duration::from_secs(60);

/// Default transmitted message.
const DEFAULT_MESSAGE: &[u8] = b"discover";

macro_rules! concurrent {
        ($($x:expr);*;) => {
            {

            spawn(move || {
                $(
                    $x;
                )*

            })
        }
    }
}

#[derive(Debug, Eq, Clone)]
pub struct Peer {
    ip: IpAddr,
    data: Vec<u8>,
}

impl PartialEq for Peer {
    fn eq(&self, other: &Self) -> bool {
        return self.ip == other.ip;
    }

    fn ne(&self, other: &Self) -> bool {
        !self.eq(other)
    }
}

#[derive(Clone)]
pub struct Discovery {
    multicast_addr: SocketAddr,
    binding_addr: SocketAddr,
    transmission_delay: Duration,
    transmission_timeout: Duration,
    message: Vec<u8>,
}

impl Default for Discovery {
    fn default() -> Self {
        Self {
            multicast_addr: SocketAddr::from((DEFAULT_V4_MULTICAST_ADDRESS, DEFAULT_PORT_NUMBER)),
            binding_addr: SocketAddr::from((INADDR_ANY, DEFAULT_PORT_NUMBER)),
            transmission_delay: DEFAULT_TRANSMISSION_DELAY,
            transmission_timeout: DEFAULT_TRANSMISSION_TIMEOUT,
            message: DEFAULT_MESSAGE.to_vec(),
        }
    }
}

impl Discovery {
    /// Example #1
    /// ```rust
    /// # use std::error::Error;
    /// # use std::thread::sleep;
    /// # use std::time::Duration;
    /// # use oxidiscovery::{Discovery, Peer};
    /// #
    /// # fn main() -> Result<(), Box<dyn Error>> {
    ///   let peer_discovery: Discovery = Default::default();
    ///
    ///   // peer_list was cloned and moved here
    ///   let mut manager = peer_discovery
    ///       .discover(move |peer_list| {
    ///           println!("{:?}", peer_list);
    ///       })?;
    ///
    ///    // Will take 60 seconds for the timeout to hit.
    ///    manager.wait();
    /// #
    /// #  Ok(())
    /// # }
    /// ```
    pub fn discover<C: Fn(&mut Vec<Peer>) + std::marker::Send + 'static>(
        &self,
        callback: C,
    ) -> Result<DiscoveryManagment, Box<dyn std::error::Error>> {
        let Discovery {
            multicast_addr,
            binding_addr,
            transmission_delay,
            transmission_timeout,
            message,
        } = self.clone();

        let peerlist = Arc::new(RwLock::new(Vec::new()));
        let stopper = Arc::new(RwLock::new(false));

        // Setup stoppers
        let (broadcast_stopper, receiver_stopper) = (stopper.clone(), stopper.clone());

        // Create the discovery socket.
        let send_socket = UdpSocket::bind(binding_addr).expect("Could not bind!");
        let receive_socket = send_socket
            .try_clone()
            .expect("Could not clone the socket!");

        // Start the broadcasting, pass the stopper.
        let broadcast_thread = concurrent! {
            broadcast(send_socket, &multicast_addr, transmission_delay, transmission_timeout, message, &broadcast_stopper).unwrap();
        };

        let peers = peerlist.clone();
        // Listen for broadcast.
        let receiver_thread = concurrent! {
            receiver(
                receive_socket,
                multicast_addr.ip(),
                binding_addr.ip(),
                &peers,
                callback,
                &receiver_stopper,
            ).unwrap();
        };
        let manager = DiscoveryManagment {
            broadcast_thread: Some(broadcast_thread),
            receiver_thread: Some(receiver_thread),
            peerlist,
            stopper,
        };
        Ok(manager)
    }
}

pub struct DiscoveryManagment {
    broadcast_thread: Option<JoinHandle<()>>,
    receiver_thread: Option<JoinHandle<()>>,
    peerlist: Arc<RwLock<Vec<Peer>>>,
    stopper: Arc<RwLock<bool>>,
}
impl DiscoveryManagment {
    pub fn stop(&mut self) {
        {
            let mut w_stop = self.stopper.write().expect("Deadlock");
            *w_stop = true;
        }

        if let Some(h) = self.broadcast_thread.take() {
            h.join().expect("Deadlock");
        }
        if let Some(h) = self.receiver_thread.take() {
            h.join().expect("Deadlock");
        }
    }

    #[allow(dead_code)]
    pub fn get_peers(&self) -> Vec<Peer> {
        let peerlist_guard = self.peerlist.read().expect("Deadlock");
        return (*peerlist_guard).clone();
    }
}
impl Drop for DiscoveryManagment {
    fn drop(&mut self) {
        if self.broadcast_thread.is_some() || self.receiver_thread.is_some() {
            //panic!("You MUST call either join on `C` to clean it up.");
            println!("Who?{:?}", self.receiver_thread.is_some());
        }
    }
}

fn receiver<C: Fn(&mut Vec<Peer>) + std::marker::Send + 'static>(
    socket: UdpSocket,
    multicast_address: IpAddr,
    binding_addr: IpAddr,
    peerlist: &RwLock<Vec<Peer>>,
    callback: C,
    stopper: &RwLock<bool>,
) -> Result<(), io::Error> {
    match (multicast_address, binding_addr) {
        (IpAddr::V4(mip), IpAddr::V4(bip)) => socket.join_multicast_v4(&mip, &bip),
        (IpAddr::V6(mip), _) => socket.join_multicast_v6(&mip, 0), // How do i build it good...
        _ => panic!("Can't combine ipv4 and ipv6."),
    }?;

    let mut buff = [0u8; 65535];
    loop {
        {
            let stop = stopper.read().expect("Deadlock");
            if *stop {
                return Ok(());
            }
        }

        // Might want this to be dynamic.
        socket.set_read_timeout(Some(DEFAULT_TRANSMISSION_DELAY))?;

        // Receive data non-blocking.
        match socket.recv_from(&mut buff) {
            Ok((amount, src)) => {
                // Succsess
                let peer = Peer {
                    ip: src.ip(),
                    data: buff[0..amount].to_vec(),
                };
                let mut peerlist_guard = peerlist.write().expect("Deadlock");
                if !peerlist_guard.contains(&peer) {
                    peerlist_guard.push(peer);
                    callback(&mut (*peerlist_guard).clone())
                }
            }
            Err(err) => {
                // Check would block... If not it's an actual error.
                if err.kind() != io::ErrorKind::WouldBlock {
                    return Err(err);
                }
            }
        };
    }
}

fn broadcast(
    socket: UdpSocket,
    multicast_address: &SocketAddr,
    transmission_delay: Duration,
    transmission_timeout: Duration,
    message: Vec<u8>,
    stopper: &RwLock<bool>,
) -> Result<(), io::Error> {
    if cfg!(debug_assertions) {
        socket.set_multicast_loop_v4(false)?;
    }

    let start_time = SystemTime::now();
    loop {
        let delta = SystemTime::now()
            .duration_since(start_time)
            .expect("Time is broken!");

        if delta >= transmission_timeout {
            let mut stop = stopper.write().expect("Deadlock");
            *stop = true;
            return Ok(());
        }
        {
            let stop = stopper.read().expect("Deadlock");
            if *stop {
                return Ok(());
            }
        }
        std::thread::sleep(transmission_delay);
        socket.send_to(&message, multicast_address.to_string())?;
    }
}
