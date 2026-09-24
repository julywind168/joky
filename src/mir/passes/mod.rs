//! MIR optimization passes and pass orchestration.
//!
//! Passes mutate verified MIR in place and keep value IDs stable. The manager
//! verifies the program before the pipeline and after every pass so a faulty
//! optimization cannot silently reach code generation.

mod const_fold;

use crate::diagnostic::Diagnostic;

use super::MirProgram;

pub(crate) use const_fold::ConstantFolding;

/// Summary of one pass invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PassStats {
    pub(crate) name: &'static str,
    pub(crate) changes: usize,
}

/// Summary of a complete optimization pipeline.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PassReport {
    pub(crate) passes: Vec<PassStats>,
}

/// A transformation over a verified MIR program.
pub(crate) trait MirPass {
    fn name(&self) -> &'static str;

    fn run(&mut self, program: &mut MirProgram) -> Result<usize, Diagnostic>;
}

/// Ordered MIR pass pipeline.
pub(crate) struct MirPassManager {
    passes: Vec<Box<dyn MirPass>>,
}

impl MirPassManager {
    pub(crate) fn new() -> Self {
        Self { passes: Vec::new() }
    }

    pub(crate) fn default_pipeline() -> Self {
        let mut manager = Self::new();
        manager.add(ConstantFolding);
        manager
    }

    pub(crate) fn add<P>(&mut self, pass: P)
    where
        P: MirPass + 'static,
    {
        self.passes.push(Box::new(pass));
    }

    pub(crate) fn run(&mut self, program: &mut MirProgram) -> Result<PassReport, Diagnostic> {
        program.verify()?;
        let mut report = PassReport::default();
        for pass in &mut self.passes {
            let changes = pass.run(program)?;
            program.verify()?;
            report.passes.push(PassStats {
                name: pass.name(),
                changes,
            });
        }
        Ok(report)
    }
}
