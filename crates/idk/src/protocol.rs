use crate::model::{new_id, valid_id, MAX_MESSAGE, PROTOCOL};
use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub protocol: u32,
    pub request_id: String,
    pub client_id: String,
    pub request: Request,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Hello,
    List {
        project: Option<String>,
    },
    Start {
        project: String,
        terminal: String,
        rows: u16,
        cols: u16,
        env: BTreeMap<String, String>,
        reopen: bool,
    },
    Attach {
        session: String,
        takeover: bool,
    },
    Detach {
        session: String,
    },
    Snapshot {
        session: String,
    },
    Input {
        session: String,
        data: String,
        epoch: u64,
    },
    Resize {
        session: String,
        rows: u16,
        cols: u16,
        epoch: u64,
    },
    Scroll {
        session: String,
        delta: i32,
    },
    Close {
        session: String,
    },
    Shutdown,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Request")
            .field("kind", &std::mem::discriminant(self))
            .field("payload", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub protocol: u32,
    pub request_id: String,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Response")
            .field("protocol", &self.protocol)
            .field("request_id", &self.request_id)
            .field("has_error", &self.error.is_some())
            .field("payload", &"[redacted]")
            .finish()
    }
}

impl Envelope {
    pub fn new(client_id: &str, request: Request) -> Self {
        Self {
            protocol: PROTOCOL,
            request_id: new_id(),
            client_id: client_id.to_owned(),
            request,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.protocol != PROTOCOL {
            bail!(
                "protocol mismatch: client {}, host {}; use the same version",
                self.protocol,
                PROTOCOL
            );
        }
        valid_id(&self.request_id)?;
        valid_id(&self.client_id)?;
        Ok(())
    }
}

impl Response {
    pub fn success<T: Serialize>(request_id: String, data: &T) -> Result<Self> {
        Ok(Self {
            protocol: PROTOCOL,
            request_id,
            data: Some(serde_json::to_value(data)?),
            error: None,
        })
    }

    pub fn failure(request_id: String, error: impl ToString) -> Self {
        Self {
            protocol: PROTOCOL,
            request_id,
            data: None,
            error: Some(error.to_string()),
        }
    }

    pub fn decode<T: DeserializeOwned>(self) -> Result<T> {
        if let Some(error) = self.error {
            bail!("{error}");
        }
        serde_json::from_value(self.data.context("missing response payload")?)
            .context("invalid response payload")
    }
}

/// One bounded length-prefixed request per connection. No unbounded read_until allocation.
pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_MESSAGE {
        bail!("IPC message exceeds allowed size");
    }
    let mut body = vec![0; size];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).context("invalid IPC message")
}

pub fn write_frame(writer: &mut impl Write, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_MESSAGE {
        bail!("IPC response exceeds allowed size; reduce terminal dimensions");
    }
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn configure_stream(stream: &UnixStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot verify local peer");
    }
    if credentials.uid != unsafe { libc::geteuid() } {
        bail!("IPC peer belongs to another user");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_bounds_and_protocol_are_checked_before_dispatch() {
        let oversized = ((MAX_MESSAGE + 1) as u32).to_be_bytes();
        assert!(read_frame::<Envelope>(&mut oversized.as_slice()).is_err());
        let mut envelope = Envelope::new(&new_id(), Request::Hello);
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &envelope).unwrap();
        let decoded: Envelope = read_frame(&mut bytes.as_slice()).unwrap();
        decoded.validate().unwrap();
        envelope.protocol = 99;
        assert!(envelope.validate().is_err());
        envelope.protocol = PROTOCOL;
        envelope.client_id = "untrusted".into();
        assert!(envelope.validate().is_err());
        assert!(read_frame::<Envelope>(&mut &bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn debug_does_not_expose_launch_secrets_or_terminal_input() {
        let request = Request::Start {
            project: new_id(),
            terminal: new_id(),
            rows: 24,
            cols: 80,
            env: BTreeMap::from([("TOKEN".into(), "private-value".into())]),
            reopen: false,
        };
        assert!(!format!("{request:?}").contains("private-value"));
        let input = Request::Input {
            session: new_id(),
            data: "private-keystrokes".into(),
            epoch: 1,
        };
        assert!(!format!("{input:?}").contains("private-keystrokes"));
        let response = Response::success(new_id(), &"private-output").unwrap();
        assert!(!format!("{response:?}").contains("private-output"));
    }
}
