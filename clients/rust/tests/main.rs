// Reuse the framework's LiteSVM setup and pinned program fixtures.
// The shared helper module also serves program scenarios outside this suite.
#[allow(dead_code)]
#[path = "../../../tests/integration-tests/src/confidential_utils.rs"]
mod confidential_utils;
#[path = "../../../tests/integration-tests/src/state_utils.rs"]
pub mod state_utils;
mod test_client_confidential;
mod test_client_verify;
pub mod utils;

// Compile and exercise integrator recipes against real program snapshots.
#[allow(dead_code)]
#[path = "../examples/confidential/mod.rs"]
mod confidential_examples;
