//! Versioned physical capture/result layouts emitted by the checked compiler.
//! These describe Local storage, not Transfer/Share or parallel admission.
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, Type};

pub(crate) const VERSION: usize = 1;

fn invalid() -> Error {
    Error::new_with_code(
        ErrorPhase::Abi,
        ErrorCode::AbiError,
        "Invalid task entry layout v1",
    )
}

fn tag(ty: &Type) -> u64 {
    match ty {
        Type::Bool => 1,
        Type::Int8 => 2,
        Type::UInt8 => 3,
        Type::Int16 => 4,
        Type::UInt16 => 5,
        Type::Int32 => 6,
        Type::UInt32 => 7,
        Type::Rune => 8,
        Type::Float32 => 9,
        Type::Int64 => 10,
        Type::UInt64 => 11,
        Type::Float64 => 12,
        _ if ty.is_owner() => 14,
        _ => 13,
    }
}

fn value(out: &mut Vec<u64>, ty: &Type) -> Result<(), Error> {
    let layout = ty.storage_layout().map_err(|_| invalid())?;
    let flags = u64::from(ty.is_owner())
        | (u64::from(ty.is_place()) << 1)
        | (u64::from(ty.reference_type().is_some()) << 2);
    let fields = if ty.is_place() {
        Vec::new()
    } else {
        ty.storage_scalars().map_err(|_| invalid())?
    };
    out.extend([
        flags,
        ty.parameter_slots() as u64,
        ty.scalar_types().len() as u64,
        layout.size as u64,
        layout.align as u64,
        fields.len() as u64,
    ]);
    for (offset, ty) in fields {
        out.extend([offset as u64, tag(&ty)]);
    }
    let copy = if ty.is_place() {
        ty.place_copy_program()
    } else {
        Vec::new()
    };
    out.push(copy.len() as u64);
    for node in copy {
        out.extend([node.offset, node.kind, node.count, node.stride, node.end]);
    }
    Ok(())
}

pub(crate) fn encode(signature: &FunctionSignature) -> Result<Vec<u64>, Error> {
    let mut out = vec![VERSION as u64, signature.params.len() as u64];
    for ty in &signature.params {
        value(&mut out, ty)?;
    }
    value(&mut out, &signature.ret)?;
    Ok(out)
}

pub(crate) struct CheckedLayout {
    _charge: Option<crate::runtime::parallel::Lease>,
    // Retain the exact compiler layout with its packet, independently of module data.
    words: Box<[u64]>,
}

struct Reader<'a>(&'a [u64]);
impl Reader<'_> {
    fn word(&mut self) -> Result<usize, Error> {
        let (&value, rest) = self.0.split_first().ok_or_else(invalid)?;
        self.0 = rest;
        usize::try_from(value).map_err(|_| invalid())
    }
    fn value(&mut self) -> Result<(usize, usize), Error> {
        let flags = self.word()?;
        let inputs = self.word()?;
        let scalars = self.word()?;
        let size = self.word()?;
        let align = self.word()?;
        if flags & !7 != 0 || !align.is_power_of_two() {
            return Err(invalid());
        }
        let fields = self.word()?;
        if fields > self.0.len() / 2 {
            return Err(invalid());
        }
        for _ in 0..fields {
            let offset = self.word()?;
            let bytes = match self.word()? {
                1..=3 => 1,
                4..=5 => 2,
                6..=9 => 4,
                10..=12 => 8,
                13..=14 => std::mem::size_of::<usize>(),
                _ => return Err(invalid()),
            };
            if offset.checked_add(bytes).is_none_or(|end| end > size) {
                return Err(invalid());
            }
        }
        let nodes = self.word()?;
        let words = nodes.checked_mul(5).ok_or_else(invalid)?;
        self.0 = self.0.get(words..).ok_or_else(invalid)?;
        let result = scalars.checked_add(size.div_ceil(8)).ok_or_else(invalid)?;
        Ok((inputs, result))
    }
}

impl CheckedLayout {
    pub(crate) fn parallel_shape(&self) -> Result<(Vec<usize>, bool), Error> {
        fn shape(reader: &mut Reader<'_>) -> Result<(usize, bool), Error> {
            let words = reader.0;
            let (inputs, _) = reader.value()?;
            if words[0] & 6 != 0 || words[2] > 1 || words[5] > 1 {
                return Err(invalid());
            }
            let owner = words[0] == 1;
            if words[5] == 1 {
                let tag = words[7];
                if !(1..=12).contains(&tag) && !(owner && tag == 14) {
                    return Err(invalid());
                }
            }
            Ok((inputs, owner))
        }
        let mut reader = Reader(&self.words[2..]);
        let mut owners = Vec::new();
        let mut offset = 0;
        for _ in 0..self.words[1] {
            let (slots, owner) = shape(&mut reader)?;
            if owner {
                owners.push(offset);
            }
            offset += slots;
        }
        Ok((owners, shape(&mut reader)?.1))
    }

    pub(crate) fn decode(words: &[u64]) -> Result<Self, Error> {
        Self::read_counts(words)?;
        let charge = crate::runtime::parallel::reserve_current(
            std::mem::size_of::<Self>() + std::mem::size_of_val(words),
        )?;
        Ok(Self {
            _charge: charge,
            words: words.into(),
        })
    }

    pub(crate) fn result_lanes(&self) -> usize {
        let mut reader = Reader(&self.words);
        reader.word().unwrap();
        let count = reader.word().unwrap();
        for _ in 0..count {
            reader.value().unwrap();
        }
        reader.0[2] as usize
    }

    pub(crate) fn slot_counts(&self) -> (usize, usize) {
        Self::read_counts(&self.words).expect("immutable checked task layout")
    }

    fn read_counts(words: &[u64]) -> Result<(usize, usize), Error> {
        let mut reader = Reader(words);
        if reader.word()? != VERSION {
            return Err(invalid());
        }
        let count = reader.word()?;
        if count > reader.0.len() / 7 {
            return Err(invalid());
        }
        let mut input_slots = 0usize;
        for _ in 0..count {
            input_slots = input_slots
                .checked_add(reader.value()?.0)
                .ok_or_else(invalid)?;
        }
        let result_slots = reader.value()?.1;
        if !reader.0.is_empty() {
            return Err(invalid());
        }
        Ok((input_slots, result_slots))
    }
}
