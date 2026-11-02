//! Compiler-owned adapter evidence shared by root and scoped call-graph checks.
//! This is call permission, not a resource transfer or borrowing certificate.
use crate::{core::builtins::Builtin, FunctionSignature};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Contract {
    Parallel,
    Scoped,
}

/// Validate the full reserved signature before granting either permission.
/// Generic host registrations still need separate binding-identity evidence.
pub(super) fn builtin(
    name: &str,
    signature: &FunctionSignature,
    suspends: bool,
) -> Option<Contract> {
    let builtin = Builtin::lookup(name)?;
    builtin.validate(signature, suspends, false).ok()?;
    Some(match builtin {
        // The reserved synchronous TextViewV2 binding keeps the immutable view
        // only until return; Host::from_binding checks its actual entry identity.
        Builtin::Stdout | Builtin::Stderr => Contract::Scoped,
        Builtin::VmCall | Builtin::VmSource | Builtin::VmMigration(_) => Contract::Scoped,
        Builtin::Vm(_) => {
            if !matches!(name, "__vm_create" | "__vm_share" | "__vm_close") {
                return None;
            }
            Contract::Scoped
        }
        Builtin::Domain(op) => {
            crate::runtime::parallel::control::Operation::domain(op)?;
            use crate::runtime::domain::Operation::*;
            match op {
                // These commands carry scalar tokens or proxy identities only;
                // message bytes return as a copy. A control receipt keeps child
                // cleanup pending until the local waiter and reply are dropped.
                // This does not admit resource references as task inputs.
                AdmissionCreate
                | AdmissionHandle
                | AdmissionClose
                | AdmissionTry
                | AdmissionAcquire
                | AdmissionStatus
                | DirectoryCreate
                | DirectoryHandle
                | DirectoryClose
                | OutboxCreate
                | OutboxCreateDisconnect
                | OutboxHandle
                | OutboxClose
                | OutboxReceive
                | OutboxReceiveBytes
                | OutboxMessageClosed
                | OutboxMessageBytes => Contract::Scoped,
                _ => Contract::Parallel,
            }
        }
        // Receivers and scalar output slots belong to the calling task. Pending
        // I/O retains a local resource lease and bounded host bytes; stop drops
        // its reactor registration before releasing that lease. This grants no
        // permission to capture a TCP resource reference in a task packet.
        Builtin::Tcp(_) => Contract::Scoped,
        // File calls copy bytes and retain control-local proxy identities.
        // Native file tickets may outlive cancellation, never language addresses.
        Builtin::Fs(_) => Contract::Scoped,
        // DNS and password jobs own input copies and results; the error slot
        // is written only by the polling task, never by a background worker.
        Builtin::Dns | Builtin::Password => Contract::Scoped,
        // Local resource owners have no cross-worker transfer contract.
        Builtin::Shutdown(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{runtime::domain::Operation, Type};

    #[test]
    fn parallel_adapter_contracts_separate_calls_from_resource_borrows() {
        for (name, op, expected) in [
            (
                "__domain_admission_create",
                Operation::AdmissionCreate,
                Some(Contract::Scoped),
            ),
            (
                "__domain_admission_handle",
                Operation::AdmissionHandle,
                Some(Contract::Scoped),
            ),
            (
                "__domain_admission_close",
                Operation::AdmissionClose,
                Some(Contract::Scoped),
            ),
            (
                "__domain_admission_try",
                Operation::AdmissionTry,
                Some(Contract::Scoped),
            ),
            (
                "__domain_admission_acquire",
                Operation::AdmissionAcquire,
                Some(Contract::Scoped),
            ),
            (
                "__domain_admission_status",
                Operation::AdmissionStatus,
                Some(Contract::Scoped),
            ),
            (
                "__outbox_receive_bytes",
                Operation::OutboxReceiveBytes,
                Some(Contract::Scoped),
            ),
            (
                "__outbox_receive",
                Operation::OutboxReceive,
                Some(Contract::Scoped),
            ),
            (
                "__directory_handle",
                Operation::DirectoryHandle,
                Some(Contract::Scoped),
            ),
            (
                "__outbox_create",
                Operation::OutboxCreate,
                Some(Contract::Scoped),
            ),
            (
                "__outbox_create_disconnect",
                Operation::OutboxCreateDisconnect,
                Some(Contract::Scoped),
            ),
            (
                "__outbox_handle",
                Operation::OutboxHandle,
                Some(Contract::Scoped),
            ),
            (
                "__outbox_close",
                Operation::OutboxClose,
                Some(Contract::Scoped),
            ),
            (
                "__outbox_message_closed",
                Operation::OutboxMessageClosed,
                Some(Contract::Scoped),
            ),
            (
                "__outbox_message_bytes",
                Operation::OutboxMessageBytes,
                Some(Contract::Scoped),
            ),
            (
                "__directory_create",
                Operation::DirectoryCreate,
                Some(Contract::Scoped),
            ),
            (
                "__directory_close",
                Operation::DirectoryClose,
                Some(Contract::Scoped),
            ),
            ("__outbox_stage", Operation::OutboxStage, None),
        ] {
            let signature = op.signature();
            assert_eq!(builtin(name, &signature, true), expected, "{name}");
            assert_eq!(builtin(name, &signature, false), None, "{name}");
            let mut bad = signature.clone();
            bad.params.push(Type::Ptr);
            assert_eq!(builtin(name, &bad, true), None, "{name}");
        }
        let signature = Operation::AdmissionHandle.signature();
        assert_eq!(builtin("user_handle", &signature, true), None);
        for name in [
            "admission.PoolOwner",
            "admission.PermitOwner",
            "domain.DirectoryOwner",
            "outbox.QueueOwner",
            "outbox.MessageOwner",
        ] {
            let reference = Type::reference(Type::Resource(name.into()), false);
            assert!(crate::runtime::parallel::scoped::Shape::of(&reference).is_none());
        }
    }

    #[test]
    fn parallel_tcp_contract_requires_checked_signature_and_binding() {
        use crate::runtime::{
            bindings::{HostAbiKind, HostBinding, HostEntry},
            parallel::Host,
            tcp::Operation,
        };
        let signature = FunctionSignature::new(
            vec![
                Type::reference(Type::Resource("tcp.ListenerOwner".into()), true),
                Type::reference(Type::UInt64, true),
            ],
            Type::Resource("tcp.StreamOwner".into()),
        );
        assert_eq!(
            builtin("__tcp_accept", &signature, true),
            Some(Contract::Scoped)
        );
        assert_eq!(builtin("user_accept", &signature, true), None);
        assert_eq!(builtin("__tcp_accept", &signature, false), None);
        let mut binding = HostBinding {
            abi: HostAbiKind::Scalar,
            signature,
            entry: HostEntry::Tcp(Operation::Accept),
            owner: None,
        };
        assert!(Host::from_binding(&binding).is_some());
        binding.signature.params[0] =
            Type::reference(Type::Resource("tcp.ListenerOwner".into()), false);
        assert_eq!(builtin("__tcp_accept", &binding.signature, true), None);
        assert!(Host::from_binding(&binding).is_none());
        binding.signature.params[0] =
            Type::reference(Type::Resource("tcp.ListenerOwner".into()), true);
        binding.abi = HostAbiKind::TextViewV2;
        assert!(Host::from_binding(&binding).is_none());
        binding.abi = HostAbiKind::Scalar;
        binding.entry = HostEntry::Tcp(Operation::Read);
        assert!(Host::from_binding(&binding).is_none());
        binding.entry = HostEntry::Direct(std::ptr::null());
        assert!(Host::from_binding(&binding).is_none());
        for name in ["ListenerOwner", "StreamOwner", "ReaderOwner", "WriterOwner"] {
            for mutable in [false, true] {
                let reference =
                    Type::reference(Type::Resource(format!("tcp.{name}").into()), mutable);
                assert!(crate::runtime::parallel::scoped::Shape::of(&reference).is_none());
            }
        }
    }

    #[test]
    fn parallel_control_binding_requires_the_checked_adapter() {
        use crate::runtime::{
            bindings::{HostAbiKind, HostBinding, HostEntry},
            parallel::Host,
        };
        for op in [
            Operation::AdmissionHandle,
            Operation::OutboxReceive,
            Operation::OutboxReceiveBytes,
            Operation::OutboxMessageBytes,
            Operation::DirectoryHandle,
        ] {
            let mut binding = HostBinding {
                abi: HostAbiKind::Scalar,
                signature: op.signature(),
                entry: HostEntry::Domain(op),
                owner: None,
            };
            assert!(Host::from_binding(&binding).is_some());
            binding.abi = HostAbiKind::TextViewV2;
            assert!(Host::from_binding(&binding).is_none());
            binding.abi = HostAbiKind::Scalar;
            binding.entry = HostEntry::Domain(Operation::AdmissionStatus);
            assert!(Host::from_binding(&binding).is_none());
            binding.entry = HostEntry::Direct(std::ptr::null());
            assert!(Host::from_binding(&binding).is_none());
        }
    }
}
