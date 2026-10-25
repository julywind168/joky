//! Native activations stay on their worker; packets own only checked flat values.
use super::{task::CheckedEntry, CpsFrame};
use crate::runtime::parallel::{
    packet::{invalid, Packet, Shape},
    pool,
};
use crate::Error;
use std::task::Poll;

pub(super) unsafe fn factory(
    mut child: Box<CpsFrame>,
    code: &CheckedEntry,
) -> Result<pool::Factory, Error> {
    let (inputs, result) = code.layout.parallel_shapes()?;
    let result_charge = crate::runtime::parallel::reserve_current(result.bytes())?;
    let source = super::task::inputs(&mut *child);
    let arena = super::arena(source);
    let mut packets = Vec::new();
    let mut offset = 0;
    for shape in inputs {
        let words = (&(*source).slots)[offset..offset + shape.lanes()].to_vec();
        let entry = shape
            .owners()
            .first()
            .and_then(|key| (*super::cleanup(source)).entries.take(offset + key));
        offset += shape.inputs();
        packets.push(Packet::capture(shape, words, entry, &mut *arena)?);
    }
    let entry = code.entry;
    let slots = code.frame_slots;
    Ok(Box::new(move || {
        let _result_charge = result_charge;
        start(entry, slots, offset, result, packets)
    }))
}

pub(crate) fn start(
    entry: super::CpsEntry,
    slots: usize,
    result_offset: usize,
    result: Shape,
    packets: Vec<Packet>,
) -> Result<Box<dyn pool::Job>, Error> {
    let shape_charge = crate::runtime::parallel::reserve_current(
        std::mem::size_of::<NativeJob>() + result.bytes(),
    )?;
    let mut frame = Box::new(CpsFrame::try_new(slots)?);
    let mut offset = 0;
    for packet in packets {
        let inputs = packet.shape.inputs();
        let words = unsafe {
            let arena = super::arena(&mut *frame);
            let cleanups = &mut (*super::cleanup(&mut *frame)).entries;
            packet.install(&*arena, cleanups, offset, None)?
        };
        frame.slots[offset..offset + words.len()].copy_from_slice(&words);
        offset += inputs;
    }
    Ok(Box::new(NativeJob {
        frame,
        entry,
        result_offset,
        result,
        _shape_charge: shape_charge,
    }))
}
struct NativeJob {
    frame: Box<CpsFrame>,
    entry: super::CpsEntry,
    result_offset: usize,
    result: Shape,
    _shape_charge: Option<crate::runtime::parallel::Lease>,
}
impl pool::Job for NativeJob {
    fn poll(&mut self) -> Poll<Result<pool::Output, Error>> {
        let result = crate::runtime::error::checked(|| unsafe { (self.entry)(&mut *self.frame) });
        match result {
            Ok(1) => Poll::Pending,
            Err(error) => {
                let error = match pool::code::<crate::vm::parallel::Code>().as_deref() {
                    Some(crate::vm::parallel::Code::Native(code)) => {
                        error.with_sources(&code.sources)
                    }
                    _ => error,
                };
                Poll::Ready(Err(error))
            }
            Ok(0) => Poll::Ready((|| {
                let words = if self.result.aggregate() {
                    self.frame.slots[self.result_offset..self.result_offset + self.result.lanes()]
                        .to_vec()
                } else if self.result.lanes() == 0 {
                    vec![]
                } else {
                    vec![self.frame.result]
                };
                let packet = unsafe {
                    Packet::returned(
                        self.result.clone(),
                        words,
                        &mut *super::arena(&mut *self.frame),
                    )?
                };
                Ok(Box::new(packet) as pool::Output)
            })()),
            Ok(_) => Poll::Ready(Err(invalid())),
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
    let arena = super::arena(frame);
    let words = packet.install(&*arena, &mut (*super::cleanup(frame)).entries, 0, key)?;
    if count != 0 {
        std::ptr::copy_nonoverlapping(words.as_ptr(), lanes, count);
    }
    Ok(())
}
