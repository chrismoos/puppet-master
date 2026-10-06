// Expo native module: generates and persists an X25519 keypair for
// HPKE-sealed push notifications. The private key lives in the shared
// Keychain so the Notification Service Extension can read it. It also
// reports the signed `aps-environment` entitlement, which is the only
// reliable indicator of which APNs host will accept the device token.

import ExpoModulesCore
import CryptoKit
import Security

private let appGroupId = "group.com.tech9.puppetmaster"
private let keychainService = "com.tech9.puppetmaster.push-crypto"
private let keychainAccount = "hpke-x25519-private-key"

public class PushCryptoModule: Module {
    public func definition() -> ModuleDefinition {
        Name("PushCrypto")

        AsyncFunction("getPublicKey") { () -> String in
            let sk = try getOrCreatePrivateKey()
            return Data(sk.publicKey.rawRepresentation).base64EncodedString()
        }

        Function("getApsEnvironment") { () -> String? in
            provisionedApsEnvironment
        }

        Function("resetPushCounter") { () -> Void in
            UserDefaults(suiteName: appGroupId)?.removeObject(forKey: "pm_push_counter")
        }

        // console.log does not reach os_log in a Release build, so a
        // notification tap on a device leaves no trace of which branch
        // the routing took. NSLog does reach it, alongside the
        // extension's own [PM-NSE] lines.
        Function("log") { (message: String) -> Void in
            NSLog("%@", message)
        }
    }
}

/// The `aps-environment` entitlement granted by the provisioning profile
/// the app was signed with, or nil when the bundle carries no profile.
///
/// A Release build signed with a development profile still mints sandbox
/// tokens, so the build configuration cannot stand in for this. Read once:
/// the profile cannot change while the app is running.
private let provisionedApsEnvironment: String? = readProvisionedApsEnvironment()

/// An App Store build carries no `embedded.mobileprovision`, so nil here
/// means the entitlement has to be inferred by the caller rather than read.
private func readProvisionedApsEnvironment() -> String? {
    guard
        let url = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
        let data = try? Data(contentsOf: url)
    else { return nil }

    // The profile is a CMS envelope wrapping an XML plist, and iOS has no
    // public CMS decoder, so the plist is sliced out of the signed blob.
    guard
        let start = data.range(of: Data("<?xml".utf8)),
        let end = data.range(of: Data("</plist>".utf8), in: start.lowerBound..<data.endIndex)
    else { return nil }

    guard
        let plist = try? PropertyListSerialization.propertyList(
            from: data[start.lowerBound..<end.upperBound], options: [], format: nil),
        let entitlements = (plist as? [String: Any])?["Entitlements"] as? [String: Any]
    else { return nil }

    return entitlements["aps-environment"] as? String
}

private func getOrCreatePrivateKey() throws -> Curve25519.KeyAgreement.PrivateKey {
    if let existing = loadPrivateKey() {
        return existing
    }
    let sk = Curve25519.KeyAgreement.PrivateKey()
    try storePrivateKey(sk)
    return sk
}

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

/// This device only, so the key does not travel in an encrypted backup. It
/// decrypts every push this device is sent, and a restore of that backup onto
/// another device has no business being able to read them. After first unlock
/// rather than when unlocked, because the Notification Service Extension has
/// to open a push that arrives while the phone is locked.
private let keyAccessibility = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly

private func storePrivateKey(_ sk: Curve25519.KeyAgreement.PrivateKey) throws {
    let attrs: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: keychainService,
        kSecAttrAccount as String: keychainAccount,
        kSecAttrAccessGroup as String: appGroupId,
        kSecValueData as String: sk.rawRepresentation,
        kSecAttrAccessible as String: keyAccessibility,
    ]
    let status = SecItemAdd(attrs as CFDictionary, nil)
    if status == errSecDuplicateItem {
        // A device that enrolled before this was device-only still carries the
        // wider attribute, and a key is stored once, so nothing would ever
        // narrow it. Narrow it in place instead of leaving every existing
        // install on the old one.
        narrowExistingKeyAccessibility()
        return
    }
    guard status == errSecSuccess else {
        throw NSError(
            domain: "PushCrypto", code: Int(status),
            userInfo: [NSLocalizedDescriptionKey: "Keychain store failed: \(status)"])
    }
}

private func narrowExistingKeyAccessibility() {
    let query: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: keychainService,
        kSecAttrAccount as String: keychainAccount,
        kSecAttrAccessGroup as String: appGroupId,
    ]
    let update: [String: Any] = [kSecAttrAccessible as String: keyAccessibility]
    SecItemUpdate(query as CFDictionary, update as CFDictionary)
}
