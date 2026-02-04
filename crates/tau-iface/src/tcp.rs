//! Async TCP stream and listener.
//!
//! Built on `AsyncFd` for non-blocking IO through the tau-rt reactor.

use std::io;
use std::net::SocketAddr;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use crate::async_fd::AsyncFd;

// ── Socket helpers ──────────────────────────────────────────────────

/// Convert a `SocketAddr` to a raw `(sockaddr_storage, socklen_t)` pair.
pub(crate) fn socket_addr_to_raw(
    addr: &SocketAddr,
) -> (libc::sockaddr_storage, libc::socklen_t) {
    match addr {
        SocketAddr::V4(v4) => {
            let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
            let sin =
                unsafe { &mut *(&mut storage as *mut _ as *mut libc::sockaddr_in) };
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
            {
                sin.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
            }
            sin.sin_family = libc::AF_INET as libc::sa_family_t;
            sin.sin_port = v4.port().to_be();
            sin.sin_addr = libc::in_addr {
                s_addr: u32::from_ne_bytes(v4.ip().octets()),
            };
            (
                storage,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        }
        SocketAddr::V6(v6) => {
            let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
            let sin6 =
                unsafe { &mut *(&mut storage as *mut _ as *mut libc::sockaddr_in6) };
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
            {
                sin6.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
            }
            sin6.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            sin6.sin6_port = v6.port().to_be();
            sin6.sin6_flowinfo = v6.flowinfo();
            sin6.sin6_addr = libc::in6_addr {
                s6_addr: v6.ip().octets(),
            };
            sin6.sin6_scope_id = v6.scope_id();
            (
                storage,
                std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
            )
        }
    }
}

/// Convert a raw `sockaddr_storage` back to a `SocketAddr`.
pub(crate) fn raw_to_socket_addr(
    storage: &libc::sockaddr_storage,
) -> io::Result<SocketAddr> {
    match storage.ss_family as libc::c_int {
        libc::AF_INET => {
            let sin = unsafe { &*(storage as *const _ as *const libc::sockaddr_in) };
            let octets = sin.sin_addr.s_addr.to_ne_bytes();
            let ip = std::net::Ipv4Addr::from(octets);
            let port = u16::from_be(sin.sin_port);
            Ok(SocketAddr::V4(std::net::SocketAddrV4::new(ip, port)))
        }
        libc::AF_INET6 => {
            let sin6 =
                unsafe { &*(storage as *const _ as *const libc::sockaddr_in6) };
            let ip = std::net::Ipv6Addr::from(sin6.sin6_addr.s6_addr);
            let port = u16::from_be(sin6.sin6_port);
            Ok(SocketAddr::V6(std::net::SocketAddrV6::new(
                ip,
                port,
                sin6.sin6_flowinfo,
                sin6.sin6_scope_id,
            )))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown address family",
        )),
    }
}

