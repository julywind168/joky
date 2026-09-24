use crate::sema::{Type, TypeTable};

#[cfg(unix)]
pub(crate) const SOCKET_OPERATION_COUNT: usize = 23;
#[cfg(not(unix))]
pub(crate) const SOCKET_OPERATION_COUNT: usize = 13;

/// Build the operation table consumed by the standalone runtime. The slot
/// order mirrors `crates/joky-runtime/src/runtime/socket.rs::operation_hooks`.
pub(crate) fn operation_metadata(types: &TypeTable) -> ([u64; SOCKET_OPERATION_COUNT], u32) {
    let mut ids = [0_u64; SOCKET_OPERATION_COUNT];
    let mut mask = 0_u32;
    let effects = types.effects();
    macro_rules! pair {
        ($slot:expr, $effect:expr, $name:expr, $params:expr, $ok:expr, $err:expr) => {
            if let Some(effect) = effects.by_name($effect) {
                if let Some(operation) = effects.operation_by_name(effect, $name) {
                    if let Some(info) = effects.operation_info(operation) {
                        if let Type::Result(result) = info.return_type {
                            if info.suspends
                                && info.parameters == $params
                                && types.result_types(result) == ($ok, $err)
                            {
                                ids[$slot] = ((operation.effect.0 as u64) << 32)
                                    | operation.operation as u64;
                                mask |= 1_u32 << $slot;
                            }
                        }
                    }
                }
            }
        };
    }
    macro_rules! tuple {
        ($slot:expr, $effect:expr, $name:expr, $params:expr, $elements:expr, $err:expr) => {
            if let Some(effect) = effects.by_name($effect) {
                if let Some(operation) = effects.operation_by_name(effect, $name) {
                    if let Some(info) = effects.operation_info(operation) {
                        if let Type::Result(result) = info.return_type {
                            let (ok, error) = types.result_types(result);
                            if info.suspends
                                && info.parameters == $params
                                && matches!(ok, Type::Tuple(id) if types.tuple_elements(id) == $elements)
                                && error == $err
                            {
                                ids[$slot] = ((operation.effect.0 as u64) << 32)
                                    | operation.operation as u64;
                                mask |= 1_u32 << $slot;
                            }
                        }
                    }
                }
            }
        };
    }

    pair!(
        0,
        "tcp",
        "connect",
        vec![Type::String, Type::U16],
        Type::tcp_stream(),
        Type::String
    );
    pair!(
        1,
        "tcp",
        "listen",
        vec![Type::String, Type::U16],
        Type::tcp_listener(),
        Type::String
    );
    pair!(
        2,
        "tcp",
        "accept",
        vec![Type::tcp_listener()],
        Type::tcp_stream(),
        Type::String
    );
    pair!(
        3,
        "tcp",
        "read",
        vec![Type::tcp_stream(), Type::U64],
        Type::Bytes,
        Type::String
    );
    pair!(
        4,
        "tcp",
        "write",
        vec![Type::tcp_stream(), Type::Bytes],
        Type::U64,
        Type::String
    );
    pair!(
        5,
        "tcp",
        "close",
        vec![Type::tcp_stream()],
        Type::Unit,
        Type::String
    );

    pair!(
        6,
        "udp",
        "bind",
        vec![Type::String, Type::U16],
        Type::udp_socket(),
        Type::String
    );
    pair!(
        7,
        "udp",
        "connect",
        vec![Type::udp_socket(), Type::String, Type::U16],
        Type::Unit,
        Type::String
    );
    pair!(
        8,
        "udp",
        "send_to",
        vec![Type::udp_socket(), Type::Bytes, Type::String, Type::U16],
        Type::U64,
        Type::String
    );
    pair!(
        9,
        "udp",
        "send",
        vec![Type::udp_socket(), Type::Bytes],
        Type::U64,
        Type::String
    );
    pair!(
        10,
        "udp",
        "recv",
        vec![Type::udp_socket(), Type::U64],
        Type::Bytes,
        Type::String
    );
    tuple!(
        11,
        "udp",
        "recv_from",
        vec![Type::udp_socket(), Type::U64],
        [Type::Bytes, Type::String, Type::U16],
        Type::String
    );
    pair!(
        12,
        "udp",
        "close",
        vec![Type::udp_socket()],
        Type::Unit,
        Type::String
    );
    #[cfg(unix)]
    {
        pair!(
            13,
            "unix",
            "connect",
            vec![Type::String],
            Type::unix_stream(),
            Type::String
        );
        pair!(
            14,
            "unix",
            "listen",
            vec![Type::String],
            Type::unix_listener(),
            Type::String
        );
        pair!(
            15,
            "unix",
            "accept",
            vec![Type::unix_listener()],
            Type::unix_stream(),
            Type::String
        );
        pair!(
            16,
            "unix",
            "read",
            vec![Type::unix_stream(), Type::U64],
            Type::Bytes,
            Type::String
        );
        pair!(
            17,
            "unix",
            "write",
            vec![Type::unix_stream(), Type::Bytes],
            Type::U64,
            Type::String
        );
        pair!(
            18,
            "unix",
            "close",
            vec![Type::unix_stream()],
            Type::Unit,
            Type::String
        );
        pair!(
            19,
            "unix_dgram",
            "bind",
            vec![Type::String],
            Type::unix_datagram(),
            Type::String
        );
        pair!(
            20,
            "unix_dgram",
            "send_to",
            vec![Type::unix_datagram(), Type::Bytes, Type::String],
            Type::U64,
            Type::String
        );
        tuple!(
            21,
            "unix_dgram",
            "recv_from",
            vec![Type::unix_datagram(), Type::U64],
            [Type::Bytes, Type::String],
            Type::String
        );
        pair!(
            22,
            "unix_dgram",
            "close",
            vec![Type::unix_datagram()],
            Type::Unit,
            Type::String
        );
    }
    (ids, mask)
}
