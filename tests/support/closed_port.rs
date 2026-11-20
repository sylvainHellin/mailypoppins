//! A localhost port nothing listens on, held for as long as a test needs it
//! (PERSO-342).
//!
//! A connect to it is refused at once: the socket is bound and never
//! listens, so the kernel answers a SYN with a reset. Being bound, without
//! `SO_REUSEADDR`, it is also never handed to another bind of port 0, in this
//! process or another, for as long as the guard lives.
//!
//! The pattern it replaces, a listener bound, its port read and the listener
//! dropped, frees the port for the next bind of port 0: a fake server of a
//! test running beside it, which then answered the connect the test expected
//! refused and took its message.
//!
//! A test that needs a port free for a later bind is a different pattern and
//! does not use this guard. The library's unit tests include this file by
//! path, and test the guard there once rather than in every suite that
//! declares `mod support;`.

#![allow(dead_code)]

use std::net::SocketAddr;

/// A bound, never listening socket on `127.0.0.1`: its port refuses every
/// connect until the guard is dropped.
pub struct ClosedPort {
    /// Held only to keep the port bound.
    _socket: tokio::net::TcpSocket,
    addr: SocketAddr,
}

impl ClosedPort {
    /// Bind a free localhost port and keep it bound without listening. Needs
    /// no runtime: a socket registers with tokio's reactor only to listen or
    /// connect.
    pub fn bind() -> ClosedPort {
        let socket = tokio::net::TcpSocket::new_v4().expect("a TCP socket");
        socket
            .bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .expect("a localhost port");
        let addr = socket.local_addr().expect("its address");
        ClosedPort {
            _socket: socket,
            addr,
        }
    }

    /// The port, which refuses every connect while the guard lives.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// `127.0.0.1` and [`ClosedPort::port`].
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}
