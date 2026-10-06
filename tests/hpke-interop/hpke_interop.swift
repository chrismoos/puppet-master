// CryptoKit HPKE interop test for #226.
//
// Ciphersuite: DHKEM(X25519, HKDF-SHA256) + HKDF-SHA256 + AES-128-GCM
// RFC 9180 Base mode.
//
// This is a CLI tool used by run.sh to test cross-language HPKE
// interop between CryptoKit and the Rust `hpke` crate. It also
// doubles as the standalone Swift-side verification of the NSE
// decryption path, including negative cases.
//
// Usage:
//   swift hpke_interop.swift generate [--info <hex>]
//   swift hpke_interop.swift open <sk_hex> <enc_hex> <ct_hex> [--info <hex>]
//   swift hpke_interop.swift seal <pk_hex> <plaintext_hex> [--info <hex>]
//   swift hpke_interop.swift selftest

import Foundation
import CryptoKit

// ── Helpers ────────────────────────────────────────────────────────

func hexEncode(_ data: Data) -> String {
    data.map { String(format: "%02x", $0) }.joined()
}

func hexDecode(_ hex: String) -> Data {
    var data = Data()
    var i = hex.startIndex
    while i < hex.endIndex {
        let j = hex.index(i, offsetBy: 2)
        let byte = UInt8(hex[i..<j], radix: 16)!
        data.append(byte)
        i = j
    }
    return data
}

let ciphersuite = HPKE.Ciphersuite(
    kem: .Curve25519_HKDF_SHA256,
    kdf: .HKDF_SHA256,
    aead: .AES_GCM_128
)

/// The production domain label matching pm_protocol::gateway::HPKE_DOMAIN.
let hpkeDomain = Data("pm-push-hpke-v1".utf8)

func parseInfo(_ args: [String]) -> Data {
    if let idx = args.firstIndex(of: "--info"), idx + 1 < args.count {
        return hexDecode(args[idx + 1])
    }
    return hpkeDomain
}

// ── Commands ───────────────────────────────────────────────────────

let args = CommandLine.arguments

guard args.count >= 2 else {
    print("Usage: swift hpke_interop.swift <generate|open|seal|selftest> [args...]")
    exit(1)
}

switch args[1] {
case "generate":
    let info = parseInfo(args)
    let sk = Curve25519.KeyAgreement.PrivateKey()
    let pk = sk.publicKey
    let plaintext = Data("hello from CryptoKit HPKE".utf8)

    var sender = try! HPKE.Sender(
        recipientKey: pk, ciphersuite: ciphersuite, info: info)
    let ct = try! sender.seal(plaintext)
    let enc = sender.encapsulatedKey

    print("SK=\(hexEncode(sk.rawRepresentation))")
    print("PK=\(hexEncode(pk.rawRepresentation))")
    print("ENC=\(hexEncode(enc))")
    print("CT=\(hexEncode(ct))")
    print("PLAINTEXT=\(hexEncode(plaintext))")

case "open":
    guard args.count >= 5 else {
        print("Usage: open <sk_hex> <enc_hex> <ct_hex> [--info <hex>]")
        exit(1)
    }
    let info = parseInfo(args)
    let sk = try! Curve25519.KeyAgreement.PrivateKey(
        rawRepresentation: hexDecode(args[2]))

    var recipient = try! HPKE.Recipient(
        privateKey: sk, ciphersuite: ciphersuite,
        info: info, encapsulatedKey: hexDecode(args[3]))

    do {
        let plaintext = try recipient.open(hexDecode(args[4]))
        print("OK=\(hexEncode(plaintext))")
        print("PLAINTEXT_UTF8=\(String(data: plaintext, encoding: .utf8) ?? "<non-utf8>")")
    } catch {
        print("FAIL=\(error)")
        exit(2)
    }

case "seal":
    guard args.count >= 4 else {
        print("Usage: seal <pk_hex> <plaintext_hex> [--info <hex>]")
        exit(1)
    }
    let info = parseInfo(args)
    let pk = try! Curve25519.KeyAgreement.PublicKey(
        rawRepresentation: hexDecode(args[2]))

    var sender = try! HPKE.Sender(
        recipientKey: pk, ciphersuite: ciphersuite, info: info)
    let ct = try! sender.seal(hexDecode(args[3]))
    let enc = sender.encapsulatedKey

    print("ENC=\(hexEncode(enc))")
    print("CT=\(hexEncode(ct))")

case "selftest":
    selftest()

default:
    print("Unknown command: \(args[1])")
    exit(1)
}

