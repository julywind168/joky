//! Bounded value packets reuse the ordinary domain FIFO and late binding.
use super::*;
use crate::runtime::buffer::Buffer;

#[cfg(test)]
mod tests;

pub(super) fn result_words(ty: &Type, mut words: Vec<u64>) -> Result<Vec<u64>, Error> {
    // Ordinary typed trampolines retain a Unit placeholder; packets do not.
    if *ty == Type::Unit {
        words.clear();
    }
    let types = ty.scalar_types();
    if types.len() != words.len()
        || types
            .iter()
            .zip(&words)
            .any(|(ty, &word)| InstanceValue::from_bits(ty, word).is_none())
    {
        return Err(invalid("Invalid domain value result"));
    }
    Ok(words)
}

/// Copy a checked pure-value packet into the private typed trampoline lanes.
///
/// # Safety
/// The packet must point to a live compiler-owned UInt64 buffer.
pub(super) unsafe fn input_words(params: &[Type], packet: &Buffer) -> Result<Vec<u64>, Error> {
    if packet.len() > crate::core::script::MAX_LANES as u64 {
        return Err(limited("Domain value packet exceeds lane limit"));
    }
    let types = params
        .iter()
        .flat_map(Type::scalar_types)
        .collect::<Vec<_>>();
    if types.len() as u64 != packet.len() {
        return Err(invalid("Domain value input length mismatch"));
    }
    let mut input = Vec::with_capacity(types.len() + usize::from(params == [Type::Unit]));
    for (i, ty) in types.iter().enumerate() {
        let word = packet.load(i as u64)?.cast::<u64>().read();
        if InstanceValue::from_bits(ty, word).is_none() {
            return Err(invalid("Invalid domain value input"));
        }
        input.push(word);
    }
    if params == [Type::Unit] {
        input.push(0);
    }
    Ok(input)
}

/// Copy checked result lanes into a caller-owned output packet.
/// # Safety
/// The packet is a live UInt64 buffer with at least MAX_LANES capacity.
pub(super) unsafe fn write_packet(
    arena: &mut Arena,
    packet: &mut Buffer,
    words: &[u64],
) -> Result<(), Error> {
    packet.truncate_owned(arena, 0)?;
    for &word in words {
        packet.initialize_next()?.cast::<u64>().write(word);
        packet.commit_next();
    }
    Ok(())
}

struct Wait {
    request: DomainRequest,
    output: Rc<RefCell<Vec<u64>>>,
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    let pointer = (*frame).host_context.cast::<Wait>();
    (*frame).host_context = std::ptr::null_mut();
    (*frame).host_stop = None;
    if !pointer.is_null() {
        drop(Box::from_raw(pointer));
    }
}

/// The compiler-owned frame must match Values/CurrentValues. Its borrowed
/// packet is a live UInt64 buffer with MAX_LANES capacity, owned by the caller
/// through completion or stop; host_context is exclusively managed here.
pub(super) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    registry: &Rc<RefCell<Domains>>,
    ambient: bool,
) -> Result<Option<u64>, Error> {
    let packet_slot = if ambient { 1 } else { 5 };
    let initial = frame.state == 0;
    let result = (|| {
        if frame.state == 0 {
            let (handles, module, name_slot) = if ambient {
                let (handle, module) = tasks::value_context(registry)?;
                (vec![handle], module, 0)
            } else {
                let count = frame.slots[3] as usize;
                if !(1..=2).contains(&count) {
                    return Err(invalid("Invalid domain value access count"));
                }
                let handles = (0..count)
                    .map(|i| source::handle(registry, frame.slots[1 + i]))
                    .collect::<Result<Vec<_>, _>>()?;
                let (module, _) = source::module(registry, frame.slots[0])?;
                (handles, module, 4)
            };
            let count = handles.len();
            let name = crate::runtime::owned_text::borrow(frame.slots[name_slot] as *const Buffer)?;
            let info = module.instance_info(name)?;
            if !info.domain_entry
                || !crate::core::state_domain::value_eligible(&info)
                || crate::core::state_domain::access_count(&info) != count
            {
                return Err(invalid("Invalid domain value entry"));
            }
            let packet = &*(frame.slots[packet_slot] as *const Buffer);
            let input = input_words(&info.signature.params[count..], packet)?;
            let output = Rc::new(RefCell::new(vec![
                0;
                info.signature.ret.scalar_types().len()
            ]));
            let request = pairs::submit_values(
                registry,
                &handles,
                &module,
                name,
                input,
                pairs::ValueOutput {
                    words: output.clone(),
                    snapshot: false,
                },
                crate::runtime::suspension::current_limits(),
            )?;
            request.register_current_wait()?;
            frame.host_context = Box::into_raw(Box::new(Wait { request, output })).cast();
            frame.host_stop = Some(stop);
            frame.state = 1;
            frame.wake = None;
            return Ok(None);
        }
        if frame.state != 1 || frame.host_context.is_null() {
            return Err(invalid("Invalid domain value wait state"));
        }
        let wait = &mut *frame.host_context.cast::<Wait>();
        let Some((outcome, rejection)) = wait.request.take_access_result() else {
            wait.request.register_current_wait()?;
            return Ok(None);
        };
        if let Some(rejection) = rejection {
            return Ok(Some(rejection as u64));
        }
        match outcome {
            DomainOutcome::Value(InstanceValue::Unit) => {
                let packet = &mut *(frame.slots[packet_slot] as *mut Buffer);
                write_packet(arena, packet, &wait.output.borrow())?;
                Ok(Some(0))
            }
            DomainOutcome::Failed(error) => Err(error),
            _ => Err(invalid("Domain value operation was cancelled or closed")),
        }
    })();
    if !matches!(result, Ok(None)) {
        stop(frame, 0);
    }
    match result {
        Err(error) if initial => {
            let op = if ambient {
                crate::runtime::domain::Operation::CurrentValues
            } else {
                crate::runtime::domain::Operation::Values
            };
            Ok(Some(
                access::source_rejection(registry, frame, op, &error) as u64
            ))
        }
        result => result,
    }
}
