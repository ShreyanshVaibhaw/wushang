//! Streaming out, VLC's "Stream": the media playing served live over HTTP as MPEG-TS, copied
//! without re-encoding, to any number of players on the network (VLC, mpv, Kodi, a smart TV).
//! The recorder (`record.rs`) muxes into a [`Broadcast`]; this module hands the bytes out.
//!
//! MPEG-TS repeats its tables every tenth of a second, so a player that connects mid-stream
//! finds its footing on its own, from the next keyframe.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Chunks a player may fall behind by (the muxer writes about 32 KB at a time) before it is
/// let go, rather than handed a stream with holes in it.
const BACKLOG: usize = 256;
const FIRST_PORT: u16 = 8080;

/// The players connected now, each fed through a channel of its own.
#[derive(Default)]
pub struct Broadcast {
    players: Mutex<Vec<SyncSender<Arc<[u8]>>>>,
    count: AtomicUsize,
}

impl Broadcast {
    fn send(&self, bytes: &[u8]) {
        let chunk: Arc<[u8]> = bytes.into();
        let mut players = self.players.lock().unwrap();
        players.retain(|p| match p.try_send(chunk.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        });
        self.count.store(players.len(), Relaxed);
    }

    fn join(&self) -> Receiver<Arc<[u8]>> {
        let (send, receive) = sync_channel(BACKLOG);
        let mut players = self.players.lock().unwrap();
        players.push(send);
        self.count.store(players.len(), Relaxed);
        receive
    }

    /// Ends every player's stream.
    fn close_all(&self) {
        self.players.lock().unwrap().clear();
        self.count.store(0, Relaxed);
    }

    pub fn players(&self) -> usize {
        self.count.load(Relaxed)
    }
}

/// What the muxer writes into: every byte goes to every player.
pub struct Writer(pub Arc<Broadcast>);

impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.send(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The HTTP server players connect to. Dropping it disconnects them all.
pub struct Server {
    pub broadcast: Arc<Broadcast>,
    pub port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Listens on `ip`, on port 8080 or the first free one after it.
    pub fn start(ip: IpAddr) -> io::Result<Server> {
        let listener = (FIRST_PORT..FIRST_PORT + 20)
            .find_map(|port| TcpListener::bind(SocketAddr::new(ip, port)).ok())
            .ok_or_else(|| io::Error::other("no free port from 8080 to 8099"))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let broadcast = Arc::new(Broadcast::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (b, s) = (broadcast.clone(), stop.clone());
        let thread = thread::spawn(move || {
            while !s.load(Relaxed) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let b = b.clone();
                        thread::spawn(move || serve(socket, &b));
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => thread::sleep(Duration::from_millis(50)),
                }
            }
        });
        Ok(Server {
            broadcast,
            port,
            stop,
            thread: Some(thread),
        })
    }

    /// The address to open on another device: this machine on the local network.
    pub fn url(&self) -> String {
        format!("http://{}:{}/", local_ip(), self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        self.broadcast.close_all();
        if let Some(t) = self.thread.take() {
            _ = t.join();
        }
    }
}

/// One player: whatever it asks for, it gets the stream from now on.
fn serve(mut socket: TcpStream, broadcast: &Broadcast) {
    _ = socket.set_nonblocking(false);
    _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
    // The request line and headers; the path doesn't matter.
    let mut request = Vec::new();
    let mut byte = [0u8; 1];
    while !request.ends_with(b"\r\n\r\n") && request.len() < 16 * 1024 {
        match socket.read(&mut byte) {
            Ok(1) => request.push(byte[0]),
            _ => return,
        }
    }
    let head = "HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nCache-Control: no-cache\r\n\
                Connection: close\r\n\r\n";
    if socket.write_all(head.as_bytes()).is_err() || request.starts_with(b"HEAD ") {
        return;
    }
    let chunks = broadcast.join();
    // The broadcast drops its end when the player falls behind or the stream stops.
    while let Ok(chunk) = chunks.recv() {
        if socket.write_all(&chunk).is_err() {
            return;
        }
    }
}

/// This machine's address on the local network: the one a route to the internet would leave
/// from. Connecting a UDP socket sends nothing.
fn local_ip() -> IpAddr {
    UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|s| s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).map(|()| s))
        .and_then(|s| s.local_addr())
        .map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |a| a.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_player_gets_the_bytes_from_when_it_joined() {
        let server = Server::start(IpAddr::V4(Ipv4Addr::LOCALHOST)).unwrap();
        let (port, broadcast) = (server.port, server.broadcast.clone());
        let connect = || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            s
        };
        let read_head = |s: &mut TcpStream| {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                s.read_exact(&mut byte).unwrap();
                head.push(byte[0]);
            }
            String::from_utf8(head).unwrap()
        };
        let wait_for = |n: usize| {
            while broadcast.players() != n {
                thread::sleep(Duration::from_millis(10));
            }
        };
        let mut writer = Writer(broadcast.clone());
        let mut a = connect();
        assert!(read_head(&mut a).contains("video/mp2t"));
        wait_for(1);
        writer.write_all(b"first").unwrap();
        let mut b = connect();
        read_head(&mut b);
        wait_for(2);
        writer.write_all(b"second").unwrap();
        let mut got = [0u8; 11];
        a.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"firstsecond");
        let mut got = [0u8; 6];
        b.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"second");
        // Stopping ends every player's stream.
        drop(server);
        assert_eq!(a.read(&mut [0u8; 1]).unwrap(), 0);
    }
}