/// Set a file descriptor to non-blocking mode.
pub(crate) fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Set SO_REUSEADDR on a socket.
pub(crate) fn set_reuseaddr(fd: RawFd) -> io::Result<()> {
    let optval: libc::c_int = 1;
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &optval as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Check the pending socket error (used after non-blocking connect).
fn get_socket_error(fd: RawFd) -> io::Result<()> {
    let mut error: libc::c_int = 0;
    let mut len: libc::socklen_t =
        std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            &mut error as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    Ok(())
}

/// Create a non-blocking socket and return its `OwnedFd`.
pub(crate) fn create_socket(domain: libc::c_int, sock_type: libc::c_int) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::socket(domain, sock_type, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    set_nonblocking(owned.as_raw_fd())?;
    Ok(owned)
}

/// Determine AF_INET or AF_INET6 from a SocketAddr.
pub(crate) fn addr_family(addr: &SocketAddr) -> libc::c_int {
    match addr {
        SocketAddr::V4(_) => libc::AF_INET,
        SocketAddr::V6(_) => libc::AF_INET6,
    }
}

// ── TcpStream ───────────────────────────────────────────────────────

/// An async TCP stream.
///
/// Wraps a non-blocking TCP socket registered with the tau-rt reactor.
pub struct TcpStream {
    async_fd: AsyncFd,
    fd: OwnedFd,
}

impl TcpStream {
    /// Connect to a remote address.
    ///
    /// Creates a non-blocking socket, initiates the connect, and awaits
    /// completion via the reactor.
    pub async fn connect(addr: SocketAddr) -> io::Result<Self> {
        let owned_fd = create_socket(addr_family(&addr), libc::SOCK_STREAM)?;
        let raw = owned_fd.as_raw_fd();

        // Initiate non-blocking connect
        let (raw_addr, addr_len) = socket_addr_to_raw(&addr);
        let result = unsafe {
            libc::connect(
                raw,
                &raw_addr as *const _ as *const libc::sockaddr,
                addr_len,
            )
        };

        if result < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(err);
            }
        }

        // Register with reactor and wait for connect completion
        let async_fd = AsyncFd::new(raw)?;

        if result != 0 {
            // Connect in progress — wait for writable (connect completion)
            async_fd.writable().await?;
            // Check for connect errors
            get_socket_error(raw)?;
        }

        Ok(TcpStream {
            async_fd,
            fd: owned_fd,
        })
    }

    /// Create a `TcpStream` from a raw fd that is already connected and non-blocking.
    ///
    /// # Safety
    /// The fd must be a valid, connected, non-blocking TCP socket.
    /// Caller transfers ownership of the fd.
    pub(crate) unsafe fn from_raw_fd(fd: RawFd) -> io::Result<Self> {
        let owned_fd = OwnedFd::from_raw_fd(fd);
        let async_fd = AsyncFd::new(fd)?;
        Ok(TcpStream {
            async_fd,
            fd: owned_fd,
        })
    }

    /// Read data from the stream.
    ///
    /// Returns the number of bytes read, or 0 for EOF.
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
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
                continue; // spurious wake
            }
            return Err(err);
        }
    }

    /// Write data to the stream.
    ///
    /// Returns the number of bytes written (may be less than `buf.len()`).
    pub async fn write(&self, buf: &[u8]) -> io::Result<usize> {
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
                continue; // spurious wake
            }
            return Err(err);
        }
    }

    /// Returns the raw file descriptor.
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

// ── TcpListener ─────────────────────────────────────────────────────

/// An async TCP listener.
///
/// Binds to an address and accepts incoming connections through the reactor.
pub struct TcpListener {
    async_fd: AsyncFd,
    fd: OwnedFd,
}

impl TcpListener {
    /// Bind to a local address and start listening.
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let owned_fd = create_socket(addr_family(&addr), libc::SOCK_STREAM)?;
        let raw = owned_fd.as_raw_fd();

        set_reuseaddr(raw)?;

        // Bind
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

        // Listen
        let result = unsafe { libc::listen(raw, 128) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }

        let async_fd = AsyncFd::new(raw)?;
        Ok(TcpListener {
            async_fd,
            fd: owned_fd,
        })
    }

    /// Accept a new incoming connection.
    ///
    /// Returns the connected stream and the peer's address.
    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        loop {
            self.async_fd.readable().await?;

            let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
            let mut addr_len: libc::socklen_t =
                std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;

            let fd = unsafe {
                libc::accept(
                    self.fd.as_raw_fd(),
                    &mut storage as *mut _ as *mut libc::sockaddr,
                    &mut addr_len,
                )
            };

            if fd >= 0 {
                set_nonblocking(fd)?;
                let addr = raw_to_socket_addr(&storage)?;
                let stream = unsafe { TcpStream::from_raw_fd(fd)? };
                return Ok((stream, addr));
            }

            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                continue; // spurious wake
            }
            return Err(err);
        }
    }

    /// Returns the raw file descriptor.
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_addr_v4_roundtrip() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let (raw, _len) = socket_addr_to_raw(&addr);
        let back = raw_to_socket_addr(&raw).unwrap();
        assert_eq!(addr, back);
    }

    #[test]
    fn socket_addr_v6_roundtrip() {
        let addr: SocketAddr = "[::1]:9090".parse().unwrap();
        let (raw, _len) = socket_addr_to_raw(&addr);
        let back = raw_to_socket_addr(&raw).unwrap();
        assert_eq!(addr, back);
    }

    #[test]
    fn socket_addr_v4_specific() {
        let addr: SocketAddr = "192.168.1.100:443".parse().unwrap();
        let (raw, len) = socket_addr_to_raw(&addr);
        assert_eq!(
            len,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t
        );
        assert_eq!(raw.ss_family as libc::c_int, libc::AF_INET);
        let back = raw_to_socket_addr(&raw).unwrap();
        assert_eq!(addr, back);
    }

    #[test]
    fn unknown_address_family_errors() {
        let storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        // ss_family is 0, which is AF_UNSPEC — should error
        let result = raw_to_socket_addr(&storage);
        assert!(result.is_err());
    }
}
