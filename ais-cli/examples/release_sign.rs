//! Release signing helper (not shipped in the binary).
//!
//!   cargo run --example release_sign -- keygen <private-key-file>   # prints the public key
//!   cargo run --example release_sign -- sign <private-key-file> <manifest>  # writes <manifest>.sig
//!   cargo run --example release_sign -- verify <public-key-file> <manifest>
//!
//! The private key file holds 32 random bytes, base64. Keep it out of the repo;
//! CI reads it from the AIS_SIGNING_KEY secret instead (pass "-" as the path).

use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::io::Read;

fn engine() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn load(path: &str) -> SigningKey {
    let text = if path == "-" {
        std::env::var("AIS_SIGNING_KEY").expect("AIS_SIGNING_KEY")
    } else {
        std::fs::read_to_string(path).expect("read key")
    };
    let bytes: [u8; 32] = engine().decode(text.trim()).expect("base64 key").try_into().expect("32-byte key");
    SigningKey::from_bytes(&bytes)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("keygen") => {
            let mut seed = [0u8; 32];
            std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut seed)).expect("/dev/urandom");
            let key = SigningKey::from_bytes(&seed);
            let path = &args[1];
            if let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, engine().encode(seed) + "\n").expect("write key");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            println!("{}", engine().encode(key.verifying_key().to_bytes()));
        }
        Some("pubkey") => println!("{}", engine().encode(load(&args[1]).verifying_key().to_bytes())),
        Some("sign") => {
            let key = load(&args[1]);
            let body = std::fs::read(&args[2]).expect("read manifest");
            let signature = engine().encode(key.sign(&body).to_bytes());
            std::fs::write(format!("{}.sig", args[2]), signature + "\n").expect("write signature");
        }
        Some("verify") => {
            let public: [u8; 32] = engine()
                .decode(std::fs::read_to_string(&args[1]).expect("read public key").trim())
                .expect("base64")
                .try_into()
                .expect("32 bytes");
            let body = std::fs::read(&args[2]).expect("read manifest");
            let signature = std::fs::read_to_string(format!("{}.sig", args[2])).expect("read signature");
            let signature: [u8; 64] = engine().decode(signature.trim()).expect("base64").try_into().expect("64 bytes");
            VerifyingKey::from_bytes(&public)
                .expect("public key")
                .verify(&body, &Signature::from_bytes(&signature))
                .expect("signature does not match");
            println!("signature OK");
        }
        _ => {
            eprintln!("usage: release_sign keygen|pubkey|sign|verify ...");
            std::process::exit(2);
        }
    }
}
