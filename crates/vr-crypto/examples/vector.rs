//! Prints a deterministic known-answer vector for the VRP/1 handshake.
//!
//! `cargo run -p vr-crypto --example vector`
//!
//! SDK authors can use these constants in their test suites: if a port produces
//! a different key or proof for the same inputs, it will never interoperate.

use vr_crypto::{derive_session_key, proof, EphemeralKey};

fn hex_to_32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let viewer_priv = hex_to_32("0000000000000000000000000000000000000000000000000000000000000001");
    let agent_priv = hex_to_32("0000000000000000000000000000000000000000000000000000000000000002");
    let viewer = EphemeralKey::from_secret_bytes(&viewer_priv).unwrap();
    let agent = EphemeralKey::from_secret_bytes(&agent_priv).unwrap();

    let session_id = "s1";
    let code = "123456";
    let v_shared = viewer.shared_secret(&agent.public_b64()).unwrap();
    let a_shared = agent.shared_secret(&viewer.public_b64()).unwrap();
    assert_eq!(v_shared, a_shared);
    let key = derive_session_key(&v_shared, session_id, code).unwrap();

    println!("session_id      = {session_id}");
    println!("pairing_code    = {code}");
    println!("viewer_pub_b64  = {}", viewer.public_b64());
    println!("agent_pub_b64   = {}", agent.public_b64());
    println!("shared_secret   = {}", hex(&v_shared));
    println!("session_key     = {}", hex(&key));
    println!("proof_agent     = {}", proof(&key, "agent", session_id));
    println!("proof_viewer    = {}", proof(&key, "viewer", session_id));
}
