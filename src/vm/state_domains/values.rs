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
    crate::runtime::domain_packet::validate(std::slice::from_ref(ty), &words)?;
    Ok(words)
}

/// Copy and validate an independent domain value packet.
///
/// # Safety
/// The packet must point to a live compiler-owned UInt64 buffer.
pub(super) unsafe fn input_words(params: &[Type], packet: &Buffer) -> Result<Vec<u64>, Error> {
    let count =
        usize::try_from(packet.len()).map_err(|_| limited("Domain packet size overflow"))?;
    let mut input = Vec::new();
    input
        .try_reserve_exact(count + usize::from(params == [Type::Unit]))
        .map_err(|_| limited("Domain packet allocation failed"))?;
    for i in 0..count {
        input.push(packet.load(i as u64)?.cast::<u64>().read());
    }
    crate::runtime::domain_packet::validate(params, &input)?;
    if params == [Type::Unit] {
        input.push(0);
    }
    Ok(input)
}

/// Copy checked result lanes into a caller-owned output packet.
/// # Safety
/// The packet is a live exclusive UInt64 buffer; it grows for String payloads.
pub(super) unsafe fn write_packet(
    arena: &mut Arena,
    packet: &mut Buffer,
    words: &[u64],
) -> Result<(), Error> {
    crate::runtime::domain_packet::write(arena, packet, words)
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
    op: crate::runtime::domain::Operation,
) -> Result<Option<u64>, Error> {
    use crate::runtime::domain::Operation;
    let ambient = op == Operation::CurrentValues;
    let many = op == Operation::When;
    let packet_slot = if ambient {
        1
    } else if many {
        3
    } else {
        5
    };
    let initial = frame.state == 0;
    let result = (|| {
        if frame.state == 0 {
            let (handles, module, name_slot) = if ambient {
                let (handle, module) = tasks::value_context(registry)?;
                (vec![handle], module, 0)
            } else if many {
                let packet = &*(frame.slots[2] as *const Buffer);
                if !(1..=64).contains(&packet.len()) {
                    return Err(invalid("Invalid when access count"));
                }
                let handles = (0..packet.len())
                    .map(|i| source::handle(registry, packet.load(i)?.cast::<u64>().read()))
                    .collect::<Result<Vec<_>, Error>>()?;
                let (module, _) = source::module(registry, frame.slots[0])?;
                (handles, module, 1)
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
            if many {
                let packet = &*(frame.slots[2] as *const Buffer);
                let tokens = (0..packet.len().min(64))
                    .map(|i| Ok(packet.load(i)?.cast::<u64>().read()))
                    .collect::<Result<Vec<_>, Error>>()?;
                return Ok(Some(access::source_tokens(registry, &tokens, &error) as u64));
            }
            Ok(Some(
                access::source_rejection(registry, frame, op, &error) as u64
            ))
        }
        result => result,
    }
}
