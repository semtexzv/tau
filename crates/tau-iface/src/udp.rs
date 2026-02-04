//! Async UDP socket.
//!
//! Built on `AsyncFd` for non-blocking IO through the tau-rt reactor.

use std::io;
use std::net::SocketAddr;
use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};

use crate::async_fd::AsyncFd;
use crate::tcp::{addr_family, create_socket, raw_to_socket_addr, socket_addr_to_raw};

/// An async UDP socket.
///
/// Supports both unconnected (send_to/recv_from) and connected (send/recv) modes.
pub struct UdpSocket {
    async_fd: AsyncFd,
    fd: OwnedFd,
}

impl UdpSocket {
    /// Bind a UDP socket to a local address.
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let owned_fd = create_socket(addr_family(&addr), libc::SOCK_DGRAM)?;
        let raw = owned_fd.as_raw_fd();

        let (raw_addr, addr_len) = socket_addr_to_raw(&addr);
        let result = unsafe {
            libc::bind(
                raw,
                &raw_addr as *const _ as *const libc::sockaddr,
                addr_len,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }

        let async_fd = AsyncFd::new(raw)?;
        Ok(UdpSocket {
            async_fd,
            fd: owned_fd,
        })
    }

    /// Connect the socket to a remote address.
    ///
    /// After connecting, use [`send`](Self::send) and [`recv`](Self::recv)
    /// instead of `send_to`/`recv_from`.
    pub fn connect(&self, addr: SocketAddr) -> io::Result<()> {
        let (raw_addr, addr_len) = socket_addr_to_raw(&addr);
        let result = unsafe {
            libc::connect(
                self.fd.as_raw_fd(),
                &raw_addr as *const _ as *const libc::sockaddr,
                addr_len,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Send data to a specific address (unconnected mode).
    pub async fn send_to(&self, buf: &[u8], addr: SocketAddr) -> io::Result<usize> {
        let (raw_addr, addr_len) = socket_addr_to_raw(&addr);
        loop {
            self.async_fd.writable().await?;
            let n = unsafe {
                libc::sendto(
                    self.fd.as_raw_fd(),
                    buf.as_ptr() as *const libc::c_void,
                    buf.len(),
                    0,
                    &raw_addr as *const _ as *const libc::sockaddr,
                    addr_len,
                )
            };
            if n >= 0 {
                return Ok(n as usize);
            }
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                continue;
            }
            return Err(err);
        }
    }

    /// Receive data and the sender's address (unconnected mode).
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        loop {
            self.async_fd.readable().await?;

            let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
            let mut addr_len: libc::socklen_t =
                std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;

            let n = unsafe {
                libc::recvfrom(
                    self.fd.as_raw_fd(),
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                    0,
                    &mut storage as *mut _ as *mut libc::sockaddr,
                    &mut addr_len,
                )
            };

            if n >= 0 {
                let addr = raw_to_socket_addr(&storage)?;
                return Ok((n as usize, addr));
            }
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                continue;
            }
            return Err(err);
        }
    }

    /// Send data on a connected socket.
    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        loop {
            self.async_fd.writable().await?;
            let n = unsafe {
                libc::send(
                    self.fd.as_raw_fd(),
                    buf.as_ptr() as *const libc::c_void,
                    buf.len(),
                    0,
                )
            };
            if n >= 0 {
                return Ok(n as usize);
            }
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                continue;
            }
            return Err(err);
        }
    }

    /// Receive data on a connected socket.
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            self.async_fd.readable().await?;
            let n = unsafe {
                libc::recv(
                    self.fd.as_raw_fd(),
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                    0,
                )
            };
            if n >= 0 {
                return Ok(n as usize);
            }
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                continue;
            }
            return Err(err);
        }
    }

    /// Returns the raw file descriptor.
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}