// ── Self-test ──────────────────────────────────────────────────────

func selftest() {
    let sk = Curve25519.KeyAgreement.PrivateKey()
    let pk = sk.publicKey
    var pass = 0

    // 1. Round-trip with production info
    do {
        var s = try! HPKE.Sender(
            recipientKey: pk, ciphersuite: ciphersuite, info: hpkeDomain)
        let ct = try! s.seal(Data("round-trip".utf8))
        let enc = s.encapsulatedKey

        var r = try! HPKE.Recipient(
            privateKey: sk, ciphersuite: ciphersuite,
            info: hpkeDomain, encapsulatedKey: enc)
        let pt = try! r.open(ct)
        assert(String(data: pt, encoding: .utf8) == "round-trip")
        pass += 1
        print("PASS  round-trip with pm-push-hpke-v1 info")
    }

    // 2. Sealed notification JSON
    do {
        let json = Data("""
        {"title":"Session needs input","body":"Agent waiting",\
        "controller_id":"c","session_id":42,"state":"needs-input",\
        "event_id":"e","deep_link":"puppetmaster://c/session/42",\
        "timestamp_unix_ms":1700000000000,"counter":1}
        """.utf8)

        var s = try! HPKE.Sender(
            recipientKey: pk, ciphersuite: ciphersuite, info: hpkeDomain)
        let ct = try! s.seal(json)
        let enc = s.encapsulatedKey

        // Build wire format: enc || ciphertext
        var sealed = Data()
        sealed.append(enc)
        sealed.append(ct)

        // Decrypt as the NSE would
        let decEnc = sealed.prefix(32)
        let decCt = sealed.dropFirst(32)
        var r = try! HPKE.Recipient(
            privateKey: sk, ciphersuite: ciphersuite,
            info: hpkeDomain, encapsulatedKey: decEnc)
        let decrypted = try! r.open(decCt)
        let parsed = try! JSONSerialization.jsonObject(with: decrypted) as! [String: Any]
        assert(parsed["title"] as! String == "Session needs input")
        assert(parsed["counter"] as! Int == 1)
        pass += 1
        print("PASS  sealed notification JSON round-trip")
    }

    // 3. Wrong key fails
    do {
        var s = try! HPKE.Sender(
            recipientKey: pk, ciphersuite: ciphersuite, info: hpkeDomain)
        let ct = try! s.seal(Data("secret".utf8))
        let enc = s.encapsulatedKey

        let wrongSk = Curve25519.KeyAgreement.PrivateKey()
        var r = try! HPKE.Recipient(
            privateKey: wrongSk, ciphersuite: ciphersuite,
            info: hpkeDomain, encapsulatedKey: enc)
        let result = try? r.open(ct)
        assert(result == nil, "wrong key must fail")
        pass += 1
        print("PASS  wrong key fails to decrypt")
    }

    // 4. Wrong info fails (domain separation is real)
    do {
        var s = try! HPKE.Sender(
            recipientKey: pk, ciphersuite: ciphersuite, info: hpkeDomain)
        let ct = try! s.seal(Data("domain-test".utf8))
        let enc = s.encapsulatedKey

        let wrongInfo = Data("pm-push-hpke-v2".utf8)
        var r = try! HPKE.Recipient(
            privateKey: sk, ciphersuite: ciphersuite,
            info: wrongInfo, encapsulatedKey: enc)
        let result = try? r.open(ct)
        assert(result == nil, "mismatched info must fail")
        pass += 1
        print("PASS  mismatched info (v2) fails — domain separation is bound")
    }

    // 5. Empty info fails
    do {
        var s = try! HPKE.Sender(
            recipientKey: pk, ciphersuite: ciphersuite, info: hpkeDomain)
        let ct = try! s.seal(Data("empty-info-test".utf8))
        let enc = s.encapsulatedKey

        var r = try! HPKE.Recipient(
            privateKey: sk, ciphersuite: ciphersuite,
            info: Data(), encapsulatedKey: enc)
        let result = try? r.open(ct)
        assert(result == nil, "empty info must fail")
        pass += 1
        print("PASS  empty info fails — info is not ignored")
    }

    print("\n\(pass)/5 tests passed")
    if pass != 5 { exit(1) }
}
