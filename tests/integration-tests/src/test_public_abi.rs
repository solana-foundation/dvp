//! Structural compatibility with the public IDL at df9919e.
use serde_json::{json, Value};

// Documentation and error wording are not part of the wire contract.
fn without_docs(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.remove("docs");
            fields.remove("message");
            fields.values_mut().for_each(without_docs);
        }
        Value::Array(items) => items.iter_mut().for_each(without_docs),
        _ => {}
    }
}

#[test]
fn public_abi_remains_compatible() {
    let baseline: Value =
        serde_json::from_str(include_str!("../../fixtures/public-abi.json")).unwrap();
    let idl: Value =
        serde_json::from_str(include_str!("../../../idl/dvp_swap_program.json")).unwrap();
    let program = &idl["program"];
    let mut actual = json!({
        "publicKey": program["publicKey"],
        "accounts": program["accounts"],
        "pdas": program["pdas"],
        "instructions": &program["instructions"].as_array().unwrap()[..6],
        "errors": &program["errors"].as_array().unwrap()[..24],
    });
    without_docs(&mut actual);
    assert_eq!(actual, baseline);
}
