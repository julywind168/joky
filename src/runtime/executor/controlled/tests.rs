use super::*;

#[test]
fn controlled_inputs_validate_before_mutation_and_reject_reentrancy() {
    let control = ControlledExecutor::new(7, 2, 4).unwrap();
    control
        .inject(10, || {
            assert!(control.executor().poll(1).is_err());
            assert!(control.advance(Duration::from_secs(1)).is_err());
            assert!(control.inject(11, || panic!("nested event")).is_err());
        })
        .unwrap();
    control.advance(Duration::from_millis(3)).unwrap();
    assert!(control.advance(Duration::MAX).is_err());
    control.executor().poll(0).unwrap();
    let trace = control.trace();
    assert_eq!(trace.steps.len(), 3);
    assert_eq!(control.elapsed(), Duration::from_millis(3));
    let replay = ControlledExecutor::replay(trace.clone(), 2).unwrap();
    assert!(replay.finish().is_err());
    assert!(replay
        .inject(11, || panic!("mismatched input ran"))
        .is_err());
    assert!(replay.executor().poll(0).is_err());
    assert_eq!(replay.elapsed(), Duration::ZERO);
    let replay = ControlledExecutor::replay(trace, 2).unwrap();
    replay.inject(10, || ()).unwrap();
    assert!(replay.advance(Duration::from_millis(4)).is_err());
    assert_eq!(replay.elapsed(), Duration::ZERO);
    assert!(ControlledExecutor::new(0, 0, 1).is_err());
    let mut bad = control.trace();
    bad.version += 1;
    assert!(ControlledExecutor::replay(bad, 2).is_err());
}

#[test]
fn controlled_invalid_batches_fail_without_consuming_ready_tasks() {
    for (ready, selected, limit) in [
        (vec![1, 2], vec![1, 1], 2),
        (vec![1, 2], vec![3], 1),
        (vec![1, 2], vec![], 1),
        (vec![1, 3], vec![1], 1),
    ] {
        let mut schedule = Schedule {
            trace: Trace {
                version: VERSION,
                seed: 8,
                steps: vec![Step::Poll {
                    max_polls: limit,
                    ready,
                    selected,
                }],
            },
            mode: Mode::Replay { cursor: 0 },
            next_task: 3,
            failure: None,
            in_event: false,
        };
        assert!(schedule
            .select(limit, vec![(TaskId(10), 1), (TaskId(20), 2)])
            .is_err());
        assert!(matches!(schedule.mode, Mode::Replay { cursor: 0 }));
        assert!(schedule.next_task().is_err());
    }
}
