//! Narrow local RPC. The agent accepts only typed lease operations from one
//! configured UID; zone, source CIDRs and journal paths are agent-owned policy.
use crate::{Admission, Lease};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, BufRead, BufReader, Write},
    os::{
        fd::AsRawFd,
        unix::net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    time::Duration,
};
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Prepare { lease: Lease },
    Commit { lease: Lease },
    Retire { lease: Lease },
    Reconcile { leases: Vec<Lease> },
}
#[derive(Serialize, Deserialize)]
struct Response {
    error: Option<String>,
}
pub struct Client {
    pub socket: PathBuf,
}
impl Client {
    fn call(&self, request: Request) -> io::Result<()> {
        let mut stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        serde_json::to_writer(&mut stream, &request)?;
        stream.write_all(b"\n")?;
        let mut data = String::new();
        BufReader::new(stream).take(65536).read_line(&mut data)?;
        let reply: Response = serde_json::from_str(&data)?;
        match reply.error {
            Some(error) => Err(io::Error::other(error)),
            None => Ok(()),
        }
    }
}
use std::io::Read;
impl Admission for Client {
    fn prepare_port(&self, lease: &Lease) -> io::Result<()> {
        self.call(Request::Prepare {
            lease: lease.clone(),
        })
    }
    fn commit_port(&self, lease: &Lease) -> io::Result<()> {
        self.call(Request::Commit {
            lease: lease.clone(),
        })
    }
    fn retire_port(&self, lease: &Lease) -> io::Result<()> {
        self.call(Request::Retire {
            lease: lease.clone(),
        })
    }
    fn reconcile_ports(&self, leases: &[Lease]) -> io::Result<()> {
        self.call(Request::Reconcile {
            leases: leases.to_vec(),
        })
    }
}
pub fn serve(
    listener: UnixListener,
    allowed_uid: u32,
    admission: &dyn Admission,
) -> io::Result<()> {
    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        } != 0
            || credentials.uid != allowed_uid
        {
            continue;
        }
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let result = (|| {
            let mut input = String::new();
            BufReader::new(&stream).take(65536).read_line(&mut input)?;
            let request: Request = serde_json::from_str(&input)?;
            match request {
                Request::Prepare { lease } => admission.prepare_port(&lease),
                Request::Commit { lease } => admission.commit_port(&lease),
                Request::Retire { lease } => admission.retire_port(&lease),
                Request::Reconcile { leases } if leases.len() <= 256 => {
                    admission.reconcile_ports(&leases)
                }
                _ => Err(io::Error::other("request limit exceeded")),
            }
        })();
        let reply = Response {
            error: result.err().map(|e| e.to_string()),
        };
        let _ = serde_json::to_writer(&mut stream, &reply)
            .and_then(|_| stream.write_all(b"\n").map_err(serde_json::Error::io));
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub socket: PathBuf,
    pub journal: PathBuf,
    pub allowed_uid: u32,
    pub policies: Vec<crate::firewalld::Policy>,
}
