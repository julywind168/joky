//! One standalone Joky program shared by CLI and resource-lifecycle acceptance.
pub fn source(create: &str) -> String {
    let old = include_str!("../fixtures/server_reload_script.jk");
    let patch2 = old.replace("fn version()->Int32 {1}", "fn version()->Int32 {2}");
    let patch3 = old
        .replace("fn version()->Int32 {1}", "fn version()->Int32 {3}")
        .replace("when \"award\"", "when \"award-v2\"");
    let migrated = old
        .replace("fn version()->Int32 {1}", "fn version()->Int32 {4}")
        .replace(
            "struct Room { score:Int32 }",
            "struct Room { score:Int64, copied:Int32 }",
        )
        .replace(
            "struct Ledger { total:Int32 }",
            "struct Ledger { total:Int64, generation:Int32 }",
        )
        .replace("Room{score:score}", "Room{score:int64(score),copied:4}")
        .replace(
            "Ledger{total:total}",
            "Ledger{total:int64(total),generation:1}",
        )
        .replace("a.score+=increment", "a.score+=int64(increment)")
        .replace("b.total+=increment*2", "b.total+=int64(increment)*2")
        .replace(
            "if b.total==a.score*2 {a.score}",
            "if b.total==a.score*2 && a.copied==4 && b.generation==1 {int32(a.score)}",
        );
    let migrated = format!(
        "{migrated}\n{}",
        r#"
fn start(old:&PreviousRoom)->Room {Room{score:int64(old.score),copied:0}}
fn advance(next:&mut Room,old:&PreviousRoom,budget:UInt64)->Bool {
    var n:UInt64=0
    while n<budget && next.copied<4 {next.copied=next.copied+1; n=n+1}
    next.copied==4
}
fn upgrade_ledger(old:&PreviousLedger)->Ledger {Ledger{total:int64(old.total),generation:1}}
"#
    );
    let quote = |s: &str| format!("{:?}", s.replace('{', "{{").replace('}', "}}"));
    include_str!("../fixtures/server_reload_host.jk")
        .replace("__CREATE__", create)
        .replace("__OLD__", &quote(old))
        .replace("__PATCH2__", &quote(&patch2))
        .replace("__PATCH3__", &quote(&patch3))
        .replace("__MIGRATED__", &quote(&migrated))
}
