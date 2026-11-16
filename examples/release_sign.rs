//! Signs a release file with the private key in `FEATHERPLAY_SIGNING_KEY` (hex, as made
//! by `release_keygen`), writing `<file>.sig` (the Ed25519 signature, as hex). The release
//! workflow signs `SHA256SUMS.txt` this way; the app checks it before trusting an update.
//! With `--public <hex>`, it first makes sure the key is the one the app trusts, so a
//! wrong key fails the release instead of shipping an update nobody would accept.

use ring::signature::{Ed25519KeyPair, KeyPair};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (path, expected) = match args.as_slice() {
        [path] => (path.clone(), None),
        [path, flag, public] if flag == "--public" => (path.clone(), Some(public.to_lowercase())),
        _ => panic!("usage: release_sign <file> [--public <hex>]"),
    };
    let key = std::env::var("FEATHERPLAY_SIGNING_KEY").expect("FEATHERPLAY_SIGNING_KEY is not set");
    let pkcs8 = unhex(key.trim()).expect("the signing key isn't hex");
    let pair = Ed25519KeyPair::from_pkcs8(&pkcs8).expect("the signing key isn't an Ed25519 key");
    let public: String = pair.public_key().as_ref().iter().map(|b| format!("{b:02x}")).collect();
    if let Some(expected) = expected {
        assert_eq!(public, expected, "the signing key isn't the one the app trusts (RELEASE_KEY in src/update.rs)");
    }
    let message = std::fs::read(&path).expect("read the file to sign");
    let signature: String = pair.sign(&message).as_ref().iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(format!("{path}.sig"), signature).expect("write the signature");
    println!("signed {path}");
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}
