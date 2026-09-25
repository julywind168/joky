//! Type system definitions

/// A Joky type
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum Type {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    String,
    CPtr(usize),
    CMutPtr(usize),
    CArray(usize),
    CStr,
    CCallback,
    Bytes,
    Hasher,
    /// Compiler-internal shared iteration coordinator; no source-level constructor.
    Batch,
    /// Compiler-internal sequential result builder; no source-level constructor.
    SeqBuilder,
    /// Compiler-internal shared byte cursor produced by `bytes.iter()`.
    BytesCursor,
    MutBytes,
    /// Opaque runtime-managed handle declared by an `@intrinsic` class in a
    /// standard module. Index into `NATIVE_TYPES`; a new handle class is a
    /// registry row, not a compiler change.
    Native(usize),
    Bool,
    Duration,
    Unit,
    Tuple(usize),
    Struct(usize),
    Class(usize),
    Enum(usize),
    Option(usize),
    Result(usize),
    List(usize),
    MutList(usize),
    Map(usize),
    /// Uniquely owned entry cursor produced by `map.entries()`; index into the
    /// shared `maps` table.
    MapCursor(usize),
    /// Key-only cursor produced by `map.keys()` / `set.iter()`.
    MapKeyCursor(usize),
    /// Value-only cursor produced by `map.values()`.
    MapValueCursor(usize),
    MutMap(usize),
    MutSet(usize),
    /// Uniquely owned index cursor produced by `MutList.into_iter()`.
    MutListCursor(usize),
    /// Uniquely owned packed-slot cursor produced by `MutMap.into_iter()`.
    MutMapCursor(usize),
    /// Key-only packed-slot cursor produced by `MutSet.into_iter()`.
    MutSetCursor(usize),
    Cown(usize),
    Function(usize),
    Dyn(usize),
    Param(usize),
    SelfType,
    Associated(usize),
}

impl Type {
    /// Handle names resolvable as native resources. Keep stable: the index is
    /// part of the serialized module cache.
    pub(crate) const NATIVE_TYPES: [&str; 12] = [
        "File",
        "SqliteConnection",
        "SqliteStatement",
        "TcpListener",
        "TcpStream",
        "UdpSocket",
        "UnixListener",
        "UnixStream",
        "UnixDatagram",
        "TcpReadHalf",
        "TcpWriteHalf",
        "TlsStream",
    ];

    #[cfg(test)]
    pub(crate) const fn file() -> Self {
        Self::Native(0)
    }

    #[cfg(test)]
    pub(crate) const fn sqlite_connection() -> Self {
        Self::Native(1)
    }

    #[cfg(test)]
    pub(crate) const fn sqlite_statement() -> Self {
        Self::Native(2)
    }

    #[cfg(test)]
    pub(crate) const fn tcp_listener() -> Self {
        Self::Native(3)
    }

    #[cfg(test)]
    pub(crate) const fn tcp_stream() -> Self {
        Self::Native(4)
    }

    #[cfg(test)]
    pub(crate) const fn udp_socket() -> Self {
        Self::Native(5)
    }

    #[cfg(test)]
    pub(crate) const fn unix_listener() -> Self {
        Self::Native(6)
    }

    #[cfg(test)]
    pub(crate) const fn unix_stream() -> Self {
        Self::Native(7)
    }

    #[cfg(test)]
    pub(crate) const fn unix_datagram() -> Self {
        Self::Native(8)
    }

    pub(crate) const fn is_c_pointer(self) -> bool {
        matches!(self, Self::CPtr(_) | Self::CMutPtr(_) | Self::CStr)
    }
    pub(crate) const fn is_c_scalar(self) -> bool {
        matches!(
            self,
            Self::I8
                | Self::I16
                | Self::I32
                | Self::I64
                | Self::U8
                | Self::U16
                | Self::U32
                | Self::U64
                | Self::F32
                | Self::F64
        )
    }
    pub(crate) const fn is_c_abi_compatible(self) -> bool {
        self.is_c_scalar() || matches!(self, Self::CPtr(_) | Self::CMutPtr(_) | Self::CStr)
    }
    pub(crate) const fn is_native_resource(self) -> bool {
        matches!(self, Self::CCallback | Self::Native(_))
    }
    /// Returns whether this is an integer type
    pub(crate) const fn is_integer(self) -> bool {
        matches!(
            self,
            Self::I8
                | Self::I16
                | Self::I32
                | Self::I64
                | Self::U8
                | Self::U16
                | Self::U32
                | Self::U64
        )
    }

    /// Returns whether this is a signed integer type
    pub(crate) const fn is_signed_integer(self) -> bool {
        matches!(self, Self::I8 | Self::I16 | Self::I32 | Self::I64)
    }

    /// Returns whether this is a float type
    pub(crate) const fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    /// Returns whether this is a numeric type (integer or float)
    pub(crate) const fn is_numeric(self) -> bool {
        self.is_integer() || self.is_float()
    }

    pub(crate) const fn has_builtin_from_string(self) -> bool {
        self.is_numeric() || matches!(self, Self::Bool)
    }
}

