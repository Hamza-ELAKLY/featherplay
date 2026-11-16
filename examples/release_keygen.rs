//! Makes the key pair that signs releases (Ed25519). The private key is written, as
//! hex, to the file given, to be stored as the repository secret
//! `FEATHERPLAY_SIGNING_KEY`; the public key, printed, goes into `src/update.rs`.
//! Run once; keep a copy of the private key somewhere safe (a password manager).

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

fn main() {
    let path = std::env::args().nth(1).expect("usage: release_keygen <private-key-file>");
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("key generation");
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    std::fs::write(&path, hex(pkcs8.as_ref())).expect("write the private key");
    println!("private key written to {path}");
    println!("public key: {}", hex(pair.public_key().as_ref()));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
