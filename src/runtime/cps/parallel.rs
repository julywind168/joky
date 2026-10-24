//! Native worker packets contain checked lanes and exclusive hook-free owners.
use super::{task::CheckedEntry, CpsFrame};
use crate::runtime::{
    cleanup::Entry,
    parallel::{pool, Lease, OwnedBuffer},
};
use crate::{Error, ErrorCode, ErrorPhase};
use std::{ptr::NonNull, task::Poll};
fn invalid() -> Error {
    Error::new_with_code(
        ErrorPhase::Abi,
        ErrorCode::InvalidCall,
        "Unsupported parallel task packet",
    )
}
struct Packet {
    words: Vec<u64>,
    owners: Vec<(usize, OwnedBuffer)>,
    _charge: Option<Lease>,
}
impl Packet {
    fn new(words: Vec<u64>) -> Result<Self, Error> {
        let charge = crate::runtime::parallel::reserve_current(
            std::mem::size_of::<Self>() + words.len() * 32,
        )?;
        Ok(Self {
            words,
            owners: Vec::new(),
            _charge: charge,
        })
    }
    unsafe fn install(
        mut self,
        frame: *mut CpsFrame,
        result_key: Option<usize>,
    ) -> Result<Vec<u64>, Error> {
        let arena = super::arena(frame);
        for (index, owner) in &mut self.owners {
            let raw = owner.take(&*arena)?;
            self.words[*index] = raw as u64;
            (*super::cleanup(frame)).entries.insert(
                result_key.unwrap_or(*index),
                Entry {
                    keep_alive: None,
                    storage_size: 0,
                    storage: None,
                    owners: vec![Some(raw)],
                    hooks: vec![],
                },
            );
        }
        Ok(self.words)
    }
}
pub(super) unsafe fn factory(
    mut child: Box<CpsFrame>,
    code: &CheckedEntry,
) -> Result<pool::Factory, Error> {
    let (owners, output_owner) = code.layout.parallel_shape()?;
    let inputs = super::task::inputs(&mut *child);
    let mut packet = Packet::new((*inputs).slots.clone())?;
    let arena = super::arena(inputs);
    for index in owners {
        let owner = NonNull::new(packet.words[index] as *mut crate::runtime::buffer::Buffer)
            .ok_or_else(invalid)?;
        let moved = OwnedBuffer::new(&*arena, owner)?;
        // Successful packet admission transfers responsibility before another
        // fallible operation. Remaining inputs still belong to the local frame.
        (*super::cleanup(inputs)).entries.take(index);
        packet.owners.push((index, moved));
    }
    let entry = code.entry;
    let slots = code.frame_slots;
    let result_lanes = code.result_lanes;
    Ok(Box::new(move || {
        let mut frame = Box::new(CpsFrame::try_new(slots)?);
        let words = unsafe { packet.install(&mut *frame, None)? };
        frame.slots[..words.len()].copy_from_slice(&words);
        Ok(Box::new(NativeJob {
            frame,
            entry,
            result_lanes,
            output_owner,
        }))
    }))
}
struct NativeJob {
    frame: Box<CpsFrame>,
    entry: super::CpsEntry,
    result_lanes: usize,
    output_owner: bool,
}
impl pool::Job for NativeJob {
    fn poll(&mut self) -> Poll<Result<pool::Output, Error>> {
        let result = crate::runtime::error::checked(|| unsafe { (self.entry)(&mut *self.frame) });
        match result {
            Ok(1) => Poll::Pending,
            Err(error) => Poll::Ready(Err(error)),
            Ok(0) => Poll::Ready((|| {
                let words = if self.result_lanes == 0 {
                    vec![]
                } else {
                    vec![self.frame.result]
                };
                let mut packet = match Packet::new(words) {
                    Ok(packet) => packet,
                    Err(error) => {
                        self.release_result();
                        return Err(error);
                    }
                };
                if self.output_owner {
                    let arena = unsafe { super::arena(&mut *self.frame) };
                    let owner =
                        NonNull::new(self.frame.result as *mut crate::runtime::buffer::Buffer)
                            .ok_or_else(invalid)?;
                    match unsafe { OwnedBuffer::new(&*arena, owner) } {
                        Ok(owner) => packet.owners.push((0, owner)),
                        Err(error) => {
                            self.release_result();
                            return Err(error);
                        }
                    }
                }
                Ok(Box::new(packet) as pool::Output)
            })()),
            Ok(_) => Poll::Ready(Err(invalid())),
        }
    }
}
impl NativeJob {
    fn release_result(&mut self) {
        if self.output_owner {
            unsafe {
                let arena = super::arena(&mut *self.frame);
                let _ = crate::runtime::buffer::Buffer::release(
                    &mut *arena,
                    self.frame.result as *mut _,
                );
            }
        }
    }
}
pub(super) unsafe fn take(
    output: pool::Output,
    frame: *mut CpsFrame,
    key: Option<usize>,
    lanes: *mut u64,
    count: usize,
) -> Result<(), Error> {
    let packet = output.downcast::<Packet>().map_err(|_| invalid())?;
    if packet.words.len() != count {
        return Err(invalid());
    }
    let words = packet.install(frame, key)?;
    if count != 0 {
        std::ptr::copy_nonoverlapping(words.as_ptr(), lanes, count);
    }
    Ok(())
}
