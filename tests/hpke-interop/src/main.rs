// Rust half of the HPKE interop harness.
//
// Ciphersuite: DHKEM(X25519, HKDF-SHA256) + HKDF-SHA256 + AES-128-GCM
// RFC 9180 Base mode.
//
// The info parameter defaults to the production domain label
// (pm-push-hpke-v1) so vectors match what the controller seals and
// the NSE opens. Pass --empty-info to use empty info for raw
// ciphersuite interop testing.
//
// Usage:
//   hpke-interop-test generate [--empty-info]
//   hpke-interop-test open <sk_hex> <enc_hex> <ct_hex> [--empty-info]
//   hpke-interop-test seal <pk_hex> <plaintext_hex> [--empty-info]

use hpke::{
    aead::AesGcm128,
    kdf::HkdfSha256,
    kem::X25519HkdfSha256,
    single_shot_open, single_shot_seal,
    Deserializable, Kem as KemTrait, OpModeR, OpModeS, Serializable,
};

const HPKE_DOMAIN: &[u8] = b"pm-push-hpke-v1";

fn info_for(args: &[String]) -> &[u8] {
    if args.iter().any(|a| a == "--empty-info") {
        b""
    } else {
        HPKE_DOMAIN
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: hpke-interop-test <generate|open|seal> [--empty-info]");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "generate" => generate(info_for(&args)),
        "open" => {
            if args.len() < 5 {
                eprintln!("Usage: open <sk_hex> <enc_hex> <ct_hex> [--empty-info]");
                std::process::exit(1);
            }
            open(&args[2], &args[3], &args[4], info_for(&args));
        }
        "seal" => {
            if args.len() < 4 {
                eprintln!("Usage: seal <pk_hex> <plaintext_hex> [--empty-info]");
                std::process::exit(1);
            }
            seal(&args[2], &args[3], info_for(&args));
        }
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            std::process::exit(1);
        }
    }
}

fn generate(info: &[u8]) {
    let mut rng = rand::thread_rng();
    let (sk, pk) = X25519HkdfSha256::gen_keypair(&mut rng);

    let plaintext = b"hello from Rust HPKE";

    let (enc, ciphertext) =
        single_shot_seal::<AesGcm128, HkdfSha256, X25519HkdfSha256, _>(
            &OpModeS::Base,
            &pk,
            info,
            plaintext,
            b"",
            &mut rng,
        )
        .expect("seal failed");

    println!("SK={}", hex::encode(sk.to_bytes()));
    println!("PK={}", hex::encode(pk.to_bytes()));
    println!("ENC={}", hex::encode(enc.to_bytes()));
    println!("CT={}", hex::encode(&ciphertext));
    println!("PLAINTEXT={}", hex::encode(plaintext));
}

fn open(sk_hex: &str, enc_hex: &str, ct_hex: &str, info: &[u8]) {
    let sk_bytes = hex::decode(sk_hex).expect("invalid sk hex");
    let enc_bytes = hex::decode(enc_hex).expect("invalid enc hex");
    let ct_bytes = hex::decode(ct_hex).expect("invalid ct hex");

    let sk = <X25519HkdfSha256 as KemTrait>::PrivateKey::from_bytes(&sk_bytes).expect("invalid sk");
    let enc =
        <X25519HkdfSha256 as KemTrait>::EncappedKey::from_bytes(&enc_bytes).expect("invalid enc");

    match single_shot_open::<AesGcm128, HkdfSha256, X25519HkdfSha256>(
        &OpModeR::Base,
        &sk,
        &enc,
        info,
        &ct_bytes,
        b"",
    ) {
        Ok(plaintext) => {
            println!("OK={}", hex::encode(&plaintext));
            println!(
                "PLAINTEXT_UTF8={}",
                String::from_utf8(plaintext).unwrap_or_else(|_| "<non-utf8>".into())
            );
        }
        Err(e) => {
            println!("FAIL={:?}", e);
            std::process::exit(2);
        }
    }
}

fn seal(pk_hex: &str, pt_hex: &str, info: &[u8]) {
    let pk_bytes = hex::decode(pk_hex).expect("invalid pk hex");
    let pt_bytes = hex::decode(pt_hex).expect("invalid pt hex");

    let pk =
        <X25519HkdfSha256 as KemTrait>::PublicKey::from_bytes(&pk_bytes).expect("invalid pk");

    let mut rng = rand::thread_rng();

    let (enc, ciphertext) =
        single_shot_seal::<AesGcm128, HkdfSha256, X25519HkdfSha256, _>(
            &OpModeS::Base,
            &pk,
            info,
            &pt_bytes,
            b"",
            &mut rng,
        )
        .expect("seal failed");

    println!("ENC={}", hex::encode(enc.to_bytes()));
    println!("CT={}", hex::encode(&ciphertext));
}
