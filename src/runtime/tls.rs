//! Bounded in-memory TLS steps. Only the Joky transport owns a socket and parks.
use super::{
    arena::Arena, buffer::Buffer, cps::CpsFrame, resource::OwnedResource, tcp::OwnerCharge,
};
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, Type};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, ServerName};
use rustls::{
    ClientConfig, ClientConnection, Connection, RootCertStore, ServerConfig, ServerConnection,
};
use std::{
    ffi::c_void,
    io::{BufRead, Write},
    ptr::NonNull,
    sync::Arc,
};

const PEM_LIMIT: usize = 128 * 1024;
const RECORD: usize = 16 * 1024;
// Reserve space for rustls handshake messages, certificate processing and record queues.
const SESSION_BUDGET: usize = 512 * 1024;
const INVALID_CONFIG: u64 = 1;
const INVALID_NAME: u64 = 2;
const CERTIFICATE: u64 = 3;
const PROTOCOL: u64 = 4;
const LIMIT: u64 = 5;
const NEED_INPUT: u64 = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Operation {
    ClientConfig,
    ServerConfig,
    Client,
    Server,
    Feed,
    Drain,
    Read,
    Write,
    Handshaking,
    Notify,
    ShareServer,
}
const OPS: &[(&str, Operation)] = &[
    ("__tls_client_config", Operation::ClientConfig),
    ("__tls_server_config", Operation::ServerConfig),
    ("__tls_client", Operation::Client),
    ("__tls_server", Operation::Server),
    ("__tls_feed", Operation::Feed),
    ("__tls_drain", Operation::Drain),
    ("__tls_read", Operation::Read),
    ("__tls_write", Operation::Write),
    ("__tls_handshaking", Operation::Handshaking),
    ("__tls_notify", Operation::Notify),
    ("__tls_server_share", Operation::ShareServer),
];
fn invalid() -> Error {
    Error::new_with_code(
        ErrorPhase::Abi,
        ErrorCode::AbiError,
        "Invalid TLS operation or resource",
    )
}
impl Operation {
    pub(crate) fn lookup(name: &str) -> Option<Self> {
        OPS.iter().find(|(n, _)| *n == name).map(|(_, op)| *op)
    }
    pub(crate) fn returns_bytes(self) -> bool {
        matches!(self, Self::Drain | Self::Read)
    }
    pub(crate) fn returns_pointer(self) -> bool {
        self.returns_bytes()
            || matches!(
                self,
                Self::ClientConfig
                    | Self::ServerConfig
                    | Self::Client
                    | Self::Server
                    | Self::ShareServer
            )
    }
    pub(crate) fn validate(self, sig: &FunctionSignature, suspends: bool) -> Result<(), Error> {
        let array = |ty: &Type| matches!(ty, Type::Record(r) if r.name == "joky/array.Array" && r.arguments == [Type::UInt8] && r.fields.len()==1 && matches!(&r.fields[0].ty, Type::Buffer(t) if **t==Type::UInt8));
        let client = Type::Resource("tls.ClientOwner".into());
        let server = Type::Resource("tls.ServerOwner".into());
        let session = Type::Resource("tls.SessionOwner".into());
        let mut_session = Type::reference(session.clone(), true);
        let error = Type::reference(Type::UInt64, true);
        let bytes = |i: usize| {
            sig.params
                .get(i)
                .filter(|ty| array(ty))
                .cloned()
                .ok_or_else(invalid)
        };
        let (params, ret) = match self {
            Self::ShareServer => (vec![Type::reference(server.clone(), false)], server),
            Self::ClientConfig => (vec![Type::UInt64, bytes(1)?, error], client),
            Self::ServerConfig => (vec![bytes(0)?, bytes(1)?, error], server),
            Self::Client => (
                vec![Type::reference(client, false), bytes(1)?, error],
                session,
            ),
            Self::Server => (vec![Type::reference(server, false), error], session),
            Self::Feed | Self::Write => (vec![mut_session, bytes(1)?], Type::UInt64),
            Self::Handshaking | Self::Notify => (vec![mut_session], Type::UInt64),
            Self::Drain | Self::Read if array(&sig.ret) => (
                if self == Self::Drain {
                    vec![mut_session]
                } else {
                    vec![mut_session, Type::UInt64, Type::UInt64, error]
                },
                sig.ret.clone(),
            ),
            _ => return Err(invalid()),
        };
        if suspends && *sig == FunctionSignature::new(params, ret) {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}
enum Value {
    Client(Arc<ClientConfig>),
    Server(Arc<ServerConfig>),
    Session(Box<Connection>),
    Invalid,
}
struct Owner {
    value: Value,
    config_bytes: usize,
    charge: OwnerCharge,
}
unsafe extern "C" fn release(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<Owner>()));
}
pub(crate) fn transferable_type(name: &str) -> bool {
    matches!(
        name,
        "tls.ClientOwner" | "tls.ServerOwner" | "tls.SessionOwner"
    )
}
// SAFETY: transfer admission checks exclusive ownership before consulting the callback.
pub(crate) unsafe fn transferable(
    token: NonNull<c_void>,
    callback: super::resource::ResourceReleaseV1,
) -> bool {
    std::ptr::fn_addr_eq(callback, release as super::resource::ResourceReleaseV1)
        && token.cast::<Owner>().as_ref().charge.shared()
}
fn adopt(
    arena: &mut Arena,
    value: Value,
    config_bytes: usize,
    charge: OwnerCharge,
) -> Result<u64, Error> {
    let token = NonNull::from(Box::leak(Box::new(Owner {
        value,
        config_bytes,
        charge,
    })))
    .cast();
    let resource = OwnedResource::from_token(token, release)
        .inspect_err(|_| unsafe { release(token.as_ptr()) })?;
    Ok(Buffer::external(arena, resource)?.as_ptr() as u64)
}
unsafe fn buffer<'a>(bits: u64) -> Result<&'a Buffer, Error> {
    (bits as *const Buffer).as_ref().ok_or_else(invalid)
}
fn tls_error(e: rustls::Error) -> u64 {
    match e {
        rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented => {
            CERTIFICATE
        }
        _ => PROTOCOL,
    }
}
fn client_config(bundled: bool, pem: &[u8]) -> Result<Value, u64> {
    let mut roots = RootCertStore::empty();
    if bundled {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    } else {
        for cert in CertificateDer::pem_slice_iter(pem) {
            roots
                .add(cert.map_err(|_| INVALID_CONFIG)?)
                .map_err(|_| INVALID_CONFIG)?;
        }
        if roots.is_empty() {
            return Err(INVALID_CONFIG);
        }
    }
    let mut config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| INVALID_CONFIG)?
            .with_root_certificates(roots)
            .with_no_client_auth();
    config.resumption = rustls::client::Resumption::disabled();
    Ok(Value::Client(Arc::new(config)))
}
fn server_config(pem: &[u8], key: &[u8]) -> Result<Value, u64> {
    let chain = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| INVALID_CONFIG)?;
    let key = PrivateKeyDer::from_pem_slice(key).map_err(|_| INVALID_CONFIG)?;
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| INVALID_CONFIG)?
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|_| INVALID_CONFIG)?;
    config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    config.send_tls13_tickets = 0;
    Ok(Value::Server(Arc::new(config)))
}
// SAFETY: slots come from the exact checked signature. Neither pointers nor
// references into Joky storage survive this call, including on protocol errors.
pub(crate) unsafe fn call(arena: &mut Arena, slots: &[u64], op: Operation) -> Result<u64, Error> {
    use Operation as O;
    let set_error = |i: usize, code: u64| *(slots[i] as *mut u64) = code;
    if matches!(op, O::ClientConfig | O::ServerConfig) {
        let pem = buffer(slots[1])?.bytes()?;
        let chain = if op == O::ServerConfig {
            buffer(slots[0])?.bytes()?
        } else {
            &[]
        };
        let bounded = pem.len() <= PEM_LIMIT && chain.len() <= PEM_LIMIT;
        let cost = if bounded {
            (pem.len() + chain.len()) * 4 + 128 * 1024
        } else {
            512
        };
        let charge = OwnerCharge::new(arena, cost)?;
        let result = if !bounded {
            Err(LIMIT)
        } else if op == O::ServerConfig {
            server_config(chain, pem)
        } else {
            client_config(slots[0] != 0, pem)
        };
        let value = result.unwrap_or_else(|code| {
            set_error(2, code);
            Value::Invalid
        });
        return adopt(arena, value, cost, charge);
    }
    let resource = buffer(slots[0])?.resource_ref()?;
    let raw = resource.control_token(release)?.cast::<Owner>();
    if op == O::ShareServer {
        let owner = &*raw;
        let Value::Server(config) = &owner.value else {
            return Err(invalid());
        };
        let charge = OwnerCharge::new(arena, owner.config_bytes)?;
        return adopt(
            arena,
            Value::Server(config.clone()),
            owner.config_bytes,
            charge,
        );
    }
    if matches!(op, O::Client | O::Server) {
        let owner = &*raw;
        let cost = owner.config_bytes;
        let charge = OwnerCharge::new(arena, SESSION_BUDGET + cost)?;
        let result = match (&owner.value, op) {
            (Value::Client(c), O::Client) => {
                let bytes = buffer(slots[1])?.bytes()?;
                std::str::from_utf8(bytes)
                    .ok()
                    .filter(|s| s.len() <= 253)
                    .and_then(|s| ServerName::try_from(s).ok())
                    .ok_or(INVALID_NAME)
                    .and_then(|name| {
                        ClientConnection::new(c.clone(), name.to_owned())
                            .map(Connection::Client)
                            .map_err(tls_error)
                    })
            }
            (Value::Server(c), O::Server) => ServerConnection::new(c.clone())
                .map(Connection::Server)
                .map_err(tls_error),
            _ => return Err(invalid()),
        };
        let value = match result {
            Ok(mut conn) => {
                conn.set_buffer_limit(Some(64 * 1024));
                Value::Session(Box::new(conn))
            }
            Err(code) => {
                set_error(if op == O::Client { 2 } else { 1 }, code);
                Value::Invalid
            }
        };
        return adopt(arena, value, cost, charge);
    }
    let _lease = resource.lease()?;
    let Value::Session(conn) = &mut (*raw).value else {
        return Err(invalid());
    };
    let out = |arena: &mut Arena, b: &[u8]| Ok(Buffer::copy_bytes(arena, b)?.as_ptr() as u64);
    match op {
        O::Handshaking => Ok(u64::from(conn.is_handshaking())),
        O::Notify => {
            conn.send_close_notify();
            Ok(0)
        }
        O::Feed => {
            let mut input = buffer(slots[1])?.bytes()?;
            if input.len() > RECORD {
                return Ok(LIMIT);
            }
            while !input.is_empty() {
                match conn.read_tls(&mut input) {
                    Ok(0) | Err(_) => return Ok(PROTOCOL),
                    Ok(_) => {}
                }
                if let Err(e) = conn.process_new_packets() {
                    return Ok(tls_error(e));
                }
            }
            Ok(0)
        }
        O::Write => {
            let bytes = buffer(slots[1])?.bytes()?;
            if bytes.len() > RECORD {
                return Ok(LIMIT);
            }
            Ok(if conn.writer().write_all(bytes).is_ok() {
                0
            } else {
                PROTOCOL
            })
        }
        O::Drain => {
            let mut bytes = Vec::new();
            conn.write_tls(&mut bytes).map_err(|_| invalid())?;
            out(arena, &bytes)
        }
        O::Read => {
            if slots[1] == 0 || slots[1] > RECORD as u64 {
                set_error(3, LIMIT);
                return out(arena, &[]);
            }
            let mut reader = conn.reader();
            match reader.fill_buf() {
                Ok(available) => {
                    let chunk = &available[..available.len().min(slots[1] as usize)];
                    let n = u8::try_from(slots[2])
                        .ok()
                        .and_then(|delimiter| chunk.iter().position(|byte| *byte == delimiter))
                        .map_or(chunk.len(), |i| i + 1);
                    // Only consume after the task can afford the result owner.
                    let result = out(arena, &chunk[..n])?;
                    if n > 0 {
                        reader.consume(n);
                    }
                    Ok(result)
                }
                Err(error) => {
                    set_error(
                        3,
                        if error.kind() == std::io::ErrorKind::WouldBlock {
                            NEED_INPUT
                        } else {
                            PROTOCOL
                        },
                    );
                    out(arena, &[])
                }
            }
        }
        _ => Err(invalid()),
    }
}
#[export_name = "__joky_tls_poll"]
pub(crate) unsafe extern "C" fn poll_native(frame: *mut CpsFrame, kind: u32) -> i32 {
    let result = (|| {
        let op = OPS
            .iter()
            .find(|(_, op)| *op as u32 == kind)
            .map(|(_, op)| *op)
            .ok_or_else(invalid)?;
        let arena = super::cps::arena(frame);
        let f = &mut *frame;
        let bits = call(&mut *arena, &f.slots, op)?;
        if op.returns_bytes() {
            let n = if op == Operation::Drain { 1 } else { 4 };
            f.slots[n] = bits;
        } else {
            f.result = bits;
        }
        f.state = super::cps::DONE;
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(e) => {
            super::error::record_error(e);
            super::cps::RUNTIME_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_session_reservation_refunds_on_failure_and_outlives_config() {
        let mut arena = Arena::default();
        let empty = Buffer::copy_bytes(&mut arena, &[]).unwrap().as_ptr() as u64;
        let name = Buffer::copy_bytes(&mut arena, b"localhost")
            .unwrap()
            .as_ptr() as u64;
        let mut error = 0u64;
        let err = (&mut error as *mut u64) as u64;
        unsafe {
            let config = call(&mut arena, &[1, empty, err], Operation::ClientConfig).unwrap();
            let baseline = arena.bytes();
            let held = arena
                .reserve_external(super::super::arena::MAX_BYTES - baseline)
                .unwrap();
            assert_eq!(
                call(&mut arena, &[config, name, err], Operation::Client)
                    .unwrap_err()
                    .code(),
                ErrorCode::ResourceLimit
            );
            drop(held);
            assert_eq!(arena.bytes(), baseline);
            let session = call(&mut arena, &[config, name, err], Operation::Client).unwrap();
            for pointer in [empty, name, config] {
                Buffer::release(&mut arena, pointer as *mut Buffer).unwrap();
            }
            assert!(arena.bytes() >= SESSION_BUDGET);
            assert_eq!(
                call(&mut arena, &[session], Operation::Handshaking).unwrap(),
                1
            );
            Buffer::release(&mut arena, session as *mut Buffer).unwrap();
            assert_eq!(arena.bytes(), 0);
        }
    }
}
