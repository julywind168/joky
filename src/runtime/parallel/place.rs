//! Compact, pointer-free descriptions of inline values sent between workers.
//! Owning lanes are relocated through OwnedBuffer, never shared as raw headers.
use super::packet::invalid;
use crate::{runtime::place::CopyNode, Error, Type};

#[derive(Clone, Debug)]
pub(crate) struct Layout {
    pub(crate) size: usize,
    pub(crate) align: usize,
    pub(crate) nodes: Vec<CopyNode>,
}
impl Layout {
    pub(crate) fn of(ty: &Type) -> Result<Self, Error> {
        if !ty.is_place() || !ty.is_fixed_array_data() {
            return Err(invalid());
        }
        let layout = ty.storage_layout().map_err(|_| invalid())?;
        let value = Self {
            size: layout.size,
            align: layout.align,
            nodes: ty.place_copy_program(),
        };
        value.validate()?;
        Ok(value)
    }
    pub(crate) fn owner_count(&self) -> usize {
        fn count(nodes: &[CopyNode], i: usize) -> usize {
            let node = nodes[i];
            if node.kind == 4 {
                return 1;
            }
            if node.kind == 2 {
                return node.count as usize * count(nodes, i + 1);
            }
            let mut sum = 0;
            let mut child = i + 1;
            while child < node.end as usize {
                sum += count(nodes, child);
                child = nodes[child].end as usize;
            }
            sum
        }
        count(&self.nodes, 0)
    }
    pub(crate) fn bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<CopyNode>()
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        fn walk(nodes: &[CopyNode], i: usize, depth: usize) -> Result<(usize, usize), Error> {
            let n = nodes.get(i).ok_or_else(invalid)?;
            if depth > 32 || n.end as usize <= i || n.end as usize > nodes.len() {
                return Err(invalid());
            }
            let mut child = i + 1;
            let mut extent = 0usize;
            let mut work = 0usize;
            let mut children = 0;
            while child < n.end as usize {
                let (size, weight) = walk(nodes, child, depth + 1)?;
                extent = extent.max(size);
                // Nullary variants have a zero-byte scalar placeholder. Their
                // tags count once, rather than once per inactive placeholder.
                if n.kind != 3 || nodes[child].kind != 0 || nodes[child].count != 0 {
                    work = work.checked_add(weight).ok_or_else(invalid)?;
                }
                child = nodes[child].end as usize;
                children += 1;
            }
            if child != n.end as usize {
                return Err(invalid());
            }
            match n.kind {
                0 if children == 0 && matches!(n.count, 0 | 1 | 2 | 4 | 8) => {
                    extent = n.count as usize;
                    work = 1;
                }
                4 if children == 0 && n.count as usize == std::mem::size_of::<usize>() => {
                    extent = n.count as usize;
                    work = 1;
                }
                1 => work = work.max(1),
                2 if children == 1 && n.count <= 65_536 => {
                    if extent > n.stride as usize {
                        return Err(invalid());
                    }
                    extent = (n.count as usize)
                        .checked_mul(n.stride as usize)
                        .ok_or_else(invalid)?;
                    work = work
                        .checked_mul((n.count as usize).max(1))
                        .ok_or_else(invalid)?;
                }
                3 if children == n.count as usize && children > 0 => {
                    extent = extent.max(4);
                    work = work.checked_add(1).ok_or_else(invalid)?;
                }
                _ => return Err(invalid()),
            }
            if work > 65_536 {
                return Err(invalid());
            }
            Ok((
                extent.checked_add(n.offset as usize).ok_or_else(invalid)?,
                work,
            ))
        }
        if self.size > 64 * 1024 * 1024
            || !self.align.is_power_of_two()
            || self.align > std::mem::align_of::<u64>()
            || self.nodes.is_empty()
            || self.nodes.len() > 65_536
            || self.nodes[0].end as usize != self.nodes.len()
            || walk(&self.nodes, 0, 0)?.0 > self.size
        {
            return Err(invalid());
        }
        Ok(())
    }
    /// # Safety
    /// Self is validated; source is a live initialized value of this exact layout.
    pub(crate) unsafe fn snapshot(&self, source: *const u8) -> Vec<u64> {
        let mut words = vec![0u64; self.size.max(1).div_ceil(8)];
        crate::runtime::place::copy_fields(words.as_mut_ptr().cast(), source, &self.nodes, false);
        words
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::parallel::packet::Shape;
    #[test]
    fn task_fixed_array_descriptors_reject_unbounded_or_foreign_storage() {
        // A compact array of two Int32 values, followed by corruptions that must
        // fail before any payload is read or a worker is started.
        let valid = vec![19, 8, 4, 2, 0, 2, 2, 4, 2, 0, 0, 4, 0, 2];
        assert!(Shape::decode(&valid).is_ok());
        let mut maximum = valid.clone();
        maximum[1] = 65536 * 4;
        maximum[6] = 65536;
        assert!(Shape::decode(&maximum).is_ok());
        for (index, value) in [
            (1, 4),
            (2, 16),
            (3, 0),
            (4, 8),
            (5, 99),
            (6, 65537),
            (7, 2),
            (8, 1),
            (11, 16),
            (13, 3),
        ] {
            let mut words = valid.clone();
            words[index] = value;
            assert!(Shape::decode(&words).is_err(), "{index}={value}");
        }
        for len in 0..valid.len() {
            assert!(Shape::decode(&valid[..len]).is_err());
        }
        let mut nested = vec![16, 1];
        nested.extend(valid);
        assert!(Shape::decode(&nested).is_err());
    }
}
