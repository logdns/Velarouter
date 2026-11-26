//! Session cryptography for Velarouter.
//!
//! ## Why P-256 + AES-GCM
//!
//! Every target runtime can do these two primitives *without* shipping a crypto
//! library of our own:
//!
//! | Runtime        | ECDH P-256                    | AES-256-GCM              |
//! |----------------|-------------------------------|--------------------------|
//! | Web (browser)  | `crypto.subtle`               | `crypto.subtle`          |
//! | iOS            | CryptoKit `P256.KeyAgreement` | CryptoKit `AES.GCM`      |
//! | Android        | JCE / Conscrypt               | JCE / Conscrypt          |
//! | Windows        | CNG `BCrypt`                  | CNG `BCrypt`             |
//! | Linux          | OpenSSL                       | OpenSSL                  |
//!
//! That makes a browser viewer a first-class peer instead of a second-class one
//! forced through a TLS terminator.
//!
//! ## Handshake
//!
//! ```text
//! viewer                                     agent
//!   |  Hello(role=viewer, session_id, ver)  -> |
//!   |  AuthInit(viewer_pub, nonce)           -> |
//!   |                                           |  key = HKDF(ECDH(priv, pub), salt(session_id||code))
//!   |  <-  AuthFinish(agent_pub, proof_agent)    |
//!   |  key = HKDF(ECDH(...))                    |
//!   |  verify proof_agent                       |
//!   |  AuthProof(proof_viewer)               -> |
//!   |  <-  AuthOk                               |  every later frame is sealed
//! ```
//!
//! The pairing code is mixed into the HKDF salt. A passive attacker who records
//! the handshake cannot derive the key; an active MITM cannot produce proofs
//! without guessing the code. For unattended devices the code can be pinned
//! long-term, which makes the same derivation a PSK handshake.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use p256::ecdh::diffie_hellman;
use p256::elliptic_curve::{sec1::ToSec1Point, Generate};
use p256::{PublicKey, SecretKey};
use rand::{CryptoRng, Rng};
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;
/// Maximum plaintext accepted by [`Sealer::open`], guards memory use.
pub const MAX_SEALED: usize = 16 * 1024 * 1024;

const HKDF_INFO: &[u8] = b"velarouter/vrp1/session-key";
const AAD: &[u8] = b"vrp1";

/// Direction byte mixed into every nonce. Prevents reflecting a peer's own
/// ciphertext back at it (the two directions never share a keystream).
pub const DIR_VIEWER_TO_AGENT: u8 = 1;
pub const DIR_AGENT_TO_VIEWER: u8 = 2;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("invalid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("invalid public key")]
    PublicKey,
    #[error("invalid private key")]
    PrivateKey,
    #[error("proof mismatch")]
    Proof,
    #[error("message too large: {0} bytes")]
    TooLarge(usize),
    #[error("message too short: {0} bytes")]
    TooShort(usize),
    #[error("decryption failed")]
    Decrypt,
    #[error("nonce space exhausted")]
    NonceExhausted,
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(s: &str) -> Result<Vec<u8>, CryptoError> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(CryptoError::Base64)
}

/// An ephemeral P-256 key pair, one per connection.
pub struct EphemeralKey {
    secret: SecretKey,
}

impl EphemeralKey {
    pub fn generate() -> Self {
        Self::generate_with(&mut rand::rand_core::UnwrapErr(rand::rngs::SysRng))
    }

    pub fn generate_with<R: CryptoRng>(rng: &mut R) -> Self {
        Self {
            secret: SecretKey::generate_from_rng(rng),
        }
    }

    /// Builds a key from 32 raw bytes. Used by known-answer tests and by
    /// deployments that keep a long-term ECDH key in a KMS/TPM.
    pub fn from_secret_bytes(bytes: &[u8; 32]) -> Result<Self, CryptoError> {
        Ok(Self {
            secret: SecretKey::from_slice(bytes).map_err(|_| CryptoError::PrivateKey)?,
        })
    }

    /// Uncompressed SEC1 public key, base64 — what [`crate::b64`] peers expect.
    pub fn public_b64(&self) -> String {
        b64(self.secret.public_key().to_sec1_point(false).as_bytes())
    }

    pub fn shared_secret(&self, peer_pub_b64: &str) -> Result<[u8; 32], CryptoError> {
        let bytes = unb64(peer_pub_b64)?;
        let peer = PublicKey::from_sec1_bytes(&bytes).map_err(|_| CryptoError::PublicKey)?;
        let shared = diffie_hellman(self.secret.to_nonzero_scalar(), peer.as_affine());
        Ok(*shared.raw_secret_bytes().as_ref())
    }
}

