//! Linux pathname sockets with a deadline covering connect itself. Socket I/O
//! timeouts set after a blocking connect do not bound a full accept backlog.
use anyhow::{ensure, Context, Result};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

pub(crate) fn connect_deadline(path: &Path, deadline: Instant) -> Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    // The product uses private filesystem sockets; Linux abstract sockets are
    // deliberately not an alternate route around pathname permissions.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    ensure!(
        path.is_absolute() && !bytes.contains(&0),
        "local socket needs an absolute pathname without NUL"
    );
    ensure!(
        bytes.len() < address.sun_path.len(),
        "local socket pathname is too long"
    );
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    let length =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;

    loop {
        remaining(deadline)?;
        // CLOEXEC is atomic with allocation, so concurrent child creation does
        // not inherit a socket that belongs only to this client request.
        let fd = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error()).context("allocate local socket");
        }
        // SAFETY: fd was just allocated above; this is its sole owner.
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        let connected =
            unsafe { libc::connect(fd, (&address as *const libc::sockaddr_un).cast(), length) };
        if connected == 0 {
            remaining(deadline)?;
            stream.set_nonblocking(false)?;
            return Ok(stream);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINPROGRESS | libc::EALREADY) => {
                wait_connected(&stream, deadline)?;
                stream.set_nonblocking(false)?;
                return Ok(stream);
            }
            Some(libc::EAGAIN | libc::EINTR) => {
                // AF_UNIX returns EAGAIN for a full listen backlog. No request
                // was sent. Discard this unconnected socket rather than treating
                // an immediate POLLOUT/SO_ERROR=0 as proof of a connection.
                drop(stream);
                std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(5)));
            }
            _ => return Err(error).context("connect to local host"),
        }
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "local socket connection deadline exceeded",
            )
        })
}

fn wait_connected(stream: &UnixStream, deadline: Instant) -> Result<()> {
    loop {
        let left = remaining(deadline)?;
        let milliseconds = left.as_millis().saturating_add(1).min(i32::MAX as u128) as i32;
        let mut poll = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, milliseconds) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("wait for local socket connection");
        }
        if result == 0 {
            continue;
        }
        remaining(deadline)?;
        if let Some(error) = stream.take_error()? {
            return Err(error).context("complete local socket connection");
        }
        stream
            .peer_addr()
            .context("local socket has no connected peer")?;
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    #[test]
    fn a_full_unix_listen_backlog_cannot_block_past_the_connect_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("full.sock");
        let listener = UnixListener::bind(&path).unwrap();
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        // Linux permits one pending connection when listen backlog is zero.
        let first = connect_deadline(&path, Instant::now() + Duration::from_secs(1)).unwrap();
        let start = Instant::now();
        let error = connect_deadline(&path, start + Duration::from_millis(60)).unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(start.elapsed() < Duration::from_millis(500));
        let (mut accepted, _) = listener.accept().unwrap();
        drop(first);
        let mut second = connect_deadline(&path, Instant::now() + Duration::from_secs(1)).unwrap();
        let (mut second_peer, _) = listener.accept().unwrap();
        second.write_all(b"ok").unwrap();
        let mut bytes = [0; 2];
        second_peer.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"ok");
        assert_eq!(accepted.read(&mut bytes).unwrap(), 0);
    }

    #[test]
    fn missing_socket_and_invalid_paths_are_not_misreported_as_a_timeout() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("absent.sock");
        let error = connect_deadline(&path, Instant::now() + Duration::from_secs(1)).unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::NotFound
        );
        assert!(connect_deadline(
            Path::new("relative.sock"),
            Instant::now() + Duration::from_secs(1)
        )
        .is_err());
        assert!(connect_deadline(
            Path::new(&format!("/{}", "x".repeat(108))),
            Instant::now() + Duration::from_secs(1)
        )
        .is_err());
        let error = connect_deadline(&path, Instant::now()).unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::TimedOut
        );
    }
}
