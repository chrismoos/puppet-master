// Notification Service Extension: decrypts HPKE-sealed push
// notifications and rewrites the alert with the real content.
//
// Ciphersuite: DHKEM(X25519, HKDF-SHA256) + HKDF-SHA256 + AES-128-GCM
// RFC 9180 Base mode.
//
// The ciphersuite, domain label, and HPKE open call are lifted from
// tests/hpke-interop/hpke_interop.swift, which has been proven
// byte-for-byte against the Rust hpke crate by run.sh.
//
// Wire format: enc (32 bytes) || ciphertext (plaintext.len() + 16 tag)
// Info parameter: "pm-push-hpke-v1" (matches pm_protocol::gateway::HPKE_DOMAIN)

import UserNotifications
import CryptoKit
import Security
private let appGroupId = "group.com.tech9.puppetmaster"
private let keychainService = "com.tech9.puppetmaster.push-crypto"
private let keychainAccount = "hpke-x25519-private-key"
private let counterKey = "pm_push_counter"

// Matches pm_protocol::gateway::PUSH_DATA_KEY and SEALED_PAYLOAD_KEY.
private let dataKey = "pm"
private let sealedPayloadKey = "pm_sealed"

// expo-notifications serialises a remote notification's content.data
// from userInfo["body"], while the app's other read path takes the
// whole userInfo. Writing both keeps either working.
private let expoDataKey = "body"

// Lifted from hpke_interop.swift lines 38-42.
private let ciphersuite = HPKE.Ciphersuite(
    kem: .Curve25519_HKDF_SHA256,
    kdf: .HKDF_SHA256,
    aead: .AES_GCM_128
)

// Lifted from hpke_interop.swift line 45.
// Matches pm_protocol::gateway::HPKE_DOMAIN (b"pm-push-hpke-v1").
private let hpkeDomain = Data("pm-push-hpke-v1".utf8)

class NotificationService: UNNotificationServiceExtension {
    private var contentHandler: ((UNNotificationContent) -> Void)?
    private var bestAttempt: UNMutableNotificationContent?

    override func didReceive(
        _ request: UNNotificationRequest,
        withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
    ) {
        self.contentHandler = contentHandler
        let content = request.content.mutableCopy() as! UNMutableNotificationContent
        self.bestAttempt = content

        // The APNs payload is { "aps": ..., "pm": { "pm_sealed": "<base64>" } }.
        // Everything else about the session is inside the seal.
        guard let pm = request.content.userInfo[dataKey] as? [String: Any],
              let sealedB64 = pm[sealedPayloadKey] as? String,
              let sealedData = Data(base64Encoded: sealedB64) else {
            contentHandler(content)
            return
        }

        guard let sk = loadPrivateKey() else {
            NSLog("[PM-NSE] loadPrivateKey failed")
            contentHandler(content)
            return
        }

        guard let plaintext = hpkeOpen(privateKey: sk, sealed: sealedData) else {
            NSLog("[PM-NSE] hpkeOpen decryption failed")
            contentHandler(content)
            return
        }

        // The plaintext is a SealedNotification JSON
        // (pm_protocol::gateway::SealedNotification).
        guard let json = try? JSONSerialization.jsonObject(with: plaintext)
                as? [String: Any] else {
            contentHandler(content)
            return
        }

        // Replay rejection: the counter must be strictly increasing.
        // The controller increments atomically per device
        // (push.rs:876, increment_push_counter).
        if let counter = (json["counter"] as? NSNumber)?.intValue {
            let defaults = UserDefaults(suiteName: appGroupId)
            let lastCounter = defaults?.integer(forKey: counterKey) ?? 0
            if counter <= lastCounter {
                contentHandler(content)
                return
            }
            defaults?.set(counter, forKey: counterKey)
        }

        // Rewrite the notification with the decrypted content.
        if let title = json["title"] as? String, !title.isEmpty {
            content.title = title
        }
        if let body = json["body"] as? String {
            content.body = body
        }

        // The cleartext payload carries no routing, so without this the
        // app has no session to open when the user taps.
        let routing = routingFields(from: json)
        var userInfo = content.userInfo
        userInfo[dataKey] = routing
        userInfo[expoDataKey] = [dataKey: routing]
        content.userInfo = userInfo

        contentHandler(content)
    }

    override func serviceExtensionTimeWillExpire() {
        if let handler = contentHandler, let content = bestAttempt {
            handler(content)
        }
    }
}

// ── Routing ──────────────────────────────────────────────────────────
//
// Key names are the app's, not the sealed JSON's: pushRoute.ts reads
// camelCase, and session_id is a number inside the seal but must be a
// string by the time resolveNotificationRoute compares it.

private func routingFields(from json: [String: Any]) -> [String: Any] {
    var routing: [String: Any] = [:]
    if let sessionId = json["session_id"] as? NSNumber {
        routing["sessionId"] = sessionId.stringValue
    } else if let sessionId = json["session_id"] as? String {
        routing["sessionId"] = sessionId
    }
    if let controllerId = json["controller_id"] as? String {
        routing["controllerId"] = controllerId
    }
    if let state = json["state"] as? String {
        routing["state"] = state
    }
    if let eventId = json["event_id"] as? String {
        routing["eventId"] = eventId
    }
    if let deepLink = json["deep_link"] as? String {
        routing["url"] = deepLink
    }
    // Only the decrypted record carries approval_id, so the tap's approvalId never existed outside the seal.
    if let approvalId = json["approval_id"] as? String, !approvalId.isEmpty {
        routing["approvalId"] = approvalId
    }
    return routing
}

// ── HPKE open ─────────────────────────────────────────────────────────
//
// Lifted from hpke_interop.swift lines 87-101 (the "open" command).
// The ciphersuite, info parameter, and Recipient construction are
// identical to the interop harness. The wire format (enc || ciphertext)
// matches pm_push::hpke::open (hpke.rs:50-54).

private func hpkeOpen(
    privateKey sk: Curve25519.KeyAgreement.PrivateKey,
    sealed: Data
) -> Data? {
    guard sealed.count >= 32 + 16 else { return nil }
    let enc = sealed.prefix(32)
    let ct = sealed.dropFirst(32)

    do {
        var recipient = try HPKE.Recipient(
            privateKey: sk, ciphersuite: ciphersuite,
            info: hpkeDomain, encapsulatedKey: enc)
        return try recipient.open(ct)
    } catch {
        return nil
    }
}

// ── Shared keychain ───────────────────────────────────────────────────
//
// Same query as PushCryptoModule.swift: service, account, and access
// group must match exactly or the extension cannot find the key the
// app generated.

private func loadPrivateKey() -> Curve25519.KeyAgreement.PrivateKey? {
    let query: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: keychainService,
        kSecAttrAccount as String: keychainAccount,
        kSecAttrAccessGroup as String: appGroupId,
        kSecReturnData as String: true,
        kSecMatchLimit as String: kSecMatchLimitOne,
    ]
    var result: AnyObject?
    let status = SecItemCopyMatching(query as CFDictionary, &result)
    guard status == errSecSuccess, let data = result as? Data else {
        return nil
    }
    return try? Curve25519.KeyAgreement.PrivateKey(rawRepresentation: data)
}
