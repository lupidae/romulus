//! `ROMULUS_DUMP=file.sql cargo test -p romulus-wasm --test dump -- --ignored --nocapture`:
//! the sandbox's model of a SQL file, and its parse tree, to compare with the engine's.

mod common;

#[test]
#[ignore = "a tool, not a test"]
#[allow(clippy::print_stdout)]
fn dump() {
    let path = std::env::var("ROMULUS_DUMP").expect("ROMULUS_DUMP=<file.sql>");
    let sql = std::fs::read_to_string(path).expect("the file");
    let text = sql.clone();
    common::with_sandbox(|compiler| async move {
        let model = common::sandbox_model(compiler, &text).await;
        println!("{}", serde_json::to_string_pretty(&model).unwrap());
    });
    if std::env::var_os("ROMULUS_DUMP_TREE").is_some() {
        println!("{}", common::tree(&sql).unwrap());
    }
}

#[test]
#[ignore = "a tool, not a test"]
#[allow(clippy::print_stdout)]
fn tree() {
    let path = std::env::var("ROMULUS_DUMP").expect("ROMULUS_DUMP=<file.sql>");
    let sql = std::fs::read_to_string(path).expect("the file");
    println!("{}", common::tree(&sql).unwrap());
}

#[test]
#[ignore = "a tool, not a test"]
#[allow(clippy::print_stdout)]
fn engine() {
    let path = std::env::var("ROMULUS_DUMP").expect("ROMULUS_DUMP=<file.sql>");
    let sql = std::fs::read_to_string(path).expect("the file");
    let compiled = common::compile(&sql);
    println!("{}", serde_json::to_string_pretty(&compiled).unwrap());
}
