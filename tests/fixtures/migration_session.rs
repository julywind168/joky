pub fn source() -> String {
    let mut source = include_str!("migration_session.jk").to_owned();
    for (marker, text) in [
        ("OLD", include_str!("migration_old.jk")),
        ("NEW", include_str!("migration_new.jk")),
        (
            "INCREMENTAL_OLD",
            include_str!("migration_incremental_old.jk"),
        ),
        (
            "INCREMENTAL_NEW",
            include_str!("migration_incremental_new.jk"),
        ),
        ("BATCH_OLD", include_str!("migration_batch_old.jk")),
        ("BATCH_NEW", include_str!("migration_batch_new.jk")),
    ] {
        source = source.replace(
            &format!("@{marker}@"),
            &format!("{:?}", text.replace('{', "{{").replace('}', "}}")),
        );
    }
    source
}
