//! Low-memory test entry: the real VM/compiler source, without the desktop,
//! HTTP service or model providers. No host execution methods are installed.
#![allow(dead_code,unused_imports)]
#[cfg(test)]
mod agent_program;

// The VM asks the service only for durable action IDs. Preserve its format;
// actual actions are never executed by the conformance/corpus runners.
#[cfg(test)]
mod agent_service {
    pub(crate) fn uuid_like()->String {
        let mut bytes=[0u8;16];
        getrandom::fill(&mut bytes).expect("VM measurement action entropy");
        bytes.iter().map(|byte|format!("{byte:02x}")).collect()
    }
}
fn main() {eprintln!("Use cargo test --bin vm-measurements --features vm-measurements; see tests/vm/README.md");}