/// Resolves a user-visible primitive type name to the internal type
pub(crate) fn primitive_type(name: &str) -> Option<Type> {
    match name {
        "Int8" => Some(Type::I8),
        "Int16" => Some(Type::I16),
        "Int32" => Some(Type::I32),
        "Int64" => Some(Type::I64),
        "UInt8" => Some(Type::U8),
        "UInt16" => Some(Type::U16),
        "UInt32" => Some(Type::U32),
        "UInt64" => Some(Type::U64),
        "Float32" => Some(Type::F32),
        "Float64" => Some(Type::F64),
        "String" => Some(Type::String),
        "CStr" => Some(Type::CStr),
        "CCallback" => Some(Type::CCallback),
        "Bytes" => Some(Type::Bytes),
        "BytesCursor" => Some(Type::BytesCursor),
        "Hasher" => Some(Type::Hasher),
        "MutBytes" => Some(Type::MutBytes),
        "Bool" => Some(Type::Bool),
        "Duration" => Some(Type::Duration),
        "Unit" => Some(Type::Unit),
        // Socket and file handles share the native resource representation;
        // the runtime enforces stream/listener capabilities.
        _ => Type::NATIVE_TYPES
            .iter()
            .position(|candidate| *candidate == name)
            .map(Type::Native),
    }
}

/// The bit width and signedness of an integer type; `None` for anything else.
pub(crate) fn integer_shape(value: Type) -> Option<(u8, bool)> {
    match value {
        Type::I8 => Some((8, true)),
        Type::I16 => Some((16, true)),
        Type::I32 => Some((32, true)),
        Type::I64 => Some((64, true)),
        Type::U8 => Some((8, false)),
        Type::U16 => Some((16, false)),
        Type::U32 => Some((32, false)),
        Type::U64 => Some((64, false)),
        _ => None,
    }
}

/// Returns the string name of a type
pub(crate) fn type_name(value: Type) -> String {
    match value {
        Type::I8 => "Int8".to_owned(),
        Type::I16 => "Int16".to_owned(),
        Type::I32 => "Int32".to_owned(),
        Type::I64 => "Int64".to_owned(),
        Type::U8 => "UInt8".to_owned(),
        Type::U16 => "UInt16".to_owned(),
        Type::U32 => "UInt32".to_owned(),
        Type::U64 => "UInt64".to_owned(),
        Type::F32 => "Float32".to_owned(),
        Type::F64 => "Float64".to_owned(),
        Type::String => "String".to_owned(),
        Type::CPtr(_) => "CPtr".to_owned(),
        Type::CMutPtr(_) => "CMutPtr".to_owned(),
        Type::CArray(_) => "CArray".to_owned(),
        Type::CStr => "CStr".to_owned(),
        Type::CCallback => "CCallback".to_owned(),
        Type::Bytes => "Bytes".to_owned(),
        Type::Batch => "<iteration state>".to_owned(),
        Type::SeqBuilder => "<result builder>".to_owned(),
        Type::BytesCursor => "BytesCursor".to_owned(),
        Type::Hasher => "Hasher".to_owned(),
        Type::MutBytes => "MutBytes".to_owned(),
        Type::Native(index) => Type::NATIVE_TYPES
            .get(index)
            .copied()
            .unwrap_or("native")
            .to_owned(),
        Type::Bool => "Bool".to_owned(),
        Type::Duration => "Duration".to_owned(),
        Type::Unit => "Unit".to_owned(),
        Type::Tuple(_) => "tuple".to_owned(),
        Type::Struct(_) => "struct".to_owned(),
        Type::Class(_) => "class".to_owned(),
        Type::Enum(_) => "enum".to_owned(),
        Type::Option(_) => "option".to_owned(),
        Type::Result(_) => "result".to_owned(),
        Type::List(_) => "list".to_owned(),
        Type::MutList(_) => "MutList".to_owned(),
        Type::Map(_) => "map".to_owned(),
        Type::MapCursor(_) => "MapCursor".to_owned(),
        Type::MapKeyCursor(_) => "MapKeyCursor".to_owned(),
        Type::MapValueCursor(_) => "MapValueCursor".to_owned(),
        Type::MutMap(_) => "MutMap".to_owned(),
        Type::MutSet(_) => "MutSet".to_owned(),
        Type::MutListCursor(_) => "MutListCursor".to_owned(),
        Type::MutMapCursor(_) => "MutMapCursor".to_owned(),
        Type::MutSetCursor(_) => "MutSetCursor".to_owned(),
        Type::Cown(_) => "Cown".to_owned(),
        Type::Function(id) => format!("fn<{id}>"),
        Type::Dyn(id) => format!("Dyn<{id}>"),
        Type::Param(id) => format!("type parameter #{id}"),
        Type::SelfType => "Self".to_owned(),
        Type::Associated(id) => format!("associated type #{id}"),
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MapInfo {
    pub(crate) key: Type,
    pub(crate) value: Type,
}
