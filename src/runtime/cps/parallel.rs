//! Native activations stay on their worker; checked inputs own values or scoped views.
use super::{task::CheckedEntry, CpsFrame};
use crate::runtime::parallel::{
    packet::{invalid, Packet, Shape},
    pool,
    scoped::{Input, InputShape, Loan, Scope},
};
use crate::Error;
use std::{rc::Rc, sync::Arc, task::Poll};

pub(super) unsafe fn scoped_eligible(child: &mut CpsFrame, code: &CheckedEntry) -> bool {
    if !code.scoped {
        return false;
    }
    let Ok((inputs, _)) = code.layout.scoped_shapes() else {
        return false;
    };
    let source = super::task::inputs(child);
    let mut offset = 0;
    for input in inputs {
        if let InputShape::Borrowed(shape) = &input {
            if !shape.eligible((&(*source).slots)[offset]) {
                return false;
            }
        }
        offset += input.inputs();
    }
    true
}

pub(super) unsafe fn factory(
    mut child: Box<CpsFrame>,
    code: &CheckedEntry,
    scope: Option<Arc<Scope>>,
) -> Result<pool::Factory, Error> {
    let (inputs, result) = if scope.is_some() {
        code.layout.scoped_shapes()?
    } else {
        let (inputs, result) = code.layout.parallel_shapes()?;
        (inputs.into_iter().map(InputShape::Owned).collect(), result)
    };
    let result_charge = crate::runtime::parallel::reserve_current(result.bytes())?;
    let source = super::task::inputs(&mut *child);
    let arena = super::arena(source);
    let mut packets = Vec::new();
    let mut offset = 0;
    for input in inputs {
        match input {
            InputShape::Borrowed(shape) => {
                packets.push(Input::borrow(
                    shape,
                    (&(*source).slots)[offset],
                    scope.as_ref().unwrap().clone(),
                )?);
                offset += 1;
            }
            InputShape::Owned(shape) => {
                let words = (&(*source).slots)[offset..offset + shape.lanes()].to_vec();
                let entry = shape
                    .owners()
                    .first()
                    .and_then(|key| (*super::cleanup(source)).entries.take(offset + key));
                offset += shape.inputs();
                packets.push(Input::owned(Packet::capture(
                    shape,
                    words,
                    entry,
                    &mut *arena,
                )?));
            }
        }
    }
    let entry = code.entry;
    let slots = code.frame_slots;
    Ok(Box::new(move || {
        let _result_charge = result_charge;
        start_inputs(entry, slots, offset, result, packets)
    }))
}

pub(crate) fn start(
    entry: super::CpsEntry,
    slots: usize,
    result_offset: usize,
    result: Shape,
    packets: Vec<Packet>,
) -> Result<Box<dyn pool::Job>, Error> {
    start_inputs(
        entry,
        slots,
        result_offset,
        result,
        packets.into_iter().map(Input::owned).collect(),
    )
}
fn start_inputs(
    entry: super::CpsEntry,
    slots: usize,
    result_offset: usize,
    result: Shape,
    packets: Vec<Input>,
) -> Result<Box<dyn pool::Job>, Error> {
    let shape_charge = crate::runtime::parallel::reserve_current(
        std::mem::size_of::<NativeJob>()
            + result.bytes()
            + packets.len() * std::mem::size_of::<Rc<Loan>>(),
    )?;
    let mut loans = Vec::new();
    let mut frame = Box::new(CpsFrame::try_new(slots)?);
    frame.parallel_return = true;
    let mut offset = 0;
    for packet in packets {
        let inputs = packet.inputs();
        let words = unsafe {
            let arena = super::arena(&mut *frame);
            let cleanups = &mut (*super::cleanup(&mut *frame)).entries;
            packet.install(&mut *arena, cleanups, offset, &mut loans)?
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
        _loans: loans,
    }))
}
struct NativeJob {
    frame: Box<CpsFrame>,
    entry: super::CpsEntry,
    result_offset: usize,
    result: Shape,
    _shape_charge: Option<crate::runtime::parallel::Lease>,
    _loans: Vec<Rc<Loan>>,
}
impl pool::Job for NativeJob {
    fn poll(&mut self) -> Poll<Result<pool::Output, Error>> {
        let _loans = crate::runtime::parallel::scoped::LoanGuard::enter(&self._loans);
        let result = crate::runtime::error::checked(|| unsafe { (self.entry)(&mut *self.frame) });
        match result {
            Ok(1) => {
                pool::wait_plan(self.frame.wait_plan());
                Poll::Pending
            }
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
                    if let Some(cleanup) = self.frame.returned_cleanup.take() {
                        Packet::capture_value(
                            self.result.clone(),
                            words,
                            Some(cleanup),
                            &mut *super::arena(&mut *self.frame),
                            true,
                        )?
                    } else {
                        Packet::returned(
                            self.result.clone(),
                            words,
                            &mut *super::arena(&mut *self.frame),
                        )?
                    }
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
    let mut packet = output.downcast::<Packet>().map_err(|_| invalid())?;
    if packet.words.len() != count {
        return Err(invalid());
    }
    if matches!(&packet.shape, Shape::Place(_)) {
        packet.install_place(&mut *super::arena(frame), *lanes as *mut u8)?;
        return Ok(());
    }
    let arena = super::arena(frame);
    let words = packet.install(&mut *arena, &mut (*super::cleanup(frame)).entries, 0, key)?;
    if count != 0 {
        std::ptr::copy_nonoverlapping(words.as_ptr(), lanes, count);
    }
    Ok(())
}
