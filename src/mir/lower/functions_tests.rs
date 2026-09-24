use super::*;

fn block(id: usize, statements: Vec<MirStatement>, terminator: MirTerminator) -> MirBlock {
    MirBlock {
        id: MirBlockId(id),
        scoped: false,
        scope_depth: 0,
        statements,
        terminator: Some(terminator),
    }
}

fn goto(target: usize) -> MirTerminator {
    MirTerminator::Goto {
        target: MirBlockId(target),
        arguments: Vec::new(),
    }
}

fn wait(destination: usize) -> MirStatement {
    MirStatement::TaskWait {
        destination: MirValueId(destination),
        scope: MirScopeId(0),
        race: false,
        continuation: MirContinuationId(0),
    }
}

fn continuation(suspend: usize, resume: usize, locals: usize) -> MirContinuation {
    MirContinuation {
        id: MirContinuationId(0),
        kind: MirContinuationKind::TaskWait,
        operation: None,
        callee: None,
        suspend_block: MirBlockId(suspend),
        resume_block: MirBlockId(resume),
        resume_destination: Some(MirValueId(0)),
        generation: 0,
        locals_before_suspend: locals,
        spill_values: Vec::new(),
        spill_slots: Vec::new(),
        frame_slots: Vec::new(),
    }
}

#[test]
fn spill_selection_uses_definition_order_instead_of_value_ids() {
    let blocks = vec![
        block(
            0,
            vec![
                MirStatement::Unit {
                    destination: MirValueId(5),
                },
                wait(0),
            ],
            goto(1),
        ),
        block(1, Vec::new(), MirTerminator::Return(Some(MirValueId(5)))),
    ];
    let mut continuations = vec![continuation(0, 1, 0)];
    finalize_continuation_spills(
        &blocks,
        &[],
        &[MirOwnership::Copy; 6],
        &mut continuations,
        MirBlockId(0),
        &HashSet::new(),
    );
    assert_eq!(continuations[0].spill_values, vec![MirValueId(5)]);

    let mut blocks = blocks;
    blocks[1].terminator = Some(MirTerminator::Return(Some(MirValueId(0))));
    finalize_continuation_spills(
        &blocks,
        &[],
        &[MirOwnership::Copy; 6],
        &mut continuations,
        MirBlockId(0),
        &HashSet::new(),
    );
    assert!(continuations[0].spill_values.is_empty());
}

#[test]
fn frame_omits_entry_locals_consumed_on_both_branches_before_reinitialization() {
    for ownership in [
        MirOwnership::Copy,
        MirOwnership::Shared,
        MirOwnership::Owned,
    ] {
        let local = MirLocalId(0);
        let blocks = vec![
            block(
                0,
                Vec::new(),
                MirTerminator::Branch {
                    condition: MirValueId(0),
                    then_block: MirBlockId(1),
                    else_block: MirBlockId(2),
                },
            ),
            block(
                1,
                vec![MirStatement::TakeLocal {
                    destination: MirValueId(1),
                    local,
                }],
                goto(3),
            ),
            block(
                2,
                vec![MirStatement::TakeLocal {
                    destination: MirValueId(2),
                    local,
                }],
                goto(3),
            ),
            block(3, vec![wait(3)], goto(4)),
            block(
                4,
                vec![MirStatement::Bind {
                    local,
                    value: Some(MirValueId(4)),
                    destination: MirValueId(5),
                }],
                MirTerminator::Return(None),
            ),
        ];
        let locals = vec![MirLocal {
            id: local,
            name: "value".to_owned(),
            ty: Type::I32,
            ownership,
            scope_depth: 0,
        }];
        let mut continuations = vec![continuation(3, 4, 1)];
        finalize_continuation_spills(
            &blocks,
            &locals,
            &[ownership; 6],
            &mut continuations,
            MirBlockId(0),
            &HashSet::from([local]),
        );
        assert!(continuations[0].frame_slots.is_empty(), "{ownership:?}");
    }
}

#[test]
fn frame_records_loop_header_binding_for_non_entry_local() {
    for ownership in [
        MirOwnership::Copy,
        MirOwnership::Shared,
        MirOwnership::Owned,
    ] {
        let local = MirLocalId(0);
        let blocks = vec![
            block(0, Vec::new(), goto(1)),
            block(
                1,
                vec![
                    MirStatement::Phi {
                        destination: MirValueId(8),
                        incoming: vec![
                            (MirBlockId(0), MirValueId(0)),
                            (MirBlockId(2), MirValueId(2)),
                        ],
                    },
                    MirStatement::Bind {
                        local,
                        value: Some(MirValueId(8)),
                        destination: MirValueId(9),
                    },
                    wait(1),
                ],
                goto(2),
            ),
            block(
                2,
                vec![MirStatement::TakeLocal {
                    destination: MirValueId(2),
                    local,
                }],
                goto(1),
            ),
        ];
        let locals = vec![MirLocal {
            id: local,
            name: "cursor".to_owned(),
            ty: Type::I32,
            ownership,
            scope_depth: 0,
        }];
        let mut continuations = vec![continuation(1, 2, 1)];
        finalize_continuation_spills(
            &blocks,
            &locals,
            &[ownership; 10],
            &mut continuations,
            MirBlockId(0),
            &HashSet::new(),
        );
        let slot = &continuations[0].frame_slots[0];
        assert_eq!(slot.local, local);
        assert_eq!(slot.value, Some(MirValueId(8)));
        assert_eq!(slot.ownership, ownership);
    }
}

#[test]
fn unreachable_predecessor_does_not_hide_live_frame_or_spill_values() {
    let local = MirLocalId(0);
    let blocks = vec![
        block(
            0,
            vec![
                MirStatement::Unit {
                    destination: MirValueId(5),
                },
                MirStatement::Bind {
                    local,
                    value: Some(MirValueId(5)),
                    destination: MirValueId(6),
                },
            ],
            goto(1),
        ),
        block(1, vec![wait(0)], goto(2)),
        block(
            2,
            vec![MirStatement::Read {
                destination: MirValueId(1),
                local,
            }],
            MirTerminator::Return(Some(MirValueId(5))),
        ),
        block(
            3,
            vec![MirStatement::TakeLocal {
                destination: MirValueId(2),
                local,
            }],
            goto(1),
        ),
    ];
    let locals = vec![MirLocal {
        id: local,
        name: "value".to_owned(),
        ty: Type::Unit,
        ownership: MirOwnership::Copy,
        scope_depth: 0,
    }];
    let mut continuations = vec![continuation(1, 2, 1)];
    finalize_continuation_spills(
        &blocks,
        &locals,
        &[MirOwnership::Copy; 7],
        &mut continuations,
        MirBlockId(0),
        &HashSet::new(),
    );
    assert_eq!(continuations[0].spill_values, vec![MirValueId(5)]);
    assert_eq!(continuations[0].frame_slots[0].value, Some(MirValueId(5)));
}