/// Derives the 32-byte session key from the ECDH secret, the session id and the
/// pairing code. Both peers must pass the same three inputs.
pub fn derive_session_key(
    shared: &[u8; 32],
    session_id: &str,
    pairing_code: &str,
) -> Result<[u8; KEY_LEN], CryptoError> {
    let mut salt = Vec::with_capacity(session_id.len() + pairing_code.len() + 1);
    salt.extend_from_slice(session_id.as_bytes());
    salt.push(b'|');
    salt.extend_from_slice(pairing_code.as_bytes());
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut okm = [0u8; KEY_LEN];
    hk.expand(HKDF_INFO, &mut okm)
        .map_err(|_| CryptoError::PrivateKey)?;
    Ok(okm)
}

/// `HMAC-SHA256(key, label || "|" || session_id)`, base64. Proves possession of
/// the session key without revealing it.
pub fn proof(key: &[u8; KEY_LEN], label: &str, session_id: &str) -> String {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(label.as_bytes());
    mac.update(b"|");
    mac.update(session_id.as_bytes());
    b64(&mac.finalize().into_bytes())
}

pub fn verify_proof(key: &[u8; KEY_LEN], label: &str, session_id: &str, got: &str) -> bool {
    let expected = proof(key, label, session_id);
    expected.as_bytes().ct_eq(got.as_bytes()).into()
}

/// Authenticated encryption for one direction of a session.
///
/// The counter is implicit: both sides start at zero and increment for every
/// message. Because VRP/1 control and media travel on ordered channels, a
/// missing message is a protocol error, not something to resynchronise silently.
pub struct Sealer {
    cipher: Aes256Gcm,
    direction: u8,
    counter: u64,
}

impl Sealer {
    pub fn new(key: &[u8; KEY_LEN], direction: u8) -> Self {
        Self {
            cipher: Aes256Gcm::new_from_slice(key).expect("32-byte key"),
            direction,
            counter: 0,
        }
    }

    fn nonce(&self, counter: u64) -> [u8; NONCE_LEN] {
        let mut n = [0u8; NONCE_LEN];
        n[0] = self.direction;
        n[4..].copy_from_slice(&counter.to_be_bytes());
        n
    }

    /// Returns `nonce-independent` ciphertext: `counter(8) || ct || tag`.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if plaintext.len() > MAX_SEALED {
            return Err(CryptoError::TooLarge(plaintext.len()));
        }
        let counter = self.counter;
        let nonce = self.nonce(counter);
        let ct = self
            .cipher
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: plaintext,
                    aad: AAD,
                },
            )
            .map_err(|_| CryptoError::Decrypt)?;
        self.counter = counter.checked_add(1).ok_or(CryptoError::NonceExhausted)?;
        let mut out = Vec::with_capacity(8 + ct.len());
        out.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&ct);
        Ok(out)
    }

    pub fn open(&mut self, sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if sealed.len() < 8 + TAG_LEN {
            return Err(CryptoError::TooShort(sealed.len()));
        }
        if sealed.len() > MAX_SEALED {
            return Err(CryptoError::TooLarge(sealed.len()));
        }
        let counter = u64::from_be_bytes(sealed[..8].try_into().unwrap());
        if counter != self.counter {
            return Err(CryptoError::Decrypt);
        }
        let nonce = self.nonce(counter);
        let pt = self
            .cipher
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &sealed[8..],
                    aad: AAD,
                },
            )
            .map_err(|_| CryptoError::Decrypt)?;
        self.counter = counter.checked_add(1).ok_or(CryptoError::NonceExhausted)?;
        Ok(pt)
    }

    pub fn counter(&self) -> u64 {
        self.counter
    }
}

/// Derives a 32-byte key from a short, human-typed secret (a connection code).
/// Used by the rendezvous channel, never for the session itself.
pub fn derive_short_secret(secret: &str, label: &str) -> [u8; KEY_LEN] {
    let hk = Hkdf::<Sha256>::new(Some(label.as_bytes()), secret.as_bytes());
    let mut okm = [0u8; KEY_LEN];
    hk.expand(b"velarouter/short-secret/v1", &mut okm)
        .expect("32 bytes is a valid HKDF length");
    okm
}

