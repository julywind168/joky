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
                // These commands carry scalar tokens or proxy identities only.
                // Referenced owners stay local to the calling task; this does
                // not admit &Pool, &Permit or mutable resource task inputs.
                AdmissionCreate | AdmissionHandle | AdmissionClose | AdmissionTry
                | AdmissionAcquire | AdmissionStatus => Contract::Scoped,
                _ => Contract::Parallel,
            }
        }
        Builtin::Tcp(_) | Builtin::Dns | Builtin::Fs(_) => Contract::Parallel,
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
                "__outbox_receive",
                Operation::OutboxReceive,
                Some(Contract::Parallel),
            ),
            (
                "__directory_handle",
                Operation::DirectoryHandle,
                Some(Contract::Parallel),
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
        for name in ["admission.PoolOwner", "admission.PermitOwner"] {
            let reference = Type::reference(Type::Resource(name.into()), false);
            assert!(crate::runtime::parallel::scoped::Shape::of(&reference).is_none());
        }
    }

    #[test]
    fn parallel_admission_binding_requires_the_checked_adapter() {
        use crate::runtime::{
            bindings::{HostAbiKind, HostBinding, HostEntry},
            parallel::Host,
        };
        let mut binding = HostBinding {
            abi: HostAbiKind::Scalar,
            signature: Operation::AdmissionHandle.signature(),
            entry: HostEntry::Domain(Operation::AdmissionHandle),
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