/// Seals a message with a fresh random nonce. Unlike [`Sealer`] this keeps no
/// counter, so messages may be lost, reordered or replayed by the transport —
/// it is meant for rendezvous traffic that travels over a shared broker.
pub fn seal_random(
    key: &[u8; KEY_LEN],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    if plaintext.len() > MAX_SEALED {
        return Err(CryptoError::TooLarge(plaintext.len()));
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::PrivateKey)?;
    let mut nonce = [0u8; NONCE_LEN];
    rand::rand_core::UnwrapErr(rand::rngs::SysRng).fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Decrypt)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn open_random(key: &[u8; KEY_LEN], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return Err(CryptoError::TooShort(sealed.len()));
    }
    if sealed.len() > MAX_SEALED {
        return Err(CryptoError::TooLarge(sealed.len()));
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::PrivateKey)?;
    let nonce: [u8; NONCE_LEN] = sealed[..NONCE_LEN]
        .try_into()
        .expect("slice length checked");
    cipher
        .decrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &sealed[NONCE_LEN..],
                aad,
            },
        )
        .map_err(|_| CryptoError::Decrypt)
}

/// Generates a 6-digit pairing code. Uniformly distributed, and rejects the
/// codes people fat-finger or that look like "no code" (`000000`).
pub fn generate_pairing_code() -> String {
    use rand::RngExt;
    let mut rng = rand::rand_core::UnwrapErr(rand::rngs::SysRng);
    loop {
        let n: u32 = rng.random_range(0..1_000_000);
        if n >= 100_000 {
            return format!("{n:06}");
        }
    }
}

/// A long-lived pairing secret used for unattended access. 32 bytes, base64.
pub fn generate_pinned_secret() -> String {
    let mut buf = [0u8; 32];
    rand::rand_core::UnwrapErr(rand::rngs::SysRng).fill_bytes(&mut buf);
    b64(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (EphemeralKey, EphemeralKey, String, String) {
        let viewer = EphemeralKey::generate();
        let agent = EphemeralKey::generate();
        (viewer, agent, "sess-1".into(), "123456".into())
    }

    #[test]
    fn both_sides_derive_the_same_key() {
        let (viewer, agent, sid, code) = pair();
        let v_shared = viewer.shared_secret(&agent.public_b64()).unwrap();
        let a_shared = agent.shared_secret(&viewer.public_b64()).unwrap();
        assert_eq!(v_shared, a_shared);
        let vk = derive_session_key(&v_shared, &sid, &code).unwrap();
        let ak = derive_session_key(&a_shared, &sid, &code).unwrap();
        assert_eq!(vk, ak);
    }

    #[test]
    fn wrong_pairing_code_yields_different_key() {
        let (viewer, agent, sid, _) = pair();
        let shared = viewer.shared_secret(&agent.public_b64()).unwrap();
        let a = derive_session_key(&shared, &sid, "123456").unwrap();
        let b = derive_session_key(&shared, &sid, "123457").unwrap();
        assert_ne!(a, b);
        assert!(!verify_proof(
            &a,
            "viewer",
            &sid,
            &proof(&b, "viewer", &sid)
        ));
    }

    #[test]
    fn seal_open_roundtrip_and_tamper_detection() {
        let key = [7u8; KEY_LEN];
        let mut v2a = Sealer::new(&key, DIR_VIEWER_TO_AGENT);
        let mut a2v = Sealer::new(&key, DIR_VIEWER_TO_AGENT);
        let mut sender = v2a.seal(b"hello").unwrap();
        assert_eq!(a2v.open(&sender).unwrap(), b"hello");
        assert_eq!(v2a.counter(), 1);

        sender[10] ^= 0xFF;
        let mut fresh = Sealer::new(&key, DIR_VIEWER_TO_AGENT);
        assert!(fresh.open(&sender).is_err());
    }

    #[test]
    fn replayed_ciphertext_is_rejected() {
        let key = [9u8; KEY_LEN];
        let mut sender = Sealer::new(&key, DIR_VIEWER_TO_AGENT);
        let msg = sender.seal(b"one").unwrap();
        let mut receiver = Sealer::new(&key, DIR_VIEWER_TO_AGENT);
        assert_eq!(receiver.open(&msg).unwrap(), b"one");
        assert!(receiver.open(&msg).is_err(), "replay must fail");
    }

    #[test]
    fn directions_do_not_collide() {
        let key = [1u8; KEY_LEN];
        let mut v2a = Sealer::new(&key, DIR_VIEWER_TO_AGENT);
        let a2v = Sealer::new(&key, DIR_AGENT_TO_VIEWER);
        let ct1 = v2a.seal(b"x").unwrap();
        let mut probe = Sealer::new(&key, DIR_AGENT_TO_VIEWER);
        assert!(a2v.counter() == 0);
        assert!(probe.open(&ct1).is_err());
    }

    #[test]
    fn pairing_codes_are_six_digits_and_not_trivial() {
        for _ in 0..200 {
            let c = generate_pairing_code();
            assert_eq!(c.len(), 6);
            assert!(c.parse::<u32>().unwrap() >= 100_000);
        }
    }
}
